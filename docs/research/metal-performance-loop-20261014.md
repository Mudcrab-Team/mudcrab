# Metal performance loop, 2026-10-14

Measurements behind the tiled GPU profile (`crates/engine/src/gpu_tuning.rs`) and the related
render changes on `t3/research-bevy-metal-performance`. Every row compares two binaries built from
adjacent commits, so a gain belongs to the change named in that row and not to a merged PR.

## Setup

- Apple M1 Pro (16-core GPU), macOS 27, Retina window 1600x900 points (3200x1800 pixels).
- `quick` profile builds; frame times are comparable with each other, not with `release`.
- Assets: a schema-26 conversion of Skyrim SE (`modern_assets_204`), worldspace 0x3c, start cell 0,0.
- Stationary: `--benchmark-frames 1800`, per-frame CSV; *load* is the first 600 measured frames,
  *steady* the rest. Routes: `--streaming-benchmark-route-speed 370` (walk) or `1500` (stress), 40 s
  after CPU settlement. Two or three runs per variant, alternated.
- Visual checks: acceptance screenshots at a fixed ground pose and the terrain-water fixture,
  compared pixel by pixel (mean absolute channel difference).
- Cold starts: each binary runs from its own `.app` bundle identifier, whose Metal cache directory
  is deleted before a cold run.

## Merged work, measured separately

| Source | Result on this machine |
|---|---|
| #212 stack (#206, #210, #211) and #248 | Stationary unchanged with the default flags (96-102 fps either side). `--prioritize-streaming --max-scene-loads 64` had slightly more load-phase frames over 16.7 ms (65-76 against 41-48). |

## Changes in this loop

