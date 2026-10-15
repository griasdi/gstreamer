//! The two archives of a release for one target:
//!
//! - `gstreamer-<id>-<target>.tar.gz`: what `stage.rs` staged out of the
//!   prefix, as exactly one top-level directory `<target>/`: `RELEASE`,
//!   `runtime/` (what goes into griasdi's installation) and `link/` (what
//!   building griasdi needs). Nothing else of the prefix is shipped.
//! - `gstreamer-<id>-<target>-source.tar.gz`: the corresponding source of
//!   those binaries, as exactly one top-level directory
//!   `gstreamer-<id>-<target>-source/`: the source trees that were built
//!   (the monorepo with the meson subprojects the build fetched, dav1d,
//!   SVT-AV1, and pkgconf on Windows), the patches already applied, and this
//!   recipe at the release's tag. No `.git` directories, no build
//!   directories, no download caches.
//!
//! Entries are sorted and carry no user or group; modes are normalised to
//! executable or not; modification times are the files' own.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail, ensure};
use flate2::Compression;
use flate2::write::GzEncoder;
use tar::{Builder, EntryType, Header, HeaderMode};

use crate::build::{build, existing};
use crate::patches;
use crate::pins::{
    DAV1D, FFMPEG_COMMIT, FFMPEG_REPO, GST_COMMIT, GST_REPO, GST_TAG, PKGCONF, SVTAV1_COMMIT,
    SVTAV1_REPO, SVTAV1_TAG,
};
use crate::plan::{Layout, Os};
use crate::release::{REPOSITORY_URL, binary_asset, release_id, release_tag, source_asset};
use crate::stage::stage;
use crate::util::{root, sha256_hex};

/// Writes both archives of the release of `target` (the host when `None`)
/// into `out`. The prefix is built first if it is not up to date; with
/// `no_build` it is taken as it is.
pub fn dist(target: Option<&str>, no_build: bool, out: &Path) -> Result<()> {
    let layout = if no_build {
        existing(target)?
    } else {
        build(target)?
    };
    let id = release_id();
    fs::create_dir_all(out).with_context(|| format!("create {}", out.display()))?;
    // Absolute, to tell whether it lies inside this repository (see
    // `recipe_files`).
    let out = fs::canonicalize(out).with_context(|| format!("resolve {}", out.display()))?;

    let staged = stage(&layout)?;
    let binary = out.join(binary_asset(&id, &layout.triple));
    write_archive(&binary, |archive| {
        archive.add_tree(&staged.top, Path::new(&layout.triple), &|_| false)
    })?;
    report(&binary)?;
    // What the archive holds is copied out of the prefix; the staged copy is
    // of no further use.
    fs::remove_dir_all(&staged.root)
        .with_context(|| format!("remove {}", staged.root.display()))?;

    let source = out.join(source_asset(&id, &layout.triple));
    write_archive(&source, |archive| add_sources(archive, &layout, &id, &out))?;
    report(&source)?;
    Ok(())
}

/// What is not source: version control, caches of what ran from the trees,
/// meson's lock file of the downloads, the recipe's own markers and the
/// monorepo's downloads (the unpacked trees are what was built).
fn source_skipped(rel: &Path) -> bool {
    let name = rel.file_name().and_then(OsStr::to_str).unwrap_or("");
    matches!(
        name,
        ".git"
            | "__pycache__"
            | ".wraplock"
            | ".griasdi-fetched"
            | ".griasdi-pin"
            | ".griasdi-pins"
    ) || rel == Path::new("subprojects/packagecache")
}

