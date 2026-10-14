//! What one target's build is: its operating system, directories, meson
//! machine file and the arguments of every build step, and the stamp that
//! says whether a finished prefix still matches them.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::patches;
use crate::pins::{
    CMAKE, DAV1D, FFMPEG_COMMIT, FFMPEG_OPTIONS, GST_COMMIT, GST_TAG, MESON, NASM_SOURCE,
    NASM_WIN64, NINJA, ORC_COMMIT, PKGCONF, PLUGINS, PYTHON, PYTHON_PLACEHOLDER, SCREEN_PLUGINS,
    SVTAV1_COMMIT, TRUSTSTORE,
};
use crate::release::RECIPE_REVISION;
use crate::util::{hex, root};

/// The host's target triple, from `rustc -vV`.
pub fn host_triple() -> Result<String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc).arg("-vV").output()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_owned)
        .context("rustc -vV names no host")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
    Macos,
}

impl Os {
    pub fn of(triple: &str) -> Self {
        if triple.contains("-linux-") {
            Os::Linux
        } else if triple.contains("-windows-") {
            Os::Windows
        } else {
            Os::Macos
        }
    }
}

/// Where one target's build lives below `<repo>/target/gstreamer/`.
pub struct Layout {
    pub triple: String,
    /// `target/gstreamer`: `tools/` (shared by every target), the sources,
    /// the build directories and the prefixes.
    pub base: PathBuf,
    /// The install prefix, `<base>/<triple>`: what a release ships.
    pub prefix: PathBuf,
    /// The GStreamer monorepo and the meson subprojects fetched into it,
    /// `<base>/src-<triple>`.
    pub src: PathBuf,
    /// `<base>/build-<triple>`: one build directory per project and the
    /// meson machine file.
    pub build: PathBuf,
}

impl Layout {
    pub fn new(triple: &str) -> Self {
        Self::below(root().join("target/gstreamer"), triple)
    }

    fn below(base: PathBuf, triple: &str) -> Self {
        Self {
            triple: triple.to_owned(),
            prefix: base.join(triple),
            src: base.join(format!("src-{triple}")),
            build: base.join(format!("build-{triple}")),
            base,
        }
    }

    /// The build tools: Python, nasm, pkgconf, and the downloads they and
    /// dav1d are made from.
    pub fn tools(&self) -> PathBuf {
        self.base.join("tools")
    }

    /// The meson machine file (cross or native), outside the prefix.
    pub fn machine_file(&self) -> PathBuf {
        self.build.join("machine.ini")
    }

    /// The unpacked dav1d release, shared by every target.
    pub fn dav1d_src(&self) -> PathBuf {
        self.tools().join("src").join(DAV1D.dir)
    }

    /// The unpacked pkgconf release (Windows).
    pub fn pkgconf_src(&self) -> PathBuf {
        self.tools().join("src").join(PKGCONF.dir)
    }

    /// pkgconf as built into the tools (Windows).
    pub fn pkgconf_exe(&self) -> PathBuf {
        self.tools().join("pkgconf/bin/pkgconf.exe")
    }

    /// The SVT-AV1 clone, shared by every target.
    pub fn svtav1_src(&self) -> PathBuf {
        self.base.join("src-svtav1")
    }
}

/// Everything about one target's build that the stamp covers.
pub struct Plan {
    pub layout: Layout,
    pub os: Os,
    pub x86_64: bool,
    /// Whether the host cannot run the target's code.
    pub cross: bool,
    /// The meson machine file: the cross file of a cross build, else a
    /// native file. It pins nasm, which meson would otherwise take from a
    /// subproject that only downloads binaries for Windows and macOS.
    pub machine_file: String,
    pub dav1d_args: Vec<String>,
    /// CMake's arguments for SVT-AV1.
    pub svtav1_args: Vec<String>,
    pub gst_args: Vec<String>,
}

