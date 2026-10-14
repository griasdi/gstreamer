//! macOS: the libraries and plugins of the runtime of a release, relocated.
//!
//! The runtime is laid out as the app bundle it goes into (`Contents/lib`,
//! `Contents/lib/gstreamer-1.0`, `Contents/libexec/gstreamer-1.0`), where
//! GStreamer finds its plugins from the location of libgstreamer
//! (`priv_gst_get_relocated_libgstreamer` in `gst/gstregistry.c`). The build
//! names every library by its absolute path, which an installed app cannot
//! find, so every file is relocated: its own name and each dependency of the
//! build become `@rpath/<file>`, and its only `LC_RPATH` leads from the file
//! to `Contents/lib` (`@loader_path`, `@loader_path/..`,
//! `@loader_path/../../lib`). Nothing of the build machine is left in the
//! names and rpaths, which [`verify`] checks.
//!
//! Which files to copy is computed, not listed: the libraries a plugin
//! depends on are read from `otool -L` and followed transitively.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use crate::util::run;

#[derive(Debug, PartialEq, Eq)]
enum Dependency {
    /// Part of macOS.
    System,
    /// A file of the build, by its name in `lib`.
    Bundled(String),
    Foreign,
}

/// What a dependency of a Mach-O file is, given the `lib` of the build. The
/// build names its libraries by their absolute path there, and what
/// [`relocation_args`] has rewritten is named `@rpath/<file>`: both are
/// bundled. Anything else, such as a library of Homebrew or of an installed
/// GStreamer framework, is foreign. `in_lib` says whether a file name is a
/// file of that `lib`.
fn classify_dependency(name: &str, in_lib: impl Fn(&str) -> bool) -> Dependency {
    if name.starts_with("/usr/lib/") || name.starts_with("/System/Library/") {
        return Dependency::System;
    }
    match Path::new(name).file_name().and_then(|n| n.to_str()) {
        Some(file) if in_lib(file) => Dependency::Bundled(file.to_owned()),
        _ => Dependency::Foreign,
    }
}

/// File names in `lib` of the libraries `roots` depend on, transitively.
/// `dependencies` gives the install names a file depends on and `lib_file` the
/// path of a file name in `lib`, if it is there. A dependency outside `lib`,
/// the system's `/usr/lib` and `/System/Library` would make the app depend on
/// an installed framework, so it is an error.
pub fn closure(
    roots: &[PathBuf],
    dependencies: impl Fn(&Path) -> Result<Vec<String>>,
    lib_file: impl Fn(&str) -> Option<PathBuf>,
) -> Result<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    let mut queue: Vec<PathBuf> = roots.to_vec();
    while let Some(path) = queue.pop() {
        for dependency in dependencies(&path)? {
            // `otool -L` lists a dylib's own install name first; a plugin's
            // lies outside `lib` and would count as foreign.
            if is_itself(&dependency, &path) {
                continue;
            }
            match classify_dependency(&dependency, |file| lib_file(file).is_some()) {
                Dependency::System => {}
                Dependency::Bundled(name) => {
                    if found.contains(&name) {
                        continue;
                    }
                    let file = lib_file(&name)
                        .with_context(|| format!("{} needs {dependency}", path.display()))?;
                    found.insert(name);
                    queue.push(file);
                }
                Dependency::Foreign => bail!(
                    "{} depends on {dependency}, which a relocated app cannot find",
                    path.display()
                ),
            }
        }
    }
    Ok(found)
}

/// Whether an install name is the one of `file` itself, which `otool -L`
/// lists first for a library.
fn is_itself(name: &str, file: &Path) -> bool {
    Path::new(name).file_name() == file.file_name()
}

/// Where `@rpath` has to lead for the file at `relative` (a path below
/// `Contents`): to `Contents/lib`, from wherever the file lies.
pub fn rpath_to_lib(relative: &str) -> String {
    let dirs: Vec<_> = Path::new(relative)
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .collect();
    match dirs.split_first() {
        // In `lib` or below it: up to `lib` itself.
        Some((first, below)) if first.as_os_str() == "lib" => {
            format!("@loader_path{}", "/..".repeat(below.len()))
        }
        _ => format!("@loader_path{}/lib", "/..".repeat(dirs.len())),
    }
}

