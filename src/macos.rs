//! macOS: the install names and rpaths of the prefix.
//!
//! `meson install` rewrites the install name of every dylib to its absolute
//! path in the prefix, and points every dependency at that path
//! (`fix_darwin` in meson's `mesonbuild/scripts/depfixer.py`). A prefix that
//! is unpacked somewhere else would name libraries that are not there. Here
//! every Mach-O file of the prefix gets `@rpath/<name>` as its own name and
//! for each dependency that lies in the prefix, and an rpath that leads from
//! the file to `lib/`; the absolute rpaths of the build go. A program that
//! links against the prefix names the libraries by their `@rpath` names and
//! brings the rpath of `<prefix>/lib` itself, which is what the `.pc` files
//! give it. The same rewrite is what bundles the libraries into an app
//! (griasdi's `relocate`), which accepts the files in either spelling.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use crate::util::run;

/// Where the system keeps its Swift runtime: an rpath of the build tools that
/// is no part of the build.
const SWIFT_RPATH: &str = "/usr/lib/swift";

/// Rewrites every Mach-O file of `prefix` as described above. `forms` are the
/// spellings of the directory the build ran in, whose paths must not be left
/// in any file. Idempotent.
pub fn relocate(prefix: &Path, forms: &[String]) -> Result<()> {
    let files = mach_o_files(prefix)?;
    let in_lib = |name: &str| prefix.join("lib").join(name).is_file();
    for file in &files {
        let rel = relative(prefix, file)?;
        let args = relocation_args(
            &rel,
            &dependencies(file)?,
            &parse_rpaths(&otool("-l", file)?),
            forms,
            in_lib,
        )?;
        if !args.is_empty() {
            run(Command::new("install_name_tool").args(&args).arg(file))?;
        }
    }
    for file in &files {
        let rel = relative(prefix, file)?;
        let rpaths = parse_rpaths(&otool("-l", file)?);
        let left = build_paths(&dependencies(file)?, &rpaths, forms);
        ensure!(
            left.is_empty(),
            "{rel} still refers to the build directory: {}",
            left.join(", ")
        );
    }
    eprintln!("relocated {} Mach-O files", files.len());
    Ok(())
}

