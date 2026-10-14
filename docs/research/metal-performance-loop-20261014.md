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