/// The `install_name_tool` arguments that relocate one file, `rel` below
/// `Contents`, whose `otool -L` list is `dependencies` and whose rpaths are
/// `rpaths`; `in_lib` says whether a file name is a file of the build's
/// `lib`. Empty when the file is relocated already. A file that has an rpath
/// of the build machine, or of an earlier relocation that leads elsewhere,
/// loses it: [`rpath_to_lib`] is the only one that stays.
fn relocation_args(
    rel: &str,
    dependencies: &[String],
    rpaths: &[String],
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
        match classify_dependency(dependency, &in_lib) {
            Dependency::System => {}
            Dependency::Bundled(file) => {
                let wanted = format!("@rpath/{file}");
                if *dependency != wanted && done.insert(dependency) {
                    args.extend(["-change".to_owned(), dependency.clone(), wanted]);
                }
            }
            Dependency::Foreign => {
                bail!("{rel} depends on {dependency}, which a relocated app cannot find")
            }
        }
    }
    let wanted = rpath_to_lib(rel);
    let mut deleted: BTreeSet<&str> = BTreeSet::new();
    for rpath in rpaths {
        if *rpath != wanted && deleted.insert(rpath) {
            args.extend(["-delete_rpath".to_owned(), rpath.clone()]);
        }
    }
    if !rpaths.contains(&wanted) {
        args.extend(["-add_rpath".to_owned(), wanted]);
    }
    Ok(args)
}

/// What is still wrong with the relocation of the file `rel` below
/// `Contents`, whose `otool -L` list is `dependencies` and whose rpaths are
/// `rpaths`; `in_runtime` says whether a file name is a library of the
/// runtime. Nothing is wrong when the list is empty: the file names itself and
/// everything it needs of the runtime by `@rpath/<file>`, needs nothing else
/// but macOS, and has the one rpath that leads to `Contents/lib`.
fn check_relocated(
    rel: &str,
    dependencies: &[String],
    rpaths: &[String],
    in_runtime: impl Fn(&str) -> bool,
) -> Vec<String> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let mut problems = Vec::new();
    let own = dependencies.iter().find(|d| file_name(d) == name);
    if name.ends_with(".dylib") && own.is_none_or(|own| *own != format!("@rpath/{name}")) {
        problems.push(format!(
            "its install name is {}, not @rpath/{name}",
            own.map_or("missing", String::as_str)
        ));
    }
    for dependency in dependencies.iter().filter(|d| Some(*d) != own) {
        let ok = dependency.starts_with("/usr/lib/")
            || dependency.starts_with("/System/Library/")
            || dependency
                .strip_prefix("@rpath/")
                .is_some_and(|file| !file.contains('/') && in_runtime(file));
        if !ok {
            problems.push(format!("it depends on {dependency}"));
        }
    }
    let wanted = rpath_to_lib(rel);
    if rpaths != [wanted.clone()] {
        problems.push(format!(
            "its rpaths are [{}], not [{wanted}]",
            rpaths.join(", ")
        ));
    }
    problems
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Copies `from` to `to` (`rel` below `Contents`) and relocates the copy. `lib`
/// is the `lib` of the build the dependencies are looked up in.
pub fn install_relocated(from: &Path, to: &Path, rel: &str, lib: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::copy(from, to).with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
    let args = relocation_args(rel, &dependencies(to)?, &rpaths(to)?, |file| {
        lib.join(file).is_file()
    })?;
    if args.is_empty() {
        return Ok(());
    }
    run(Command::new("install_name_tool").args(&args).arg(to))
}

/// Fails when the relocated file `file` (`rel` below `Contents`) is not
/// relocated as [`check_relocated`] describes.
pub fn verify(file: &Path, rel: &str, in_runtime: impl Fn(&str) -> bool) -> Result<()> {
    let problems = check_relocated(rel, &dependencies(file)?, &rpaths(file)?, in_runtime);
    ensure!(
        problems.is_empty(),
        "{rel} is not relocated: {}",
        problems.join("; ")
    );
    Ok(())
}

/// The file the library `<name>` (`-l<name>`) is in `lib`: `lib<name>.dylib`,
/// which the build makes a link to the file that carries the version
/// (`libglib-2.0.dylib` to `libglib-2.0.0.dylib`). `None` when there is no
/// such library, which then is a static one or the system's.
pub fn dylib_file(lib: &Path, name: &str) -> Result<Option<String>> {
    let path = lib.join(format!("lib{name}.dylib"));
    if !path.exists() {
        return Ok(None);
    }
    let real = fs::canonicalize(&path).with_context(|| format!("resolve {}", path.display()))?;
    Ok(real.file_name().and_then(|n| n.to_str()).map(str::to_owned))
}

