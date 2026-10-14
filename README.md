# griasdi-gstreamer

The GStreamer that [griasdi](https://github.com/griasdi/griasdi) links and
ships, built from source with a fixed recipe, and published as prebuilt install
prefixes together with the source they were built from.

griasdi uses GStreamer 1.28.7 with exactly the plugins its player and screen
sharing need, and FFmpeg's LGPL decoders for H.264, VP9 and AAC, dav1d and
SVT-AV1 for AV1. The libraries are built from source because no distribution
carries that set on every platform, and because griasdi changes a few lines of
LGPL code (`patches/`). Publishing those changes means publishing the
corresponding source, and that is what a release does: each prefix comes with
the source of its binaries. griasdi itself only downloads, verifies and
unpacks a release.

## What a release contains

A release is named `<GStreamer tag>-<recipe revision>`, for example `1.28.7-1`,
and published from the git tag `v1.28.7-1`. Its assets, at
`https://github.com/griasdi/gstreamer/releases/download/v<id>/<asset>`, are for
each target `<T>` (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`, `aarch64-apple-darwin`, `x86_64-apple-darwin`):

| Asset | Content |
|---|---|
| `gstreamer-<id>-<T>.tar.gz` | The install prefix, as exactly one top-level directory `<T>/`: `lib/`, `include/`, `bin/`, `libexec/`, `share/`, and on Linux `griasdi-pkgconfig/`. `RELEASE` holds the id, `NOTICE` says what it is and where its source is, `share/licenses/<component>/` holds the license texts of every component built into it. On Windows `bin/pkgconf.exe` is the pkgconf that cargo's pkg-config probes use. |
| `gstreamer-<id>-<T>-source.tar.gz` | The corresponding source of those binaries, as exactly one top-level directory `gstreamer-<id>-<T>-source/`: the GStreamer monorepo with the meson subprojects the build fetched, dav1d, SVT-AV1 (and pkgconf on Windows), all with the patches applied, and this repository at the tag. `SOURCES.txt` lists what is where and where it came from. No `.git` directories, no build directories. |
| `SHA256SUMS` | `sha256sum` format, over all assets. |

Every `.pc` file in a prefix derives its prefix from its own location
(`${pcfiledir}`), so a prefix works wherever it is unpacked. On macOS the
libraries are named `@rpath/<library>` and carry an rpath to the `lib/` of their
prefix. On Linux the build is one shared `libgstreamer-full-1.0.so` with every
library static inside it; the real `.pc` files in `lib/pkgconfig` describe the
static archives and are not for linking, the shims in `griasdi-pkgconfig` are:
they name every GLib and GStreamer pkg-config name the Rust crates probe
(`glib-2.0`, `gobject-2.0`, `gio-2.0`, `gstreamer-1.0`, `gstreamer-base-1.0`,
`gstreamer-app-1.0`, `gstreamer-video-1.0`) and `dav1d`. On Windows and macOS
there is one DLL or dylib per library and plugin.

## How griasdi uses it

griasdi pins the release id and the SHA-256 of the archive of each target, and
nothing else. To get a prefix it downloads
`gstreamer-<id>-<T>.tar.gz`, checks the SHA-256 against its pin and unpacks it
into its own `target/gstreamer/`, which gives `target/gstreamer/<T>/`. The file
`target/gstreamer/<T>/RELEASE` holding the pinned id means the prefix is up to
date. The sums to pin are in the release's `SHA256SUMS`.

## Building locally

The tool is a Rust program; everything it builds lands in `target/gstreamer/` of
the checkout: `tools/` (the build tools and downloads, shared), `src-<T>/` (the
GStreamer monorepo with the fetched subprojects, patched), `build-<T>/` (the
build directories and the meson machine file) and the prefix `<T>/`.

```sh
cargo run --release -- build [--target <T>]
cargo run --release -- dist [--target <T>] --out <dir>
cargo run --release -- release-id
cargo run --release -- check-tag <tag>
```

- `build` builds the prefix of `<T>` (the host without `--target`) if it is not
  up to date, and finishes it: relocatable `.pc` files, license texts, `NOTICE`,
  `RELEASE`. `<T>/.recipe` holds a hash of every input (the pins, the build
  options, the patches, the recipe revision); a prefix with a matching hash
  is returned at once, any other change rebuilds from scratch. A build that
  failed after the install only finishes again. A full build takes about five
  minutes on a 16 core desktop and longer on fewer cores.
- `dist` runs `build`, then writes the two archives of the release into `<dir>`
  and prints their `sha256sum` lines.
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
- macOS: the Xcode command line tools, `flex`, `bison` and `pkgconf`
  (`brew install flex bison pkgconf`). `x86_64-apple-darwin` on an Apple Silicon
  Mac is a cross build and builds the host's prefix first.
- Windows: Visual Studio with the MSVC build tools (meson and cmake find them).
  pkgconf is built into the tools and shipped as `bin/pkgconf.exe`.

`dist` also needs `git` to see which files of this repository go into the source
archive: tracked files and untracked ones that are not ignored.

## Cutting a release

1. Make the change. `RECIPE_REVISION` in `src/release.rs` is bumped by every
   change that alters what a build produces: a pin, an option, a patch, the way
   a prefix is finished. A new GStreamer tag changes the pins in `src/pins.rs`
   and starts again at revision `1`. `cargo run --release -- release-id` prints
   the id.
2. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
   `cargo test` pass. The CI's `check` job runs them for every push to `main`.
3. Commit and push, then tag and push the tag:
   `git tag v<id> && git push origin v<id>`. The CI refuses a tag that is not `v`
   plus the id of the commit it is on.
4. The tag builds the five targets natively (`.zeughaus-ci/`: `linux-amd64` and
   `linux-arm64` in the `ubuntu24` image, `windows-amd64` on the Windows VM,
   `darwin-arm64` and `darwin-amd64` on atik's host), each with `dist`. The
   `release` job then creates the GitHub release `v<id>` with all ten archives and
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

The prefixes are made of other people's code and stay under their licenses.
GStreamer, GLib and FFmpeg (built without its GPL parts, `gpl=disabled`) are
under the LGPL-2.1-or-later, the other libraries mostly under MIT or BSD style
licenses. `share/licenses/<component>/` of a prefix carries the license texts of
every component built into it, `NOTICE` says where the corresponding source is
and how to replace the libraries, and the source archive of the same release is
that corresponding source.