impl Plan {
    pub fn new(layout: Layout) -> Result<Self> {
        let triple = layout.triple.as_str();
        let base = &layout.base;
        let prefix = &layout.prefix;
        let os = Os::of(triple);
        let x86_64 = triple.starts_with("x86_64");
        let host = host_triple()?;
        let cross = triple != host;
        let mut binaries = Vec::new();
        if x86_64 {
            let nasm = match os {
                Os::Windows => base.join("tools/src").join(NASM_WIN64.dir).join("nasm.exe"),
                _ => base.join("tools/nasm/bin/nasm"),
            };
            binaries.push(format!(
                "nasm = '{}'",
                nasm.display().to_string().replace('\\', "/")
            ));
        }
        if os == Os::Windows {
            // meson's pkg-config, for SVT-AV1's `SvtAv1Enc.pc`: the pkgconf
            // `tools()` builds, which a Windows release ships in `bin/`.
            binaries.push(format!(
                "pkg-config = '{}'",
                layout
                    .pkgconf_exe()
                    .display()
                    .to_string()
                    .replace('\\', "/")
            ));
        }
        if cross {
            // GLib's code generators run on the build machine. Without them
            // in the cross file, meson's gnome module asks for a build-machine
            // GLib, `--wrap-mode=forcefallback` configures the GLib
            // subproject a second time for it, and meson 1.12 fails on that
            // ("Summary section 'Build environment' already have key 'host
            // cpu'"). The host's own GStreamer build has them: the scripts
            // run with the build tools' Python (`PYTHON_PLACEHOLDER`, filled
            // in when the file is written), the programs are linked
            // statically.
            let host_bin = base.join(&host).join("bin");
            let path = |p: &Path| p.display().to_string();
            for script in ["glib-mkenums", "glib-genmarshal", "gdbus-codegen"] {
                binaries.push(format!(
                    "{script} = ['{PYTHON_PLACEHOLDER}', '{}']",
                    path(&host_bin.join(script))
                ));
            }
            for program in ["glib-compile-resources", "glib-compile-schemas"] {
                binaries.push(format!("{program} = '{}'", path(&host_bin.join(program))));
            }
        }
        let machine_file = if cross {
            cross_file(triple, &binaries)?
        } else {
            format!("[binaries]\n{}\n", binaries.join("\n"))
        };
        let machine_ini = layout.machine_file().display().to_string();
        let machine_args = |args: &mut Vec<String>| {
            args.push(
                if cross {
                    "--cross-file"
                } else {
                    "--native-file"
                }
                .into(),
            );
            args.push(machine_ini.clone());
        };

        let mut dav1d_args: Vec<String> = [
            "--default-library=static",
            "--buildtype=release",
            "--libdir=lib",
            // The `.pc` file derives its prefix from its own location, so
            // the prefix can move after the build.
            "-Dpkgconfig.relocatable=true",
            "-Denable_tools=false",
            "-Denable_tests=false",
        ]
        .map(String::from)
        .to_vec();
        if os == Os::Windows {
            // The app's executable links the C runtime statically and has
            // dav1d linked into it.
            dav1d_args.extend(["--vsenv".into(), "-Db_vscrt=mt".into()]);
        }
        machine_args(&mut dav1d_args);

        let mut svtav1_args: Vec<String> = [
            "-G",
            "Ninja",
            "-DCMAKE_BUILD_TYPE=Release",
            "-DBUILD_SHARED_LIBS=OFF",
            "-DBUILD_APPS=OFF",
            "-DBUILD_TESTING=OFF",
            // GCC's LTO objects in a static archive would break the
            // gst-full link.
            "-DSVT_AV1_LTO=OFF",
            // The code ends up in `libgstreamer-full-1.0.so`.
            "-DCMAKE_POSITION_INDEPENDENT_CODE=ON",
            "-DCMAKE_INSTALL_LIBDIR=lib",
        ]
        .map(String::from)
        .to_vec();
        svtav1_args.push(format!(
            "-DCMAKE_INSTALL_PREFIX={}",
            prefix.display().to_string().replace('\\', "/")
        ));
        if os == Os::Windows {
            // Linked into gstsvtav1.dll, whose GStreamer DLLs use the
            // DLL C runtime (/MD); dav1d differs (`b_vscrt=mt`) because
            // it is linked into the exe.
            svtav1_args.push("-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL".into());
        }
        if cross && triple == "aarch64-unknown-linux-gnu" {
            svtav1_args.extend(
                [
                    "-DCMAKE_SYSTEM_NAME=Linux",
                    "-DCMAKE_SYSTEM_PROCESSOR=aarch64",
                    "-DCMAKE_C_COMPILER=aarch64-linux-gnu-gcc",
                    "-DCMAKE_CXX_COMPILER=aarch64-linux-gnu-g++",
                ]
                .map(String::from),
            );
        }
        if os == Os::Macos {
            let arch = if x86_64 { "x86_64" } else { "arm64" };
            svtav1_args.push(format!("-DCMAKE_OSX_ARCHITECTURES={arch}"));
            if cross {
                // x86_64-apple-darwin is built on an arm64 Mac: SVT-AV1 picks
                // its assembly by CMAKE_SYSTEM_PROCESSOR, which would be the
                // host's.
                svtav1_args.extend([
                    "-DCMAKE_SYSTEM_NAME=Darwin".to_owned(),
                    format!("-DCMAKE_SYSTEM_PROCESSOR={arch}"),
                ]);
            }
        }

        let mut gst_args: Vec<String> = [
            "--buildtype=release",
            "--libdir=lib",
            // The `.pc` files derive their prefix from their own location
            // (`${pcfiledir}`), so the prefix can move after the build.
            "-Dpkgconfig.relocatable=true",
            // Every dependency with a wrap from its subproject, never from
            // the build machine: the result runs where nothing is installed.
            "--wrap-mode=forcefallback",
            "-Dauto_features=disabled",
            "-Dorc=enabled",
            "-Dtools=enabled",
            "-Dtls=disabled",
            "-Dgpl=disabled",
            "-Dbase=enabled",
            "-Dgood=enabled",
            "-Dbad=enabled",
            "-Dugly=disabled",
            "-Dlibav=enabled",
            "-Drs=disabled",
            "-Ddevtools=disabled",
            "-Dges=disabled",
            "-Drtsp_server=disabled",
            "-Dpython=disabled",
            "-Dlibnice=disabled",
            "-Dgst-examples=disabled",
            "-Dtests=disabled",
            "-Dexamples=disabled",
            "-Dintrospection=disabled",
            "-Dnls=disabled",
            "-Ddoc=disabled",
            "-Dgst-plugins-base:app=enabled",
            "-Dgst-plugins-base:typefind=enabled",
            "-Dgst-plugins-base:playback=enabled",
            "-Dgst-plugins-base:pbtypes=enabled",
            "-Dgst-plugins-base:videoconvertscale=enabled",
            "-Dgst-plugins-base:audioconvert=enabled",
            "-Dgst-plugins-base:audioresample=enabled",
            "-Dgst-plugins-base:volume=enabled",
            "-Dgst-plugins-base:opus=enabled",
            "-Dgst-plugins-good:autodetect=enabled",
            "-Dgst-plugins-good:isomp4=enabled",
            "-Dgst-plugins-good:matroska=enabled",
            "-Dgst-plugins-good:audioparsers=enabled",
            "-Dgst-plugins-good:audiofx=enabled",
            "-Dgst-plugins-bad:videoparsers=enabled",
            "-Dgst-plugins-bad:mpegtsdemux=enabled",
            "-Dgst-plugins-base:audiotestsrc=enabled",
            "-Dgst-plugins-base:videotestsrc=enabled",
            "-Dgst-plugins-base:videorate=enabled",
            "-Dglib:tests=false",
        ]
        .map(String::from)
        .to_vec();
        gst_args.extend(FFMPEG_OPTIONS.iter().map(|o| format!("-DFFmpeg:{o}")));
        if x86_64 {
            // nasm: without it FFmpeg silently drops its SIMD decoders.
            gst_args.push("-DFFmpeg:x86asm=enabled".into());
        }
        gst_args.push(match os {
            Os::Windows => "-DFFmpeg:w32threads=enabled".into(),
            _ => "-DFFmpeg:pthreads=enabled".into(),
        });
        gst_args.push("-Dgst-plugins-bad:svtav1=enabled".into());
        if os != Os::Macos {
            gst_args.push("-Dgst-plugins-bad:nvcodec=enabled".into());
        }
        // SVT-AV1's `SvtAv1Enc.pc`, installed by `build_svtav1`. A
        // per-machine option, so it reaches the host machine of a cross
        // build too, ahead of the cross file's `pkg_config_libdir`.
        gst_args.push(format!(
            "--pkg-config-path={}",
            prefix.join("lib/pkgconfig").display()
        ));
        let mut plugins: Vec<&str> = PLUGINS.to_vec();
        match os {
            Os::Linux => {
                gst_args.extend(
                    [
                        "--default-library=static",
                        "-Dgst-full=enabled",
                        "-Dgst-full-target-type=shared_library",
                        "-Dgstreamer:registry=false",
                        "-Dgst-full-libraries=gstreamer-base-1.0,gstreamer-app-1.0,\
                         gstreamer-video-1.0,gstreamer-audio-1.0,gstreamer-pbutils-1.0",
                        // glib-rs calls two GIO functions (`Object::new`
                        // refuses `GInitable` types), and `gst-full-libraries`
                        // takes GStreamer's libraries only. The `gio` plugin
                        // (built, never registered: it is not in
                        // `gst-full-plugins`) puts the static GIO on the link
                        // line, `--undefined` pulls the two in, and the
                        // version script exports every `g_*` symbol.
                        "-Dgst-plugins-base:gio=enabled",
                        "-Dc_link_args=['-Wl,--undefined=g_initable_get_type',\
                         '-Wl,--undefined=g_async_initable_get_type']",
                        // ALSA only: libasound is loaded by the app already
                        // (cpal); PulseAudio and PipeWire serve it through
                        // their ALSA plugins.
                        "-Dgst-plugins-base:alsa=enabled",
                        // ximagesrc for X11 capture; libX11, Xext, Xfixes
                        // and Xdamage are linked dynamically from the
                        // system.
                        "-Dgst-plugins-good:ximagesrc=enabled",
                        "-Dgst-plugins-good:ximagesrc-xshm=enabled",
                        "-Dgst-plugins-good:ximagesrc-xfixes=enabled",
                        "-Dgst-plugins-good:ximagesrc-xdamage=enabled",
                        // AMD and Intel AV1 encoding through VA-API. libva
                        // and libdrm come from the monorepo's pinned wraps
                        // (`--wrap-mode=forcefallback`) and are linked
                        // statically (libva through
                        // `patches/libva-static-library-and-header-copy.patch`);
                        // `auto_features=disabled` keeps
                        // libdrm's vendor parts and gudev out, so the plugin
                        // probes `/dev/dri/renderD*` itself.
                        "-Dgst-plugins-bad:va=enabled",
                        "-Dlibva:with_x11=no",
                        "-Dlibva:with_glx=no",
                        "-Dlibva:with_wayland=no",
                        "-Dlibva:with_win32=no",
                        "-Dlibdrm:tests=false",
                    ]
                    .map(String::from),
                );
                gst_args.push(format!("-Dlibva:driverdir={}", va_driver_dirs(triple)));
                plugins.push("alsa");
                plugins.extend(SCREEN_PLUGINS);
                // AMD and Intel AV1 encoding (`vaav1enc`, `vaav1lpenc`);
                // libva is static, the driver is the system's.
                plugins.push("va");
                plugins.push("ximagesrc");
                // The meson check compares the plugins' static library
                // names (`libgst<name>.a`), not the plugin names.
                let names: Vec<String> = plugins.iter().map(|p| format!("libgst{p}.a")).collect();
                gst_args.push(format!("-Dgst-full-plugins={}", names.join(";")));
            }
            Os::Windows => {
                gst_args.extend(
                    [
                        "--vsenv",
                        "--default-library=shared",
                        "-Dgst-full=disabled",
                        // A shared build reaches the plugins' doc setup,
                        // which reads a variable gstreamer only defines with
                        // libgstcheck (`subprojects/gstreamer/docs/meson.build`).
                        "-Dgstreamer:check=enabled",
                        "-Dgst-plugins-bad:wasapi2=enabled",
                        "-Dgst-plugins-good:directsound=enabled",
                        // d3d11screencapturesrc; window capture
                        // (`window-capture-mode=client`) needs WGC.
                        // DirectXMath comes from its wrap.
                        "-Dgst-plugins-bad:d3d11=enabled",
                        "-Dgst-plugins-bad:d3d11-wgc=enabled",
                        // nvcodec's `gstnvdecoder.cpp` (1.28.7) includes the
                        // d3d11 headers under `#ifdef G_OS_WIN32` before any
                        // GLib header defines it, unless the GL or D3D12
                        // headers came first; without one of the two it does
                        // not compile. D3D12 is the smaller: Windows headers
                        // and the DirectX-Headers wrap.
                        "-Dgst-plugins-bad:d3d12=enabled",
                        // AMD and Intel AV1 encoding (`amfav1enc`,
                        // `qsvav1enc`). Both need only d3d11: amfcodec ships
                        // the AMF headers and loads the runtime at run time,
                        // qsv builds its bundled oneVPL dispatcher.
                        "-Dgst-plugins-bad:amfcodec=enabled",
                        "-Dgst-plugins-bad:qsv=enabled",
                    ]
                    .map(String::from),
                );
            }
            Os::Macos => {
                gst_args.extend(
                    [
                        "--default-library=shared",
                        "-Dgst-full=disabled",
                        "-Dgstreamer:check=enabled",
                        "-Dgst-plugins-good:osxaudio=enabled",
                    ]
                    .map(String::from),
                );
            }
        }
        machine_args(&mut gst_args);

        Ok(Self {
            os,
            x86_64,
            cross,
            machine_file,
            dav1d_args,
            svtav1_args,
            gst_args,
            layout,
        })
    }

