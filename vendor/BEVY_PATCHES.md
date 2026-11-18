# Bevy changes carried by Mudcrab

Mudcrab builds on Bevy 0.19.0. When Bevy has not released a fix that Mudcrab needs, Mudcrab
carries the change in a vendored copy of the crate, wired in through `[patch.crates-io]` in the
root `Cargo.toml`. A carried change is temporary. Replace it with Bevy's own version when Bevy
releases one. Each vendored crate has a `README.mudcrab.md` with its source hash and details.

To add an entry: vendor each published crate unchanged in one commit, apply the change in the next
commit, add a `// mudcrab:` comment at each touched spot, and fill in a row below.

Upstream states below are as of the date in the table.

| Crate | Change | Upstream | Upstream state | Applied | Files | Mudcrab additions | Removal rule |
|---|---|---|---|---|---|---|---|
| `bevy_pbr` | Do not give a morphed mesh the missing-descriptor index `u32::MAX`; bound the `layer_count` read | [bevy#26504](https://github.com/bevyengine/bevy/pull/26504), fixes [bevy#25966](https://github.com/bevyengine/bevy/issues/25966) | **Open**, not merged, 2026-11-18 | PR head `f9a00902b05f4583828f55b4c9d905d3f7e187a2` (3 commits). The Rust hunks apply unchanged to 0.19.0. The shader hunk is ported by hand from `morph.wesl` to `morph.wgsl`. | `src/render/morph.rs`, `src/render/mesh.rs`, `src/render/morph.wgsl` | A `mudcrab:` marker comment in `layer_count`; and the `prev_weight_at` bound (own addition, in the second commit) | Drop the patch when a Bevy release contains #26504 or an equivalent. If Bevy merges a different change, use Bevy's. Keep the `prev_weight_at` bound only if Bevy does not fix that read. |
| `bevy_pbr`, `bevy_render` | Clear stale GPU bin-unpacking jobs and bind groups every frame, so a skipped view (such as a shadow view of a light that left the view) cannot write work items into other jobs' ranges. Fixes flicker and wrong materials on macOS with shadow-casting point and spot lights | [bevy#26095](https://github.com/bevyengine/bevy/pull/26095), fixes [bevy#25595](https://github.com/bevyengine/bevy/issues/25595) | **Open**, not merged, 2026-11-18 | PR head `4c1641e4628ecc79e9a86f32e5e285280221f8a4`. Both hunks are ported by hand: the PR targets Bevy `main`, which has separate bin-unpacking and uniform-allocation resources, and 0.19.0 has only `BinUnpackingBuffers` and `BinUnpackingBindGroups` | `bevy_render/src/batching/gpu_preprocessing.rs`, `bevy_pbr/src/render/gpu_preprocess.rs` | `mudcrab:` marker comments at the touched spots; the port to the 0.19.0 types | Drop the patch when a Bevy release contains #26095 or an equivalent. If Bevy lands a different change, use Bevy's. |
| `bevy_pbr` | Remove a write-only list that leaked one staging buffer handle per view per frame with GPU clustering on | [bevy#26588](https://github.com/bevyengine/bevy/pull/26588), fixes [bevy#26283](https://github.com/bevyengine/bevy/issues/26283) | **Open**, not merged, 2026-11-18 | PR head `88ce16beee7c77fa3d340809bfb34bceffecea70`; the hunk applies unchanged to 0.19.0 | `bevy_pbr/src/cluster/gpu.rs` | A `mudcrab:` marker comment. Mudcrab turns GPU clustering off on Metal (`crates/engine/src/renderer_init.rs`) and keeps Bevy's default (on) on other backends, so this fix matters on Vulkan and DX12 only. Not exercised by the macOS validation run | Drop the patch when a Bevy release contains #26588 or an equivalent. |
| `bevy_image` | Widen the `zstd` requirement to `0.14.3` for four RustSec advisories | none (Mudcrab-only manifest change) | n/a | see [`bevy_image/README.mudcrab.md`](bevy_image/README.mudcrab.md) | `Cargo.toml` | the whole change | Drop it when the Bevy image release Mudcrab uses accepts the fixed Zstd dependencies. |

## Checking a morph change

The out-of-bounds read does not fault on every run, so a plain run proves nothing. Use Metal
shader validation in a hidden window and count `Invalid Metal usage` lines. Bevy 0.19.0 without
the morph fix gave 20 in 5 minutes on a release build. The target is 0. Needs a converted pack with
streamed actors, and a closed-loop route JSON around the start cell.

```sh
MTL_SHADER_VALIDATION=1 MTL_SHADER_VALIDATION_REPORT_TO_STDERR=1 MTL_SHADER_VALIDATION_FAIL_MODE=allow \
RUST_LOG=info,wgpu_hal=warn,wgpu_core=warn,naga=warn \
  python3 scripts/workspace.py cargo run --locked --bin engine -- \
  --assets <pack with actors> --worldspace 60 --grid-x 6 --grid-y -12 --stream-radius 2 \
  --always-active --hidden-window --no-render-diagnostics --route <closed-loop route.json> \
  --benchmark-duration 300 --benchmark-vsync --benchmark-output <bench.json> > <run.log> 2>&1
grep -c "Invalid Metal usage" <run.log>
```

Add `--release` for a faithful comparison. Do not set `MTL_DEBUG_LAYER=1`: it writes about 700 MB
of warnings per minute. The engine reads Point-light shadows from `settings.json`, not from a flag.
For the flicker class (bevy#26095), run the same command with `--lights` and a `settings.json`
that has `"point_lights": true` and `"point_light_shadows": true`, kept in a scratch `HOME`
so a personal settings file stays untouched. A run with no error lines is evidence, not proof:
a hidden run without validation did not fault even before the fix.

## Not carried yet

Bevy and wgpu changes that fix problems Mudcrab has hit, and that are not carried yet: bevy#26246
(wrong materials for the first frames), bevy#25739 (crash when `MeshMorphWeights` is missing),
bevy#26418 (bind group caching, performance) and the wgpu Metal command-buffer work (wgpu#10506,
#10570, #10875), which needs wgpu 30 and so waits for the Bevy 0.20 upgrade. Add a row above when
one is carried.
