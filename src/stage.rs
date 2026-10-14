//! What a release ships for one target, staged out of the finished prefix:
//! `<target>/RELEASE`, `<target>/runtime/` and `<target>/link/`, below
//! `target/gstreamer/stage-<target>/`, from where `dist.rs` archives it.
//!
//! - `runtime/` is exactly what goes into griasdi's installation, laid out as
//!   the package lays it out (the paths below are package-relative), with the
//!   notice and the license texts that have to travel with the libraries:
//!   - Linux: `lib/griasdi/libgstreamer-full-1.0.so` (every GLib and GStreamer
//!     library and every plugin static inside it),
//!     `share/doc/griasdi/GSTREAMER-NOTICE.txt` and
//!     `share/doc/griasdi/GSTREAMER-LICENSES/`.
//!   - macOS, relative to `griasdi.app/`: `Contents/lib/*.dylib`,
//!     `Contents/lib/gstreamer-1.0/libgst<plugin>.dylib`,
//!     `Contents/libexec/gstreamer-1.0/gst-plugin-scanner`, all relocated
//!     (`macos.rs`), and the notice and licenses in `Contents/Resources/`.
//!   - Windows, relative to the directory of `griasdi.exe`: the DLLs at the
//!     top, with the Visual C++ runtime they need, the plugins in
//!     `lib/gstreamer-1.0`, and the notice and licenses at the top. There is
//!     no plugin scanner: it sits in `libexec`, where it cannot find the DLLs
//!     (a process only looks next to its own exe), so GStreamer scans
//!     in-process, which is fine for a fixed set of plugins.
//! - `link/` is only for building griasdi and is never packaged: the `.pc`
//!   files in `link/pkgconfig` that resolve to the runtime, the static dav1d
//!   in `link/lib`, on Windows the import libraries of the runtime's DLLs and
//!   `link/bin/pkgconf.exe`, the pkg-config cargo's probes run. No headers: no
//!   `-sys` crate of griasdi reads them.
//!
//! The bundle layout follows how GStreamer finds itself, so the app needs no
//! environment variables (setting them is `unsafe` in edition 2024, which
//! griasdi's workspace forbids). `priv_gst_get_relocated_libgstreamer` in
//! `gst/gstregistry.c` (1.28) derives the plugin directory from the location
//! of libgstreamer:
//!
//! - Windows: `<dir of gstreamer-1.0-0.dll>` is the prefix (a trailing `bin` or
//!   `lib` is stripped by GLib, so the DLLs next to the exe make the exe's
//!   directory the prefix); plugins are searched in
//!   `<prefix>\lib\gstreamer-1.0`.
//! - macOS: plugins are searched in `<dir of libgstreamer>/gstreamer-1.0`.
//!
//! Which files to copy is computed, not listed: the libraries a plugin or an
//! app imports are read from its PE import table (Windows, `pe.rs`) or
//! `otool -L` (macOS, `macos.rs`) and followed transitively.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::macos;
use crate::notice::{LICENSES_DIR, NOTICE_FILE, notice};
use crate::pe;
use crate::pins::{MACOS_PLUGINS, PLUGINS, SCREEN_PLUGINS, WINDOWS_PLUGINS};
use crate::pkgconfig::{self, Module};
use crate::plan::{Layout, Os};
use crate::release::release_id;

/// The pkg-config modules griasdi's `-sys` crates probe (gstreamer-rs and
/// glib-rs), what `link/pkgconfig` has to name: they link the runtime of the
/// release, so the libraries of these modules and of everything they require
/// are in it.
const LINKED_MODULES: [&str; 8] = [
    "glib-2.0",
    "gobject-2.0",
    "gio-2.0",
    "gstreamer-1.0",
    "gstreamer-base-1.0",
    "gstreamer-app-1.0",
    "gstreamer-video-1.0",
    "gstreamer-audio-1.0",
];

/// dav1d, whose decoder griasdi links statically into the app
/// (`gst-plugin-dav1d`): its `.pc` file and its library are in `link/`, not
/// in the runtime.
const DAV1D_MODULE: &str = "dav1d";

/// The `libdir` of the files in `link/pkgconfig`, over `${prefix}`, which is
/// the directory of the release (`<target>/`): where the libraries a file
/// links lie.
const LIBDIR_LINUX: &str = "${prefix}/runtime/lib/griasdi";
const LIBDIR_MACOS: &str = "${prefix}/runtime/Contents/lib";
/// The import libraries of the DLLs, and the static libraries on every target.
const LIBDIR_LINK: &str = "${prefix}/link/lib";