fn add_sources(archive: &mut Archive, layout: &Layout, id: &str, out: &Path) -> Result<()> {
    let top = PathBuf::from(format!("gstreamer-{id}-{}-source", layout.triple));
    let os = Os::of(&layout.triple);
    archive.add_bytes(
        &top.join("SOURCES.txt"),
        sources_text(os, &layout.triple, id).as_bytes(),
    )?;
    let mut trees = vec![
        (format!("gstreamer-{GST_TAG}"), layout.src.clone()),
        (DAV1D.dir.to_owned(), layout.dav1d_src()),
        (
            format!("SVT-AV1-{}", SVTAV1_TAG.trim_start_matches('v')),
            layout.svtav1_src(),
        ),
    ];
    if os == Os::Windows {
        trees.push((PKGCONF.dir.to_owned(), layout.pkgconf_src()));
    }
    for (name, dir) in &trees {
        ensure!(dir.is_dir(), "{} is missing: {name}", dir.display());
        archive.add_tree(dir, &top.join(name), &source_skipped)?;
    }
    add_recipe(archive, &top.join("recipe"), out)
}

/// This repository, as git sees it (tracked files, and untracked ones that
/// are not ignored, so that a local run of an uncommitted recipe carries it
/// too), under `name`; `out` is the directory the archives are written to.
fn add_recipe(archive: &mut Archive, name: &Path, out: &Path) -> Result<()> {
    let repo = fs::canonicalize(root()).context("resolve the repository")?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .context("failed to run git")?;
    ensure!(
        output.status.success(),
        "git ls-files failed in {}: the recipe has to be built from a git checkout\n{}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let files = recipe_files(&output.stdout, &repo, out);
    ensure!(
        !files.is_empty(),
        "git lists no files in {}",
        repo.display()
    );
    archive.add_directory(&repo, name)?;
    let mut directories: BTreeSet<PathBuf> = BTreeSet::new();
    for file in &files {
        // The directories above the file, once each.
        let mut ancestors: Vec<&Path> = file
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        ancestors.reverse();
        for dir in ancestors {
            if directories.insert(dir.to_path_buf()) {
                archive.add_directory(&repo.join(dir), &name.join(dir))?;
            }
        }
        archive.add_path(&repo.join(file), &name.join(file))?;
    }
    Ok(())
}

/// The files of the recipe, relative to `repo`, from the NUL separated output
/// of `git ls-files`: sorted, each once, and none that lies in `out`. The
/// archives are written to `out`, which may be a directory of the repository
/// that git does not ignore (the CI's artifacts directory); an archive that
/// held the archive being written, and itself, would never end.
fn recipe_files(listing: &[u8], repo: &Path, out: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = listing
        .split(|&b| b == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(String::from_utf8_lossy(path).into_owned()))
        .filter(|path| !repo.join(path).starts_with(out))
        .collect();
    files.sort();
    files.dedup();
    files
}

/// `SOURCES.txt` at the top of the source archive.
fn sources_text(os: Os, triple: &str, id: &str) -> String {
    let svtav1 = SVTAV1_TAG.trim_start_matches('v');
    let pkgconf = if os == Os::Windows {
        format!(
            "  {pkgconf}/\n      pkgconf, the release tarball {url}\n      (SHA-256 {sha})\n",
            pkgconf = PKGCONF.dir,
            url = PKGCONF.url,
            sha = PKGCONF.sha256
        )
    } else {
        String::new()
    };
    let patches: String = patches::for_os(os)
        .map(|p| format!("      patches/{}\n", p.file))
        .collect();
    format!(
        "\
Corresponding source of GStreamer for griasdi
=============================================

Release {id}, target {triple}. This archive holds the source that the binaries of
{binary} were built from, as the build had it: the patches are
applied, nothing is built. The GSTREAMER-NOTICE.txt in the runtime of that archive says
what it contains and under which licenses.

  gstreamer-{GST_TAG}/
      The GStreamer monorepo at tag {GST_TAG} (commit {GST_COMMIT}),
      {GST_REPO}, with the meson subprojects the
      build fetched into subprojects/: GLib, libffi, pcre2, zlib, opus, orc, FFmpeg's
      meson port ({FFMPEG_REPO}, commit {FFMPEG_COMMIT}) and the others the monorepo's
      wrap files name, each with its origin and SHA-256 in the wrap file. Without
      .git directories and without the download cache subprojects/packagecache.
  {dav1d}/
      dav1d, the release tarball {dav1d_url}
      (SHA-256 {dav1d_sha})
  SVT-AV1-{svtav1}/
      SVT-AV1, {SVTAV1_REPO} at tag {SVTAV1_TAG} (commit {SVTAV1_COMMIT})
{pkgconf}  recipe/
      This recipe, the repository {REPOSITORY_URL} at tag {tag}. Its
      patches/ are applied to the trees above:
{patches}
To rebuild the binaries, run in recipe/
  cargo run --release -- build --target {triple}
It fetches the same sources again, from the pins in src/pins.rs, and applies the same
patches. README.md lists what the build needs.
",
        binary = binary_asset(id, triple),
        dav1d = DAV1D.dir,
        dav1d_url = DAV1D.url,
        dav1d_sha = DAV1D.sha256,
        tag = release_tag(),
    )
}

// ---------------------------------------------------------------- tar.gz

/// A `.tar.gz` that is written next to its final name and moved there when it
/// is complete.
struct Archive {
    tar: Builder<GzEncoder<BufWriter<File>>>,
    partial: PathBuf,
    path: PathBuf,
}

/// Writes the archive `path`, filled by `fill`. A failed archive leaves
/// nothing behind.
fn write_archive(path: &Path, fill: impl FnOnce(&mut Archive) -> Result<()>) -> Result<()> {
    let mut partial = path
        .file_name()
        .context("an archive without a file name")?
        .to_owned();
    partial.push(".partial");
    let partial = path.with_file_name(partial);
    let file = File::create(&partial).with_context(|| format!("create {}", partial.display()))?;
    let mut archive = Archive {
        tar: Builder::new(GzEncoder::new(BufWriter::new(file), Compression::default())),
        partial,
        path: path.to_owned(),
    };
    if let Err(error) = fill(&mut archive) {
        let _ = fs::remove_file(&archive.partial);
        return Err(error);
    }
    archive.finish()
}

impl Archive {
    fn finish(self) -> Result<()> {
        let encoder = self.tar.into_inner().context("finish the tar stream")?;
        let mut file = encoder.finish().context("finish the gzip stream")?;
        file.flush()?;
        drop(file);
        fs::rename(&self.partial, &self.path)
            .with_context(|| format!("move {} to {}", self.partial.display(), self.path.display()))
    }

    /// Adds the directory `dir` and everything below it as `name`, except
    /// what `skipped` (given a path relative to `dir`) names.
    fn add_tree(&mut self, dir: &Path, name: &Path, skipped: &dyn Fn(&Path) -> bool) -> Result<()> {
        self.add_directory(dir, name)?;
        self.add_below(dir, name, Path::new(""), skipped)
    }

    fn add_below(
        &mut self,
        dir: &Path,
        name: &Path,
        rel: &Path,
        skipped: &dyn Fn(&Path) -> bool,
    ) -> Result<()> {
        let here = dir.join(rel);
        let mut entries: Vec<_> = fs::read_dir(&here)
            .with_context(|| format!("read {}", here.display()))?
            .collect::<io::Result<_>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let rel = rel.join(entry.file_name());
            if skipped(&rel) {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.add_directory(&entry.path(), &name.join(&rel))?;
                self.add_below(dir, name, &rel, skipped)?;
            } else if kind.is_file() || kind.is_symlink() {
                self.add_path(&entry.path(), &name.join(&rel))?;
            } else {
                eprintln!(
                    "skipping {}: neither file, directory nor link",
                    entry.path().display()
                );
            }
        }
        Ok(())
    }

    /// A directory entry named `name`, with the modes and time of `dir`.
    fn add_directory(&mut self, dir: &Path, name: &Path) -> Result<()> {
        let meta = fs::symlink_metadata(dir).with_context(|| format!("stat {}", dir.display()))?;
        let mut header = header_of(&meta);
        header.set_entry_type(EntryType::Directory);
        header.set_size(0);
        self.tar
            .append_data(&mut header, name, io::empty())
            .with_context(|| format!("add {}", name.display()))
    }

    /// A regular file or a symlink named `name`, with the content (or target)
    /// of `path`.
    fn add_path(&mut self, path: &Path, name: &Path) -> Result<()> {
        let meta =
            fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
        let mut header = header_of(&meta);
        if meta.file_type().is_symlink() {
            let target =
                fs::read_link(path).with_context(|| format!("read link {}", path.display()))?;
            header.set_entry_type(EntryType::Symlink);
            header.set_size(0);
            self.tar
                .append_link(&mut header, name, &target)
                .with_context(|| format!("add {}", name.display()))
        } else if meta.is_file() {
            let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
            header.set_entry_type(EntryType::Regular);
            header.set_size(meta.len());
            self.tar
                .append_data(&mut header, name, file)
                .with_context(|| format!("add {}", name.display()))
        } else {
            bail!("{} is neither a file nor a link", path.display())
        }
    }

    /// A regular file named `name` with the content `data`.
    fn add_bytes(&mut self, name: &Path, data: &[u8]) -> Result<()> {
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_size(data.len() as u64);
        header.set_mtime(now());
        self.tar
            .append_data(&mut header, name, data)
            .with_context(|| format!("add {}", name.display()))
    }
}

