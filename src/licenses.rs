//! The license texts a release carries: `share/licenses/<component>/` holds,
//! for every component built into the prefix, the license files of the source
//! tree it was built from.
//!
//! Which components were built is read from the build, not listed here:
//!
//! - meson's introspection of the monorepo build (`meson-info/`) names every
//!   `meson.build` it read, which are the monorepo's own modules and every
//!   subproject it configured (the wraps' unpacked trees, with their nested
//!   subprojects), and every source file it compiled. A component is a
//!   subproject directory; its license files are those directly in it and in
//!   every directory between it and a compiled source file, so a library
//!   bundled inside a module (one with a license file of its own) is carried
//!   too.
//! - dav1d, SVT-AV1 and (Windows) pkgconf are built by this recipe from their
//!   own trees: the license files directly in the tree, and in each
//!   directory of `third_party/` of SVT-AV1.
//!
//! A component without a license text is an error, so a subproject that a
//! GStreamer bump brings in cannot be shipped without one.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use crate::pins::{DAV1D, PKGCONF, SVTAV1_TAG};
use crate::plan::{Os, Plan};

/// A component of the prefix and the license files it carries, relative to
/// `share/licenses/<name>/`.
#[derive(Debug)]
pub struct Component {
    pub name: String,
    pub files: Vec<String>,
}

/// A source tree whose license files go into the prefix.
struct Source {
    name: String,
    dir: PathBuf,
    /// Directories of the tree, relative to it, to look for license files in
    /// besides the tree's own.
    also: BTreeSet<PathBuf>,
}

/// Components whose license files are only some of those in their tree.
/// FFmpeg is built with `-Dgpl=disabled`: its LGPL texts and the `LICENSE.md`
/// that explains which part is under which license are carried; the GPL
/// texts it also ships are for parts that are not built.
const ONLY: [(&str, &[&str]); 1] = [(
    "FFmpeg",
    &["COPYING.LGPLv2.1", "COPYING.LGPLv3", "LICENSE.md"],
)];

/// Components that ship no license file: the license notice at the top of a
/// source file of the tree stands in (component name prefix, that file).
/// libdrm 2.4.124 is licensed under the MIT license in the header of each of
/// its files.
const HEADER_LICENSES: [(&str, &str); 1] = [("libdrm", "xf86drm.c")];

/// Subprojects that only provide programs meson runs during the build
/// (prebuilt bison, flex, nasm and pkg-config for macOS and Windows): nothing
/// of them is built into or shipped with the prefix, so they are no
/// components.
const BUILD_TOOLS: [&str; 4] = [
    "macos-bison-binary",
    "win-flex-bison-binaries",
    "win-nasm",
    "win-pkgconfig",
];

/// Writes `share/licenses/` into the prefix of `plan` and returns the
/// components it holds, by name.
pub fn collect(plan: &Plan) -> Result<Vec<Component>> {
    let layout = &plan.layout;
    let mut sources = gstreamer_sources(&layout.src, &layout.build.join("gstreamer"))?;
    sources.push(Source {
        name: DAV1D.dir.to_owned(),
        dir: layout.dav1d_src(),
        also: BTreeSet::new(),
    });
    sources.push(Source {
        name: format!("SVT-AV1-{}", SVTAV1_TAG.trim_start_matches('v')),
        dir: layout.svtav1_src(),
        also: third_party_dirs(&layout.svtav1_src())?,
    });
    if plan.os == Os::Windows {
        sources.push(Source {
            name: PKGCONF.dir.to_owned(),
            dir: layout.pkgconf_src(),
            also: BTreeSet::new(),
        });
    }

    let dest = layout.prefix.join("share/licenses");
    if dest.exists() {
        fs::remove_dir_all(&dest).with_context(|| format!("remove {}", dest.display()))?;
    }
    let mut components: Vec<Component> = Vec::new();
    for source in &sources {
        ensure!(
            !components.iter().any(|c| c.name == source.name),
            "two components are named {}",
            source.name
        );
        let files = copy_licenses(source, &dest.join(&source.name))?;
        components.push(Component {
            name: source.name.clone(),
            files,
        });
    }
    components.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(components)
}

/// The monorepo's modules and subprojects as meson configured and compiled
/// them in `build`.
fn gstreamer_sources(src: &Path, build: &Path) -> Result<Vec<Source>> {
    let info = build.join("meson-info");
    let read = |name: &str| -> Result<Value> {
        let path = info.join(name);
        let text = fs::read_to_string(&path).with_context(|| {
            format!(
                "read {}: the meson build directory is gone; remove .installed and .recipe \
                 from the prefix to build again",
                path.display()
            )
        })?;
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
    };
    let files = string_list(&read("intro-buildsystem_files.json")?);
    let sources = compiled_sources(&read("intro-targets.json")?);
    // meson may have been given the source directory through a symlink.
    let mut roots = vec![src.to_owned()];
    if let Ok(canonical) = fs::canonicalize(src) {
        roots.push(canonical);
    }
    for root in roots {
        let dirs = subproject_dirs(&root, &files);
        if dirs.is_empty() {
            continue;
        }
        let compiled = compiled_dirs(&root, &sources);
        return dirs
            .into_iter()
            .filter(|rel| {
                !rel.file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| BUILD_TOOLS.contains(&name))
            })
            .map(|rel| {
                let name = rel
                    .file_name()
                    .and_then(OsStr::to_str)
                    .context("a subproject directory without a name")?
                    .to_owned();
                // A nested subproject is a component of its own.
                let also = compiled
                    .iter()
                    .filter_map(|dir| dir.strip_prefix(&rel).ok())
                    .filter(|dir| !dir.components().any(|c| c.as_os_str() == "subprojects"))
                    .map(Path::to_path_buf)
                    .collect();
                Ok(Source {
                    name,
                    dir: root.join(&rel),
                    also,
                })
            })
            .collect();
    }
    bail!(
        "meson names no subproject below {}: nothing to take the license texts from",
        src.display()
    )
}