/// Where a prefix keeps the license texts of its components:
/// `<component>/<files>`.
const PREFIX_LICENSES: &str = "share/licenses";

/// The staged part of a release.
pub struct Staged {
    /// `target/gstreamer/stage-<target>`: removed when the archive is written.
    pub root: PathBuf,
    /// `<root>/<target>`: what the archive of the release holds.
    pub top: PathBuf,
}

/// Stages the release of the finished prefix of `layout` into a fresh
/// directory.
pub fn stage(layout: &Layout) -> Result<Staged> {
    let os = Os::of(&layout.triple);
    let root = layout.base.join(format!("stage-{}", layout.triple));
    if root.exists() {
        fs::remove_dir_all(&root).with_context(|| format!("remove {}", root.display()))?;
    }
    let top = root.join(&layout.triple);
    let (runtime, link) = (top.join("runtime"), top.join("link"));
    for dir in [&runtime, &link] {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    fs::write(top.join("RELEASE"), format!("{}\n", release_id()))?;
    let prefix = &layout.prefix;
    match os {
        Os::Linux => stage_linux(prefix, &runtime, &link, &layout.triple)?,
        Os::Windows => stage_windows(
            prefix,
            &layout.pkgconf_exe(),
            &runtime,
            &link,
            &layout.triple,
        )?,
        Os::Macos => stage_macos(prefix, &runtime, &link, &layout.triple)?,
    }
    let files = check_staged(&top)?;
    eprintln!(
        "staged {} files ({} bytes) for {}: runtime/ {} bytes, link/ {} bytes",
        files.count,
        files.bytes,
        layout.triple,
        size_of_tree(&runtime)?,
        size_of_tree(&link)?
    );
    Ok(Staged { root, top })
}

// ---------------------------------------------------------------- Linux

fn stage_linux(prefix: &Path, runtime: &Path, link: &Path, triple: &str) -> Result<()> {
    let library = prefix.join("lib/libgstreamer-full-1.0.so");
    ensure!(
        library.is_file(),
        "{} holds no GStreamer build (cargo run --release -- build)",
        prefix.display()
    );
    copy_file(
        &library,
        &runtime.join("lib/griasdi/libgstreamer-full-1.0.so"),
    )?;
    write_docs(&runtime.join("share/doc/griasdi"), prefix, triple)?;

    let real = prefix.join("lib/pkgconfig");
    let pkgconfig = link.join("pkgconfig");
    fs::create_dir_all(&pkgconfig)?;
    for name in LINKED_MODULES {
        let module = read_module(&real, name)?;
        write_file(
            &pkgconfig.join(format!("{name}.pc")),
            &pkgconfig::shim_pc(name, &module.version, LIBDIR_LINUX),
        )?;
    }
    stage_dav1d(prefix, link, &pkgconfig)
}

/// The static dav1d: its `.pc` file and `libdav1d.a`.
fn stage_dav1d(prefix: &Path, link: &Path, pkgconfig: &Path) -> Result<()> {
    let module = read_module(&prefix.join("lib/pkgconfig"), DAV1D_MODULE)?;
    write_file(
        &pkgconfig.join(format!("{DAV1D_MODULE}.pc")),
        &pkgconfig::link_pc(&module, LIBDIR_LINK, &|name| format!("-l{name}"))?,
    )?;
    let library = prefix.join("lib/libdav1d.a");
    ensure!(library.is_file(), "{} is missing", library.display());
    copy_file(&library, &link.join("lib/libdav1d.a"))
}

// -------------------------------------------------------------- Windows

fn stage_windows(
    prefix: &Path,
    pkgconf: &Path,
    runtime: &Path,
    link: &Path,
    triple: &str,
) -> Result<()> {
    let bin = prefix.join("bin");
    ensure!(
        bin.join("gstreamer-1.0-0.dll").is_file(),
        "{} holds no GStreamer build (cargo run --release -- build)",
        prefix.display()
    );

    let mut roots = Vec::new();
    let mut plugins = 0;
    for name in PLUGINS
        .iter()
        .chain(&SCREEN_PLUGINS)
        .chain(&WINDOWS_PLUGINS)
    {
        let file = format!("gst{name}.dll");
        let from = prefix.join("lib/gstreamer-1.0").join(&file);
        ensure!(from.is_file(), "{} is missing", from.display());
        copy_file(&from, &runtime.join("lib/gstreamer-1.0").join(&file))?;
        roots.push(from);
        plugins += 1;
    }

    // What griasdi links: the import library of every library of the modules
    // its `-sys` crates probe, which is shipped, and the DLL it belongs to,
    // which the runtime needs. A library the build has no import library of
    // and no DLL either is the system's (`ws2_32`), and a static library has
    // no DLL (dav1d.lib, which the build made for the MSVC linker).
    let modules = pkgconfig::closure(&prefix.join("lib/pkgconfig"), &modules_to_read())?;
    let available = pe::dlls_in(&bin)?;
    let mut libraries = Vec::new();
    for name in pkgconfig::public_libs(&modules, &modules_to_read()) {
        let import = prefix.join("lib").join(format!("{name}.lib"));
        let dll = pe::dll_of_lib(&available, &name)?;
        if import.is_file() {
            copy_file(&import, &link.join("lib").join(format!("{name}.lib")))?;
        } else if let Some(dll) = &dll {
            bail!(
                "{} is missing: the build has {}, which a module that griasdi links names as \
                 -l{name}, but no import library to link it with",
                import.display(),
                dll.display()
            );
        }
        if let Some(dll) = dll {
            libraries.push(dll.clone());
            roots.push(dll);
        }
    }
    ensure!(
        link.join("lib/dav1d.lib").is_file(),
        "dav1d.lib is not among the libraries griasdi links: the decoder is linked statically"
    );

    let mut closure = pe::dll_closure(&available, &roots, |path| fs::read(path))?;
    // The libraries griasdi links are in the runtime even when no plugin
    // imports them.
    for dll in libraries {
        if let Some(name) = dll.file_name().and_then(|n| n.to_str()) {
            closure.bundled.entry(name.to_owned()).or_insert(dll);
        }
    }
    for (name, from) in &closure.bundled {
        copy_file(from, &runtime.join(name))?;
    }
    // GStreamer is built against the dynamic C runtime; the app itself is
    // linked statically. Machines without the Visual C++ redistributable would
    // fail to start without these.
    for name in &closure.runtime {
        copy_file(&pe::find_vc_runtime(name)?, &runtime.join(name))?;
    }
    write_docs(runtime, prefix, triple)?;
    eprintln!(
        "{} DLLs and {plugins} plugins in the runtime",
        closure.bundled.len() + closure.runtime.len(),
    );

    let pkgconfig = link.join("pkgconfig");
    fs::create_dir_all(&pkgconfig)?;
    for module in modules.values() {
        write_file(
            &pkgconfig.join(format!("{}.pc", module.name)),
            &pkgconfig::link_pc(module, LIBDIR_LINK, &|name| format!("-l{name}"))?,
        )?;
    }
    // The pkg-config cargo's probes run against the `.pc` files.
    ensure!(pkgconf.is_file(), "{} was not built", pkgconf.display());
    copy_file(pkgconf, &link.join("bin/pkgconf.exe"))
}

// ---------------------------------------------------------------- macOS

fn stage_macos(prefix: &Path, runtime: &Path, link: &Path, triple: &str) -> Result<()> {
    let lib = prefix.join("lib");
    let contents = runtime.join("Contents");
    ensure!(
        lib.join("libgstreamer-1.0.0.dylib").is_file(),
        "{} holds no GStreamer build (cargo run --release -- build --target {triple})",
        prefix.display()
    );

    // The plugins and the scanner, by their paths below the prefix, which are
    // the paths below `Contents` as well.
    let mut files: Vec<String> = PLUGINS
        .iter()
        .chain(&MACOS_PLUGINS)
        .map(|name| format!("lib/gstreamer-1.0/libgst{name}.dylib"))
        .collect();
    files.push("libexec/gstreamer-1.0/gst-plugin-scanner".to_owned());
    for file in &files {
        ensure!(
            prefix.join(file).is_file(),
            "{} is missing",
            prefix.join(file).display()
        );
    }

    // The libraries griasdi links, by file: `libglib-2.0.dylib` of the build
    // is a link to the file that carries the version.
    let modules = pkgconfig::closure(&prefix.join("lib/pkgconfig"), &modules_to_read())?;
    let mut linked: BTreeMap<String, String> = BTreeMap::new();
    for name in pkgconfig::public_libs(&modules, &modules_to_read()) {
        if let Some(file) = macos::dylib_file(&lib, &name)? {
            linked.insert(name, file);
        }
    }

    let mut roots: Vec<PathBuf> = files.iter().map(|file| prefix.join(file)).collect();
    roots.extend(linked.values().map(|file| lib.join(file)));
    let mut names = macos::closure(&roots, macos::dependencies, |name| {
        let path = lib.join(name);
        path.is_file().then_some(path)
    })?;
    names.extend(linked.values().cloned());

    let mut relocated = files.clone();
    for name in &names {
        let rel = format!("lib/{name}");
        macos::install_relocated(&lib.join(name), &contents.join(&rel), &rel, &lib)?;
        relocated.push(rel);
    }
    for file in &files {
        macos::install_relocated(&prefix.join(file), &contents.join(file), file, &lib)?;
    }
    for rel in &relocated {
        macos::verify(&contents.join(rel), rel, |file| names.contains(file))?;
    }
    eprintln!(
        "{} GStreamer files in the runtime, {} of them plugins",
        relocated.len(),
        files.len() - 1
    );
    // In `Resources`: `codesign --verify --deep` takes a file directly under
    // `Contents` for code that is not signed. The license texts go there too,
    // as a directory of files that the bundle's signature seals.
    write_docs(&contents.join("Resources"), prefix, triple)?;

    // Every library a file of `link/pkgconfig` names is linked by the file of
    // the runtime that carries it, which has the version in its name.
    let mut words: BTreeMap<String, String> = BTreeMap::new();
    for module in modules.values() {
        for word in module.libs.iter().chain(&module.libs_private) {
            let Some(name) = word.strip_prefix("-l") else {
                continue;
            };
            if let Some(file) = macos::dylib_file(&lib, name)?
                && names.contains(&file)
            {
                words.insert(name.to_owned(), macos::link_word(&file));
            }
        }
    }
    let word = |name: &str| {
        words
            .get(name)
            .cloned()
            .unwrap_or_else(|| format!("-l{name}"))
    };
    let pkgconfig = link.join("pkgconfig");
    fs::create_dir_all(&pkgconfig)?;
    for module in modules.values() {
        let libdir = if module.name == DAV1D_MODULE {
            LIBDIR_LINK
        } else {
            LIBDIR_MACOS
        };
        write_file(
            &pkgconfig.join(format!("{}.pc", module.name)),
            &pkgconfig::link_pc(module, libdir, &word)?,
        )?;
    }
    let library = lib.join("libdav1d.a");
    ensure!(library.is_file(), "{} is missing", library.display());
    copy_file(&library, &link.join("lib/libdav1d.a"))
}

// --------------------------------------------------------------- shared

/// The modules whose `.pc` files `link/pkgconfig` carries, besides what they
/// require: [`LINKED_MODULES`] and dav1d.
fn modules_to_read() -> Vec<&'static str> {
    LINKED_MODULES
        .iter()
        .copied()
        .chain([DAV1D_MODULE])
        .collect()
}

