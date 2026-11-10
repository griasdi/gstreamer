//! Building GStreamer and its C dependencies from source, the same recipe on
//! every platform: `cargo run --release -- build [--target <triple>]`.
//!
//! The source is the GStreamer monorepo at a pinned tag and commit; its
//! meson subprojects (GLib, libffi, pcre2, zlib, opus, orc, FFmpeg's meson
//! port, ...) are fetched by meson itself, the wrap files pinned by upstream
//! with SHA-256 and the two git wraps (FFmpeg, orc) rewritten to pinned
//! commits here. dav1d (AV1, for the `gst-plugin-dav1d` crate) is built from
//! its release tarball into the same prefix, SVT-AV1 (`svtav1enc`, on all
//! three OSes) from its git tag with CMake. The build tools come from `uv`
//! (meson, ninja, cmake); nasm is built from its source tarball on Linux and
//! macOS and is the official pinned binary on Windows; Windows also gets
//! pkgconf, built with meson, for cargo's pkg-config probes and as meson's
//! pkg-config. The few source fixes are patch files in `patches/`
//! (`patches.rs`).
//!
//! - Linux: one shared `libgstreamer-full-1.0.so` (meson's gst-full, every
//!   library static inside it) with exactly the plugins the player needs.
//!   The static archives in `<prefix>/lib` are never linked into the app.
//! - Windows and macOS: a shared build (one DLL or dylib per library and
//!   plugin) of the same source and plugin set.
//!
//! Everything lands below `target/gstreamer/`: `tools/` (shared), the
//! source in `src-<triple>/`, the build directories and the meson machine
//! file in `build-<triple>/` and the install prefix in `<triple>/`, which is
//! the build output: a release is staged out of it (`stage.rs`). After the
//! install the prefix is tidied (`finish.rs`).
//! `<prefix>/.recipe` holds a hash of the whole recipe; a build with a
//! matching hash returns at once, any other change rebuilds from scratch. A
//! build that failed while finishing (`<prefix>/.installed` matches) is not
//! compiled again, only finished.
//!
//! References: GStreamer's monorepo README ("gst-full"), `meson.options` of
//! the monorepo and of `subprojects/FFmpeg` at the pinned commits, and
//! https://mesonbuild.com/Cross-compilation.html for the cross files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::finish;
use crate::patches;
use crate::pins::{
    DAV1D, FFMPEG_COMMIT, GIT_WRAPS, GST_COMMIT, GST_REPO, GST_TAG, OPUS, ORC_COMMIT,
    PYTHON_PLACEHOLDER, SVTAV1_COMMIT, SVTAV1_REPO, SVTAV1_TAG, TARGETS,
};
use crate::plan::{Layout, Os, Plan, host_triple};
use crate::tools::{Tools, fetch, jobs, tools};
use crate::util::{git_checkout_env, run};

/// `target` (the host when `None`) as a target triple GStreamer is built for.
fn resolve_target(target: Option<&str>) -> Result<String> {
    let triple = match target {
        Some(t) => t.to_owned(),
        None => host_triple()?,
    };
    ensure!(
        TARGETS.contains(&triple.as_str()),
        "GStreamer is not built for {triple}; the targets are {}",
        TARGETS.join(", ")
    );
    Ok(triple)
}

/// The layout of `target` (the host when `None`) whose install prefix a build
/// left finished, taken as it is: whether it still matches this recipe is not
/// checked, and nothing is built.
pub fn existing(target: Option<&str>) -> Result<Layout> {
    let triple = resolve_target(target)?;
    let layout = Layout::new(&triple);
    ensure!(
        layout.prefix.join(".recipe").is_file(),
        "{} holds no finished build of GStreamer for {triple}: build it first, or leave out \
         --no-build",
        layout.prefix.display()
    );
    Ok(layout)
}