/// `file` relative to `prefix`, with `/` as separator.
fn relative(prefix: &Path, file: &Path) -> Result<String> {
    let rel = file
        .strip_prefix(prefix)
        .with_context(|| format!("{} is not in {}", file.display(), prefix.display()))?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

/// The regular files below `lib`, `bin` and `libexec` that are Mach-O images.
fn mach_o_files(prefix: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut queue: Vec<PathBuf> = ["lib", "bin", "libexec"]
        .iter()
        .map(|dir| prefix.join(dir))
        .filter(|dir| dir.is_dir())
        .collect();
    while let Some(dir) = queue.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                queue.push(entry.path());
            } else if kind.is_file() && is_mach_o(&entry.path())? {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

fn is_mach_o(path: &Path) -> Result<bool> {
    let mut magic = [0u8; 4];
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(file.read_exact(&mut magic).is_ok() && is_mach_o_magic(magic))
}

/// The magic numbers of 32 and 64 bit Mach-O images (either byte order) and
/// of fat files, which are universal binaries here: nothing else in a prefix
/// starts like that.
fn is_mach_o_magic(magic: [u8; 4]) -> bool {
    matches!(
        magic,
        [0xCF, 0xFA, 0xED, 0xFE]
            | [0xCE, 0xFA, 0xED, 0xFE]
            | [0xFE, 0xED, 0xFA, 0xCF]
            | [0xFE, 0xED, 0xFA, 0xCE]
            | [0xCA, 0xFE, 0xBA, 0xBE]
    )
}

/// The `install_name_tool` arguments that relocate one file, `rel` below the
/// prefix, whose `otool -L` list is `dependencies` and whose rpaths are
/// `rpaths`; `in_lib` says whether a file name is a file of the prefix's
/// `lib`. Empty when the file is relocated already. A dependency that is
/// neither the system's nor in the prefix is an error: the prefix would not
/// run where only it is installed.
fn relocation_args(
    rel: &str,
    dependencies: &[String],
    rpaths: &[String],
    forms: &[String],
    in_lib: impl Fn(&str) -> bool,
) -> Result<Vec<String>> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let mut args: Vec<String> = Vec::new();
    // `otool -L` lists a dylib's own install name first.
    let own = dependencies.iter().find(|d| file_name(d) == name);
    if name.ends_with(".dylib") {
        let wanted = format!("@rpath/{name}");
        if own.is_none_or(|own| *own != wanted) {
            args.extend(["-id".to_owned(), wanted]);
        }
    }
    let mut done: BTreeSet<&str> = BTreeSet::new();
    for dependency in dependencies.iter().filter(|d| Some(*d) != own) {
        if dependency.starts_with("/usr/lib/")
            || dependency.starts_with("/System/Library/")
            || dependency.starts_with("@rpath/")
        {
            continue;
        }
        if !starts_with_any(dependency, forms) {
            bail!("{rel} depends on {dependency}, which a relocated prefix cannot find");
        }
        let file = file_name(dependency);
        ensure!(
            in_lib(file),
            "{rel} depends on {dependency}, which is not in the prefix's lib"
        );
        if done.insert(dependency) {
            args.extend([
                "-change".to_owned(),
                dependency.clone(),
                format!("@rpath/{file}"),
            ]);
        }
    }
    for rpath in rpaths {
        if rpath.starts_with('/') && rpath != SWIFT_RPATH {
            args.extend(["-delete_rpath".to_owned(), rpath.clone()]);
        }
    }
    let wanted = rpath_to_lib(rel);
    if !rpaths.contains(&wanted) {
        args.extend(["-add_rpath".to_owned(), wanted]);
    }
    Ok(args)
}

/// The dependencies and rpaths that still lead into the build directory.
fn build_paths(dependencies: &[String], rpaths: &[String], forms: &[String]) -> Vec<String> {
    dependencies
        .iter()
        .chain(rpaths)
        .filter(|path| starts_with_any(path, forms))
        .cloned()
        .collect()
}

fn starts_with_any(path: &str, forms: &[String]) -> bool {
    forms.iter().any(|form| path.starts_with(form.as_str()))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Where `@rpath` has to lead for the file at `relative` (below the prefix):
/// to `lib`, from wherever the file lies.
fn rpath_to_lib(relative: &str) -> String {
    let up: String = Path::new(relative)
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .map(|_| "../")
        .collect();
    format!("@loader_path/{up}lib")
}

/// The install names `file` depends on.
fn dependencies(file: &Path) -> Result<Vec<String>> {
    Ok(parse_otool(&otool("-L", file)?))
}

/// `otool <flag> <file>`.
fn otool(flag: &str, file: &Path) -> Result<String> {
    let output = Command::new("otool")
        .arg(flag)
        .arg(file)
        .output()
        .context("failed to run otool")?;
    ensure!(
        output.status.success(),
        "otool {flag} {} failed",
        file.display()
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The install names in the output of `otool -L`: the tab-indented lines.
fn parse_otool(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| line.starts_with('\t'))
        .filter_map(|line| {
            let line = line.trim();
            Some(line.rsplit_once(" (compatibility")?.0.to_owned())
        })
        .collect()
}

/// The `LC_RPATH` paths in the output of `otool -l`.
fn parse_rpaths(output: &str) -> Vec<String> {
    let mut rpaths = Vec::new();
    let mut in_rpath = false;
    for line in output.lines().map(str::trim) {
        if let Some(command) = line.strip_prefix("cmd ") {
            in_rpath = command == "LC_RPATH";
        } else if in_rpath
            && let Some(path) = line.strip_prefix("path ")
            && let Some((path, _)) = path.rsplit_once(" (offset")
        {
            rpaths.push(path.to_owned());
            in_rpath = false;
        }
    }
    rpaths
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD: &str = "/ci/target/gstreamer";

    fn forms() -> Vec<String> {
        vec![BUILD.to_owned()]
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn rpath_leads_from_every_place_in_the_prefix_to_its_lib() {
        assert_eq!(
            rpath_to_lib("lib/libglib-2.0.0.dylib"),
            "@loader_path/../lib"
        );
        assert_eq!(
            rpath_to_lib("lib/gstreamer-1.0/libgstapp.dylib"),
            "@loader_path/../../lib"
        );
        assert_eq!(rpath_to_lib("bin/gst-launch-1.0"), "@loader_path/../lib");
        assert_eq!(
            rpath_to_lib("libexec/gstreamer-1.0/gst-plugin-scanner"),
            "@loader_path/../../lib"
        );
    }

    #[test]
    fn otool_output_lists_every_dependency() {
        let output = "\
plugin.dylib:
\t/ci/target/gstreamer/p/lib/libgstapp.dylib (compatibility version 0.0.0, current version 0.0.0)
\t@rpath/libgstbase-1.0.0.dylib (compatibility version 2807.0.0, current version 2807.7.0)
\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1351.0.0)
";
        assert_eq!(
            parse_otool(output),
            [
                "/ci/target/gstreamer/p/lib/libgstapp.dylib",
                "@rpath/libgstbase-1.0.0.dylib",
                "/usr/lib/libSystem.B.dylib"
            ]
        );
    }

    #[test]
    fn rpaths_are_read_from_the_load_commands_only() {
        let output = "\
Load command 12
          cmd LC_RPATH
      cmdsize 48
         path /ci/target/gstreamer/p/lib (offset 12)
Load command 13
          cmd LC_LOAD_DYLIB
         name /usr/lib/libSystem.B.dylib (offset 24)
Load command 14
          cmd LC_RPATH
         path @loader_path/../lib (offset 12)
";
        assert_eq!(
            parse_rpaths(output),
            ["/ci/target/gstreamer/p/lib", "@loader_path/../lib"]
        );
    }

    #[test]
    fn a_plugin_names_itself_and_its_libraries_by_rpath() {
        let args = relocation_args(
            "lib/gstreamer-1.0/libgstapp.dylib",
            &strings(&[
                "/ci/target/gstreamer/p/lib/gstreamer-1.0/libgstapp.dylib",
                "/ci/target/gstreamer/p/lib/libgstbase-1.0.0.dylib",
                "/ci/target/gstreamer/p/lib/libgstbase-1.0.0.dylib",
                "/usr/lib/libSystem.B.dylib",
            ]),
            &strings(&["/ci/target/gstreamer/p/lib", "/usr/lib/swift"]),
            &forms(),
            |name| name == "libgstbase-1.0.0.dylib",
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "-id",
                "@rpath/libgstapp.dylib",
                "-change",
                "/ci/target/gstreamer/p/lib/libgstbase-1.0.0.dylib",
                "@rpath/libgstbase-1.0.0.dylib",
                "-delete_rpath",
                "/ci/target/gstreamer/p/lib",
                "-add_rpath",
                "@loader_path/../../lib",
            ]
        );
    }

    #[test]
    fn a_relocated_file_needs_no_change() {
        let args = relocation_args(
            "lib/libgstbase-1.0.0.dylib",
            &strings(&[
                "@rpath/libgstbase-1.0.0.dylib",
                "@rpath/libglib-2.0.0.dylib",
                "/usr/lib/libSystem.B.dylib",
            ]),
            &strings(&["@loader_path/../lib"]),
            &forms(),
            |_| true,
        )
        .unwrap();
        assert!(args.is_empty(), "{args:?}");
    }

    #[test]
    fn an_executable_gets_an_rpath_but_no_install_name() {
        let args = relocation_args(
            "bin/gst-launch-1.0",
            &strings(&["/ci/target/gstreamer/p/lib/libgstreamer-1.0.0.dylib"]),
            &[],
            &forms(),
            |_| true,
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "-change",
                "/ci/target/gstreamer/p/lib/libgstreamer-1.0.0.dylib",
                "@rpath/libgstreamer-1.0.0.dylib",
                "-add_rpath",
                "@loader_path/../lib"
            ]
        );
    }

    #[test]
    fn a_dependency_outside_the_prefix_and_the_system_is_an_error() {
        let foreign = relocation_args(
            "lib/libx.dylib",
            &strings(&["@rpath/libx.dylib", "/opt/homebrew/lib/libfoo.dylib"]),
            &[],
            &forms(),
            |_| true,
        );
        assert!(foreign.is_err());
        let missing = relocation_args(
            "lib/libx.dylib",
            &strings(&["/ci/target/gstreamer/p/lib/libgone.dylib"]),
            &[],
            &forms(),
            |_| false,
        );
        assert!(missing.is_err());
    }

    #[test]
    fn only_mach_o_images_are_recognised() {
        assert!(is_mach_o_magic([0xCF, 0xFA, 0xED, 0xFE]));
        assert!(is_mach_o_magic([0xCA, 0xFE, 0xBA, 0xBE]));
        assert!(!is_mach_o_magic(*b"#!/b"));
        assert!(!is_mach_o_magic(*b"\x7fELF"));
    }
}