/// The module `name` of the `.pc` files in `dir`.
fn read_module(dir: &Path, name: &str) -> Result<Module> {
    let path = dir.join(format!("{name}.pc"));
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    pkgconfig::parse(name, &text).with_context(|| format!("parse {}", path.display()))
}

/// Copies `from` to `to`, making the directory of `to`.
fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::copy(from, to).with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
    Ok(())
}

/// Writes `text` to `file`, making its directory.
fn write_file(file: &Path, text: &str) -> Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(file, text).with_context(|| format!("write {}", file.display()))
}

/// Writes into `dir` what has to travel with the GStreamer libraries of
/// `prefix` in every package: the notice as `GSTREAMER-NOTICE.txt`, and the
/// license texts of every component of the prefix (`share/licenses/<component>/`
/// there) as `GSTREAMER-LICENSES/<component>/`. The LGPL and the permissive
/// licenses of the libraries ask for their texts to go with the binaries, so
/// the two are never written apart.
fn write_docs(dir: &Path, prefix: &Path, triple: &str) -> Result<()> {
    write_file(&dir.join(NOTICE_FILE), &notice(triple))?;
    copy_licenses(&prefix.join(PREFIX_LICENSES), &dir.join(LICENSES_DIR))
}

/// Copies the license tree `from` to the new directory `to`. A prefix without
/// license texts, or with an empty tree, is an error: nothing may be shipped
/// without them.
fn copy_licenses(from: &Path, to: &Path) -> Result<()> {
    ensure!(
        from.is_dir(),
        "{} does not exist: the build collects the license texts of its components there",
        from.display()
    );
    let files = copy_tree(from, to)?;
    ensure!(files > 0, "{} holds no license file", from.display());
    Ok(())
}

