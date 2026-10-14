//! Finishing the prefix after `meson install`: what makes the install tree a
//! release that can be unpacked anywhere and that carries its own license
//! texts and notice.
//!
//! - Every `.pc` file derives its prefix from its own location
//!   (`${pcfiledir}`). meson writes them that way (`pkgconfig.relocatable`);
//!   what a build system without that option wrote (CMake's `SvtAv1Enc.pc`)
//!   is rewritten here.
//! - The two scripts GLib installs with the absolute prefix in them find
//!   their prefix from their own location; GLib's gdb auto-load files, whose
//!   directory name is the absolute library path, are dropped.
//! - Linux: the shim `.pc` files in `griasdi-pkgconfig`. Windows: the
//!   `pkgconf.exe` cargo's pkg-config probes use. macOS: install names and
//!   rpaths (`macos.rs`).
//! - `share/licenses/`, `NOTICE` and `RELEASE`.
//! - A last scan of every file for the directory the build ran in. A text
//!   file that still names it is an error; compiled-in strings of binaries
//!   (debug paths of static archives, `__FILE__` of an assertion) are
//!   listed.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use crate::licenses::{self, Component};
use crate::macos;
use crate::patches;
use crate::pins::{
    DAV1D, FFMPEG_COMMIT, FFMPEG_REPO, GST_COMMIT, GST_REPO, GST_TAG, PKGCONF, SVTAV1_COMMIT,
    SVTAV1_REPO, SVTAV1_TAG,
};
use crate::plan::{Layout, Os, Plan};
use crate::release::{RECIPE_REVISION, REPOSITORY_URL, asset_url, release_id, source_asset};

/// The pkg-config names the Rust crates probe (gstreamer-rs, glib-rs); each
/// shim points at gst-full.
const SHIMMED: [&str; 7] = [
    "glib-2.0",
    "gobject-2.0",
    "gio-2.0",
    "gstreamer-1.0",
    "gstreamer-base-1.0",
    "gstreamer-app-1.0",
    "gstreamer-video-1.0",
];

/// Makes the installed prefix of `plan` relocatable and complete.
pub fn finish(plan: &Plan) -> Result<()> {
    let layout = &plan.layout;
    let prefix = layout.prefix.as_path();
    // What the build left in files: the directory it ran in (the prefix, the
    // sources and the build directories lie below it), in every spelling.
    let build_forms = path_forms(&layout.base);
    let prefix_forms = path_forms(prefix);

    remove_gdb_auto_load(prefix)?;
    relocate_scripts(prefix, &prefix_forms)?;
    relocate_pc_files(prefix, &prefix_forms)?;
    match plan.os {
        Os::Linux => write_shims(prefix)?,
        Os::Windows => ship_pkgconf(layout)?,
        Os::Macos => macos::relocate(prefix, &build_forms)?,
    }

    let components = licenses::collect(plan)?;
    for component in &components {
        eprintln!(
            "license texts: {}: {}",
            component.name,
            component.files.join(", ")
        );
    }
    fs::write(
        prefix.join("NOTICE"),
        notice(plan.os, &layout.triple, &components),
    )?;
    fs::write(prefix.join("RELEASE"), format!("{}\n", release_id()))?;

    let left = scan(prefix, &build_forms)?;
    ensure!(
        left.text.is_empty(),
        "the prefix still names the build directory {}, in: {}\nrelocate or drop these files in \
         src/finish.rs",
        build_forms[0],
        left.text.join(", ")
    );
    eprintln!(
        "{} binary files still contain the build directory as a compiled-in string (debug \
         information, assertion file names); none is read at run time:",
        left.binary.len()
    );
    for file in &left.binary {
        eprintln!("  {file}");
    }
    Ok(())
}

// ------------------------------------------------------------ build path

