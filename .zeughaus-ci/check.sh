#!/bin/bash
# /// ci
# on = ["push main", "tag v*"]
# platform = "linux-amd64"
# image = "ubuntu24"
# cache = [".ci-cache", "target"]
# when_busy = "freeze"
# timeout_minutes = 60
# ///
# The gate of the recipe: formatting, clippy with warnings denied and the
# tests, with the toolchain of the image. A tag first checks that it names the
# release id of this checkout.
set -euo pipefail
. .zeughaus-ci/lib.sh
check_tag
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