/// The layout of `target` (the host when `None`), with its install prefix
/// built and finished if it is not up to date.
pub fn build(target: Option<&str>) -> Result<Layout> {
    let triple = resolve_target(target)?;
    let plan = Plan::new(Layout::new(&triple))?;
    let layout = &plan.layout;
    let stamp = plan.stamp()?;
    let finished = layout.prefix.join(".recipe");
    let installed = layout.prefix.join(".installed");
    let current =
        |marker: &Path| fs::read_to_string(marker).ok().as_deref() == Some(stamp.as_str());
    if current(&finished) {
        return Ok(plan.layout);
    }
    eprintln!(
        "building GStreamer {GST_TAG} for {triple} into {}",
        layout.prefix.display()
    );
    if !current(&installed) {
        if plan.cross {
            // The GLib tools the cross file names come from the host's build.
            build(None)?;
        }
        let tools = tools(&plan)?;
        for dir in [&layout.prefix, &layout.build] {
            if dir.exists() {
                fs::remove_dir_all(dir).with_context(|| format!("remove {}", dir.display()))?;
            }
        }
        fs::create_dir_all(&layout.prefix)?;
        fs::create_dir_all(&layout.build)?;
        // The tools' Python path is not part of the stamp: it is an internal
        // detail of target/gstreamer/tools.
        let python = tools.python.display().to_string().replace('\\', "/");
        fs::write(
            layout.machine_file(),
            plan.machine_file.replace(PYTHON_PLACEHOLDER, &python),
        )?;
        if let Some(clang_file) = &plan.clang_file {
            fs::write(layout.clang_file(), clang_file)?;
        }
        build_dav1d(&plan, &tools)?;
        build_opus(&plan, &tools)?;
        build_svtav1(&plan, &tools)?;
        fetch_gstreamer(&plan, &tools)?;
        if let Some(args) = &plan.ffmpeg_args {
            build_ffmpeg(&plan, &tools, args)?;
        }
        build_gstreamer(&plan, &tools)?;
        fs::write(&installed, &stamp)?;
    }
    finish::finish(&plan)?;
    fs::write(&finished, &stamp)?;
    Ok(plan.layout)
}

fn build_dav1d(plan: &Plan, tools: &Tools) -> Result<()> {
    let layout = &plan.layout;
    let src = fetch(&layout.tools(), &DAV1D, &tools.python)?;
    let build = layout.build.join("dav1d");
    run(tools
        .meson()
        .arg("setup")
        .arg(&build)
        .arg(&src)
        .arg(format!("--prefix={}", layout.prefix.display()))
        .args(&plan.dav1d_args))?;
    run(tools.meson().arg("install").arg("-C").arg(&build))?;
    if plan.os == Os::Windows {
        // meson names a static library `libdav1d.a` on MSVC too; the MSVC
        // linker that rustc drives looks for `dav1d.lib`.
        let lib = layout.prefix.join("lib");
        if lib.join("libdav1d.a").is_file() && !lib.join("dav1d.lib").exists() {
            fs::copy(lib.join("libdav1d.a"), lib.join("dav1d.lib"))?;
        }
    }
    Ok(())
}

