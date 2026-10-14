# Image of every Linux job: Ubuntu 24.04, the Rust toolchain and the tools the
# GStreamer build needs. The same file builds natively on both Linux
# platforms: under podman on tuco-pc for linux-amd64, under Apple `container`
# on atik for linux-arm64. Nothing is cross compiled, so there are no
# foreign-architecture packages here.
# Built by the runner with context `.zeughaus-ci/`.
FROM docker.io/library/ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive

# GStreamer is not taken from the distribution: the recipe builds it from
# source. What it needs from the system is:
# - git, curl, ca-certificates: fetch the sources and the tools.
# - build-essential: the C and C++ compiler and make (nasm is built with it).
# - pkg-config: how meson finds the libraries the plugins link dynamically.
# - flex, bison: the parser generators of GStreamer's own parser.
# - the X11 and ALSA development packages, for the same libraries: ximagesrc
#   links X11, Xext, Xfixes and Xdamage, the alsa plugin links libasound.
# The meson, ninja, cmake and Python of the build come from uv at run time,
# not from here.
RUN apt-get update && apt-get install -y --no-install-recommends \
      git curl ca-certificates build-essential pkg-config flex bison \
      libasound2-dev libx11-dev libxext-dev libxfixes-dev libxdamage-dev \
 && rm -rf /var/lib/apt/lists/*

# Every download below is pinned with its SHA-256 for both architectures;
# `uname -m` (x86_64 or aarch64) picks the one of the platform building the
# image.

# uv 0.12.23: the Python, meson, ninja and cmake of the GStreamer build
# (src/tools.rs). Hashes as in the release's `.sha256` files.
ARG UV_SHA256_X86_64=9167d72b3319674b6303c4cbe071854bba13ebdf3d76b1a7cbdc175471fb66d6
ARG UV_SHA256_AARCH64=6524bd338177ed50d035d39354e12545e993bbeba2ecbddf0480c5b3a81d313f
RUN arch=$(uname -m) \
 && case "$arch" in x86_64) sha=$UV_SHA256_X86_64 ;; aarch64) sha=$UV_SHA256_AARCH64 ;; *) exit 1 ;; esac \
 && curl -fsSL -o /tmp/uv.tar.gz \
      "https://github.com/astral-sh/uv/releases/download/0.12.23/uv-$arch-unknown-linux-gnu.tar.gz" \
 && echo "$sha  /tmp/uv.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/uv.tar.gz -C /usr/local/bin --strip-components=1 \
 && rm /tmp/uv.tar.gz

# The Rust toolchain: the stable channel with clippy and rustfmt for the
# gate (check). The cargo of the platform jobs builds the recipe's tool.
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal \
      --default-toolchain stable -c clippy -c rustfmt
