//! The notice that travels with the libraries of a release, and the name of
//! the directory of license texts next to it.

use crate::release::{REPOSITORY_URL, asset_url, release_id, source_asset};

/// The notice that travels with the libraries, and the directory of license
/// texts next to it, as the packages of griasdi name them.
pub const NOTICE_FILE: &str = "GSTREAMER-NOTICE.txt";
pub const LICENSES_DIR: &str = "GSTREAMER-LICENSES";

/// Notice shipped next to the libraries (LGPL-2.1 section 6 and 4: where the
/// source is, how it was built, and that the libraries can be replaced).
/// `triple` is the target of the libraries: the source archive that belongs to
/// them is named after it. The license texts are in [`LICENSES_DIR`], which
/// `stage.rs` puts next to the notice.
pub fn notice(triple: &str) -> String {
    let id = release_id();
    let source = asset_url(&id, &source_asset(&id, triple));
    format!(
        "\
This package includes GStreamer and the libraries it is built with (GLib, FFmpeg's
libavcodec/libavformat/libavfilter/libavutil, orc, opus, libffi, pcre2, zlib, SVT-AV1
and others), under the LGPL-2.1-or-later and the licenses of the bundled libraries;
FFmpeg is built without its GPL parts.

They are release {id} of {REPOSITORY_URL},
built there for {triple} from the GStreamer source and
changed only by the source fixes in the patches/ directory of that repository.
They are dynamically linked: replace the library files to use another build.

Licenses:     the directory {LICENSES_DIR} next to this file holds the license texts of
              every one of these libraries, in one directory per component.
Source code:  {source}
              holds the corresponding source of these libraries (GStreamer, FFmpeg,
              SVT-AV1 and the other libraries it builds) with the source fixes applied,
              and the recipe to build them again. SVT-AV1 is under the
              BSD-3-Clause-Clear license and the Alliance for Open Media Patent License 1.0.
Licensing:    https://gstreamer.freedesktop.org/documentation/frequently-asked-questions/licensing.html
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_names_the_release_and_the_source_of_its_target() {
        let notice = notice("aarch64-apple-darwin");
        let id = release_id();
        assert!(notice.contains(&format!(
            "release {id} of https://github.com/griasdi/gstreamer"
        )));
        assert!(notice.contains(&format!(
            "https://github.com/griasdi/gstreamer/releases/download/v{id}/\
             gstreamer-{id}-aarch64-apple-darwin-source.tar.gz"
        )));
        assert!(notice.contains("replace the library files"));
        // The notice of another target names that target's source, not this one's.
        assert!(!notice.contains("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn the_notice_names_the_directory_of_the_license_texts_next_to_it() {
        assert!(
            notice("x86_64-unknown-linux-gnu")
                .contains("directory GSTREAMER-LICENSES next to this file")
        );
    }

    #[test]
    fn the_notice_leaves_no_placeholder_behind() {
        let notice = notice("x86_64-pc-windows-msvc");
        assert!(!notice.contains('{') && !notice.contains('}'));
        // The Rust dependency of the app that is under another license is
        // griasdi's own to name.
        assert!(!notice.contains("livesync"));
    }
}