/// The static libopus griasdi's voice engine links (`opusic-sys` with
/// `OPUS_LIB_DIR`), in its own install tree ([`Layout::opus_prefix`]): with
/// CMake and clang-cl on Windows, with meson elsewhere ([`Plan::opus_args`]).
fn build_opus(plan: &Plan, tools: &Tools) -> Result<()> {
    let layout = &plan.layout;
    let src = fetch(&layout.tools(), &OPUS, &tools.python)?;
    let build = layout.build.join("opus");
    // libopus falls back to its plain C paths without telling when the
    // compiler lacks the intrinsics. meson writes what it found into
    // `config.h` (opus 1.6.1 `meson.build`), CMake passes it as defines,
    // which are on the compile lines of `build.ninja`.
    type Defined = fn(&str, &str) -> bool;
    let (configured, defined): (PathBuf, Defined) = if plan.os == Os::Windows {
        run(tools
            .cmake()
            .arg("-S")
            .arg(&src)
            .arg("-B")
            .arg(&build)
            .args(&plan.opus_args))?;
        (build.join("build.ninja"), |text, name| {
            text.split_whitespace()
                .any(|word| word == format!("-D{name}"))
        })
    } else {
        run(tools
            .meson()
            .arg("setup")
            .arg(&build)
            .arg(&src)
            .arg(format!("--prefix={}", layout.opus_prefix().display()))
            .args(&plan.opus_args))?;
        (build.join("config.h"), |text, name| {
            text.lines()
                .any(|line| line.trim() == format!("#define {name} 1"))
        })
    };
    let config = fs::read_to_string(&configured)
        .with_context(|| format!("read {}", configured.display()))?;
    let required: &[(&str, &str)] = if plan.x86_64 {
        &[
            (
                "OPUS_HAVE_RTCD",
                "libopus was configured without run-time CPU detection",
            ),
            (
                "OPUS_X86_MAY_HAVE_AVX2",
                "libopus was configured without run-time AVX2 dispatch",
            ),
        ]
    } else {
        &[(
            "OPUS_ARM_MAY_HAVE_NEON_INTR",
            "libopus was configured without NEON intrinsics",
        )]
    };
    for (define, error) in required {
        ensure!(defined(&config, define), "{error}");
    }
    if plan.os == Os::Windows {
        run(tools
            .cmake()
            .arg("--build")
            .arg(&build)
            .arg("--parallel")
            .arg(jobs().to_string()))?;
        run(tools.cmake().arg("--install").arg(&build))?;
    } else {
        run(tools.meson().arg("install").arg("-C").arg(&build))?;
    }
    Ok(())
}

/// SVT-AV1's source at the pinned tag in `src-svtav1`, shared by every
/// target; a clone of the same commit is reused.
fn fetch_svtav1(layout: &Layout) -> Result<PathBuf> {
    let src = layout.svtav1_src();
    let marker = src.join(".griasdi-fetched");
    if fs::read_to_string(&marker).ok().as_deref() == Some(SVTAV1_COMMIT) {
        return Ok(src);
    }
    if src.exists() {
        fs::remove_dir_all(&src)?;
    }
    run(git_checkout_env(
        Command::new("git")
            .args(["clone", "--depth", "1", "--branch", SVTAV1_TAG, SVTAV1_REPO])
            .arg(&src),
    ))?;
    ensure!(
        git_head(&src)? == SVTAV1_COMMIT,
        "tag {SVTAV1_TAG} of {SVTAV1_REPO} is not commit {SVTAV1_COMMIT}"
    );
    fs::write(&marker, SVTAV1_COMMIT)?;
    Ok(src)
}

/// The static SVT-AV1 encoder library and its `SvtAv1Enc.pc` in the prefix,
/// for gst-plugins-bad's svtav1 plugin.
fn build_svtav1(plan: &Plan, tools: &Tools) -> Result<()> {
    let layout = &plan.layout;
    let src = fetch_svtav1(layout)?;
    let build = layout.build.join("svtav1");
    run(tools
        .cmake()
        .arg("-S")
        .arg(&src)
        .arg("-B")
        .arg(&build)
        .args(&plan.svtav1_args))?;
    run(tools
        .cmake()
        .arg("--build")
        .arg(&build)
        .arg("--parallel")
        .arg(jobs().to_string()))?;
    run(tools.cmake().arg("--install").arg(&build))?;
    ensure!(
        layout.prefix.join("lib/pkgconfig/SvtAv1Enc.pc").is_file(),
        "SVT-AV1 installed no SvtAv1Enc.pc"
    );
    Ok(())
}

