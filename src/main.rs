//! griasdi-gstreamer: builds GStreamer and its C dependencies from source for
//! griasdi, and packages the result with its corresponding source.
//!
//! - `build [--target <triple>]`: the install prefix of the target (the host
//!   without `--target`) in `target/gstreamer/<triple>/`, built if it is not up
//!   to date (`build.rs`) and tidied, with the license texts of its components
//!   (`finish.rs`).
//! - `dist [--target <triple>] [--no-build] --out <dir>`: `build`, then the two
//!   archives of the release into `<dir>` (`dist.rs`): the runtime and link
//!   parts staged out of the prefix (`stage.rs`), and the source. With
//!   `--no-build` the prefix is taken as it is, finished or not up to date,
//!   and is never built.
//! - `release-id`: the release id, `<GStreamer tag>-<recipe revision>`.
//! - `check-tag <tag>`: succeeds if the tag is `v` plus the release id; the CI
//!   refuses any other.
//!
//! The targets are `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
//! `x86_64-pc-windows-msvc` and `aarch64-apple-darwin`. Each is built
//! natively, on its own platform; `aarch64-unknown-linux-gnu` on x86_64 is the
//! cross build the recipe supports besides.

mod build;
mod dist;
mod finish;
mod licenses;
mod macos;
mod notice;
mod patches;
mod pe;
mod pins;
mod pkgconfig;
mod plan;
mod release;
mod stage;
mod tools;
mod util;

use std::path::PathBuf;

use anyhow::Result;

const USAGE: &str = "usage: griasdi-gstreamer <build [--target <triple>] \
    | dist [--target <triple>] [--no-build] --out <dir> | release-id | check-tag <tag>>";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Build {
        target: Option<String>,
    },
    Dist {
        target: Option<String>,
        no_build: bool,
        out: PathBuf,
    },
    ReleaseId,
    CheckTag(String),
}

/// The command in `args` (without the program name), or what is wrong with
/// them.
fn parse(args: &[String]) -> Result<Command, String> {
    let (command, rest) = args.split_first().ok_or("no command")?;
    match command.as_str() {
        "build" => {
            let flags = Flags::parse(rest)?;
            if flags.out.is_some() {
                return Err("build takes no --out".to_owned());
            }
            if flags.no_build {
                return Err("build takes no --no-build".to_owned());
            }
            Ok(Command::Build {
                target: flags.target,
            })
        }
        "dist" => {
            let flags = Flags::parse(rest)?;
            Ok(Command::Dist {
                target: flags.target,
                no_build: flags.no_build,
                out: flags.out.ok_or("dist needs --out <dir>")?,
            })
        }
        "release-id" if rest.is_empty() => Ok(Command::ReleaseId),
        "check-tag" => match rest {
            [tag] => Ok(Command::CheckTag(tag.clone())),
            _ => Err("check-tag takes the tag".to_owned()),
        },
        other => Err(format!("unknown command {other}")),
    }
}

/// The flags of `build` and `dist`.
struct Flags {
    target: Option<String>,
    out: Option<PathBuf>,
    no_build: bool,
}

impl Flags {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut flags = Flags {
            target: None,
            out: None,
            no_build: false,
        };
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            let value = |args: &mut std::slice::Iter<String>| {
                args.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match flag.as_str() {
                "--target" if flags.target.is_none() => flags.target = Some(value(&mut args)?),
                "--out" if flags.out.is_none() => {
                    flags.out = Some(PathBuf::from(value(&mut args)?))
                }
                "--no-build" if !flags.no_build => flags.no_build = true,
                other => return Err(format!("unexpected argument {other}")),
            }
        }
        Ok(flags)
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse(&args) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            std::process::exit(2);
        }
    };
    match command {
        Command::Build { target } => {
            let layout = build::build(target.as_deref())?;
            eprintln!("GStreamer is in {}", layout.prefix.display());
        }
        Command::Dist {
            target,
            no_build,
            out,
        } => dist::dist(target.as_deref(), no_build, &out)?,
        Command::ReleaseId => println!("{}", release::release_id()),
        Command::CheckTag(tag) => release::check_tag(&tag)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn build_and_dist_take_a_target_and_dist_needs_an_out_directory() {
        assert_eq!(
            parse(&args(&["build"])),
            Ok(Command::Build { target: None })
        );
        assert_eq!(
            parse(&args(&["build", "--target", "aarch64-apple-darwin"])),
            Ok(Command::Build {
                target: Some("aarch64-apple-darwin".to_owned())
            })
        );
        assert_eq!(
            parse(&args(&["dist", "--out", "/tmp/d", "--target", "x"])),
            Ok(Command::Dist {
                target: Some("x".to_owned()),
                no_build: false,
                out: PathBuf::from("/tmp/d")
            })
        );
        assert!(parse(&args(&["dist"])).is_err());
        assert!(parse(&args(&["dist", "--target", "x"])).is_err());
        assert!(parse(&args(&["build", "--out", "/tmp/d"])).is_err());
    }

    #[test]
    fn dist_can_take_the_prefix_as_it_is_and_build_cannot() {
        assert_eq!(
            parse(&args(&["dist", "--no-build", "--out", "/tmp/d"])),
            Ok(Command::Dist {
                target: None,
                no_build: true,
                out: PathBuf::from("/tmp/d")
            })
        );
        assert!(parse(&args(&["build", "--no-build"])).is_err());
        assert!(parse(&args(&["dist", "--no-build", "--no-build", "--out", "d"])).is_err());
    }

    #[test]
    fn a_flag_without_a_value_or_given_twice_is_refused() {
        assert!(parse(&args(&["build", "--target"])).is_err());
        assert!(parse(&args(&["build", "--target", "a", "--target", "b"])).is_err());
        assert!(parse(&args(&["build", "--tagret", "a"])).is_err());
        assert!(parse(&args(&["build", "x86_64-unknown-linux-gnu"])).is_err());
    }

    #[test]
    fn release_id_and_check_tag_take_exactly_what_they_need() {
        assert_eq!(parse(&args(&["release-id"])), Ok(Command::ReleaseId));
        assert!(parse(&args(&["release-id", "x"])).is_err());
        assert_eq!(
            parse(&args(&["check-tag", "v1.28.7-1"])),
            Ok(Command::CheckTag("v1.28.7-1".to_owned()))
        );
        assert!(parse(&args(&["check-tag"])).is_err());
        assert!(parse(&args(&["check-tag", "a", "b"])).is_err());
        assert!(parse(&[]).is_err());
        assert!(parse(&args(&["publish"])).is_err());
    }
}