    /// SHA-256 over every input of the build: the pins, the arguments, the
    /// machine file, the recipe revision and the patches of this target.
    pub fn stamp(&self) -> Result<String> {
        let mut h = Sha256::new();
        let patches = patches::digest(self.os)?;
        for part in [
            GST_TAG,
            GST_COMMIT,
            FFMPEG_COMMIT,
            ORC_COMMIT,
            PYTHON,
            MESON,
            NINJA,
            TRUSTSTORE,
            CMAKE,
            SVTAV1_COMMIT,
            NASM_SOURCE.sha256,
            NASM_WIN64.sha256,
            DAV1D.sha256,
            PKGCONF.sha256,
            RECIPE_REVISION,
            &patches,
            &self.layout.triple,
            &self.machine_file,
        ] {
            h.update(part.as_bytes());
            h.update([0]);
        }
        for arg in self
            .dav1d_args
            .iter()
            .chain(&self.svtav1_args)
            .chain(&self.gst_args)
        {
            h.update(arg.as_bytes());
            h.update([0]);
        }
        Ok(hex(&h.finalize()))
    }
}

/// The meson cross file for `triple` on this host, with the extra
/// `binaries` lines.
fn cross_file(triple: &str, binaries: &[String]) -> Result<String> {
    let file = match triple {
        "aarch64-unknown-linux-gnu" => "\
[binaries]
c = 'aarch64-linux-gnu-gcc'
cpp = 'aarch64-linux-gnu-g++'
ar = 'aarch64-linux-gnu-ar'
strip = 'aarch64-linux-gnu-strip'
pkg-config = 'pkg-config'

[properties]
pkg_config_libdir = ['/usr/lib/aarch64-linux-gnu/pkgconfig', '/usr/share/pkgconfig']

[host_machine]
system = 'linux'
cpu_family = 'aarch64'
cpu = 'aarch64'
endian = 'little'
"
        .to_owned(),
        "x86_64-apple-darwin" => "\
[binaries]
c = ['clang', '-arch', 'x86_64']
cpp = ['clang++', '-arch', 'x86_64']
objc = ['clang', '-arch', 'x86_64']
objcpp = ['clang++', '-arch', 'x86_64']
# For SVT-AV1's `SvtAv1Enc.pc` (`--pkg-config-path`): a cross build gets no
# pkg-config unless the cross file names one (README.md: `brew install pkgconf`).
pkg-config = 'pkg-config'

[host_machine]
system = 'darwin'
# A cross file has to say which Apple system: meson only autodetects it for
# the build machine, and GStreamer's meson.build reads it (`subsystem`).
subsystem = 'macos'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
"
        .to_owned(),
        other => bail!("no cross build to {other} from this host"),
    };
    let extra: String = binaries.iter().map(|b| format!("{b}\n")).collect();
    Ok(file.replacen("[binaries]\n", &format!("[binaries]\n{extra}"), 1))
}

/// Where libva looks for the system's VA drivers (`iHD`, `radeonsi`), in
/// order; libva splits the list at `:` (libva 2.22.0 `va/va.c`). A static
/// libva built into gst-full would otherwise only search its own prefix.
fn va_driver_dirs(triple: &str) -> &'static str {
    match triple {
        "aarch64-unknown-linux-gnu" => {
            "/usr/lib/aarch64-linux-gnu/dri:/usr/lib64/dri:/usr/lib/dri:/run/opengl-driver/lib/dri"
        }
        _ => "/usr/lib/x86_64-linux-gnu/dri:/usr/lib64/dri:/usr/lib/dri:/run/opengl-driver/lib/dri",
    }
}
