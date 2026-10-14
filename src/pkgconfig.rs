//! The `.pc` files of the `link/` part of a release: what griasdi's build
//! scripts read through pkg-config to link against the runtime of a release.
//!
//! The `.pc` files the build installs describe the prefix: its headers, its
//! tools and its libraries at the places they have there. The `link/` part has
//! no headers and no tools, and the libraries lie in the runtime (or, for the
//! static ones, in `link/lib`), so each file is written again from what it
//! says about the module: its name, version, what it requires and which
//! libraries it links. Every file derives its prefix from its own location
//! (`${pcfiledir}`), so a release works wherever it is unpacked.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

/// A `.pc` file, as far as the `link/` part needs it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    /// The name other files require it by: the file name without `.pc`.
    pub name: String,
    /// The `Name:` field, a name for people.
    pub display_name: String,
    pub description: String,
    pub version: String,
    /// The `Requires:` field as written, and the module names in it.
    pub requires_text: String,
    pub requires: Vec<String>,
    /// The same for `Requires.private:`.
    pub requires_private_text: String,
    pub requires_private: Vec<String>,
    /// The words of `Libs:` and `Libs.private:`.
    pub libs: Vec<String>,
    pub libs_private: Vec<String>,
}

/// The module `name` from the text of its `.pc` file.
pub fn parse(name: &str, text: &str) -> Result<Module> {
    let mut module = Module {
        name: name.to_owned(),
        ..Module::default()
    };
    for line in text.lines() {
        let line = line.trim();
        // A variable has its `=` before any `:`, a field its `:` first
        // (`Cflags: -DX=1`, `driverdir=/a:/b`).
        let Some(at) = line.find([':', '=']) else {
            continue;
        };
        if line.as_bytes()[at] != b':' || line.starts_with('#') {
            continue;
        }
        let (key, value) = (&line[..at], line[at + 1..].trim());
        match key {
            "Name" => module.display_name = value.to_owned(),
            "Description" => module.description = value.to_owned(),
            "Version" => module.version = value.to_owned(),
            "Requires" => {
                module.requires_text = value.to_owned();
                module.requires = parse_requires(value);
            }
            "Requires.private" => {
                module.requires_private_text = value.to_owned();
                module.requires_private = parse_requires(value);
            }
            "Libs" => module.libs = words(value),
            "Libs.private" => module.libs_private = words(value),
            _ => {}
        }
    }
    ensure!(!module.version.is_empty(), "{name}.pc has no Version");
    Ok(module)
}

fn words(value: &str) -> Vec<String> {
    value.split_whitespace().map(str::to_owned).collect()
}

/// The module names in a `Requires:` value: `a, b >= 1.0 c` is `a`, `b`, `c`.
fn parse_requires(value: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut skip_version = false;
    for token in value
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        if skip_version {
            skip_version = false;
        } else if matches!(token, "<" | "<=" | "=" | ">=" | ">" | "!=") {
            skip_version = true;
        } else {
            // `name>=1.0` written without spaces; `>=1.0` leaves no name.
            let name = token.split(['<', '>', '=', '!']).next().unwrap_or(token);
            if !name.is_empty() {
                names.push(name.to_owned());
            }
        }
    }
    names
}

/// The modules `names` and every module they require, public or private,
/// read with `read` (the text of the `.pc` file of a module name).
pub fn closure_with(
    names: &[&str],
    read: impl Fn(&str) -> Result<String>,
) -> Result<BTreeMap<String, Module>> {
    let mut modules = BTreeMap::new();
    let mut queue: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    while let Some(name) = queue.pop() {
        if modules.contains_key(&name) {
            continue;
        }
        let text = read(&name)?;
        let module = parse(&name, &text).with_context(|| format!("parse {name}.pc"))?;
        queue.extend(
            module
                .requires
                .iter()
                .chain(&module.requires_private)
                .cloned(),
        );
        modules.insert(name, module);
    }
    Ok(modules)
}

/// [`closure_with`] over the `.pc` files in `dir`.
pub fn closure(dir: &Path, names: &[&str]) -> Result<BTreeMap<String, Module>> {
    closure_with(names, |name| {
        let path = dir.join(format!("{name}.pc"));
        fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
    })
}

