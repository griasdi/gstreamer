#!/bin/bash
# /// ci
# on = ["tag v*"]
# platform = "linux-arm64"
# image = "ubuntu24"
# cache = [".ci-cache", "target"]
# when_busy = "freeze"
# timeout_minutes = 240
# ///
# The aarch64 Linux prefix of GStreamer and its source, built natively in an
# Apple `container` on atik.
set -euo pipefail
. .zeughaus-ci/lib.sh
check_tag
cargo run --release -- dist --target aarch64-unknown-linux-gnu --out "$CI_OUTPUT"
