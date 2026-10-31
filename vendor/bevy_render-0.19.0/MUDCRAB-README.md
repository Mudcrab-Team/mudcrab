# bevy_render 0.19.0: Mudcrab production patches

This directory contains the published `bevy_render` 0.19.0 crate with the production
changes listed below. The root `[patch.crates-io]` entry selects this copy.

## Provenance and licensing

- Source: crates.io `bevy_render` 0.19.0.
- Published archive SHA-256: `a89b2cc850a4c3fd12a6a088a4fc3e80118b2e1b569da8936f6d4d99ca1b6ee7`.
- Upstream revision: `c6f634ca9f406d68ba5109d921247b654cb42c10`,
  `crates/bevy_render`; also recorded in `.cargo_vcs_info.json`.
- Upstream licence: MIT OR Apache-2.0. `LICENSE-MIT` and `LICENSE-APACHE`
  retain the published bytes. Registry bookkeeping files are not included.

## Production changes

- Pipeline readiness, render assets, erased render assets and mesh allocation
  publish owner-maintained revisions at relevant mutations. Render-asset, mesh
  allocation and pipeline-availability stamps are process-wide, so resource
  replacement cannot alias an old retained proof. Erased assets pair their local
  revision with a collection scope. Immutable lookups do not certify writes
  through escaped GPU handles.
- Native binned/sorted phases expose membership queries; multidrawable batch
  sets expose their entities. Draw-registry structural revisions support exact
  dependency tracking. Missing draw functions skip their item and iteration
  continues, including advancing and warning once for a missing sorted item.
- Initialized sampled fallback images omit attachment capability;
  uninitialized depth/MSAA targets keep attachment use. This intentionally
  changes the published D2 color-fallback usage flags: these shared default
  sampling inputs cannot be render targets. The shadow cache rejects textures
  with attachment/storage capability because writes through escaped handles
  have no owner revision. Keeping attachment capability on a default sampled
  fallback would therefore refuse reuse for otherwise unchanged materials.
  Code requiring a render target must allocate and track its own image;
  the cache still refuses untracked writable textures. Copy writes remain
  subject to the cooperative publication contract in the PBR README.
- GPU readback drains requests directly into the mapped collection. Trace spans
  separate readback encoding, encoder finishing and submission. Initialization
  helpers package ordinary wgpu resources without raw Vulkan callbacks.
- The sparse-buffer label omits a redundant formatting borrow, and GPU readback
  avoids a drain/collect allocation; the pending command-buffer drain takes
  its encoders with `mem::take` for the same reason. These keep the patched member compatible
  with current Clippy checks; splitting readback fields permits direct draining.
  Crate-local formatting configuration preserves the published style.
  Adjacent tests cover the ordinary production APIs.

The renderer is a workspace member so its ordinary tests can be selected by
package name; its explicit development optimization remains at level 3.

## Differences from the published archive

- `MUDCRAB-README.md`
- `rustfmt.toml`
- `src/erased_render_asset.rs`
- `src/erased_render_asset_tests.rs`
- `src/gpu_readback.rs`
- `src/mesh/allocator.rs`
- `src/render_asset.rs`
- `src/render_asset_tests.rs`
- `src/render_phase/draw.rs`
- `src/render_phase/mod.rs`
- `src/render_resource/pipeline_cache.rs`
- `src/render_resource/sparse_buffer_vec.rs`
- `src/renderer/mod.rs`
- `src/renderer/render_context.rs`
- `src/settings.rs`
- `src/texture/fallback_image.rs`
- `src/texture/fallback_image_tests.rs`

This inventory records the retained patches. It makes no claim that tests,
GPU behavior or performance have passed. Before removing the root patch, migrate
consumers of the added APIs and regenerate the lockfile. Re-evaluate these patches
against the published sources on every Bevy upgrade.
