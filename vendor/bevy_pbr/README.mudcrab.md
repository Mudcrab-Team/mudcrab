# Vendored `bevy_pbr` 0.19.0, with bevyengine/bevy#26504, #26095 and #26588

Source: the published [`bevy_pbr` 0.19.0 crate](https://crates.io/crates/bevy_pbr/0.19.0),
SHA-256 `38ebc777de2d7f45888c68e8e04949bfb8a40aee62f700283fc340f525e67f52`. Upstream provenance is
kept in `.cargo_vcs_info.json` (Bevy commit `c6f634ca9f406d68ba5109d921247b654cb42c10`,
`crates/bevy_pbr`), and the unmodified published manifest is `Cargo.toml.orig`. The manifest, both
license files (`MIT OR Apache-2.0`) and `README.md` are unchanged. The crate's own lockfile and its
`.cargo-ok` marker are omitted, so Mudcrab's root `Cargo.lock` controls resolution.

The first commit that added this directory is identical to the published crate, except that it
omits the crate's own `Cargo.lock` and `.cargo-ok` marker. To see every Mudcrab change, download the
published crate (`https://static.crates.io/crates/bevy_pbr/bevy_pbr-0.19.0.crate`), unpack it and run
`diff -r` against this directory. `grep -rn "mudcrab:" src` lists the touched spots.
The ledger of all carried Bevy changes is [`../BEVY_PATCHES.md`](../BEVY_PATCHES.md).

## Why

Mudcrab's NPC bodies have a morph target. A visible morphed mesh whose morph descriptor is not
ready gets the missing-descriptor index `u32::MAX` in its GPU mesh uniform. The shader then reads
`morph_descriptors[u32::MAX]` about 80 GiB past a 100-byte buffer. On Metal this is a `BIF0` GPU page
fault (`bevyengine/bevy#25966`). The uniform is not rewritten while the instance does not change,
so a still actor also keeps the base shape.

## Carried changes

| Change | Where it comes from | Files |
|---|---|---|
| Store the mesh asset id in `MorphWeightsInfo`; record entities whose descriptor is pending; make the mesh instance builder return `None` (so the instance is extracted again) until the descriptor exists | `bevyengine/bevy#26504`, applied unchanged to 0.19.0 | `src/render/morph.rs`, `src/render/mesh.rs` |
| `layer_count` returns 0 for a descriptor index outside `morph_descriptors` | `bevyengine/bevy#26504`, ported by hand: the PR edits `morph.wesl` on Bevy `main`, and 0.19.0 has `morph.wgsl`. The comment carries a marker that Mudcrab's engine-side guard recognises. | `src/render/morph.wgsl` |
| `prev_weight_at` falls back to the current weight when its offset or index is outside `prev_morph_weights` | **Mudcrab's own addition, not in the PR.** The no-previous-weights offset is `!0`; weight 0 would read index `u32::MAX`. Reached only with motion vectors. | `src/render/morph.wgsl`, comment in `src/render/morph.rs` |
| Clear the bin-unpacking bind groups at the start of `prepare_preprocess_bind_groups` | `bevyengine/bevy#26095` (bevy_render half: see `../bevy_render/README.mudcrab.md`), ported by hand to 0.19.0's `BinUnpackingBindGroups` | `src/render/gpu_preprocess.rs` |
| Remove the write-only `metadata_staging_pending_buffers` list (a GPU clustering leak) | `bevyengine/bevy#26588`, applied unchanged. Mudcrab turns GPU clustering off on Metal, so it matters on Vulkan and DX12 only | `src/cluster/gpu.rs` |
| Add `FogFalloff::PowerRamp` (fog mode 5): `s = saturate((d - start) / (end - start))`, intensity `min(s^power, max)` with max in the fog colour's alpha, and a fog colour that runs from a near colour (`bi`) to the far colour by the intensity. It draws the weather record's exterior fog (#891) | **Mudcrab's own addition, not a Bevy change** | `src/fog.rs`, `src/render/fog.rs`, `src/render/fog.wgsl`, `src/render/mesh_view_types.wgsl`, `src/render/pbr_functions.wgsl` |

Every Mudcrab comment starts with `mudcrab:`, so `grep -rn "mudcrab:" src` lists the touched spots.

## Updating or removing this copy

Drop this directory and the `bevy_pbr` entry in the root `[patch.crates-io]` when a Bevy release
that Mudcrab uses contains `bevyengine/bevy#26504`, `#26095` and `#26588`, or equivalent fixes (each can go on its own). If Bevy lands a different
change, take Bevy's version. Keep the `prev_weight_at` bound only if Bevy does not fix that read. The power ramp fog is Mudcrab's own addition: keep it, since no Bevy release replaces it.
After any change run `cargo test --locked -p engine` and the Metal shader-validation run in
`vendor/BEVY_PATCHES.md`, and confirm it reports 0 out-of-bounds reads.
