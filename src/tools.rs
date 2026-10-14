//! The build tools: a private Python with meson, ninja and cmake from PyPI
//! (installed with `uv`), nasm, and on Windows pkgconf; and the pinned
//! downloads they and dav1d are made from.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use crate::pins::{
    CMAKE, Download, MESON, NASM_SOURCE, NASM_WIN64, NINJA, PKGCONF, PYTHON, TRUSTSTORE, VSENV_RUN,
};
use crate::plan::{Os, Plan};
use crate::util::{available, git_checkout_env, run, sha256_hex};

/// The build tools: `PATH` with ninja and nasm first, and the Python that
/// runs meson and unpacks the downloads (Windows' `tar` cannot be relied on
/// for `.tar.xz`).
pub struct Tools {
    path: OsString,
    pub python: PathBuf,
}

impl Tools {
    /// meson as `python -m mesonbuild.mesonmain`: meson records the command
    /// it was started with and runs its internal steps the same way, so the
    /// build never goes through a launcher executable.
    pub fn meson(&self) -> Command {
        let mut c = Command::new(&self.python);
        c.args(["-m", "mesonbuild.mesonmain"])
            .env("PATH", &self.path);
        git_checkout_env(&mut c);
        c
    }

    /// CMake from the tools' scripts directory; on Windows inside the MSVC
    /// environment ([`VSENV_RUN`]).
    pub fn cmake(&self) -> Command {
        let mut c = if cfg!(windows) {
            let mut c = Command::new(&self.python);
            c.args(["-c", VSENV_RUN, "cmake"]);
            c
        } else {
            Command::new("cmake")
        };
        c.env("PATH", &self.path);
        c
    }
}

/// The build tools for the target of `plan`, installed on first use into
/// `<base>/tools` and shared by every target.
pub fn tools(plan: &Plan) -> Result<Tools> {
    for tool in ["uv", "git"] {
        ensure!(
            available(tool),
            "{tool} is needed to build GStreamer and was not found"
        );
    }
    if plan.os != Os::Windows {
        let cc = match plan.layout.triple.as_str() {
            "aarch64-unknown-linux-gnu" if plan.cross => "aarch64-linux-gnu-gcc",
            _ => "cc",
        };
        for tool in [cc, "flex", "bison"] {
            ensure!(
                available(tool),
                "{tool} is needed to build GStreamer and was not found"
            );
        }
        if plan.x86_64 {
            ensure!(
                available("make"),
                "make is needed to build nasm and was not found"
            );
        }
    }

    // A private uv-managed Python with meson, ninja and truststore installed
    // into it. uv's own Python, never the system's: a machine with foreign-
    // architecture packages (the CI image has arm64 ones) may have a python3
    // of another architecture, and meson takes the build machine's CPU from
    // the Python it runs on. The packages go straight into that Python, so
    // meson and its internal steps run on one interpreter with no venv in
    // between. The Python lives in target/, so it is cached together with
    // everything built with it.
    let dir = plan.layout.tools();
    let install = dir.join("python");
    let uv = |args: &[&str]| {
        let mut c = Command::new("uv");
        c.env("UV_PYTHON_INSTALL_DIR", &install).args(args);
        c
    };
    let find = || -> Option<PathBuf> {
        let out = uv(&["python", "find", "--managed-python", PYTHON])
            .output()
            .ok()?;
        let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
        (out.status.success() && path.is_file()).then_some(path)
    };
    let pins = format!("{PYTHON} {MESON} {NINJA} {TRUSTSTORE} {CMAKE}");
    let stamp = install.join(".griasdi-pins");
    let python = match find() {
        Some(python) if fs::read_to_string(&stamp).ok().as_deref() == Some(pins.as_str()) => python,
        _ => {
            if install.exists() {
                fs::remove_dir_all(&install)?;
            }
            // --no-bin: nothing goes into the user's ~/.local/bin.
            run(&mut uv(&["python", "install", "--no-bin", PYTHON]))?;
            let python = find().context("uv installed no Python")?;
            run(
                uv(&["pip", "install", "--break-system-packages", "--python"])
                    .arg(&python)
                    .arg(format!("meson=={MESON}"))
                    .arg(format!("ninja=={NINJA}"))
                    .arg(format!("truststore=={TRUSTSTORE}"))
                    .arg(format!("cmake=={CMAKE}")),
            )?;
            fs::write(&stamp, &pins)?;
            python
        }
    };
    // ninja is a script of that Python.
    let paths = Command::new(&python)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_path('scripts')); \
             print(sysconfig.get_path('purelib'))",
        ])
        .output()?;
    ensure!(paths.status.success(), "{} failed", python.display());
    let paths = String::from_utf8(paths.stdout)?;
    let mut paths = paths.lines().map(|l| PathBuf::from(l.trim()));
    let (Some(scripts), Some(purelib)) = (paths.next(), paths.next()) else {
        bail!("{} named no scripts and site-packages", python.display());
    };
    if cfg!(windows) {
        // `platform.system()`, which meson calls at every start, asks WMI
        // first. With ninja starting many meson processes at once WMI fails
        // with E_OUTOFMEMORY (0x8007000e) and takes the process down
        // (0xC000070A); seen on the CI's Windows VM with Python 3.12.13,
        // which already has the fix for CPython issue 125315. Without
        // `_wmi`, `platform` reads the same facts from the environment and
        // `sys.getwindowsversion()`.
        fs::write(
            purelib.join("sitecustomize.py"),
            "import sys\n\
             # Set by griasdi-gstreamer: keep platform from querying WMI.\n\
             sys.modules['_wmi'] = None\n",
        )?;
    }
    let mut path = vec![scripts];
    if plan.x86_64 {
        path.push(match plan.os {
            Os::Windows => nasm_windows(&dir, &python)?,
            _ => nasm_from_source(&dir, &python)?,
        });
    }
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let tools = Tools {
        path: std::env::join_paths(path)?,
        python,
    };
    if plan.os == Os::Windows {
        pkgconf_windows(&dir, &tools)?;
    }
    Ok(tools)
}

