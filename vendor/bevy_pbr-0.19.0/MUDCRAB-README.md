# bevy_pbr 0.19.0: Mudcrab production patches

This directory contains the published `bevy_pbr` 0.19.0 crate with the production
changes listed below. The root `[patch.crates-io]` entry selects this copy.

## Provenance and licensing

- Source: crates.io `bevy_pbr` 0.19.0.
- Published archive SHA-256: `38ebc777de2d7f45888c68e8e04949bfb8a40aee62f700283fc340f525e67f52`.
- Upstream revision: `c6f634ca9f406d68ba5109d921247b654cb42c10`,
  `crates/bevy_pbr`; also recorded in `.cargo_vcs_info.json`.
- Upstream licence: MIT OR Apache-2.0. `LICENSE-MIT` and `LICENSE-APACHE`
  retain the published bytes. Registry bookkeeping files are not included.

## Production changes

- Early/late shadow-pass diagnostics use paired recorder calls around the
  existing pass body, preserving dynamic-label lifetime and handled errors.
- Material bind-group allocators publish readiness revisions when availability
  changes, including bindless slab invalidation. Process-wide stamps also
  invalidate retained proofs when an allocator is replaced.
- The shadow cache retains directional texture slices only when physical
  ownership, complete dependency keys and current draw-producer certificates
  agree. Unknown material/command writers, incomplete draws and untracked
  writable textures refuse reuse. Stock caster, mesh, skin and morph producers
  publish mutations through the existing preparation/queue paths.
- Multiple directional cameras receive distinct physical slices. Device-limit
  overflow uses per-camera atlases and copies shared spot layers at their shader
  indices. COPY_SRC/COPY_DST capability required by this transport remains.
- Point/spot views retain their native queues without creating directional cache
  bookkeeping. Camera topology changes invalidate/prune owners.
- Poisoned dependency state refuses certificates. A poisoned owner lock withdraws
  ownership and disables retention until resource recreation. Late dependency
  changes revoke ownership and require a cleared render on the next frame.
- Ordinary cumulative/per-view decision counters, failure/retry behavior and
  separate sibling test modules remain. Conservative global revisions may
  invalidate maps whose own casters did not change.

Asset-backed image updates must publish through `Assets<Image>` or fresh mutable
`RenderAssets<GpuImage>` access. Writes through escaped raw handles are outside
this cooperative writer contract. The default shadow path has one native pass
body. Runtime correctness and performance require engine integration validation.

PBR and the patched renderer are workspace members, so root workspace formatting,
Clippy and nextest commands include both crates' test targets and use the root
`[patch.crates-io]` entries. Select PBR directly with
`cargo nextest run -p bevy_pbr --all-targets` from the repository root.
The crate-local `rustfmt.toml` preserves upstream's 2021 formatting style;
explicit development optimization remains at level 3 for both patched crates.
Workspace `--all-features` checks intentionally cover optional processors too.

## Differences from the published archive

- `MUDCRAB-README.md`
- `rustfmt.toml`
- `src/lib.rs`
- `src/material.rs`
- `src/material_bind_groups.rs`
- `src/render/light.rs`
- `src/render/light_shadow_cache_tests.rs`
- `src/render/mesh.rs`
- `src/render/morph.rs`
- `src/render/skin.rs`
- `src/render/skin_shadow_cache_tests.rs`
- `src/shadow_cache.rs`
- `src/shadow_cache_tests.rs`

This inventory includes formatting-only differences. It makes no claim that tests,
GPU behavior or performance have passed. Before removing the root patch, migrate
consumers of the added APIs and regenerate the lockfile. Re-evaluate these patches
against the published sources on every Bevy upgrade.
