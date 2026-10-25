//! The identity of a release: its id `<GStreamer tag>-<recipe revision>`, the
//! git tag `v<id>` that publishes it, and the names of its assets.

use anyhow::{Result, bail};

use crate::pins::GST_TAG;

/// Where the releases are published.
pub const REPOSITORY_URL: &str = "https://github.com/griasdi/gstreamer";

/// The second half of the release id. It is bumped whenever a change to this
/// repository changes what a release ships (a pin, an option, a patch, what is
/// packed out of the prefix and how), and starts again at 1 with a new
/// GStreamer tag. A release is published by pushing the tag [`release_tag`];
/// the CI refuses a tag that is not that. Whether the prefix under
/// `target/gstreamer` is built again for a new revision is
/// [`crate::plan::BUILD_REVISION`] and the other inputs of its stamp.
pub const RECIPE_REVISION: &str = "3";

/// The release id, for example `1.28.7-1`.
pub fn release_id() -> String {
    format!("{GST_TAG}-{RECIPE_REVISION}")
}

/// The git tag of the release, for example `v1.28.7-1`.
pub fn release_tag() -> String {
    format!("v{}", release_id())
}

/// Fails unless `tag` is [`release_tag`].
pub fn check_tag(tag: &str) -> Result<()> {
    check_tag_against(tag, &release_id())
}

fn check_tag_against(tag: &str, id: &str) -> Result<()> {
    if tag.strip_prefix('v') == Some(id) {
        return Ok(());
    }
    bail!(
        "tag {tag} is not v{id}: this checkout builds GStreamer {GST_TAG} with recipe revision \
         {RECIPE_REVISION}; tag v{id}, or change RECIPE_REVISION in src/release.rs"
    )
}

/// The archive of the runtime and the link files of `target` in release `id`.
pub fn binary_asset(id: &str, target: &str) -> String {
    format!("gstreamer-{id}-{target}.tar.gz")
}

/// The archive of the source the binaries of `target` in release `id` are
/// built from.
pub fn source_asset(id: &str, target: &str) -> String {
    format!("gstreamer-{id}-{target}-source.tar.gz")
}

/// Where `asset` of release `id` is published.
pub fn asset_url(id: &str, asset: &str) -> String {
    format!("{REPOSITORY_URL}/releases/download/v{id}/{asset}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_v_plus_the_release_id_is_the_release_tag() {
        assert!(check_tag_against("v1.28.7-1", "1.28.7-1").is_ok());
        for wrong in [
            "1.28.7-1",
            "v1.28.7-2",
            "v1.28.7",
            "v1.28.8-1",
            "vv1.28.7-1",
            "V1.28.7-1",
            "v1.28.7-1\n",
            "refs/tags/v1.28.7-1",
            "",
        ] {
            assert!(check_tag_against(wrong, "1.28.7-1").is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn the_tag_of_this_checkout_is_v_plus_its_id() {
        assert_eq!(release_tag(), format!("v{}", release_id()));
        assert!(check_tag(&release_tag()).is_ok());
        assert!(release_id().starts_with(GST_TAG));
    }

    #[test]
    fn assets_are_named_after_release_and_target() {
        assert_eq!(
            binary_asset("1.28.7-1", "aarch64-apple-darwin"),
            "gstreamer-1.28.7-1-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            source_asset("1.28.7-1", "x86_64-pc-windows-msvc"),
            "gstreamer-1.28.7-1-x86_64-pc-windows-msvc-source.tar.gz"
        );
        assert_eq!(
            asset_url("1.28.7-1", "SHA256SUMS"),
            "https://github.com/griasdi/gstreamer/releases/download/v1.28.7-1/SHA256SUMS"
        );
    }
}