/// The strings of a JSON array; anything else is skipped.
fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// Every source file the targets of meson's `intro-targets.json` compile.
fn compiled_sources(targets: &Value) -> Vec<String> {
    let mut sources = Vec::new();
    for target in targets.as_array().into_iter().flatten() {
        for group in target["target_sources"].as_array().into_iter().flatten() {
            sources.extend(string_list(&group["sources"]));
        }
    }
    sources
}

/// The directories (relative to `src`) of the subprojects the build system
/// `files` (absolute paths of every `meson.build` meson read) belong to:
/// `subprojects/<dir>`, and `subprojects/<dir>/subprojects/<nested>` where a
/// subproject carries subprojects of its own.
fn subproject_dirs(src: &Path, files: &[String]) -> BTreeSet<PathBuf> {
    let mut dirs = BTreeSet::new();
    for file in files {
        let Ok(rel) = Path::new(file).strip_prefix(src) else {
            continue;
        };
        let parts: Vec<&OsStr> = rel.components().map(|c| c.as_os_str()).collect();
        let mut i = 0;
        // `subprojects/<dir>` only counts with something below the dir.
        while i + 2 < parts.len() {
            if parts[i] == "subprojects" {
                dirs.insert(parts[..=i + 1].iter().collect::<PathBuf>());
                i += 2;
            } else {
                i += 1;
            }
        }
    }
    dirs
}

/// The directories, relative to `src`, of the compiled source files below
/// it.
fn compiled_dirs(src: &Path, sources: &[String]) -> BTreeSet<PathBuf> {
    sources
        .iter()
        .filter_map(|file| Path::new(file).strip_prefix(src).ok())
        .filter_map(Path::parent)
        .map(Path::to_path_buf)
        .collect()
}

/// The directories directly inside `dir/third_party`, relative to `dir`.
fn third_party_dirs(dir: &Path) -> Result<BTreeSet<PathBuf>> {
    let third_party = dir.join("third_party");
    let mut dirs = BTreeSet::new();
    if !third_party.is_dir() {
        return Ok(dirs);
    }
    for entry in
        fs::read_dir(&third_party).with_context(|| format!("read {}", third_party.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.insert(Path::new("third_party").join(entry.file_name()));
        }
    }
    Ok(dirs)
}

/// Whether a file of this name carries the license of the code around it:
/// `COPYING`, `LICENSE.md`, `COPYING.LGPLv2.1`, `PATENTS.md` and the like,
/// but no source or data file that happens to be named after licenses
/// (`license-translations.dict`).
fn is_license_file(name: &str) -> bool {
    const STEMS: [&str; 7] = [
        "copying",
        "license",
        "licence",
        "copyright",
        "notice",
        "patents",
        "unlicense",
    ];
    const NOT_TEXT: [&str; 10] = [
        "c", "h", "cc", "cpp", "py", "dict", "in", "json", "xml", "po",
    ];
    let lower = name.to_ascii_lowercase();
    let extension = lower.rsplit_once('.').map(|(_, extension)| extension);
    STEMS.iter().any(|stem| {
        lower
            .strip_prefix(stem)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '-', '_']))
    }) && !extension.is_some_and(|e| NOT_TEXT.contains(&e))
}

