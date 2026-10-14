//! What the build fetches and with which options: the tags, commits and
//! SHA-256 of every download, the plugins and the FFmpeg features. A change
//! here changes the binaries, so it comes with a new
//! [`crate::release::RECIPE_REVISION`] (and, since the prefix has to be built
//! again, [`crate::plan::BUILD_REVISION`] or another input of its stamp).

/// The GStreamer release: tag and the commit it must resolve to.
pub const GST_TAG: &str = "1.28.7";
pub const GST_COMMIT: &str = "070125524a8422e29d3b69a372ed4f62fd343ffa";
pub const GST_REPO: &str = "https://gitlab.freedesktop.org/gstreamer/gstreamer.git";
/// GStreamer's meson port of FFmpeg 7.1.1: the head of branch
/// `meson-7.1.1` that `subprojects/FFmpeg.wrap` names.
pub const FFMPEG_REPO: &str = "https://gitlab.freedesktop.org/gstreamer/meson-ports/ffmpeg.git";
pub const FFMPEG_COMMIT: &str = "cde7209117ec2be9da087b9e5f96d48450fdecd2";
/// orc at tag 0.4.42, the revision `subprojects/orc.wrap` names.
pub const ORC_COMMIT: &str = "141218e37a1da2ffa1f3f1c996092f61782f3235";
/// Build tools from PyPI, installed with `uv`.
pub const PYTHON: &str = "3.12";
pub const MESON: &str = "1.12.1";
pub const NINJA: &str = "1.13.2";
/// Lets meson's downloads use the operating system's certificate store; the
/// Python uv installs on Windows has no other.
pub const TRUSTSTORE: &str = "0.10.4";
/// CMake from PyPI, like meson and ninja; SVT-AV1 4.2.0 needs at least 3.16.
pub const CMAKE: &str = "4.4.4";
/// SVT-AV1 (svtav1enc), built with CMake into the prefix on every operating
/// system. A git clone checked against the commit, like the monorepo:
/// GitLab's generated archives carry no stable SHA-256.
pub const SVTAV1_REPO: &str = "https://gitlab.com/AOMediaCodec/SVT-AV1.git";
pub const SVTAV1_TAG: &str = "v4.2.0";
pub const SVTAV1_COMMIT: &str = "9292ec8e32bce26f781f277ec8739b53426c4300";
/// Runs its arguments as a command inside the MSVC environment, which meson
/// finds for itself (`mesonbuild/utils/vsenv.py` of meson 1.12.1); CMake
/// needs it on Windows the same way meson's `--vsenv` does. `setup_vsenv`
/// runs meson's own command to capture the environment, which must be set
/// first, as `python -m mesonbuild.mesonmain` (`set_meson_command`).
pub const VSENV_RUN: &str = "import subprocess, sys\n\
                         import mesonbuild.mesonmain\n\
                         from mesonbuild.utils.universal import set_meson_command\n\
                         from mesonbuild.utils.vsenv import setup_vsenv\n\
                         set_meson_command(mesonbuild.mesonmain.__file__)\n\
                         setup_vsenv(True)\n\
                         sys.exit(subprocess.call(sys.argv[1:]))\n";
/// Stands for the build tools' Python in a planned machine file.
pub const PYTHON_PLACEHOLDER: &str = "@GRIASDI_TOOLS_PYTHON@";

/// A pinned download.
pub struct Download {
    pub url: &'static str,
    pub sha256: &'static str,
    /// The directory the archive unpacks to.
    pub dir: &'static str,
}

pub const NASM_SOURCE: Download = Download {
    url: "https://www.nasm.us/pub/nasm/releasebuilds/3.02/nasm-3.02.tar.xz",
    sha256: "87336eba53b4acfe917424ab5d500d2b0054d9f5148d35c2273ccf2cfb712f0d",
    dir: "nasm-3.02",
};
pub const NASM_WIN64: Download = Download {
    url: "https://www.nasm.us/pub/nasm/releasebuilds/3.02/win64/nasm-3.02-win64.zip",
    sha256: "161d0bfaff53c2f9e9f3e69fd0672323ebabafd1268976a5cec11be92a19aee7",
    dir: "nasm-3.02",
};
pub const DAV1D: Download = Download {
    url: "https://download.videolan.org/pub/videolan/dav1d/1.5.4/dav1d-1.5.4.tar.xz",
    sha256: "686616b7c69eb88d44459391ab25cac13b6647a3b288835c5784e71c1514a5c5",
    dir: "dav1d-1.5.4",
};
pub const PKGCONF: Download = Download {
    url: "https://distfiles.ariadne.space/pkgconf/pkgconf-3.0.7.tar.xz",
    sha256: "c926ff491cbd9a331a589160811bd97ab1749b4d5198a519338f2cdfabe6940a",
    dir: "pkgconf-3.0.7",
};