/// Copies the directories and regular files below `from` into `to`, which is
/// made, and returns the number of files. Anything else (a link, a device) is
/// an error: what is shipped has to be what is seen.
fn copy_tree(from: &Path, to: &Path) -> Result<usize> {
    fs::create_dir_all(to).with_context(|| format!("create {}", to.display()))?;
    let mut files = 0;
    for entry in fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files += copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            copy_file(&entry.path(), &target)?;
            files += 1;
        } else {
            bail!(
                "{} is neither a file nor a directory",
                entry.path().display()
            );
        }
    }
    Ok(files)
}

/// What a staged tree holds.
#[derive(Debug)]
struct Counts {
    count: usize,
    bytes: u64,
}

/// Checks the staged release `top`: `RELEASE`, `runtime/` and `link/` and
/// nothing else at the top; below it only directories and regular files (the
/// consumer unpacks no link); every `.pc` file derives its prefix from its own
/// location.
fn check_staged(top: &Path) -> Result<Counts> {
    let mut names: Vec<String> = fs::read_dir(top)
        .with_context(|| format!("read {}", top.display()))?
        .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<_>>()?;
    names.sort();
    ensure!(
        names == ["RELEASE", "link", "runtime"],
        "the staged release holds {names:?}, not RELEASE, link and runtime"
    );
    let mut counts = Counts { count: 0, bytes: 0 };
    let mut queue = vec![top.to_path_buf()];
    while let Some(dir) = queue.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                queue.push(entry.path());
            } else if kind.is_file() {
                counts.count += 1;
                counts.bytes += entry.metadata()?.len();
                if entry.path().extension().is_some_and(|e| e == "pc") {
                    let text = fs::read_to_string(entry.path())
                        .with_context(|| format!("read {}", entry.path().display()))?;
                    ensure!(
                        text.starts_with("prefix=${pcfiledir}/../..\n"),
                        "{} does not derive its prefix from its own location",
                        entry.path().display()
                    );
                }
            } else {
                bail!(
                    "{} is neither a file nor a directory; a release holds no links",
                    entry.path().display()
                );
            }
        }
    }
    Ok(counts)
}