/// Clones the monorepo at the pinned commit, pins its git wraps and checks
/// them out, and applies the patches (`patches.rs`). The other subprojects
/// (SHA-256-pinned wrap files) are fetched by `meson setup`, only those the
/// build uses; those a patch touches are unpacked here, before it applies. A
/// complete earlier fetch of the same pins and patches is reused.
fn fetch_gstreamer(plan: &Plan, tools: &Tools) -> Result<()> {
    let src = &plan.layout.src;
    let marker = src.join(".griasdi-fetched");
    let pins = format!(
        "{GST_COMMIT} {FFMPEG_COMMIT} {ORC_COMMIT} {}",
        patches::digest(plan.os)?
    );
    if fs::read_to_string(&marker).ok().as_deref() == Some(pins.as_str()) {
        return Ok(());
    }
    if src.exists() {
        fs::remove_dir_all(src)?;
    }
    run(git_checkout_env(
        Command::new("git")
            .args(["clone", "--depth", "1", "--branch", GST_TAG, GST_REPO])
            .arg(src),
    ))?;
    ensure!(
        git_head(src)? == GST_COMMIT,
        "tag {GST_TAG} of {GST_REPO} is not commit {GST_COMMIT}"
    );
    for (wrap, _, commit) in GIT_WRAPS {
        let path = src.join("subprojects").join(wrap);
        let text = fs::read_to_string(&path)?;
        let pinned =
            pin_revision(&text, commit).with_context(|| format!("{wrap} has no revision line"))?;
        fs::write(&path, pinned)?;
    }
    // One at a time: the downloads share meson's `.wraplock`, and on Windows
    // `msvcrt.locking` gives up after about ten seconds (`OSError: [Errno 36]
    // Resource deadlock avoided`, meson 1.12.1 `utils/platform.py`) while the
    // FFmpeg clone holds it.
    run(tools
        .meson()
        .args(["subprojects", "download", "--num-processes", "1"])
        .args(GIT_WRAPS.map(|(_, dir, _)| dir))
        .current_dir(src))?;
    for (_, dir, commit) in GIT_WRAPS {
        let head = git_head(&src.join("subprojects").join(dir))?;
        ensure!(
            head == commit,
            "subproject {dir} is at {head}, not {commit}"
        );
    }
    for patch in patches::for_os(plan.os) {
        if let Some(subproject) = patch.subproject {
            run(tools
                .meson()
                .args(["subprojects", "download", subproject])
                .current_dir(src))?;
        }
        patches::apply(src, patch)?;
    }
    fs::write(&marker, &pins)?;
    Ok(())
}