/// The libraries (the `<name>` of `-l<name>`) that linking `roots` takes: those
/// of their `Libs` and of the `Libs` of every module they require publicly. A
/// private requirement is linked only statically.
pub fn public_libs(modules: &BTreeMap<String, Module>, roots: &[&str]) -> BTreeSet<String> {
    let mut libs = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut queue: Vec<String> = roots.iter().map(|n| (*n).to_owned()).collect();
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(module) = modules.get(&name) {
            libs.extend(
                module
                    .libs
                    .iter()
                    .filter_map(|word| word.strip_prefix("-l"))
                    .map(str::to_owned),
            );
            queue.extend(module.requires.iter().cloned());
        }
    }
    libs
}

/// The `.pc` file of `module` for `link/pkgconfig`. `libdir` is the value of
/// its `libdir` variable, an expression over `${prefix}`, the directory of the
/// release (`<target>/`), and `lib` maps the name of a library (`-l<name>`) to
/// the word that links it, which differs where the file of the library is
/// named by its version. Every `-L` is replaced by `-L${libdir}`; the headers
/// are not shipped, so `Cflags` is empty.
///
/// A word that names an absolute path, or a variable the file does not
/// define, is an error: the result would only work where it was built.
pub fn link_pc(module: &Module, libdir: &str, lib: &dyn Fn(&str) -> String) -> Result<String> {
    let rewrite = |words: &[String]| -> Result<Vec<String>> {
        let mut out = Vec::new();
        for word in words {
            if word.starts_with("-L") {
                continue;
            }
            check_word(&module.name, word)?;
            out.push(match word.strip_prefix("-l") {
                Some(name) => lib(name),
                None => word.clone(),
            });
        }
        Ok(out)
    };
    let mut out = format!("prefix=${{pcfiledir}}/../..\nlibdir={libdir}\n\n");
    let display_name = if module.display_name.is_empty() {
        &module.name
    } else {
        &module.display_name
    };
    out.push_str(&format!("Name: {display_name}\n"));
    if !module.description.is_empty() {
        out.push_str(&format!("Description: {}\n", module.description));
    }
    out.push_str(&format!("Version: {}\n", module.version));
    if !module.requires_text.is_empty() {
        out.push_str(&format!("Requires: {}\n", module.requires_text));
    }
    if !module.requires_private_text.is_empty() {
        out.push_str(&format!(
            "Requires.private: {}\n",
            module.requires_private_text
        ));
    }
    out.push_str(&format!(
        "Libs: -L${{libdir}}{}\n",
        join(&rewrite(&module.libs)?)
    ));
    let private = rewrite(&module.libs_private)?;
    if !private.is_empty() {
        out.push_str(&format!("Libs.private:{}\n", join(&private)));
    }
    out.push_str("Cflags:\n");
    Ok(out)
}

/// The words after a key: each one preceded by a space.
fn join(words: &[String]) -> String {
    words.iter().map(|w| format!(" {w}")).collect()
}

/// Fails for a word of `Libs` that would tie the file to the machine that
/// built it.
fn check_word(module: &str, word: &str) -> Result<()> {
    // An absolute path, alone or inside `-Wl,-rpath,/x` or `-I=/x`; or a
    // drive letter.
    let absolute = word
        .split([',', '='])
        .any(|part| part.starts_with('/') || is_drive_path(part));
    ensure!(
        !absolute,
        "{module}.pc: `{word}` names an absolute path; the result would only work where it was built"
    );
    let mut rest = word;
    while let Some(start) = rest.find("${") {
        let Some(len) = rest[start..].find('}') else {
            bail!("{module}.pc: `{word}` has an unterminated variable");
        };
        let variable = &rest[start + 2..start + len];
        ensure!(
            matches!(variable, "prefix" | "libdir" | "pcfiledir"),
            "{module}.pc: `{word}` uses the variable {variable}, which the file for link/ does not define"
        );
        rest = &rest[start + len + 1..];
    }
    Ok(())
}

/// `C:\x` or `C:/x`.
fn is_drive_path(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(drive), Some(':'), Some('/' | '\\')) if drive.is_ascii_alphabetic()
    )
}

