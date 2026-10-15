# griasdi-gstreamer

The GStreamer that [griasdi](https://github.com/griasdi/griasdi) links and
ships, built from source with a fixed recipe, and published as the prebuilt
libraries that go into griasdi's installation, the files that link them, and
the source they were built from.

griasdi uses GStreamer 1.28.7 with exactly the plugins its player and screen
sharing need, and FFmpeg's LGPL decoders for H.264, VP9 and AAC, dav1d and
SVT-AV1 for AV1. The libraries are built from source because no distribution
carries that set on every platform, and because griasdi changes a few lines of
LGPL code (`patches/`). Publishing those changes means publishing the
corresponding source, and that is what a release does: each archive of
libraries comes with the source of its binaries. griasdi itself only downloads,
verifies and unpacks a release.

## What a release contains

A release is named `<GStreamer tag>-<recipe revision>`, for example `1.28.7-2`,
and published from the git tag `v1.28.7-2`. Its assets, at
`https://github.com/griasdi/gstreamer/releases/download/v<id>/<asset>`, are for
each target `<T>` (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`, `aarch64-apple-darwin`):

| Asset | Content |
|---|---|
| `gstreamer-<id>-<T>.tar.gz` | One top-level directory `<T>/` with exactly `RELEASE` (the id and a newline), `runtime/` and `link/`, described below. No headers, no tools, no static libraries of GLib and GStreamer. |
| `gstreamer-<id>-<T>-source.tar.gz` | The corresponding source of those binaries, as exactly one top-level directory `gstreamer-<id>-<T>-source/`: the GStreamer monorepo with the meson subprojects the build fetched, dav1d, SVT-AV1 (and pkgconf on Windows), all with the patches applied, and this repository at the tag. `SOURCES.txt` lists what is where and where it came from. No `.git` directories, no build directories. |
| `SHA256SUMS` | `sha256sum` format, over all assets. |

### `runtime/`

Exactly what goes into griasdi's installation, laid out as the package lays it
out; griasdi copies it into the package verbatim. The paths are relative to
the package root:

| Target | Content |
|---|---|
| Linux, both architectures | The root of the tarball, of the AppDir's `usr/` and of the deb and rpm's `/usr`: `lib/griasdi/libgstreamer-full-1.0.so`, `share/doc/griasdi/GSTREAMER-NOTICE.txt` and `share/doc/griasdi/GSTREAMER-LICENSES/<component>/`. The library is GStreamer's `gst-full`: every GLib and GStreamer library and every plugin, static inside it. |
| macOS, both architectures | Relative to `griasdi.app/`: `Contents/lib/*.dylib`, `Contents/lib/gstreamer-1.0/libgst<plugin>.dylib`, `Contents/libexec/gstreamer-1.0/gst-plugin-scanner`, and `Contents/Resources/GSTREAMER-NOTICE.txt` with `Contents/Resources/GSTREAMER-LICENSES/`. Every Mach-O is relocated: install names are `@rpath/<file>` and the one `LC_RPATH` of each file leads to `Contents/lib` (`@loader_path` in `lib`, `@loader_path/..` in `lib/gstreamer-1.0`, `@loader_path/../../lib` for the scanner); nothing of the build machine is left. Not signed: griasdi signs. |
| Windows | Relative to the directory of `griasdi.exe`: the DLLs (`*.dll`) directly at the top, with the Visual C++ runtime DLLs they need (the app itself links the C runtime statically), the plugins in `lib/gstreamer-1.0/gst<plugin>.dll`, and `GSTREAMER-NOTICE.txt` with `GSTREAMER-LICENSES/`. No plugin scanner: it sits in `libexec`, where it cannot find the DLLs, so GStreamer scans in-process. |

The files are computed, not listed: the runtime is the closure of the plugins
and of the libraries griasdi's `-sys` crates link (the pkg-config names
`glib-2.0`, `gobject-2.0`, `gio-2.0`, `gstreamer-1.0`, `gstreamer-base-1.0`,
`gstreamer-app-1.0`, `gstreamer-video-1.0` and `gstreamer-audio-1.0`), read
from the PE import tables (Windows) or `otool -L` (macOS). The plugins are the
lists in `src/pins.rs`: for every target `coreelements`, `typefindfunctions`,
`app`, `playback`, `pbtypes`, `videoconvertscale`, `audioconvert`,
`audioresample`, `volume`, `autodetect`, `isomp4`, `matroska`, `audioparsers`,
`audiofx`, `mpegtsdemux`, `videoparsersbad`, `opus`, `libav`, `audiotestsrc`,
`videotestsrc` and `videorate`; on Windows also `svtav1`, `nvcodec`,
`wasapi2`, `directsound`, `d3d11`, `amfcodec` and `qsv`; on macOS also
`osxaudio` and `svtav1`. On Linux they are all inside the library.

`GSTREAMER-NOTICE.txt` says what the libraries are, which release of this
repository they come from, where the corresponding source is and that the
libraries can be replaced; `GSTREAMER-LICENSES/<component>/` holds the license
texts of every component built into them.

### `link/`

What building griasdi needs and nothing else; it is never packaged.

- `link/pkgconfig/*.pc` for the modules named above, `dav1d`, and what they
  require. Every file derives its prefix from its own location
  (`${pcfiledir}`), so a release works wherever it is unpacked. `--libs`
  resolves to the runtime: on Linux `-L<T>/runtime/lib/griasdi
  -lgstreamer-full-1.0` (every module is a shim for that one library), on macOS
  `-L<T>/runtime/Contents/lib` with each library named by its versioned file
  (`-lgstreamer-1.0.0`, so no unversioned links are needed), on Windows
  `-L<T>/link/lib`. `Cflags` is empty: the headers are not shipped, because no
  `-sys` crate of griasdi reads them.
- `link/lib/`: `libdav1d.a` (on Windows `dav1d.lib`), which griasdi links
  statically (`gst-plugin-dav1d`); on Windows also the import libraries of the
  runtime's DLLs.
- Windows: `link/bin/pkgconf.exe`, the pkgconf that cargo's pkg-config probes
  use.

There are no links in a release: no symbolic or hard links in either part.

## How griasdi uses it

griasdi pins the release id and the SHA-256 of the archive of each target, and
nothing else. To get the libraries it downloads `gstreamer-<id>-<T>.tar.gz`,
checks the SHA-256 against its pin and unpacks it into its own
`target/gstreamer/`, which gives `target/gstreamer/<T>/`. The file
`target/gstreamer/<T>/RELEASE` holding the pinned id means that is up to date.
The sums to pin are in the release's `SHA256SUMS`.

To build, `PKG_CONFIG_PATH` of each target is `target/gstreamer/<T>/link/pkgconfig`
(on Windows with `target/gstreamer/<T>/link/bin/pkgconf.exe` as pkg-config), and
the executable finds the libraries of the development build in
`target/gstreamer/<T>/runtime` (Linux: `runtime/lib/griasdi`, macOS:
`runtime/Contents/lib`, Windows: `runtime` itself). The packages copy `runtime/`
into the package root; griasdi relocates and signs its own executables
(macOS) and appends the paragraph about its own Rust dependency under another
license to the notice on Windows.

## Building locally

The tool is a Rust program; everything it builds lands in `target/gstreamer/` of
the checkout: `tools/` (the build tools and downloads, shared), `src-<T>/` (the
GStreamer monorepo with the fetched subprojects, patched), `build-<T>/` (the
build directories and the meson machine file), the prefix `<T>/` and, while
`dist` runs, `stage-<T>/`.

```sh
cargo run --release -- build [--target <T>]
cargo run --release -- dist [--target <T>] [--no-build] --out <dir>
cargo run --release -- release-id
cargo run --release -- check-tag <tag>
```

- `build` builds the prefix of `<T>` (the host without `--target`) if it is not
  up to date, and tidies it: relocatable `.pc` files, the license texts of its
  components in `share/licenses/`. The prefix is the build output, with its
  `bin/`, `include/` and static libraries; it is not what a release ships.
  `<T>/.recipe` holds a hash of every input (the pins, the build options, the
  patches, `BUILD_REVISION` in `src/plan.rs`); a prefix with a matching hash is
  returned at once, any other change rebuilds from scratch. A build that failed
  after the install only finishes again. A full build takes about five minutes
  on a 16 core desktop and longer on fewer cores.
- `dist` runs `build`, stages `runtime/` and `link/` out of the prefix into
  `stage-<T>/<T>/` (`src/stage.rs`), then writes the two archives of the release
  into `<dir>` and prints their `sha256sum` lines. With `--no-build` the prefix
  is taken as it is and never built, whether it is up to date or not: for a
  `target/gstreamer` that was copied to another place, which the stamp, being
  made of absolute paths, does not recognise.
- `release-id` prints the release id; `check-tag` succeeds if its argument is `v`
  plus that id.

What the build needs, besides a stable Rust toolchain, network access to
GitLab (freedesktop.org, gitlab.com), github.com, videolan.org, nasm.us,
ariadne.space and PyPI, and `git` and `curl`:

- All: [`uv`](https://docs.astral.sh/uv/). It installs a private Python with
  meson, ninja and cmake into `target/gstreamer/tools`; nothing of the system's
  Python is used. nasm is built from its source tarball (Linux, macOS) or is the
  official binary (Windows).
- Linux: a C and C++ compiler and `make` (`build-essential`), `flex`, `bison`,
  `pkg-config`, and the development packages of what the plugins link
  dynamically: `libasound2-dev`, `libx11-dev`, `libxext-dev`, `libxfixes-dev`,
  `libxdamage-dev` (Debian and Ubuntu names). `.zeughaus-ci/ubuntu24.Containerfile`
  is such a machine. Building `aarch64-unknown-linux-gnu` on x86_64 is a cross
  build with `aarch64-linux-gnu-gcc` and the arm64 development packages, after
  the host's prefix (the GLib code generators come from it).
- macOS: the Xcode command line tools (`otool` and `install_name_tool` relocate
  the runtime), `flex`, `bison` and `pkgconf` (`brew install flex bison
  pkgconf`).
- Windows: Visual Studio with the MSVC build tools (meson and cmake find them)
  and the Visual C++ redistributable, whose DLLs the runtime carries (from
  `VCToolsRedistDir` of the Visual Studio environment, else `System32`).
  pkgconf is built into the tools and shipped as `link/bin/pkgconf.exe`.

`dist` also needs `git` to see which files of this repository go into the source
archive: tracked files and untracked ones that are not ignored.

## Cutting a release

1. Make the change. `RECIPE_REVISION` in `src/release.rs` is bumped by every
   change that alters what a release ships: a pin, an option, a patch, what is
   staged out of the prefix and how. A new GStreamer tag changes the pins in
   `src/pins.rs` and starts again at revision `1`. `cargo run --release --
   release-id` prints the id. Whether the prefix is built again for it is the
   stamp's business: a change to the staging alone leaves the prefix, and the CI
   caches of it, as they are; a change of what the build or the tidying
   produces that the pins, options and patches do not show bumps
   `BUILD_REVISION` in `src/plan.rs`.
2. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
   `cargo test` pass. The CI's `check` job runs them for every push to `main`.
3. Commit and push, then tag and push the tag:
   `git tag v<id> && git push origin v<id>`. The CI refuses a tag that is not `v`
   plus the id of the commit it is on.
4. The tag builds the four targets natively (`.zeughaus-ci/`: `linux-amd64` and
   `linux-arm64` in the `ubuntu24` image, `windows-amd64` on the Windows VM,
   `darwin-arm64` on atik's host), each with `dist`. The `release` job then
   creates the GitHub release `v<id>` with all eight archives and
   `SHA256SUMS`; for a tag that already has a release it replaces the release
   with the new build.
5. griasdi moves to the new id and the SHA-256 sums of its release.

A published tag is never moved. Whatever was wrong with a release is fixed by
the next recipe revision.

## License

The recipe, the code of this repository, is under either of the
[MIT license](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE),
at your option.

The patches in `patches/` are under the licenses of the files they modify (GLib
and gst-plugins-bad: LGPL-2.1-or-later; libva: MIT), see
[patches/README.md](patches/README.md).

The libraries of a release are made of other people's code and stay under their
licenses. GStreamer, GLib and FFmpeg (built without its GPL parts,
`gpl=disabled`) are under the LGPL-2.1-or-later, the other libraries mostly
under MIT or BSD style licenses. `GSTREAMER-LICENSES/<component>/` of the runtime
carries the license texts of every component built into it,
`GSTREAMER-NOTICE.txt` says where the corresponding source is and how to replace
the libraries, and the source archive of the same release is that corresponding
source.