/// Downloads `download` into `dir/downloads`, checks its SHA-256 and
/// unpacks it into `dir/src` with `python`; returns the unpacked directory.
pub fn fetch(dir: &Path, download: &Download, python: &Path) -> Result<PathBuf> {
    let downloads = dir.join("downloads");
    fs::create_dir_all(&downloads)?;
    let name = download
        .url
        .rsplit('/')
        .next()
        .context("url without a file name")?;
    let file = downloads.join(name);
    let ok = |file: &Path| sha256_hex(file).is_ok_and(|hash| hash == download.sha256);
    if !ok(&file) {
        run(Command::new("curl")
            .args(["-fsSL", "--retry", "3", "-o"])
            .arg(&file)
            .arg(download.url))?;
        ensure!(
            ok(&file),
            "{} does not have the pinned SHA-256 {}",
            download.url,
            download.sha256
        );
    }
    let src = dir.join("src");
    let unpacked = src.join(download.dir);
    if unpacked.exists() {
        fs::remove_dir_all(&unpacked)?;
    }
    fs::create_dir_all(&src)?;
    run(Command::new(python)
        .args([
            "-c",
            "import sys, tarfile, zipfile\n\
             archive, dest = sys.argv[1:3]\n\
             if archive.endswith('.zip'):\n    zipfile.ZipFile(archive).extractall(dest)\n\
             else:\n    tarfile.open(archive).extractall(dest, filter='data')\n",
        ])
        .arg(&file)
        .arg(&src))?;
    ensure!(
        unpacked.is_dir(),
        "{name} did not unpack to {}",
        download.dir
    );
    Ok(unpacked)
}

/// nasm built from source into `tools/nasm`; returns its `bin`.
fn nasm_from_source(dir: &Path, python: &Path) -> Result<PathBuf> {
    let prefix = dir.join("nasm");
    let bin = prefix.join("bin");
    let stamp = prefix.join(".griasdi-pin");
    if fs::read_to_string(&stamp).ok().as_deref() == Some(NASM_SOURCE.sha256) {
        return Ok(bin);
    }
    if prefix.exists() {
        fs::remove_dir_all(&prefix)?;
    }
    let src = fetch(dir, &NASM_SOURCE, python)?;
    run(Command::new("./configure")
        .arg(format!("--prefix={}", prefix.display()))
        .current_dir(&src))?;
    run(Command::new("make")
        .arg(format!("-j{}", jobs()))
        .current_dir(&src))?;
    run(Command::new("make").arg("install").current_dir(&src))?;
    fs::write(&stamp, NASM_SOURCE.sha256)?;
    Ok(bin)
}

/// The official nasm binary for Windows; returns its directory.
fn nasm_windows(dir: &Path, python: &Path) -> Result<PathBuf> {
    let unpacked = dir.join("src").join(NASM_WIN64.dir);
    let stamp = unpacked.join(".griasdi-pin");
    if fs::read_to_string(&stamp).ok().as_deref() == Some(NASM_WIN64.sha256) {
        return Ok(unpacked);
    }
    let unpacked = fetch(dir, &NASM_WIN64, python)?;
    fs::write(&stamp, NASM_WIN64.sha256)?;
    Ok(unpacked)
}

/// pkgconf built with meson into `tools/pkgconf`; a Windows release ships
/// its `pkgconf.exe` in `bin/`, for cargo's pkg-config probes.
fn pkgconf_windows(dir: &Path, tools: &Tools) -> Result<()> {
    let prefix = dir.join("pkgconf");
    let stamp = prefix.join(".griasdi-pin");
    if fs::read_to_string(&stamp).ok().as_deref() == Some(PKGCONF.sha256) {
        return Ok(());
    }
    if prefix.exists() {
        fs::remove_dir_all(&prefix)?;
    }
    let src = fetch(dir, &PKGCONF, &tools.python)?;
    let build = dir.join("build-pkgconf");
    if build.exists() {
        fs::remove_dir_all(&build)?;
    }
    run(tools
        .meson()
        .arg("setup")
        .arg(&build)
        .arg(&src)
        .args(["--vsenv", "--buildtype=release", "--default-library=static"])
        .arg(format!("--prefix={}", prefix.display())))?;
    run(tools.meson().arg("install").arg("-C").arg(&build))?;
    fs::write(&stamp, PKGCONF.sha256)?;
    Ok(())
}

pub fn jobs() -> usize {
    std::thread::available_parallelism().map_or(4, usize::from)
}
