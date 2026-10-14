#!/bin/bash
# /// ci
# on = ["tag v*"]
# platform = "darwin-arm64"
# cache = [".ci-cache", "target"]
# timeout_minutes = 240
# ///
# The GStreamer prefix and source for Apple Silicon Macs, built on atik's host
# with the host's toolchain.
set -euo pipefail
. .zeughaus-ci/lib.sh
check_tag
cargo run --release -- dist --target aarch64-apple-darwin --out "$CI_OUTPUT"