/// The spellings of `path` that tools write into text files: with `/`
/// (first); where the path has backslashes (Windows) also with `\` only, as
/// it is given (Rust joins `target/gstreamer` onto a `\` path), and each of
/// these with `\\` (a string literal).
fn path_forms(path: &Path) -> Vec<String> {
    let native = path.display().to_string();
    let slash = native.replace('\\', "/");
    let mut forms = vec![slash.clone()];
    if native.contains('\\') {
        let backslash = slash.replace('/', "\\");
        for form in [
            backslash.clone(),
            backslash.replace('\\', "\\\\"),
            native.clone(),
            native.replace('\\', "\\\\"),
        ] {
            if !forms.contains(&form) {
                forms.push(form);
            }
        }
    }
    forms
}

/// `text` with every spelling of the prefix in `forms` replaced by `with`,
/// where the spelling ends a path component (`/p/x` is no part of `/p/xy`).
/// `None` when there was none.
fn replace_path(text: &str, forms: &[String], with: &str) -> Option<String> {
    let mut out = text.to_owned();
    let mut changed = false;
    for form in forms {
        let mut next = String::with_capacity(out.len());
        let mut from = 0;
        for (at, _) in out.match_indices(form.as_str()) {
            let end = at + form.len();
            let ends_component = out[end..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.')));
            if at >= from && ends_component {
                next.push_str(&out[from..at]);
                next.push_str(with);
                from = end;
                changed = true;
            }
        }
        next.push_str(&out[from..]);
        out = next;
    }
    changed.then_some(out)
}

// ------------------------------------------------------------ .pc files

/// `text` of a `.pc` file that lies `depth` directories below the prefix,
/// with the prefix replaced by `${pcfiledir}` and the way up from the file's
/// directory, so that the file is right wherever the prefix is unpacked.
/// `None` when the file names no prefix.
fn relocatable_pc(text: &str, prefix_forms: &[String], depth: usize) -> Option<String> {
    let with = format!("${{pcfiledir}}{}", "/..".repeat(depth));
    replace_path(text, prefix_forms, &with)
}

fn relocate_pc_files(prefix: &Path, prefix_forms: &[String]) -> Result<()> {
    let mut rewritten = 0;
    for file in files_below(prefix)? {
        if file.extension().is_none_or(|e| e != "pc") {
            continue;
        }
        let text = fs::read_to_string(&file)
            .with_context(|| format!("read {}: a .pc file that is not text", file.display()))?;
        let depth = file
            .strip_prefix(prefix)
            .ok()
            .and_then(Path::parent)
            .map_or(0, |dir| dir.components().count());
        if let Some(fixed) = relocatable_pc(&text, prefix_forms, depth) {
            fs::write(&file, fixed).with_context(|| format!("write {}", file.display()))?;
            rewritten += 1;
        }
    }
    eprintln!("made {rewritten} .pc files relocatable (the others were already)");
    Ok(())
}

/// `<prefix>/griasdi-pkgconfig`: a shim `.pc` per [`SHIMMED`] name that links
/// `libgstreamer-full-1.0.so` with the version of the real library, and
/// `dav1d.pc` for `dav1d-sys`. The real `.pc` files describe the static
/// archives, which must never reach the app.
fn write_shims(prefix: &Path) -> Result<()> {
    let real = prefix.join("lib/pkgconfig");
    let shims = prefix.join("griasdi-pkgconfig");
    fs::create_dir_all(&shims)?;
    ensure!(
        prefix.join("lib/libgstreamer-full-1.0.so").is_file(),
        "the build produced no libgstreamer-full-1.0.so"
    );
    for name in SHIMMED {
        let pc = fs::read_to_string(real.join(format!("{name}.pc")))
            .with_context(|| format!("{name}.pc was not installed"))?;
        let version = pc
            .lines()
            .find_map(|l| l.strip_prefix("Version:"))
            .map(str::trim)
            .with_context(|| format!("{name}.pc has no Version"))?;
        fs::write(shims.join(format!("{name}.pc")), shim_pc(name, version))?;
    }
    let dav1d = fs::read_to_string(real.join("dav1d.pc")).context("dav1d.pc was not installed")?;
    let shim = shim_of_real_pc(&dav1d).context("dav1d.pc has no prefix line")?;
    fs::write(shims.join("dav1d.pc"), shim)?;
    Ok(())
}

/// The shim of one pkg-config `name` at `version`: links the gst-full library
/// of the prefix the shim lies in.
fn shim_pc(name: &str, version: &str) -> String {
    format!(
        "prefix=${{pcfiledir}}/..\n\
         libdir=${{prefix}}/lib\n\
         \n\
         Name: {name}\n\
         Description: {name}, provided by gstreamer-full (griasdi build)\n\
         Version: {version}\n\
         Libs: -L${{libdir}} -lgstreamer-full-1.0\n\
         Cflags:\n"
    )
}

/// A `.pc` file of `lib/pkgconfig` as it reads one directory up, in
/// `griasdi-pkgconfig`: its `prefix=` line is the only one that changes.
/// `None` when it has none.
fn shim_of_real_pc(text: &str) -> Option<String> {
    let mut found = false;
    let lines: Vec<&str> = text
        .lines()
        .map(|line| {
            if !found && line.starts_with("prefix=") {
                found = true;
                "prefix=${pcfiledir}/.."
            } else {
                line
            }
        })
        .collect();
    found.then(|| lines.join("\n") + "\n")
}

// -------------------------------------------------------------- scripts

/// Drops `share/gdb/auto-load`: GLib installs its gdb helpers under the
/// absolute path of the library they belong to, which only matches where the
/// build ran.
fn remove_gdb_auto_load(prefix: &Path) -> Result<()> {
    let dir = prefix.join("share/gdb/auto-load");
    if !dir.exists() {
        return Ok(());
    }
    fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    // `share/gdb` goes with it when nothing else is in it.
    let _ = fs::remove_dir(prefix.join("share/gdb"));
    eprintln!("removed share/gdb/auto-load (named after the build directory)");
    Ok(())
}

/// How a script that bakes in the absolute prefix is fixed; `None` when it
/// does not look the way this recipe knows.
type ScriptFix = fn(&str, &[String]) -> Option<String>;

/// The scripts GLib installs with the absolute prefix in them.
const SCRIPTS: [(&str, ScriptFix); 2] = [
    ("bin/gdbus-codegen", relocate_gdbus_codegen),
    ("bin/glib-gettextize", relocate_gettextize),
];

fn relocate_scripts(prefix: &Path, prefix_forms: &[String]) -> Result<()> {
    for (file, fix) in SCRIPTS {
        let path = prefix.join(file);
        if !path.is_file() {
            continue;
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read {}: not a text file", path.display()))?;
        if !prefix_forms.iter().any(|f| text.contains(f.as_str())) {
            continue;
        }
        let fixed = fix(&text, prefix_forms).with_context(|| {
            format!("{file} names the install path in a way this recipe does not know")
        })?;
        fs::write(&path, fixed).with_context(|| format!("write {}", path.display()))?;
        eprintln!("{file} finds its prefix from its own location now");
    }
    Ok(())
}

/// `gdbus-codegen` looks for its Python modules next to itself and falls
/// back to the hard-coded `<datadir>/glib-2.0`; the fallback becomes the
/// same directory relative to the script (`filedir` is the script's).
fn relocate_gdbus_codegen(text: &str, prefix_forms: &[String]) -> Option<String> {
    let mut changed = false;
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            let code = line.trim_start();
            if code.starts_with("path = os.path.join(")
                && code.ends_with("'glib-2.0')")
                && prefix_forms.iter().any(|f| line.contains(f.as_str()))
            {
                changed = true;
                let indent = &line[..line.len() - code.len()];
                format!(
                    "{indent}path = os.path.join(os.path.abspath(filedir), '..', 'share', 'glib-2.0')"
                )
            } else {
                line.to_owned()
            }
        })
        .collect();
    changed.then(|| lines.join("\n") + "\n")
}