/// The targets GStreamer is built for.
pub const TARGETS: [&str; 5] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

/// The plugins the player and screen sharing need on every platform, by
/// plugin name (`gst<name>.dll`, `libgst<name>.dylib`): the ones the player
/// pipelines use (`crates/griasdi-desktop/src/pipeline.rs` of griasdi) and
/// screen sharing needs. Linux links them into gst-full; the runtime of a
/// Windows or macOS release carries their files (`stage.rs`). dav1d is not
/// among them: its decoder is linked into the app (`gst-plugin-dav1d`).
pub const PLUGINS: [&str; 21] = [
    // queue, typefind, identity; the typefinders decodebin3 relies on.
    "coreelements",
    "typefindfunctions",
    // appsrc (the bytes Rust fetched), appsink (the frames for iced).
    "app",
    // decodebin3, parsebin.
    "playback",
    "pbtypes",
    "videoconvertscale",
    "audioconvert",
    "audioresample",
    "volume",
    // autoaudiosink.
    "autodetect",
    // MP4 (H.264, AAC, AV1) and WebM (VP9, Opus, AV1).
    "isomp4",
    "matroska",
    "audioparsers",
    // scaletempo, so speed changes keep the pitch.
    "audiofx",
    // tsdemux for live streams (HLS segments are MPEG-TS).
    "mpegtsdemux",
    "videoparsersbad",
    "opus",
    // avdec_h264, avdec_vp9, avdec_aac: the LGPL FFmpeg of the prefix.
    "libav",
    // The screen share's test sources and frame rate (`griasdi-screen`).
    "audiotestsrc",
    "videotestsrc",
    "videorate",
];

/// Screen sharing's encoders on Linux and Windows (macOS carries only svtav1,
/// see [`MACOS_PLUGINS`]; it has no NVIDIA encoder): svtav1enc and nvav1enc,
/// which loads the NVIDIA driver at run time.
pub const SCREEN_PLUGINS: [&str; 2] = ["svtav1", "nvcodec"];

/// The audio sinks `autoaudiosink` picks from on Windows, the screen capture
/// source (`d3d11screencapturesrc`), the capture devices (`mfvideosrc`), and
/// the AMD and Intel AV1 encoders (`amfav1enc`, `qsvav1enc`); besides
/// [`PLUGINS`] and [`SCREEN_PLUGINS`].
pub const WINDOWS_PLUGINS: [&str; 6] = [
    "wasapi2",
    "directsound",
    "d3d11",
    "amfcodec",
    "qsv",
    "mediafoundation",
];
/// The audio sink, the capture devices (`avfvideosrc`) and the screen share's
/// encoder (`svtav1enc`) on macOS, besides [`PLUGINS`].
pub const MACOS_PLUGINS: [&str; 3] = ["osxaudio", "svtav1", "applemedia"];

/// The FFmpeg components gst-libav uses; every other FFmpeg feature is off.
pub const FFMPEG_OPTIONS: [&str; 18] = [
    "avcodec=enabled",
    "avformat=enabled",
    "avfilter=enabled",
    "avutil=enabled",
    "avdevice=disabled",
    "swresample=disabled",
    "swscale=disabled",
    "postproc=disabled",
    "ffmpeg=disabled",
    "ffplay=disabled",
    "ffprobe=disabled",
    "h264_decoder=enabled",
    "vp9_decoder=enabled",
    "aac_decoder=enabled",
    // `avdec_mjpeg`, for capture devices that send their large modes as MJPEG.
    // The FFmpeg meson port selects `blockdsp hpeldsp exif idctdsp jpegtables`
    // by itself (`subprojects/FFmpeg/depgraph.py`).
    "mjpeg_decoder=enabled",
    // What the VP9 decoder selects; with every automatic feature off they
    // are not pulled in on their own and the decoder would be dropped.
    "vp9_parser=enabled",
    "vp9_superframe_split_bsf=enabled",
    "gpl=disabled",
];

/// The value meson's git wraps are rewritten to, and checked against after
/// the download.
pub const GIT_WRAPS: [(&str, &str, &str); 2] = [
    ("FFmpeg.wrap", "FFmpeg", FFMPEG_COMMIT),
    ("orc.wrap", "orc", ORC_COMMIT),
];