/// Copies the license files of `source` to `dest` (keeping their directories
/// relative to the tree) and returns their paths relative to `dest`.
fn copy_licenses(source: &Source, dest: &Path) -> Result<Vec<String>> {
    // The tree itself, and every directory between it and a compiled source.
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::from([PathBuf::new()]);
    for dir in &source.also {
        let mut current = Some(dir.as_path());
        while let Some(d) = current {
            dirs.insert(d.to_path_buf());
            current = d.parent();
        }
    }
    let mut found: BTreeSet<PathBuf> = BTreeSet::new();
    for dir in &dirs {
        let Ok(entries) = fs::read_dir(source.dir.join(dir)) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_str().is_some_and(is_license_file) && entry.path().is_file() {
                found.insert(dir.join(name));
            }
        }
    }
    if let Some((_, only)) = ONLY.iter().find(|(name, _)| *name == source.name) {
        found.retain(|file| {
            file.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| only.contains(&name))
        });
    }
    if found.is_empty() {
        let (_, file) = HEADER_LICENSES
            .iter()
            .find(|(prefix, _)| source.name.starts_with(prefix))
            .with_context(|| {
                format!(
                    "{} has no license file in {}; add the component to HEADER_LICENSES in \
                     src/licenses.rs if its license is stated in a source file",
                    source.name,
                    source.dir.display()
                )
            })?;
        let path = source.dir.join(file);
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let notice = header_license(&text)
            .with_context(|| format!("{} carries no license notice", path.display()))?;
        fs::create_dir_all(dest)?;
        fs::write(
            dest.join("LICENSE"),
            format!(
                "{} ships no license file. This is the license notice at the top of {file}.\n\n{notice}",
                source.name
            ),
        )?;
        return Ok(vec!["LICENSE".to_owned()]);
    }
    let mut names = Vec::new();
    for rel in &found {
        let to = dest.join(rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        let from = source.dir.join(rel);
        fs::copy(&from, &to)
            .with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
        names.push(rel.to_string_lossy().replace('\\', "/"));
    }
    Ok(names)
}

/// The license notice at the top of `text`: the first block comment that
/// grants permission, without its comment decoration.
fn header_license(text: &str) -> Option<String> {
    let start = text.find("/*")?;
    let mut from = start;
    loop {
        let end = from + text[from..].find("*/")? + 2;
        let block = &text[from..end];
        if block.contains("Permission is hereby granted") {
            let lines: Vec<&str> = block
                .trim_start_matches("/*")
                .trim_end_matches("*/")
                .lines()
                .map(|line| {
                    let line = line.trim_start();
                    line.strip_prefix("* ")
                        .or_else(|| line.strip_prefix('*'))
                        .unwrap_or(line)
                        .trim_end()
                })
                .collect();
            return Some(lines.join("\n").trim().to_owned() + "\n");
        }
        from = end + text[end..].find("/*")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_files_are_recognised_by_name() {
        for yes in [
            "COPYING",
            "LICENSE",
            "LICENCE.md",
            "LICENSE.build",
            "LICENSE-BSD2.md",
            "COPYING.LGPLv2.1",
            "PATENTS.md",
            "license.txt",
            "NOTICE",
        ] {
            assert!(is_license_file(yes), "{yes}");
        }
        for no in [
            "README.md",
            "README.static-linking",
            "AUTHORS",
            "licenses",
            "copyrighted.c",
            "license-translations.dict",
            "noticeboard",
            "meson.build",
        ] {
            assert!(!is_license_file(no), "{no}");
        }
    }

    #[test]
    fn subprojects_are_where_meson_read_build_files_below_subprojects() {
        let files: Vec<String> = [
            "/w/src/meson.build",
            "/w/src/subprojects/orc/meson.build",
            "/w/src/subprojects/glib-2.86.3/glib/meson.build",
            "/w/src/subprojects/glib-2.86.3/subprojects/gvdb/meson.build",
            "/w/src/subprojects/glib-2.86.3/subprojects/gvdb/gvdb/meson.build",
            "/w/src/subprojects/gstreamer.wrap",
            "/usr/bin/gcc",
            "/w/tools/python/bin/python3",
        ]
        .map(String::from)
        .to_vec();
        let dirs = subproject_dirs(Path::new("/w/src"), &files);
        let expected: BTreeSet<PathBuf> = [
            "subprojects/orc",
            "subprojects/glib-2.86.3",
            "subprojects/glib-2.86.3/subprojects/gvdb",
        ]
        .map(PathBuf::from)
        .into();
        assert_eq!(dirs, expected);
    }

    #[test]
    fn compiled_sources_name_the_directories_below_the_source_tree() {
        let sources: Vec<String> = [
            "/w/src/subprojects/opus-1.5.2/silk/a.c",
            "/w/src/subprojects/opus-1.5.2/silk/b.c",
            "/w/src/subprojects/opus-1.5.2/celt/c.c",
            "/w/build/generated.c",
        ]
        .map(String::from)
        .to_vec();
        let dirs = compiled_dirs(Path::new("/w/src"), &sources);
        let expected: BTreeSet<PathBuf> =
            ["subprojects/opus-1.5.2/silk", "subprojects/opus-1.5.2/celt"]
                .map(PathBuf::from)
                .into();
        assert_eq!(dirs, expected);
    }

    #[test]
    fn the_license_notice_is_cut_out_of_the_header_comment() {
        let source = "/**\n * \\file x.c\n * Interface\n */\n\n/*\n * Copyright 1999 Somebody.\n * All Rights Reserved.\n *\n * Permission is hereby granted, free of charge, to any person.\n */\n\n#include <stdio.h>\n/* later */\n";
        assert_eq!(
            header_license(source).as_deref(),
            Some(
                "Copyright 1999 Somebody.\nAll Rights Reserved.\n\nPermission is hereby granted, free of charge, to any person.\n"
            )
        );
        assert_eq!(header_license("/* just a comment */\nint x;\n"), None);
        assert_eq!(header_license("no comment at all"), None);
    }
}
