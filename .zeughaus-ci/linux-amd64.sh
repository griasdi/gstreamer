#!/bin/bash
# /// ci
# on = ["tag v*"]
# platform = "linux-amd64"
# image = "ubuntu24"
# cache = [".ci-cache", "target"]
# when_busy = "freeze"
# timeout_minutes = 240
# ///
# The x86_64 Linux prefix of GStreamer and its source, built natively on
# tuco-pc.
set -euo pipefail
. .zeughaus-ci/lib.sh
check_tag
cargo run --release -- dist --target x86_64-unknown-linux-gnu --out "$CI_OUTPUT"