/// `text` (a wrap file) with its `revision = ` line set to `commit`.
fn pin_revision(text: &str, commit: &str) -> Option<String> {
    let mut found = false;
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("revision") && line.contains('=') {
                found = true;
                format!("revision = {commit}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    found.then(|| lines.join("\n") + "\n")
}

fn git_head(dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()?;
    ensure!(
        output.status.success(),
        "git rev-parse failed in {}",
        dir.display()
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Removes the files `rels` below `dir` that exist.
fn remove_files(dir: &Path, rels: &[&str]) -> Result<()> {
    for rel in rels {
        let path = dir.join(rel);
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
    }
    Ok(())
}

/// Windows: FFmpeg's meson port on its own, compiled with clang-cl
/// ([`Plan::ffmpeg_args`]), installed into the prefix, where the GStreamer
/// build finds it through pkg-config.
fn build_ffmpeg(plan: &Plan, tools: &Tools, args: &[String]) -> Result<()> {
    let layout = &plan.layout;
    let src = layout.src.join("subprojects/FFmpeg");
    // As in `build_gstreamer`: the nasm of the machine file, not the wrap's.
    remove_files(&src, &["subprojects/nasm.wrap"])?;
    let build = layout.build.join("ffmpeg");
    run(tools
        .meson()
        .arg("setup")
        .arg(&build)
        .arg(&src)
        .arg(format!("--prefix={}", layout.prefix.display()))
        .args(args))?;
    check_ffmpeg_config(plan, &build.join("config.h"))?;
    run(tools.meson().arg("compile").arg("-C").arg(&build))?;
    run(tools.meson().arg("install").arg("-C").arg(&build))?;
    Ok(())
}

/// FFmpeg falls back to plain C without telling when nasm, inline assembly
/// or NEON is missing; its decoders would then be several times slower.
fn check_ffmpeg_config(plan: &Plan, config: &Path) -> Result<()> {
    let text = fs::read_to_string(config).with_context(|| format!("read {}", config.display()))?;
    let required: &[(&str, &str)] = if plan.x86_64 {
        &[
            (
                "#define HAVE_X86ASM 1",
                "FFmpeg was configured without nasm",
            ),
            (
                "#define HAVE_INLINE_ASM 1",
                "FFmpeg was configured without inline assembly: its H.264 and VP9 entropy \
                 decoders would be plain C",
            ),
        ]
    } else {
        &[("#define HAVE_NEON 1", "FFmpeg was configured without NEON")]
    };
    for (define, error) in required {
        ensure!(text.contains(define), "{error}");
    }
    Ok(())
}

fn build_gstreamer(plan: &Plan, tools: &Tools) -> Result<()> {
    let layout = &plan.layout;
    // FFmpeg's nasm wrap only downloads unpinned binaries (and none on
    // Linux), and `--wrap-mode=forcefallback` would prefer it over the nasm
    // the machine file names; without the wrap meson takes that one.
    let mut wraps = vec![
        "subprojects/nasm.wrap",
        "subprojects/FFmpeg/subprojects/nasm.wrap",
    ];
    if plan.ffmpeg_args.is_some() {
        // The only provider of `libavcodec`: without it, forcefallback looks
        // the modules up with pkg-config and finds the clang-cl build in the
        // prefix (`--pkg-config-path`).
        wraps.push("subprojects/FFmpeg.wrap");
    }
    remove_files(&layout.src, &wraps)?;
    let build = layout.build.join("gstreamer");
    run(tools
        .meson()
        .arg("setup")
        .arg(&build)
        .arg(&layout.src)
        .arg(format!("--prefix={}", layout.prefix.display()))
        .args(&plan.gst_args))?;
    if plan.ffmpeg_args.is_some() {
        ensure!(
            !build.join("subprojects/FFmpeg").exists(),
            "the GStreamer build configured its own FFmpeg instead of the clang-cl build in the \
             prefix"
        );
    } else {
        check_ffmpeg_config(plan, &build.join("subprojects/FFmpeg/config.h"))?;
    }
    run(tools.meson().arg("compile").arg("-C").arg(&build))?;
    run(tools.meson().arg("install").arg("-C").arg(&build))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_one_of_the_four_or_an_error_that_names_them() {
        for target in TARGETS {
            assert_eq!(resolve_target(Some(target)).unwrap(), target);
        }
        let error = resolve_target(Some("riscv64-unknown-linux-gnu"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("not built for riscv64-unknown-linux-gnu"),
            "{error}"
        );
        assert!(error.contains("x86_64-pc-windows-msvc"), "{error}");
    }

    #[test]
    fn a_wrap_gets_the_pinned_revision_and_nothing_else_changes() {
        let wrap = "[wrap-git]\ndirectory=FFmpeg\nurl=https://example.org/ffmpeg.git\n\
                    revision=meson-7.1.1\ndepth=1\n\n[provide]\nlibavcodec = libavcodec_dep\n";
        assert_eq!(
            pin_revision(wrap, "cde7209").unwrap(),
            "[wrap-git]\ndirectory=FFmpeg\nurl=https://example.org/ffmpeg.git\n\
             revision = cde7209\ndepth=1\n\n[provide]\nlibavcodec = libavcodec_dep\n"
        );
    }

    #[test]
    fn a_wrap_with_windows_line_endings_is_written_back_with_unix_ones() {
        assert_eq!(
            pin_revision("[wrap-git]\r\nrevision = main\r\n", "abc").unwrap(),
            "[wrap-git]\nrevision = abc\n"
        );
    }

    #[test]
    fn a_wrap_without_a_revision_line_is_refused() {
        assert_eq!(pin_revision("[wrap-git]\ndirectory=orc\n", "abc"), None);
    }
}
