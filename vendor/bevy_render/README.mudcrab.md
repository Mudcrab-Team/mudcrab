# Vendored `bevy_render` 0.19.0, with bevyengine/bevy#26095

Source: the published [`bevy_render` 0.19.0 crate](https://crates.io/crates/bevy_render/0.19.0),
SHA-256 `a89b2cc850a4c3fd12a6a088a4fc3e80118b2e1b569da8936f6d4d99ca1b6ee7`. Upstream provenance is
kept in `.cargo_vcs_info.json` (Bevy commit `c6f634ca9f406d68ba5109d921247b654cb42c10`,
`crates/bevy_render`), and the unmodified published manifest is `Cargo.toml.orig`. The manifest, both
license files (`MIT OR Apache-2.0`) and `README.md` are unchanged. The crate's own lockfile and its
`.cargo-ok` marker are omitted, so Mudcrab's root `Cargo.lock` controls resolution.

The first commit that added this directory is identical to the published crate, except that it
omits the crate's own `Cargo.lock` and `.cargo-ok` marker. To see every Mudcrab change, download the
published crate (`https://static.crates.io/crates/bevy_render/bevy_render-0.19.0.crate`), unpack it and run
`diff -r` against this directory. `grep -rn "mudcrab:" src` lists the touched spots.
The ledger of all carried Bevy changes is [`../BEVY_PATCHES.md`](../BEVY_PATCHES.md).

## Why

On a view that a frame skips (for example the shadow view of a point or spot light that left the
view), Bevy kept last frame's bin-unpacking jobs. A job points into a metadata buffer that is
cleared and refilled every frame, so the job pointed at another job's metadata. `unpack_bins` then
wrote work items into other jobs' ranges. Bevy's shaders have no runtime bounds checks, so on Metal
the writes land in neighbouring buffers: flicker and wrong materials with shadow-casting point and
spot lights (`bevyengine/bevy#25595`).

## Carried change

| Change | Where it comes from | Files |
|---|---|---|
| Clear the unpacking jobs of every view in `clear_bin_unpacking_buffers`, and no longer clear them per view in `write_binned_instance_buffers` | `bevyengine/bevy#26095` (open), ported by hand: the PR targets Bevy `main`, which has separate bin-unpacking and uniform-allocation resources, and 0.19.0 has only `BinUnpackingBuffers`. The `bevy_pbr` half is in `../bevy_pbr/README.mudcrab.md` | `src/batching/gpu_preprocessing.rs` |

Every Mudcrab comment starts with `mudcrab:`, so `grep -rn "mudcrab:" src` lists the touched spots.

## Updating or removing this copy

Drop this directory and the `bevy_render` entry in the root `[patch.crates-io]` when a Bevy release
that Mudcrab uses contains `bevyengine/bevy#26095` or an equivalent fix. If Bevy lands a different
change, take Bevy's version, together with the `bevy_pbr` half. After any change run
`cargo test --locked -p engine` and a hidden Metal shader-validation run with point-light shadows on.
