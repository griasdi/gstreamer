#!/bin/bash
# /// ci
# on = ["tag v*"]
# platform = "darwin-amd64"
# cache = [".ci-cache", "target"]
# timeout_minutes = 240
# ///
# The GStreamer prefix and source for Intel Macs, built on atik's host (an
# Apple Silicon Mac) for x86_64-apple-darwin, which also builds the host's
# prefix first.
set -euo pipefail
. .zeughaus-ci/lib.sh
check_tag
cargo run --release -- dist --target x86_64-apple-darwin --out "$CI_OUTPUT"