/// The header of an entry for `meta`: owner `0`, no names, modes normalised
/// to what a consumer needs (executable or not), the file's own time.
fn header_of(meta: &fs::Metadata) -> Header {
    let mut header = Header::new_gnu();
    header.set_metadata_in_mode(meta, HeaderMode::Deterministic);
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    header.set_mtime(mtime);
    header
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Prints the size and the `sha256sum` line of `path`.
fn report(path: &Path) -> Result<()> {
    let size = fs::metadata(path)?.len();
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .context("an archive without a file name")?;
    eprintln!("wrote {} ({size} bytes)", shown(path));
    println!("{}  {name}", sha256_hex(path)?);
    Ok(())
}

/// `path` for people: without the `\\?\` that `fs::canonicalize` puts in
/// front of a path on Windows.
fn shown(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_control_caches_and_markers_are_no_source() {
        for skipped in [
            ".git",
            "subprojects/FFmpeg/.git",
            "subprojects/glib-2.86.3/gio/__pycache__",
            ".griasdi-fetched",
            "subprojects/.wraplock",
            "subprojects/packagecache",
        ] {
            assert!(source_skipped(Path::new(skipped)), "{skipped}");
        }
        for kept in [
            ".gitignore",
            ".gitattributes",
            "subprojects/glib.wrap",
            "subprojects/packagefiles/glib/x.patch",
            "docs/packagecache",
        ] {
            assert!(!source_skipped(Path::new(kept)), "{kept}");
        }
    }

    #[test]
    fn the_source_listing_names_the_release_and_the_patches_of_the_target() {
        let linux = sources_text(Os::Linux, "x86_64-unknown-linux-gnu", "1.28.7-1");
        assert!(linux.contains("gstreamer-1.28.7-1-x86_64-unknown-linux-gnu.tar.gz"));
        assert!(linux.contains("patches/libva-static-library-and-header-copy.patch"));
        assert!(!linux.contains("pkgconf"));
        let windows = sources_text(Os::Windows, "x86_64-pc-windows-msvc", "1.28.7-1");
        assert!(windows.contains("pkgconf-3.0.8/"));
        assert!(!windows.contains("libva"));
        assert!(!linux.contains('{') && !windows.contains('{'));
    }
    #[test]
    fn a_verbatim_windows_path_is_shown_without_its_prefix() {
        assert_eq!(shown(Path::new(r"\\?\W:\out\a.tar.gz")), r"W:\out\a.tar.gz");
        assert_eq!(shown(Path::new("/tmp/out/a.tar.gz")), "/tmp/out/a.tar.gz");
    }

    #[test]
    fn the_output_directory_is_no_part_of_the_recipe() {
        let listing =
            b"README.md\0src/main.rs\0out/a.tar.gz\0out/a.tar.gz.partial\0src/main.rs\0\0";
        let repo = Path::new("/repo");
        assert_eq!(
            recipe_files(listing, repo, Path::new("/repo/out")),
            [PathBuf::from("README.md"), PathBuf::from("src/main.rs")]
        );
        // An output directory elsewhere keeps every file, once each.
        assert_eq!(
            recipe_files(listing, repo, Path::new("/elsewhere")).len(),
            4
        );
        // `outer/x` is no file of `out`.
        assert_eq!(
            recipe_files(b"outer/x\0", repo, Path::new("/repo/out")),
            [PathBuf::from("outer/x")]
        );
    }

    /// The archive of a small tree: one top-level directory, sorted entries,
    /// the skipped names absent, a symlink kept as a link (the source has
    /// some), the executable bit kept, no owner.
    #[cfg(unix)]
    #[test]
    fn an_archive_has_one_top_level_directory_and_keeps_links_and_modes() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = std::env::temp_dir().join(format!(
            "griasdi-gstreamer-test-{}-{}",
            std::process::id(),
            now()
        ));
        let tree = dir.join("tree");
        fs::create_dir_all(tree.join("lib/sub")).unwrap();
        fs::create_dir_all(tree.join("bin")).unwrap();
        fs::write(tree.join("lib/libx.so.1"), b"so").unwrap();
        symlink("libx.so.1", tree.join("lib/libx.so")).unwrap();
        fs::write(tree.join("lib/sub/data"), b"data").unwrap();
        fs::write(tree.join("bin/tool"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(tree.join("bin/tool"), fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(tree.join(".skipped"), b"stamp").unwrap();

        let out = dir.join("out.tar.gz");
        write_archive(&out, |archive| {
            archive.add_tree(&tree, Path::new("x86_64-unknown-linux-gnu"), &|rel| {
                rel == Path::new(".skipped")
            })
        })
        .unwrap();
        assert!(!dir.join("out.tar.gz.partial").exists());

        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(File::open(&out).unwrap()));
        let mut seen = Vec::new();
        for entry in tar.entries().unwrap() {
            let entry = entry.unwrap();
            let name = entry
                .path()
                .unwrap()
                .to_string_lossy()
                .trim_end_matches('/')
                .to_owned();
            let header = entry.header();
            assert_eq!(header.uid().unwrap(), 0, "{name}");
            assert!(
                header.username().unwrap().is_none_or(str::is_empty),
                "{name}"
            );
            let mode = header.mode().unwrap();
            let kind = header.entry_type();
            let link = entry
                .link_name()
                .unwrap()
                .map(|l| l.to_string_lossy().into_owned());
            seen.push((name, kind, mode, link));
        }
        let names: Vec<&str> = seen.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "x86_64-unknown-linux-gnu",
                "x86_64-unknown-linux-gnu/bin",
                "x86_64-unknown-linux-gnu/bin/tool",
                "x86_64-unknown-linux-gnu/lib",
                "x86_64-unknown-linux-gnu/lib/libx.so",
                "x86_64-unknown-linux-gnu/lib/libx.so.1",
                "x86_64-unknown-linux-gnu/lib/sub",
                "x86_64-unknown-linux-gnu/lib/sub/data",
            ]
        );
        let by_name = |n: &str| seen.iter().find(|s| s.0 == n).unwrap();
        assert_eq!(by_name("x86_64-unknown-linux-gnu/bin/tool").2, 0o755);
        assert_eq!(by_name("x86_64-unknown-linux-gnu/lib/libx.so.1").2, 0o644);
        let link = by_name("x86_64-unknown-linux-gnu/lib/libx.so");
        assert_eq!(link.1, EntryType::Symlink);
        assert_eq!(link.3.as_deref(), Some("libx.so.1"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