/// The word that links the library in the file `file` (`libglib-2.0.0.dylib`):
/// `-lglib-2.0.0`.
pub fn link_word(file: &str) -> String {
    let name = file.strip_prefix("lib").unwrap_or(file);
    format!("-l{}", name.strip_suffix(".dylib").unwrap_or(name))
}

/// The install names `file` depends on.
pub fn dependencies(file: &Path) -> Result<Vec<String>> {
    Ok(parse_otool(&otool("-L", file)?))
}

/// The `LC_RPATH` paths of `file`.
pub fn rpaths(file: &Path) -> Result<Vec<String>> {
    Ok(parse_rpaths(&otool("-l", file)?))
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
    use std::collections::HashMap;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn otool_output_lists_every_dependency() {
        let output = "\
plugin.dylib:
\t@rpath/libgstsoup.dylib (compatibility version 0.0.0, current version 0.0.0)
\t@rpath/libglib-2.0.0.dylib (compatibility version 8201.0.0, current version 8201.4.0)
\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1356.0.0)
";
        assert_eq!(
            parse_otool(output),
            [
                "@rpath/libgstsoup.dylib",
                "@rpath/libglib-2.0.0.dylib",
                "/usr/lib/libSystem.B.dylib",
            ]
        );
    }

    #[test]
    fn reads_rpaths_from_otool() {
        let output = "\
Load command 12
          cmd LC_RPATH
      cmdsize 48
         path /Library/Frameworks/GStreamer.framework/Versions/1.0/lib (offset 12)
Load command 13
          cmd LC_LOAD_DYLIB
         name @rpath/libx.dylib (offset 24)
Load command 14
          cmd LC_RPATH
         path @executable_path/../lib (offset 12)
";
        assert_eq!(
            parse_rpaths(output),
            [
                "/Library/Frameworks/GStreamer.framework/Versions/1.0/lib",
                "@executable_path/../lib"
            ]
        );
    }

    #[test]
    fn only_the_builds_libraries_and_the_system_are_bundled() {
        // The one file in the build's `lib`.
        let in_lib = |file: &str| file == "libglib-2.0.0.dylib";
        assert_eq!(
            classify_dependency("/usr/lib/libSystem.B.dylib", in_lib),
            Dependency::System
        );
        assert_eq!(
            classify_dependency(
                "/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit",
                in_lib
            ),
            Dependency::System
        );
        assert_eq!(
            classify_dependency("@rpath/libglib-2.0.0.dylib", in_lib),
            Dependency::Bundled("libglib-2.0.0.dylib".into())
        );
        // What the build itself writes: the absolute path in target/gstreamer
        // (only the file name counts).
        assert_eq!(
            classify_dependency("/gst/lib/libglib-2.0.0.dylib", in_lib),
            Dependency::Bundled("libglib-2.0.0.dylib".into())
        );
        assert_eq!(
            classify_dependency("/opt/homebrew/lib/libpango-1.0.0.dylib", in_lib),
            Dependency::Foreign
        );
        assert_eq!(
            classify_dependency(
                "/Library/Frameworks/GStreamer.framework/Versions/1.0/lib/libx.dylib",
                in_lib
            ),
            Dependency::Foreign
        );
        // An `@rpath` name of a library the build does not have cannot be found either.
        assert_eq!(
            classify_dependency("@rpath/libswift_Concurrency.dylib", in_lib),
            Dependency::Foreign
        );
    }

    #[test]
    fn rpath_leads_from_every_place_in_the_runtime_to_its_lib() {
        assert_eq!(rpath_to_lib("lib/libglib-2.0.0.dylib"), "@loader_path");
        assert_eq!(
            rpath_to_lib("lib/gstreamer-1.0/libgstapp.dylib"),
            "@loader_path/.."
        );
        assert_eq!(
            rpath_to_lib("libexec/gstreamer-1.0/gst-plugin-scanner"),
            "@loader_path/../../lib"
        );
    }

    /// The files of a build's `lib`, as paths by name, and their
    /// dependencies by path.
    struct Build {
        lib: HashMap<&'static str, PathBuf>,
        dependencies: HashMap<PathBuf, Vec<String>>,
    }

    fn build() -> Build {
        let files: [(&str, &[&str]); 4] = [
            (
                "libgstreamer-1.0.0.dylib",
                &[
                    "@rpath/libgstreamer-1.0.0.dylib",
                    "@rpath/libglib-2.0.0.dylib",
                    "/usr/lib/libSystem.B.dylib",
                ],
            ),
            (
                "libglib-2.0.0.dylib",
                &[
                    "/gst/lib/libglib-2.0.0.dylib",
                    "@rpath/libintl.8.dylib",
                    "/usr/lib/libSystem.B.dylib",
                ],
            ),
            (
                "libintl.8.dylib",
                &["@rpath/libintl.8.dylib", "/usr/lib/libSystem.B.dylib"],
            ),
            ("libunused.dylib", &["@rpath/libunused.dylib"]),
        ];
        let mut build = Build {
            lib: HashMap::new(),
            dependencies: HashMap::new(),
        };
        for (name, dependencies) in files {
            let path = PathBuf::from("lib").join(name);
            build.lib.insert(name, path.clone());
            build.dependencies.insert(path, strings(dependencies));
        }
        build
    }

    fn closure_of(build: &Build, roots: &[&str]) -> Result<BTreeSet<String>> {
        let mut dependencies = build.dependencies.clone();
        for root in roots {
            dependencies
                .entry(PathBuf::from(root))
                .or_insert_with(|| strings(&["@rpath/libgstreamer-1.0.0.dylib"]));
        }
        closure(
            &roots.iter().map(PathBuf::from).collect::<Vec<_>>(),
            |path| {
                dependencies
                    .get(path)
                    .cloned()
                    .with_context(|| format!("otool {} failed", path.display()))
            },
            |name| build.lib.get(name).cloned(),
        )
    }

    #[test]
    fn the_closure_is_what_the_roots_depend_on_through_the_builds_libraries() {
        let build = build();
        // A plugin that is no file of `lib` depends on libgstreamer, which
        // depends on glib, which depends on intl; `libunused` is nobody's.
        let found = closure_of(&build, &["plugins/libgstapp.dylib"]).unwrap();
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            [
                "libglib-2.0.0.dylib",
                "libgstreamer-1.0.0.dylib",
                "libintl.8.dylib"
            ]
        );
    }

    #[test]
    fn a_dependency_that_is_not_the_builds_or_the_systems_is_an_error() {
        let mut build = build();
        build.dependencies.insert(
            PathBuf::from("lib/libintl.8.dylib"),
            strings(&[
                "@rpath/libintl.8.dylib",
                "/opt/homebrew/lib/libiconv.2.dylib",
            ]),
        );
        let error = closure_of(&build, &["plugins/libgstapp.dylib"])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("lib/libintl.8.dylib depends on /opt/homebrew/lib/libiconv.2.dylib"),
            "{error}"
        );
    }

    #[test]
    fn a_plugin_names_itself_and_its_libraries_by_rpath_and_drops_the_build_rpath() {
        let args = relocation_args(
            "lib/gstreamer-1.0/libgstapp.dylib",
            &strings(&[
                "/ci/gst/lib/gstreamer-1.0/libgstapp.dylib",
                "/ci/gst/lib/libgstbase-1.0.0.dylib",
                "/ci/gst/lib/libgstbase-1.0.0.dylib",
                "/usr/lib/libSystem.B.dylib",
            ]),
            &strings(&["/ci/gst/lib", "/usr/lib/swift"]),
            |name| name == "libgstbase-1.0.0.dylib",
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "-id",
                "@rpath/libgstapp.dylib",
                "-change",
                "/ci/gst/lib/libgstbase-1.0.0.dylib",
                "@rpath/libgstbase-1.0.0.dylib",
                "-delete_rpath",
                "/ci/gst/lib",
                "-delete_rpath",
                "/usr/lib/swift",
                "-add_rpath",
                "@loader_path/..",
            ]
        );
    }

    #[test]
    fn a_file_relocated_by_an_earlier_run_gets_the_rpath_of_this_one_only() {
        // The prefix of the first release carries `@loader_path/../lib`.
        let args = relocation_args(
            "lib/libgstbase-1.0.0.dylib",
            &strings(&[
                "@rpath/libgstbase-1.0.0.dylib",
                "@rpath/libglib-2.0.0.dylib",
                "/usr/lib/libSystem.B.dylib",
            ]),
            &strings(&["@loader_path/../lib"]),
            |_| true,
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "-delete_rpath",
                "@loader_path/../lib",
                "-add_rpath",
                "@loader_path"
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
            &strings(&["@loader_path"]),
            |_| true,
        )
        .unwrap();
        assert!(args.is_empty(), "{args:?}");
    }

    #[test]
    fn an_executable_gets_an_rpath_but_no_install_name() {
        let args = relocation_args(
            "libexec/gstreamer-1.0/gst-plugin-scanner",
            &strings(&["/ci/gst/lib/libgstreamer-1.0.0.dylib"]),
            &[],
            |_| true,
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "-change",
                "/ci/gst/lib/libgstreamer-1.0.0.dylib",
                "@rpath/libgstreamer-1.0.0.dylib",
                "-add_rpath",
                "@loader_path/../../lib"
            ]
        );
    }

    #[test]
    fn a_dependency_outside_the_build_and_the_system_is_not_relocated() {
        let foreign = relocation_args(
            "lib/libx.dylib",
            &strings(&["@rpath/libx.dylib", "/opt/homebrew/lib/libfoo.dylib"]),
            &[],
            |file| file == "libx.dylib",
        );
        assert!(foreign.is_err());
    }

    fn check(rel: &str, dependencies: &[&str], rpaths: &[&str]) -> Vec<String> {
        check_relocated(rel, &strings(dependencies), &strings(rpaths), |file| {
            file.starts_with("libg")
        })
    }

    #[test]
    fn a_relocated_file_is_checked_by_its_names_and_its_one_rpath() {
        assert!(
            check(
                "lib/gstreamer-1.0/libgstapp.dylib",
                &[
                    "@rpath/libgstapp.dylib",
                    "@rpath/libglib-2.0.0.dylib",
                    "/usr/lib/libSystem.B.dylib",
                    "/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit",
                ],
                &["@loader_path/.."],
            )
            .is_empty()
        );
        // The scanner is an executable: no install name to check.
        assert!(
            check(
                "libexec/gstreamer-1.0/gst-plugin-scanner",
                &["@rpath/libglib-2.0.0.dylib"],
                &["@loader_path/../../lib"],
            )
            .is_empty()
        );
    }

    #[test]
    fn everything_that_is_left_of_the_build_machine_is_found() {
        let problems = check(
            "lib/libglib-2.0.0.dylib",
            &[
                "/ci/gst/lib/libglib-2.0.0.dylib",
                "/ci/gst/lib/libintl.8.dylib",
                "@rpath/libmissing.dylib",
                "@rpath/sub/libglib.dylib",
            ],
            &["/ci/gst/lib", "@loader_path"],
        );
        assert_eq!(
            problems,
            [
                "its install name is /ci/gst/lib/libglib-2.0.0.dylib, not @rpath/libglib-2.0.0.dylib",
                "it depends on /ci/gst/lib/libintl.8.dylib",
                "it depends on @rpath/libmissing.dylib",
                "it depends on @rpath/sub/libglib.dylib",
                "its rpaths are [/ci/gst/lib, @loader_path], not [@loader_path]",
            ]
        );
        // A rpath that leads to the right place from another file is wrong here.
        let wrong = check(
            "lib/gstreamer-1.0/libgstapp.dylib",
            &["@rpath/libgstapp.dylib"],
            &["@loader_path/../../lib"],
        );
        assert_eq!(
            wrong,
            ["its rpaths are [@loader_path/../../lib], not [@loader_path/..]"]
        );
        // A library with no install name at all.
        let nameless = check("lib/libg.dylib", &[], &["@loader_path"]);
        assert_eq!(
            nameless,
            ["its install name is missing, not @rpath/libg.dylib"]
        );
    }

    #[test]
    fn a_library_is_linked_by_the_name_of_its_file() {
        assert_eq!(link_word("libglib-2.0.0.dylib"), "-lglib-2.0.0");
        assert_eq!(link_word("libintl.8.dylib"), "-lintl.8");
        assert_eq!(link_word("libgstreamer-1.0.0.dylib"), "-lgstreamer-1.0.0");
    }
}
