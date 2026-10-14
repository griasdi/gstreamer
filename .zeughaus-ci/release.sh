#!/bin/bash
# /// ci
# needs = ["check", "linux-amd64", "linux-arm64", "windows-amd64", "darwin-arm64", "darwin-amd64"]
# machine = "sadala"
# secrets = ["GRIASDI_GITHUB_TOKEN"]
# when_busy = "run"
# timeout_minutes = 60
# ///
# The GitHub release of a tag: the install prefix and the source of every
# platform job, and SHA256SUMS over the ten files. The release id is the tag
# without its `v`; a platform job that built another release id fails this
# job, because its files are missing.
#
# Runs on sadala (aarch64, 2 cores, 3 GB, a small /tmp tmpfs) without cargo
# and without `gh`: curl, jq, sha256sum and git. The GitHub REST API through
# curl, the token in a header file of a private directory, never on a command
# line or in the log.
set -euo pipefail

. .zeughaus-ci/lib.sh
sadala_work

if [ "$CI_EVENT" != tag ]; then
    echo "the release is made for a tag, not for $CI_EVENT" >&2
    exit 1
fi
tag=$CI_REF
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+-[0-9]+$ ]]; then
    echo "tag $tag is not a release id (v<GStreamer version>-<recipe revision>)" >&2
    exit 1
fi
R=${tag#v}

mkdir "$work/release" "$work/private"

# The prefixes and sources of the platform jobs: exactly the two files of this
# release id per job, as hard links (a copy where the inputs lie on another
# file system). Any other file of a job is an error.
names=()
for job in $RELEASE_JOBS; do
    triple=$(job_triple "$job")
    dir=$CI_INPUTS/$job
    if [ ! -d "$dir" ]; then
        echo "no artifacts of job $job in $CI_INPUTS" >&2
        exit 1
    fi
    for name in "gstreamer-$R-$triple.tar.gz" "gstreamer-$R-$triple-source.tar.gz"; do
        if [ ! -f "$dir/$name" ]; then
            echo "job $job has no $name" >&2
            exit 1
        fi
        ln "$dir/$name" "$work/release/$name" 2>/dev/null || cp "$dir/$name" "$work/release/$name"
        names+=("$name")
    done
    count=$(find "$dir" -mindepth 1 -maxdepth 1 | wc -l)
    if [ "$count" -ne 2 ]; then
        echo "job $job has files besides its two files of $R:" >&2
        ls -A "$dir" >&2
        exit 1
    fi
done

# The checksums of the ten files, sorted by name; the list does not name itself.
(cd "$work/release" && sha256sum -- "${names[@]}" | sort -k2,2 > SHA256SUMS)

(
    umask 077
    printf 'Authorization: Bearer %s\n' "$GRIASDI_GITHUB_TOKEN" > "$work/private/github.hdr"
)
unset GRIASDI_GITHUB_TOKEN
api=https://api.github.com/repos/$CI_REPO_SLUG
github() {
    curl -fsS -H "@$work/private/github.hdr" -H 'Accept: application/vnd.github+json' \
        -H 'X-GitHub-Api-Version: 2022-11-28' "$@"
}
# The HTTP status of a request whose failure is an answer, not an error.
github_status() {
    curl -sS -o "$work/response" -w '%{http_code}' -H "@$work/private/github.hdr" \
        -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' "$@"
}
# The release of tag $1, if there is one; the tag itself stays.
delete_release_of() {
    local code
    code=$(github_status "$api/releases/tags/$1")
    case "$code" in
    200) github -X DELETE "$api/releases/$(jq -r .id "$work/response")" >/dev/null ;;
    404) ;;
    *)
        echo "looking up release $1: HTTP $code" >&2
        exit 1
        ;;
    esac
}

sha=$(git rev-parse HEAD)
gst=${R%-*}
rev=${R##*-}
{
    printf 'GStreamer %s for griasdi, release `%s` (recipe revision %s).\n\n' "$gst" "$R" "$rev"
    printf 'Built by the recipe at this tag, from commit `%s`. For each target there are two files:\n\n' "$sha"
    printf -- '- `gstreamer-%s-<target>.tar.gz`: the install prefix (GStreamer and its plugins and libraries) built for the target.\n' "$R"
    printf -- '- `gstreamer-%s-<target>-source.tar.gz`: the source the prefix was built from.\n\n' "$R"
    printf 'Targets: x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, x86_64-pc-windows-msvc, aarch64-apple-darwin, x86_64-apple-darwin.\n\n'
    printf 'How the recipe builds these and how to use them: [README.md](https://github.com/%s/blob/%s/README.md).\n\n' "$CI_REPO_SLUG" "$tag"
    printf 'SHA256SUMS:\n\n```\n'
    cat "$work/release/SHA256SUMS"
    printf '```\n'
} > "$work/notes.md"

# A rerun replaces the release; the tag is the one pushed by hand and stays.
delete_release_of "$tag"
jq -n --arg tag "$tag" --arg sha "$sha" --arg name "GStreamer $R for griasdi" --rawfile body "$work/notes.md" \
    '{tag_name: $tag, target_commitish: $sha, name: $name, body: $body, prerelease: false}' \
    > "$work/release.json"
id=$(github -X POST --data-binary "@$work/release.json" "$api/releases" | jq -r .id)
echo "created release $tag ($id)"
for file in "$work/release"/*; do
    github -X POST -H 'Content-Type: application/octet-stream' -T "$file" \
        "https://uploads.github.com/repos/$CI_REPO_SLUG/releases/$id/assets?name=$(basename "$file")" \
        >/dev/null
    echo "uploaded $(basename "$file")"
done