/// `glib-gettextize` sets `prefix`, `datarootdir` and `datadir` to absolute
/// paths; `prefix` becomes the directory above the script's, the others are
/// derived from it.
fn relocate_gettextize(text: &str, prefix_forms: &[String]) -> Option<String> {
    let mut changed = false;
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            if prefix_forms
                .iter()
                .any(|f| line.strip_prefix("prefix=") == Some(f.as_str()))
            {
                changed = true;
                "prefix=$(cd \"$(dirname \"$0\")/..\" && pwd)".to_owned()
            } else if let Some(form) = prefix_forms.iter().find(|f| line.contains(f.as_str())) {
                changed = true;
                line.replace(form.as_str(), "${prefix}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    changed.then(|| lines.join("\n") + "\n")
}

// ---------------------------------------------------------------- pkgconf

/// Copies the pkgconf of the build tools to `bin/pkgconf.exe`: the one cargo's
/// pkg-config probes run against the prefix.
fn ship_pkgconf(layout: &Layout) -> Result<()> {
    let from = layout.pkgconf_exe();
    ensure!(from.is_file(), "{} was not built", from.display());
    let bin = layout.prefix.join("bin");
    fs::create_dir_all(&bin)?;
    fs::copy(&from, bin.join("pkgconf.exe"))
        .with_context(|| format!("copy {} into {}", from.display(), bin.display()))?;
    Ok(())
}

// ---------------------------------------------------------------- notice

/// `NOTICE`: what the prefix is, where the corresponding source is and how
/// to rebuild or replace the libraries (LGPL-2.1 sections 4 and 6).
fn notice(os: Os, triple: &str, components: &[Component]) -> String {
    let id = release_id();
    let source = asset_url(&id, &source_asset(&id, triple));
    let sums = asset_url(&id, "SHA256SUMS");
    let dav1d = DAV1D.dir;
    let dav1d_url = DAV1D.url;
    let dav1d_sha = DAV1D.sha256;
    let svtav1 = SVTAV1_TAG;
    let linux_libraries = if os == Os::Linux {
        ", libva, libdrm"
    } else {
        ""
    };
    let pkgconf = if os == Os::Windows {
        format!(
            "pkgconf ({}) is built as well and shipped as bin/pkgconf.exe (ISC license).\n",
            PKGCONF.dir.trim_start_matches("pkgconf-")
        )
    } else {
        String::new()
    };
    let linking = match os {
        Os::Linux => {
            "The libraries are dynamically linked into the application as one shared library,\n\
             lib/libgstreamer-full-1.0.so, with GLib, FFmpeg and the other libraries static\n\
             inside it: replace that file with one built from the corresponding source to use\n\
             another build. libasound and the X11 libraries (libX11, libXext, libXfixes,\n\
             libXdamage) are the system's and not part of this prefix."
        }
        Os::Windows => {
            "The libraries are dynamically linked: one DLL per library and per plugin. Replace\n\
             the files with ones built from the corresponding source to use another build."
        }
        Os::Macos => {
            "The libraries are dynamically linked: one dylib per library and per plugin.\n\
             Replace the files with ones built from the corresponding source to use another\n\
             build."
        }
    };
    let patches: String = patches::for_os(os)
        .map(|p| format!("  patches/{}\n", p.file))
        .collect();
    let components: String = components
        .iter()
        .map(|c| format!("  share/licenses/{}/\n", c.name))
        .collect();
    format!(
        "\
GStreamer for griasdi
=====================

This directory is the install prefix of GStreamer {GST_TAG} and the libraries it is
built with, for {triple}: release {id} of {REPOSITORY_URL}
(GStreamer tag {GST_TAG}, recipe revision {RECIPE_REVISION}). The file RELEASE holds the
release id.

It holds GStreamer with the plugins griasdi needs, and GLib, FFmpeg's libavcodec,
libavformat, libavfilter and libavutil (built without FFmpeg's GPL parts), orc, opus,
libffi, pcre2, zlib{linux_libraries}, dav1d ({dav1d}) and SVT-AV1 ({svtav1}).
{pkgconf}
Licenses
--------

GStreamer, GLib and FFmpeg are under the LGPL-2.1-or-later; the other libraries are
under permissive licenses (MIT, BSD, zlib), SVT-AV1 under the BSD-3-Clause-Clear and
the Alliance for Open Media Patent License 1.0. The license texts of every component
built into this prefix are in share/licenses/:
{components}
Corresponding source
--------------------

The source these binaries were built from, with the patches applied, and the recipe
that built them (the recipe is the directory recipe/ of the archive) are in
  {source}
with the checksums in
  {sums}

The sources are upstream's, as the recipe pins them:
  GStreamer monorepo  {GST_REPO} (tag {GST_TAG}, commit {GST_COMMIT})
  FFmpeg              {FFMPEG_REPO} (commit {FFMPEG_COMMIT})
  SVT-AV1             {SVTAV1_REPO} (tag {SVTAV1_TAG}, commit {SVTAV1_COMMIT})
  dav1d               {dav1d_url}
                      (release tarball, SHA-256 {dav1d_sha})
  the other libraries are the meson subprojects that the monorepo's wrap files name
They are changed only by the patches in the recipe's patches/ directory, each under the
license of the file it modifies:
{patches}
Replacing the libraries
-----------------------

{linking}

To rebuild from the source of the release, unpack the archive and run, in recipe/,
  cargo run --release -- build --target {triple}
(README.md lists what the build needs).
"
    )
}

// ------------------------------------------------------------------ scan

/// The files of the prefix that still contain the build directory.
#[derive(Debug, Default)]
struct Leftovers {
    /// Text files, symlinks and path names: a prefix that is unpacked
    /// somewhere else would be wrong in these.
    text: Vec<String>,
    /// Binary files: compiled-in strings.
    binary: Vec<String>,
}

/// Whether `data` is a binary file (a NUL byte in its first 8000 bytes, the
/// test `grep -I` and git apply).
fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
}

fn contains(data: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && data.windows(needle.len()).any(|w| w == needle)
}

/// Every file, symlink and directory below `dir`, as a path, sorted; the
/// second field says whether it is a regular file.
fn entries_below(dir: &Path) -> Result<Vec<(PathBuf, bool)>> {
    let mut found = Vec::new();
    let mut queue = vec![dir.to_path_buf()];
    while let Some(dir) = queue.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                queue.push(entry.path());
            }
            found.push((entry.path(), kind.is_file()));
        }
    }
    found.sort();
    Ok(found)
}

