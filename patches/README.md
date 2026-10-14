# Source patches

The recipe changes upstream's source in three places. Each change is a unified
diff in this directory, applied by the recipe with `git apply` to the GStreamer
monorepo checkout (`target/gstreamer/src-<target>/`) after the meson
subprojects it touches are unpacked (`src/patches.rs`). The paths in a patch are
relative to the root of that checkout.

A patch that does not apply is an error, not a warning: the source it patches
has moved on, and a GStreamer bump must not drop a fix without anyone noticing.
Applying a patch twice is harmless; the recipe recognises a tree the patch has
been applied to.

The header of every patch (the lines before the first `diff`, which `git apply`
ignores) says what is wrong without it and why the change fixes it.

| Patch | Changes | Targets | License of the changed file |
|---|---|---|---|
| `gst-plugins-bad-svtav1enc-low-delay.patch` | `ext/svtav1/gstsvtav1enc.c` of gst-plugins-bad 1.28.7: the packet loop no longer blocks in SVT-AV1's low-delay mode | all | LGPL-2.1-or-later |
| `libva-static-library-and-header-copy.patch` | `va/meson.build` of libva 2.22.0: static libraries that follow `--default-library`, and the headers copied at setup | Linux | MIT |
| `glib-macos-no-pipe2.patch` | `glib/glib-unixprivate.h` of GLib 2.86.3: no `pipe2` on macOS, where the SDK declares it but older systems lack it | macOS | LGPL-2.1-or-later |

## License

Each patch is under the license of the file it modifies: GLib and
gst-plugins-bad are LGPL-2.1-or-later, libva is MIT. The patches are not under
the `MIT OR Apache-2.0` license of the recipe's code, and the source archive of
a release carries them applied, together with the full source they modify.

## Changing a patch

Edit the file in the tree the patch applies to, regenerate the diff with
`git diff` against the pristine file (the paths `a/subprojects/...` and
`b/subprojects/...`), keep the header, and bump `RECIPE_REVISION` in
`src/release.rs`: the patches are part of what a release is built from, and
editing one rebuilds the prefix on the next run. A new patch is registered in
`src/patches.rs` with the targets it applies to and the subproject it needs
unpacked first; a test fails for a patch file that is not registered.
