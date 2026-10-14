//! The source patches: unified diffs in `patches/`, applied with `git apply`
//! to the monorepo checkout once the meson subprojects they touch are
//! unpacked. A patch that does not apply is an error, so a GStreamer bump
//! cannot drop a fix silently. Each patch carries its explanation in its
//! header and is under the license of the file it modifies
//! (`patches/README.md`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use crate::plan::Os;
use crate::util::{hex, root, run};

/// One patch of `patches/`.
pub struct Patch {
    /// The file name in `patches/`.
    pub file: &'static str,
    /// The only operating system it applies to; every one when `None`.
    pub os: Option<Os>,
    /// The meson subproject it touches, which has to be unpacked first
    /// (`meson subprojects download`); `None` for a file of the monorepo
    /// itself.
    pub subproject: Option<&'static str>,
}

/// The paths in a patch are relative to the root of the monorepo checkout.
pub const PATCHES: [Patch; 3] = [
    Patch {
        file: "gst-plugins-bad-svtav1enc-low-delay.patch",
        os: None,
        subproject: None,
    },
    Patch {
        file: "libva-static-library-and-header-copy.patch",
        os: Some(Os::Linux),
        subproject: Some("libva"),
    },
    Patch {
        file: "glib-macos-no-pipe2.patch",
        os: Some(Os::Macos),
        subproject: Some("glib"),
    },
];

/// The patches that apply to the build for `os`, in order.
pub fn for_os(os: Os) -> impl Iterator<Item = &'static Patch> {
    PATCHES.iter().filter(move |p| p.os.is_none_or(|o| o == os))
}

impl Patch {
    pub fn path(&self) -> PathBuf {
        root().join("patches").join(self.file)
    }
}

/// SHA-256 over the names and contents of the patches for `os`: part of what
/// a build is stamped with, so editing a patch rebuilds.
pub fn digest(os: Os) -> Result<String> {
    let mut hash = Sha256::new();
    for patch in for_os(os) {
        let path = patch.path();
        let text = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        hash.update(patch.file.as_bytes());
        hash.update([0]);
        hash.update(&text);
        hash.update([0]);
    }
    Ok(hex(&hash.finalize()))
}

/// Applies `patch` to the monorepo checkout `src`. Idempotent: a tree the
/// patch has been applied to is left alone. Fails, with git's reasons, when
/// the patch applies neither forwards nor backwards.
pub fn apply(src: &Path, patch: &Patch) -> Result<()> {
    let file = patch.path();
    ensure!(file.is_file(), "{} is missing", file.display());
    let text = fs::read(&file).with_context(|| format!("read {}", file.display()))?;
    ensure!(
        !has_crlf(&text),
        "{} has CRLF line endings, which do not match the LF sources it patches; \
         .gitattributes keeps patches LF, check out again or run `git add --renormalize .`",
        file.display()
    );
    if git_apply(src, &file, &["--reverse", "--check"])?
        .status
        .success()
    {
        return Ok(());
    }
    let check = git_apply(src, &file, &["--check"])?;
    ensure!(
        check.status.success(),
        "patches/{} does not apply to {}: the source it patches has moved on; check whether \
         upstream fixed the problem or update the patch\n{}",
        patch.file,
        src.display(),
        String::from_utf8_lossy(&check.stderr).trim()
    );
    run(Command::new("git")
        .arg("-C")
        .arg(src)
        .args(["apply", "--whitespace=nowarn"])
        .arg(&file))
}

/// Whether `text` has a Windows line ending.
fn has_crlf(text: &[u8]) -> bool {
    text.windows(2).any(|pair| pair == b"\r\n")
}

/// `git apply <flags> <file>` in `src`, with its output.
fn git_apply(src: &Path, file: &Path, flags: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(src)
        .args(["apply", "--whitespace=nowarn"])
        .args(flags)
        .arg(file)
        .output()
        .context("failed to run git")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(os: Os) -> Vec<&'static str> {
        for_os(os).map(|p| p.file).collect()
    }

    #[test]
    fn each_operating_system_gets_the_patches_for_its_sources() {
        assert_eq!(
            files(Os::Linux),
            [
                "gst-plugins-bad-svtav1enc-low-delay.patch",
                "libva-static-library-and-header-copy.patch"
            ]
        );
        assert_eq!(
            files(Os::Windows),
            ["gst-plugins-bad-svtav1enc-low-delay.patch"]
        );
        assert_eq!(
            files(Os::Macos),
            [
                "gst-plugins-bad-svtav1enc-low-delay.patch",
                "glib-macos-no-pipe2.patch"
            ]
        );
    }

    #[test]
    fn every_patch_file_is_registered_and_every_registered_patch_exists() {
        let dir = root().join("patches");
        let mut on_disk: Vec<String> = fs::read_dir(&dir)
            .expect("patches/ is part of the repository")
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| name.ends_with(".patch"))
            .collect();
        on_disk.sort();
        let mut registered: Vec<String> = PATCHES.iter().map(|p| p.file.to_owned()).collect();
        registered.sort();
        assert_eq!(on_disk, registered);
    }

    #[test]
    fn the_patches_of_the_repository_have_unix_line_endings() {
        for patch in &PATCHES {
            let text = fs::read(patch.path()).expect("registered patches exist");
            assert!(!has_crlf(&text), "{}", patch.file);
        }
        assert!(has_crlf(b"+x\r\n"));
        assert!(!has_crlf(b"+x\n+y\r"));
    }
}
