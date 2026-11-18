# Mudcrab patch to bevy_render 0.19.0

This is the published `bevy_render` 0.19.0 crate (archive SHA-256
`a89b2cc850a4c3fd12a6a088a4fc3e80118b2e1b569da8936f6d4d99ca1b6ee7`).
The MIT and Apache-2.0 license files are retained.

`src/render_resource/pipeline_cache.rs` allows asynchronous pipeline creation on
macOS Metal while preserving explicit synchronous requests and synchronous behavior
for other macOS backends. `src/lib.rs` updates the corresponding API documentation.

Remove this patch when a Bevy release provides and validates equivalent macOS Metal
async compilation; reassess it on every Bevy upgrade. Track bevyengine/bevy#26151.
PR #428 also vendors this crate. Integrate both patches into one copy if either lands.

Because this is a path dependency, registry advisory scanners may not identify it
as the published crate. Check Bevy advisories manually and update the lockfile when
refreshing the vendor copy.