/// The shim of the module `name` at `version` for Linux, where one
/// `libgstreamer-full-1.0.so` holds every GLib and GStreamer library: it links
/// that library from the runtime of the release. The real `.pc` files describe
/// the static archives, which must never reach the app.
pub fn shim_pc(name: &str, version: &str, libdir: &str) -> String {
    format!(
        "prefix=${{pcfiledir}}/../..\n\
         libdir={libdir}\n\
         \n\
         Name: {name}\n\
         Description: {name}, provided by gstreamer-full (griasdi build)\n\
         Version: {version}\n\
         Libs: -L${{libdir}} -lgstreamer-full-1.0\n\
         Cflags:\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const GSTREAMER: &str = "\
prefix=${pcfiledir}/../..
datadir=${prefix}/share
includedir=${prefix}/include
libdir=${prefix}/lib
exec_prefix=${prefix}
toolsdir=${exec_prefix}/bin
pluginsdir=${libdir}/gstreamer-1.0
Name: gstreamer-1.0
Description: Streaming media framework
Version: 1.28.7
Requires: glib-2.0, gobject-2.0
Requires.private: gmodule-no-export-2.0
Libs: -L${libdir} -lgstreamer-1.0 -Wl,-rpath,${libdir}
Libs.private: -L${libdir} -lintl -lm -framework Cocoa
Cflags: -I${includedir}/gstreamer-1.0
";

    fn identity(name: &str) -> String {
        format!("-l{name}")
    }

    #[test]
    fn a_module_is_what_its_fields_say_and_variables_are_no_fields() {
        let module = parse("gstreamer-1.0", GSTREAMER).unwrap();
        assert_eq!(module.display_name, "gstreamer-1.0");
        assert_eq!(module.version, "1.28.7");
        assert_eq!(module.requires, ["glib-2.0", "gobject-2.0"]);
        assert_eq!(module.requires_private, ["gmodule-no-export-2.0"]);
        assert_eq!(
            module.libs,
            ["-L${libdir}", "-lgstreamer-1.0", "-Wl,-rpath,${libdir}"]
        );
        assert_eq!(
            module.libs_private,
            ["-L${libdir}", "-lintl", "-lm", "-framework", "Cocoa"]
        );
        // A variable whose value has a `:` is no field, a field whose value has an `=` is one.
        let libva = "driverdir=/usr/lib/dri:/usr/lib64/dri\nVersion: 1.22.0\nCflags: -DX=1\n";
        assert_eq!(parse("libva", libva).unwrap().version, "1.22.0");
        assert!(parse("none", "Name: x\n").is_err());
    }

    #[test]
    fn requires_names_the_modules_and_leaves_the_version_constraints() {
        assert_eq!(
            parse_requires("glib-2.0 >= 2.62, gobject-2.0>=2.0 gio-2.0 = 2.86 orc-0.4"),
            ["glib-2.0", "gobject-2.0", "gio-2.0", "orc-0.4"]
        );
        assert_eq!(parse_requires("a >=1.0, b <2"), ["a", "b"]);
        assert!(parse_requires("").is_empty());
    }

    /// `.pc` files by module name: each requires the one after it, and the
    /// last requires the first privately.
    fn chain() -> HashMap<&'static str, String> {
        HashMap::from([
            (
                "a",
                "Version: 1\nRequires: b\nLibs: -L${libdir} -la\n".to_owned(),
            ),
            (
                "b",
                "Version: 2\nRequires.private: c\nLibs: -L${libdir} -lb\n".to_owned(),
            ),
            (
                "c",
                "Version: 3\nRequires: a\nLibs: -L${libdir} -lc\n".to_owned(),
            ),
        ])
    }

    fn read(files: &HashMap<&'static str, String>) -> impl Fn(&str) -> Result<String> {
        let files = files.clone();
        move |name| {
            files
                .get(name)
                .cloned()
                .with_context(|| format!("no {name}.pc"))
        }
    }

    #[test]
    fn the_closure_follows_private_requirements_too_and_ends_in_a_cycle() {
        let modules = closure_with(&["a"], read(&chain())).unwrap();
        assert_eq!(modules.keys().collect::<Vec<_>>(), ["a", "b", "c"]);
    }

    #[test]
    fn a_required_module_without_a_file_is_an_error() {
        let mut files = chain();
        files.remove("c");
        let error = closure_with(&["a"], read(&files)).unwrap_err().to_string();
        assert_eq!(error, "no c.pc");
    }

    #[test]
    fn only_public_requirements_are_linked() {
        let modules = closure_with(&["a"], read(&chain())).unwrap();
        // c is required privately by b: its library is not linked.
        assert_eq!(
            public_libs(&modules, &["a"])
                .into_iter()
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        // From c, everything is public: c, a, b.
        assert_eq!(
            public_libs(&modules, &["c"])
                .into_iter()
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn a_link_file_points_the_libraries_at_the_given_directory_and_ships_no_headers() {
        let module = parse("gstreamer-1.0", GSTREAMER).unwrap();
        let text = link_pc(&module, "${prefix}/link/lib", &identity).unwrap();
        assert_eq!(
            text,
            "\
prefix=${pcfiledir}/../..
libdir=${prefix}/link/lib

Name: gstreamer-1.0
Description: Streaming media framework
Version: 1.28.7
Requires: glib-2.0, gobject-2.0
Requires.private: gmodule-no-export-2.0
Libs: -L${libdir} -lgstreamer-1.0 -Wl,-rpath,${libdir}
Libs.private: -lintl -lm -framework Cocoa
Cflags:
"
        );
    }

    #[test]
    fn a_library_is_linked_by_the_word_the_caller_names() {
        let module = parse("gstreamer-1.0", GSTREAMER).unwrap();
        let versioned = |name: &str| match name {
            "gstreamer-1.0" => "-lgstreamer-1.0.0".to_owned(),
            "intl" => "-lintl.8".to_owned(),
            other => format!("-l{other}"),
        };
        let text = link_pc(&module, "${prefix}/runtime/Contents/lib", &versioned).unwrap();
        assert!(text.contains("libdir=${prefix}/runtime/Contents/lib\n"));
        assert!(text.contains("Libs: -L${libdir} -lgstreamer-1.0.0 -Wl,-rpath,${libdir}\n"));
        assert!(text.contains("Libs.private: -lintl.8 -lm -framework Cocoa\n"));
    }

    #[test]
    fn the_flags_of_a_static_library_stay() {
        let dav1d = "prefix=${pcfiledir}/../..\nlibdir=${prefix}/lib\n\nName: libdav1d\n\
                     Description: AV1 decoding library\nVersion: 1.5.4\n\
                     Libs: -L${libdir} -ldav1d -pthread -ldl\nCflags: -I${includedir} -pthread\n";
        let module = parse("dav1d", dav1d).unwrap();
        assert_eq!(
            link_pc(&module, "${prefix}/link/lib", &identity).unwrap(),
            "prefix=${pcfiledir}/../..\nlibdir=${prefix}/link/lib\n\nName: libdav1d\n\
             Description: AV1 decoding library\nVersion: 1.5.4\n\
             Libs: -L${libdir} -ldav1d -pthread -ldl\nCflags:\n"
        );
    }

    #[test]
    fn a_word_that_ties_the_file_to_the_build_machine_is_refused() {
        for bad in [
            "/usr/local/lib/libx.a",
            "-Wl,-rpath,/home/ci/target/lib",
            "-Wl,-rpath=/opt/x",
            r"C:\ci\lib\x.lib",
            "C:/ci/lib/x.lib",
            "-l${exec_prefix}",
            "${includedir}/x",
            "-Wl,${libdir",
        ] {
            let module = Module {
                name: "x".into(),
                version: "1".into(),
                libs: vec![bad.to_owned()],
                ..Module::default()
            };
            assert!(
                link_pc(&module, "${prefix}/lib", &identity).is_err(),
                "{bad}"
            );
        }
        // The variables the file defines and relative words are fine.
        let module = Module {
            name: "x".into(),
            version: "1".into(),
            libs: [
                "-Wl,-rpath,${libdir}",
                "-framework",
                "AppKit",
                "-lm",
                "${prefix}/x",
            ]
            .map(String::from)
            .to_vec(),
            ..Module::default()
        };
        assert!(link_pc(&module, "${prefix}/lib", &identity).is_ok());
    }

    #[test]
    fn a_shim_links_gst_full_from_the_given_directory() {
        let shim = shim_pc("glib-2.0", "2.86.3", "${prefix}/runtime/lib/griasdi");
        assert_eq!(
            shim,
            "prefix=${pcfiledir}/../..\n\
             libdir=${prefix}/runtime/lib/griasdi\n\
             \n\
             Name: glib-2.0\n\
             Description: glib-2.0, provided by gstreamer-full (griasdi build)\n\
             Version: 2.86.3\n\
             Libs: -L${libdir} -lgstreamer-full-1.0\n\
             Cflags:\n"
        );
    }
}