/// The regular files below `dir`, sorted.
fn files_below(dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(entries_below(dir)?
        .into_iter()
        .filter_map(|(path, is_file)| is_file.then_some(path))
        .collect())
}

/// Looks for `forms` (the build directory) in every file of the prefix, in
/// the target of every symlink and in every path name.
fn scan(prefix: &Path, forms: &[String]) -> Result<Leftovers> {
    // As part of a path name inside the prefix: `home/user/...`, `C/Users/...`.
    let in_names: Vec<String> = forms
        .iter()
        .filter(|f| !f.contains('\\'))
        .map(|f| f.trim_start_matches('/').replacen(':', "", 1))
        .collect();
    let mut left = Leftovers::default();
    for (path, is_file) in entries_below(prefix)? {
        let rel = path
            .strip_prefix(prefix)
            .with_context(|| format!("{} is not in {}", path.display(), prefix.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if in_names.iter().any(|n| rel.contains(n.as_str())) {
            left.text.push(format!("{rel} (its name)"));
        } else if is_file {
            let data = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            if forms.iter().any(|f| contains(&data, f.as_bytes())) {
                if is_binary(&data) {
                    left.binary.push(rel);
                } else {
                    left.text.push(rel);
                }
            }
        } else if let Ok(target) = fs::read_link(&path) {
            let target = target.to_string_lossy();
            if forms.iter().any(|f| target.contains(f.as_str())) {
                left.text.push(format!("{rel} (a symlink to {target})"));
            }
        }
    }
    Ok(left)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forms(path: &str) -> Vec<String> {
        path_forms(Path::new(path))
    }

    #[test]
    fn a_unix_path_has_one_spelling_and_a_windows_path_several() {
        assert_eq!(forms("/ci/target/gstreamer"), ["/ci/target/gstreamer"]);
        assert_eq!(
            forms(r"C:\ci\target\gstreamer"),
            [
                "C:/ci/target/gstreamer",
                r"C:\ci\target\gstreamer",
                r"C:\\ci\\target\\gstreamer",
            ]
        );
        // Rust joins `target/gstreamer` onto a `\` path as it is.
        assert_eq!(
            forms(r"C:\ci\target/gstreamer"),
            [
                "C:/ci/target/gstreamer",
                r"C:\ci\target\gstreamer",
                r"C:\\ci\\target\\gstreamer",
                r"C:\ci\target/gstreamer",
                r"C:\\ci\\target/gstreamer",
            ]
        );
    }

    #[test]
    fn a_pc_file_finds_its_prefix_from_its_own_directory() {
        let pc = "prefix=/ci/p\nexec_prefix=${prefix}\nlibdir=${exec_prefix}/lib\n\n\
                  Name: x\nLibs: -L/ci/p/lib -lx\nCflags: -I/ci/p/include/x -DY\n";
        assert_eq!(
            relocatable_pc(pc, &forms("/ci/p"), 2).unwrap(),
            "prefix=${pcfiledir}/../..\nexec_prefix=${prefix}\nlibdir=${exec_prefix}/lib\n\n\
             Name: x\nLibs: -L${pcfiledir}/../../lib -lx\nCflags: -I${pcfiledir}/../../include/x -DY\n"
        );
        assert_eq!(
            relocatable_pc("prefix=/ci/p\n", &forms("/ci/p"), 3).unwrap(),
            "prefix=${pcfiledir}/../../..\n"
        );
    }

    #[test]
    fn a_relocatable_pc_file_is_left_alone() {
        let pc = "prefix=${pcfiledir}/../..\nlibdir=${prefix}/lib\n";
        assert_eq!(relocatable_pc(pc, &forms("/ci/p"), 2), None);
    }

    #[test]
    fn a_longer_path_is_not_the_prefix() {
        let pc = "prefix=/ci/p\nother=/ci/pp/lib\nthird=/ci/p-old\nfourth=/ci/p.d\n";
        assert_eq!(
            relocatable_pc(pc, &forms("/ci/p"), 1).unwrap(),
            "prefix=${pcfiledir}/..\nother=/ci/pp/lib\nthird=/ci/p-old\nfourth=/ci/p.d\n"
        );
    }

    #[test]
    fn windows_spellings_of_the_prefix_are_all_replaced() {
        let pc = "prefix=C:/ci/p\nlibdir=C:\\ci\\p\\lib\nx=C:\\\\ci\\\\p\\\\bin\n";
        assert_eq!(
            relocatable_pc(pc, &forms(r"C:\ci\p"), 2).unwrap(),
            "prefix=${pcfiledir}/../..\nlibdir=${pcfiledir}/../..\\lib\n\
             x=${pcfiledir}/../..\\\\bin\n"
        );
    }

    #[test]
    fn the_shims_link_gst_full_from_their_own_prefix() {
        let shim = shim_pc("glib-2.0", "2.86.3");
        assert!(shim.starts_with("prefix=${pcfiledir}/..\nlibdir=${prefix}/lib\n"));
        assert!(shim.contains("Version: 2.86.3\n"));
        assert!(shim.contains("Libs: -L${libdir} -lgstreamer-full-1.0\n"));
        assert!(shim.ends_with("Cflags:\n"));
    }

    #[test]
    fn the_dav1d_shim_is_the_real_file_one_directory_up() {
        let real = "prefix=${pcfiledir}/../..\nincludedir=${prefix}/include\nlibdir=${prefix}/lib\n\
                    \nName: dav1d\nVersion: 1.5.4\nLibs: -L${libdir} -ldav1d\nCflags: -I${includedir}/dav1d\n";
        assert_eq!(
            shim_of_real_pc(real).unwrap(),
            real.replacen("prefix=${pcfiledir}/../..", "prefix=${pcfiledir}/..", 1)
        );
        assert_eq!(shim_of_real_pc("Name: dav1d\n"), None);
    }

    #[test]
    fn gdbus_codegen_falls_back_to_its_own_share_directory() {
        let script = "\
if not os.path.isfile(os.path.join(path, 'codegen', 'codegen_main.py')):
    path = os.path.join('/ci/target/gstreamer/p/share', 'glib-2.0')

sys.path.insert(0, path)
";
        assert_eq!(
            relocate_gdbus_codegen(script, &forms("/ci/target/gstreamer/p")).unwrap(),
            "\
if not os.path.isfile(os.path.join(path, 'codegen', 'codegen_main.py')):
    path = os.path.join(os.path.abspath(filedir), '..', 'share', 'glib-2.0')

sys.path.insert(0, path)
"
        );
        assert_eq!(
            relocate_gdbus_codegen("path = os.path.join(a, 'glib-2.0')\n", &forms("/ci/p")),
            None
        );
    }

    #[test]
    fn gettextize_takes_its_prefix_from_the_script_directory() {
        let script = "\
# Directory where the sources are stored.
prefix=/ci/target/gstreamer/p
case `uname` in
MINGW32*)
    prefix=\"`dirname $0`/..\"
    ;;
esac

datarootdir=/ci/target/gstreamer/p/share
datadir=/ci/target/gstreamer/p/share
gettext_dir=$datadir/glib-2.0/gettext
";
        assert_eq!(
            relocate_gettextize(script, &forms("/ci/target/gstreamer/p")).unwrap(),
            "\
# Directory where the sources are stored.
prefix=$(cd \"$(dirname \"$0\")/..\" && pwd)
case `uname` in
MINGW32*)
    prefix=\"`dirname $0`/..\"
    ;;
esac

datarootdir=${prefix}/share
datadir=${prefix}/share
gettext_dir=$datadir/glib-2.0/gettext
"
        );
    }

    #[test]
    fn binary_files_are_told_by_a_nul_byte_near_the_start() {
        assert!(is_binary(b"\x7fELF\0\0"));
        assert!(!is_binary(b"prefix=/x\n"));
        assert!(contains(b"abc/ci/p/lib", b"/ci/p"));
        assert!(!contains(b"abc", b"/ci/p"));
        assert!(!contains(b"abc", b""));
    }

    #[test]
    fn the_notice_names_the_release_and_where_its_source_is() {
        let text = notice(Os::Linux, "x86_64-unknown-linux-gnu", &[]);
        let id = release_id();
        assert!(text.contains(&format!(
            "https://github.com/griasdi/gstreamer/releases/download/v{id}/\
             gstreamer-{id}-x86_64-unknown-linux-gnu-source.tar.gz"
        )));
        assert!(text.contains("libgstreamer-full-1.0.so"));
        assert!(text.contains("patches/libva-static-library-and-header-copy.patch"));
        assert!(!text.contains("glib-macos-no-pipe2.patch"));
        assert!(!text.contains('{') && !text.contains('}'));
    }
}