| Change | Result |
|---|---|
| SHA-2 instructions for SHA-256 on Apple Silicon (`sha2` `asm`) | About 0.9 s less CPU while loading; frame times and LOD completion unchanged. |
| Water reflection on its own layer with a shadowless copy of the sun | No sun cascades built for the reflection: steady 9.35 -> 8.3 ms. Reflected terrain is unshadowed; fixture renders identically. |
| `NoIndirectDrawing` on tiled cameras | Steady 8.05 -> 6.9 ms; 1500 u/s route 127 -> 152 fps; screenshots match. |
| Async pipeline compilation on macOS (vendored `bevy_render`, bevyengine/bevy#26151) | Cold start to world ready 14.4 s -> 2.4-3.8 s, frames over 100 ms 10-13 -> 2; warm start 0.88 -> 0.69 s; steady unchanged. |
| CPU light clustering on tiled GPUs | Steady 6.9 -> 6.0 ms; stress route 151 -> 176 fps; Riverwood with `--lights` 8.7 -> 8.2 ms; screenshots match, including point lights. |
| Reuse a still camera's water reflection | Stationary steady 6.0 -> 5.3 ms; routes unchanged (the camera moves). |

## Tried and dropped

| Experiment | Why it was dropped |
|---|---|
| Reflection drawn only in the water's mirrored screen rectangle (sub-view and narrowed frustum) | Pixel-exact but only 0.2 ms; a view's fixed cost dominates. |
| Async compute pool of 8 threads | World ready sooner, but steady frames slower and more frames over 33 ms. |
| Larger mesh slabs (16 or 64 MiB, growth 2.0) | No change: the allocator's load cost is per-mesh copying. |
| 4 or 8 model spawns per frame instead of 32 | Same load spikes, slower world readiness. |
| Upload budget 4 or 64 MiB instead of 16 | No consistent change in load spikes. |
| CPU instance buffers (`use_gpu_instance_buffer_builder: false`) | Slower and logs errors every frame. |
| `ClusterConfig::None` per camera | wgpu validation error. |
| Hiding all full-detail terrain (measurement only) | 0.2 ms: terrain draws are not a significant cost, so merged terrain (#235/#236) was not pursued for speed. |

## Where the time goes now

- Steady state is render-thread bound at about 5.3 ms stationary and 5.1 ms on the walk route;
  the main thread is at about 3-4 ms. Above a render scale of 0.5 the GPU becomes the limit:
  steady 5.3 / 6.6 / 8.0 / 11.8 ms at 0.5 / 0.67 / 0.75 / 1.0.
- The remaining load-phase spikes (about 45 frames over 16.7 ms in the first 600) are Bevy's mesh
  allocation and glTF scene spawning while LOD chunks arrive, after the world is ready.

## Future wins

At 5.3 ms the frame budget has room to spare, so none of these are urgent. When frame time matters
again, prefer patches to Bevy (or wgpu) and engine-side changes over visual trade-offs: the earlier
screen-space reflection experiment showed how easily a "cheap" setting becomes a visible regression.
Nothing below has been measured unless it says so.

Most of the list is not specific to Metal. This loop found these costs on an M1 Pro, but the work
behind them runs on the CPU or once per view, so every backend pays it. Those items come first.

### Every backend: Bevy patches

- **Mesh allocation during LOD arrival.** Most of the remaining load-phase spikes are in Bevy's
  `allocate_and_free_meshes`. Larger slabs and other upload budgets did not help (see *Tried and
  dropped*), because the cost is copying each mesh in turn. That copying happens on the CPU before
  any backend sees it. A patch that coalesces a frame's copies per slab, or spreads allocation across
  frames under a byte budget like `RenderAssetBytesPerFrame`, would target it directly.
- **Per-view fixed cost.** The reflection view costs about 1.6 ms even when cut down to the water's
  screen rectangle (0.2 ms saved), so the cost is per view, not per pixel: extraction, visibility,
  specialization and phase queueing all run again for the second camera. Sharing visibility and
  specialization results between a camera and its mirror (same entities, mirrored frustum) would be
  a larger Bevy change, but it is the biggest single item left in the stationary frame.
- **Opt a camera out of directional shadow cascades.** Bevy builds cascades for every active camera
  that a shadow-casting directional light can see. On tiled GPUs the reflection camera avoids that
  today through a separate render layer and a shadowless copy of the sun (`ReflectionSun`). A
  camera-level opt-out in `bevy_light`/`bevy_pbr` would do the same with no duplicate light, and
  would let the desktop profile drop the reflection's cascades too. It is a small upstream change.
- **Render-thread work in general.** Steady frames are render-thread bound (about 5.3 ms against
  3-4 ms on the main thread). A `trace_chrome` capture of the render-thread spans should come
  before any patch here; the likely candidates are mesh specialization and batch preparation for
  views whose contents did not change.

### Every backend: engine-side, no visual change

- **Port #241 (cell-crossing index) onto #200's batched LOD visibility.** It conflicted with the
  batching and was left out; the hitch it targets still exists.
- **Scene spawning for LOD chunks.** The other half of the load spikes is
  `world_instance_spawner_system` spawning glTF scenes. Building LOD chunk entities directly from
  their meshes, or reusing a spawned hierarchy, would avoid the scene spawner's per-entity work.
  Lowering spawns per frame only delayed readiness without removing the spikes.
- **Bindless `TerrainMaterial`.** `StandardMaterial` is already bindless on Metal; terrain is not.
  Hiding all full-detail terrain saved only 0.2 ms, so this is low priority.

### Metal and macOS only

- **Upstream the async pipeline patch.** `vendor/bevy_render` exists only to drop
  `target_os = "macos"` from the two `create_pipeline_task` cfgs (bevyengine/bevy#26151). Other
  platforms already compile asynchronously. Track the upstream change and delete the vendored crate
  and the `[patch.crates-io]` entry when a Bevy release carries it. Each Bevy upgrade until then
  needs the patch re-applied to the new version.
- **Indirect drawing on Metal.** `NoIndirectDrawing` wins because wgpu's Metal backend issues
  `multi_draw_indirect` as one draw per command and has no indirect count. Implementing it with
  Metal indirect command buffers in wgpu would let tiled GPUs use Bevy's GPU culling and indirect
  path again. This is the largest patch here and should be measured against the CPU path first.
- **Pipeline binary caching on Metal.** wgpu's `PipelineCache` is Vulkan-only. Backing it with
  `MTLBinaryArchive` would make the cold start (now 2.4-3.8 s) behave more like a warm one. It is
  worth doing only if cold starts still matter once the async compilation is upstream.

### Spending the headroom

- **Raise the render scale on larger GPUs.** 0.67-0.75 holds 125-150 fps stationary on an M1 Pro
  (see *Where the time goes now*). Picking the default from the adapter (core count or a frame-time
  probe) instead of a fixed 0.5 would give sharper output where the GPU allows it, without
  penalising base M-series chips.

### Visual trade-offs (not pursued)

Recorded so they are not rediscovered as "free" wins. Each saves GPU or render-thread time but
changes the image, so they come after everything above:

- No shadows from small clutter (shadow casting gated by the same bounds as `VisibilityRange`).
- Reflect only distant LOD terrain and large objects in water.
- Two sun cascades instead of three (0.8 ms in the ablation, measured before the reflection stopped
  building cascades).
