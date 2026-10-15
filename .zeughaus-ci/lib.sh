# Sourced by every shell job (`. .zeughaus-ci/lib.sh`). It has no `# /// ci`
# block, so the runner treats it as a helper and never runs it on its own.

# Persistent caches live in the workspace under .ci-cache (listed in each
# job's `cache`); target/ is cached as well.
export CARGO_HOME="$CI_WORKSPACE/.ci-cache/cargo"
case "$(uname -s)" in
Linux)
    # The toolchain of the image (ubuntu24.Containerfile). The jobs on
    # sadala's host have none and run no cargo.
    export RUSTUP_HOME=/opt/rustup
    export PATH="/opt/cargo/bin:$CARGO_HOME/bin:$PATH"
    ;;
Darwin)
    # The host's rustup (~/.rustup); its proxies are on the runner's PATH.
    export PATH="$CARGO_HOME/bin:$PATH"
    ;;
esac
# cargo runs rustc in each dependency's directory, and rustup honours a
# `rust-toolchain.toml` it finds there: the parallel rustc processes of a
# cold build then install the components it asks for at once and fail on
# each other. The CI builds with the stable toolchain it has, whatever a
# dependency asks for.
export RUSTUP_TOOLCHAIN=stable
export CARGO_TERM_COLOR=always
export CARGO_INCREMENTAL=0

# A tag must name the release id of this checkout (`v` + `cargo run --release -- release-id`),
# because the release is named after it. Every job of a release calls this
# before it builds anything.
check_tag() {
    [ "$CI_EVENT" = tag ] || return 0
    if ! cargo run --release --quiet -- check-tag "$CI_REF"; then
        echo "tag $CI_REF does not match the release id of this checkout (see: cargo run -- release-id)" >&2
        exit 1
    fi
}

# The platform jobs of a release, whose prefixes and sources the `release` job
# collects.
RELEASE_JOBS="linux-amd64 linux-arm64 windows-amd64 darwin-arm64"

# The target triple a platform job builds (the `--target` of its dist).
job_triple() {
    case "$1" in
    linux-amd64) echo x86_64-unknown-linux-gnu ;;
    linux-arm64) echo aarch64-unknown-linux-gnu ;;
    windows-amd64) echo x86_64-pc-windows-msvc ;;
    darwin-arm64) echo aarch64-apple-darwin ;;
    *)
        echo "no target for job $1" >&2
        return 1
        ;;
    esac
}

# The jobs on sadala handle every artifact of a release, well over a GB: their
# scratch directory lies in the workspace on disk, not in sadala's /tmp (a
# small tmpfs), next to the inputs it links. At the end they drop it and their
# inputs, which are copies of artifacts the CI keeps anyway: the runner keeps
# the directories of its last runs, inputs included, and sadala's disk is
# small. Sets `work`.
sadala_work() {
    work=$(mktemp -d "$CI_WORKSPACE/.ci-work.XXXXXX")
    trap 'rm -rf "$work" "${CI_INPUTS:?}"/*' EXIT
}