/// The size in bytes of the files below `dir`.
fn size_of_tree(dir: &Path) -> Result<u64> {
    let mut bytes = 0;
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            bytes += size_of_tree(&entry.path())?;
        } else {
            bytes += entry.metadata()?.len();
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::tests::pe_image as test_image;
    use std::collections::BTreeSet;

    /// A scratch directory below the system's, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!(
                "griasdi-gstreamer-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn put(file: &Path, content: impl AsRef<[u8]>) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    /// Every file below `dir` by its path below it, `/` separated.
    fn listing(dir: &Path) -> BTreeSet<String> {
        let mut files = BTreeSet::new();
        let mut queue = vec![dir.to_path_buf()];
        while let Some(current) = queue.pop() {
            for entry in fs::read_dir(&current).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    queue.push(path);
                } else {
                    let rel = path.strip_prefix(dir).unwrap();
                    files.insert(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        files
    }

    fn pc(name: &str, version: &str, requires: &str, libs: &str) -> String {
        let requires = if requires.is_empty() {
            String::new()
        } else {
            format!("Requires: {requires}\n")
        };
        format!(
            "prefix=${{pcfiledir}}/../..\nlibdir=${{prefix}}/lib\n\nName: {name}\n\
             Description: the module {name}\nVersion: {version}\n{requires}\
             Libs: -L${{libdir}} {libs}\nCflags: -I${{includedir}}/{name}\n"
        )
    }

    /// The `.pc` files of the modules griasdi links, as a build has them,
    /// each requiring the one before it, and dav1d.
    fn write_pc_files(prefix: &Path, libs: &dyn Fn(&str) -> String, dav1d: &str) {
        let mut previous = "";
        for name in LINKED_MODULES {
            put(
                &prefix.join(format!("lib/pkgconfig/{name}.pc")),
                pc(name, "2.86.3", previous, &libs(name)),
            );
            previous = name;
        }
        put(
            &prefix.join("lib/pkgconfig/dav1d.pc"),
            pc("dav1d", "1.5.4", "", dav1d),
        );
    }

    fn write_licenses(prefix: &Path) {
        put(&prefix.join("share/licenses/glib-2.86.3/COPYING"), "LGPL");
        put(
            &prefix.join("share/licenses/SVT-AV1-4.2.0/third_party/x/LICENSE"),
            "BSD",
        );
    }

    #[test]
    fn the_modules_griasdi_links_are_the_ones_its_sys_crates_probe() {
        assert_eq!(
            LINKED_MODULES,
            [
                "glib-2.0",
                "gobject-2.0",
                "gio-2.0",
                "gstreamer-1.0",
                "gstreamer-base-1.0",
                "gstreamer-app-1.0",
                "gstreamer-video-1.0",
                "gstreamer-audio-1.0",
            ]
        );
        assert_eq!(modules_to_read().last(), Some(&"dav1d"));
    }

    #[test]
    fn the_linux_runtime_is_the_library_the_notice_and_the_licenses_and_nothing_else() {
        let scratch = Scratch::new("linux");
        let prefix = scratch.0.join("prefix");
        put(&prefix.join("lib/libgstreamer-full-1.0.so"), "so");
        put(&prefix.join("lib/libdav1d.a"), "a");
        put(&prefix.join("bin/gst-launch-1.0"), "tool");
        put(&prefix.join("include/gst/gst.h"), "header");
        put(&prefix.join("lib/libglib-2.0.a"), "static glib");
        write_pc_files(
            &prefix,
            &|_| "-lglib-2.0 -lintl".to_owned(),
            "-ldav1d -pthread -ldl",
        );
        write_licenses(&prefix);

        let (runtime, link) = (scratch.0.join("runtime"), scratch.0.join("link"));
        stage_linux(&prefix, &runtime, &link, "x86_64-unknown-linux-gnu").unwrap();

        assert_eq!(
            listing(&runtime).into_iter().collect::<Vec<_>>(),
            [
                "lib/griasdi/libgstreamer-full-1.0.so",
                "share/doc/griasdi/GSTREAMER-LICENSES/SVT-AV1-4.2.0/third_party/x/LICENSE",
                "share/doc/griasdi/GSTREAMER-LICENSES/glib-2.86.3/COPYING",
                "share/doc/griasdi/GSTREAMER-NOTICE.txt",
            ]
        );
        let notice =
            fs::read_to_string(runtime.join("share/doc/griasdi/GSTREAMER-NOTICE.txt")).unwrap();
        let id = release_id();
        assert!(notice.contains(&format!(
            "gstreamer-{id}-x86_64-unknown-linux-gnu-source.tar.gz"
        )));

        let mut link_files: Vec<String> = listing(&link).into_iter().collect();
        link_files.sort();
        assert_eq!(
            link_files,
            [
                "lib/libdav1d.a",
                "pkgconfig/dav1d.pc",
                "pkgconfig/gio-2.0.pc",
                "pkgconfig/glib-2.0.pc",
                "pkgconfig/gobject-2.0.pc",
                "pkgconfig/gstreamer-1.0.pc",
                "pkgconfig/gstreamer-app-1.0.pc",
                "pkgconfig/gstreamer-audio-1.0.pc",
                "pkgconfig/gstreamer-base-1.0.pc",
                "pkgconfig/gstreamer-video-1.0.pc",
            ]
        );
        // A shim of the version of the real module, linking gst-full of the runtime.
        let shim = fs::read_to_string(link.join("pkgconfig/gstreamer-audio-1.0.pc")).unwrap();
        assert!(shim.contains("Version: 2.86.3\n"));
        assert!(shim.contains("libdir=${prefix}/runtime/lib/griasdi\n"));
        assert!(shim.contains("Libs: -L${libdir} -lgstreamer-full-1.0\n"));
        assert!(!shim.contains("-lglib-2.0") && !shim.contains("include"));
        // dav1d keeps its flags and links the static library of `link/lib`.
        let dav1d = fs::read_to_string(link.join("pkgconfig/dav1d.pc")).unwrap();
        assert!(dav1d.contains("libdir=${prefix}/link/lib\n"));
        assert!(dav1d.contains("Libs: -L${libdir} -ldav1d -pthread -ldl\n"));
        assert!(dav1d.ends_with("Cflags:\n"));
    }

    /// A fake Windows build: the DLLs of the library modules and two plugins.
    fn windows_prefix(root: &Path) -> PathBuf {
        let prefix = root.join("prefix");
        // gstreamer-1.0-0.dll imports glib, which imports intl; the plugins
        // import gstreamer, the app plugin also gstapp.
        let dlls: [(&str, &[&str]); 5] = [
            ("gstreamer-1.0-0.dll", &["glib-2.0-0.dll", "KERNEL32.dll"]),
            (
                "glib-2.0-0.dll",
                &["intl-8.dll", "api-ms-win-crt-heap-l1-1-0.dll"],
            ),
            ("intl-8.dll", &[]),
            ("gstapp-1.0-0.dll", &["gstreamer-1.0-0.dll"]),
            ("unrelated-1.dll", &[]),
        ];
        for (name, imports) in dlls {
            put(&prefix.join("bin").join(name), test_image(imports));
        }
        for name in PLUGINS
            .iter()
            .chain(&SCREEN_PLUGINS)
            .chain(&WINDOWS_PLUGINS)
        {
            put(
                &prefix.join(format!("lib/gstreamer-1.0/gst{name}.dll")),
                test_image(&["gstreamer-1.0-0.dll"]),
            );
            // The import library of a plugin is no part of a release.
            put(
                &prefix.join(format!("lib/gstreamer-1.0/gst{name}.lib")),
                "lib",
            );
        }
        for lib in [
            "glib-2.0",
            "intl",
            "gstreamer-1.0",
            "gstapp-1.0",
            "dav1d",
            "unrelated-1",
        ] {
            put(&prefix.join(format!("lib/{lib}.lib")), "import library");
        }
        write_pc_files(
            &prefix,
            &|name| match name {
                "glib-2.0" => "-lglib-2.0 -lintl".to_owned(),
                "gstreamer-app-1.0" => "-lgstapp-1.0 -lws2_32".to_owned(),
                _ => "-lgstreamer-1.0".to_owned(),
            },
            "-ldav1d",
        );
        write_licenses(&prefix);
        prefix
    }

    #[test]
    fn the_windows_runtime_is_the_closure_of_the_plugins_and_the_linked_libraries() {
        let scratch = Scratch::new("windows");
        let prefix = windows_prefix(&scratch.0);
        let pkgconf = scratch.0.join("pkgconf.exe");
        put(&pkgconf, "pkgconf");

        let (runtime, link) = (scratch.0.join("runtime"), scratch.0.join("link"));
        stage_windows(&prefix, &pkgconf, &runtime, &link, "x86_64-pc-windows-msvc").unwrap();

        let runtime_files = listing(&runtime);
        // The DLLs at the top: the closure, not `unrelated-1.dll`.
        for dll in [
            "gstreamer-1.0-0.dll",
            "glib-2.0-0.dll",
            "intl-8.dll",
            "gstapp-1.0-0.dll",
        ] {
            assert!(runtime_files.contains(dll), "{dll} in {runtime_files:?}");
        }
        assert!(!runtime_files.contains("unrelated-1.dll"));
        // The plugins below `lib/gstreamer-1.0`, without their import libraries.
        let plugins = runtime_files
            .iter()
            .filter(|f| f.starts_with("lib/gstreamer-1.0/"))
            .count();
        assert_eq!(
            plugins,
            PLUGINS.len() + SCREEN_PLUGINS.len() + WINDOWS_PLUGINS.len()
        );
        assert!(runtime_files.contains("lib/gstreamer-1.0/gstwasapi2.dll"));
        assert!(runtime_files.contains("GSTREAMER-NOTICE.txt"));
        assert!(runtime_files.contains("GSTREAMER-LICENSES/glib-2.86.3/COPYING"));
        assert!(!runtime_files.iter().any(|f| f.ends_with(".lib")));

        // `link/`: the import libraries of the libraries that are linked, no more.
        let link_files = listing(&link);
        for lib in ["glib-2.0", "intl", "gstreamer-1.0", "gstapp-1.0", "dav1d"] {
            assert!(link_files.contains(&format!("lib/{lib}.lib")), "{lib}");
        }
        assert!(!link_files.contains("lib/unrelated-1.lib"));
        // The system's `ws2_32` that a module names has no import library here.
        assert!(!link_files.iter().any(|f| f.contains("ws2_32")));
        assert!(link_files.contains("bin/pkgconf.exe"));
        let glib = fs::read_to_string(link.join("pkgconfig/glib-2.0.pc")).unwrap();
        assert!(glib.contains("libdir=${prefix}/link/lib\n"));
        assert!(glib.contains("Libs: -L${libdir} -lglib-2.0 -lintl\n"));
        assert!(link_files.contains("pkgconfig/dav1d.pc"));
    }

    #[test]
    fn a_library_griasdi_links_without_an_import_library_is_an_error() {
        let scratch = Scratch::new("windows-missing");
        let prefix = windows_prefix(&scratch.0);
        fs::remove_file(prefix.join("lib/gstapp-1.0.lib")).unwrap();
        let pkgconf = scratch.0.join("pkgconf.exe");
        put(&pkgconf, "pkgconf");
        let error = stage_windows(
            &prefix,
            &pkgconf,
            &scratch.0.join("runtime"),
            &scratch.0.join("link"),
            "x86_64-pc-windows-msvc",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("gstapp-1.0.lib is missing"), "{error}");
    }

    #[test]
    fn a_prefix_without_license_texts_cannot_be_staged() {
        let scratch = Scratch::new("licenses");
        let prefix = scratch.0.join("prefix");
        put(&prefix.join("lib/libgstreamer-full-1.0.so"), "so");
        put(&prefix.join("lib/libdav1d.a"), "a");
        write_pc_files(&prefix, &|_| "-lx".to_owned(), "-ldav1d");
        let error = stage_linux(
            &prefix,
            &scratch.0.join("runtime"),
            &scratch.0.join("link"),
            "x86_64-unknown-linux-gnu",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("does not exist"), "{error}");
        // An empty tree is none either.
        fs::create_dir_all(prefix.join("share/licenses/glib")).unwrap();
        let error = stage_linux(
            &prefix,
            &scratch.0.join("runtime2"),
            &scratch.0.join("link2"),
            "x86_64-unknown-linux-gnu",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("holds no license file"), "{error}");
    }

    #[test]
    fn a_staged_release_is_release_runtime_and_link_without_links() {
        let scratch = Scratch::new("check");
        let top = scratch.0.join("x86_64-unknown-linux-gnu");
        put(&top.join("RELEASE"), "1.28.7-2\n");
        put(&top.join("runtime/lib/x.so"), "so");
        put(
            &top.join("link/pkgconfig/a.pc"),
            "prefix=${pcfiledir}/../..\n",
        );
        let counts = check_staged(&top).unwrap();
        assert_eq!(counts.count, 3);

        // A `.pc` file with a prefix of its own.
        put(&top.join("link/pkgconfig/b.pc"), "prefix=/home/ci/gst\n");
        let error = check_staged(&top).unwrap_err().to_string();
        assert!(error.contains("b.pc does not derive its prefix"), "{error}");
        fs::remove_file(top.join("link/pkgconfig/b.pc")).unwrap();

        // Something else at the top.
        put(&top.join("include/gst.h"), "header");
        let error = check_staged(&top).unwrap_err().to_string();
        assert!(error.contains("not RELEASE, link and runtime"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_a_staged_release_is_refused() {
        let scratch = Scratch::new("symlink");
        let top = scratch.0.join("x86_64-unknown-linux-gnu");
        put(&top.join("RELEASE"), "1.28.7-2\n");
        put(&top.join("runtime/lib/libx.so.1"), "so");
        put(&top.join("link/lib/libdav1d.a"), "a");
        std::os::unix::fs::symlink("libx.so.1", top.join("runtime/lib/libx.so")).unwrap();
        let error = check_staged(&top).unwrap_err().to_string();
        assert!(error.contains("a release holds no links"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn license_texts_are_copied_as_files_and_a_link_among_them_is_refused() {
        let scratch = Scratch::new("copy-licenses");
        let from = scratch.0.join("licenses");
        put(&from.join("glib/COPYING"), "LGPL");
        put(&from.join("glib/sub/LICENSE"), "MIT");
        let to = scratch.0.join("out/GSTREAMER-LICENSES");
        copy_licenses(&from, &to).unwrap();
        assert_eq!(
            listing(&to).into_iter().collect::<Vec<_>>(),
            ["glib/COPYING", "glib/sub/LICENSE"]
        );
        std::os::unix::fs::symlink("COPYING", from.join("glib/COPYING.link")).unwrap();
        let error = copy_licenses(&from, &scratch.0.join("again"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("neither a file nor a directory"), "{error}");
    }
}
