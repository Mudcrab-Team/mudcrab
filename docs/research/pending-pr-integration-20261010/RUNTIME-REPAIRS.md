> Written with an AI assistant (Codex).

# Runtime repairs after Riverwood feedback

The first evaluation exposed missing door data and a pickup path limited to debug objects. Both are repaired at code checkpoint `c27b7234a078f195c1dd5731a17bfe3cf4e7bb19`. Loading still has performance limits; functional checks and timing acceptance are separate.

## Doors and prompts

The converter never populated the optional `door_links` table consumed by #128. Both readers now export it atomically from canonical winning REFR XTEL records, requiring a door destination, a resolvable cell and finite arrival coordinates. Arrival comes from XTEL, not the destination door's own pose. Interior names use canonical CELL FULL text, with an editor-ID fallback. Database-cache identity includes the new projection source.

The existing producer-27 pack was backed up, then annotated under its exclusive asset lock. It contains 2,263 links and seven exterior Riverwood doors, including Sleeping Giant Inn and both Riverwood Trader entrances. Database integrity and the full 76,213-file / 13.7 GB size-and-hash check pass. Meshes, textures, LOD producer identity and world schema were not relabeled. The local annotation receipt is `target/runtime-door-repair-receipt.json`.

An interaction prompt below the crosshair displays `E · Pick up`, `E · Drop`, `E · Enter {interior name}` or `E · Exit to {world}`. The closest reachable action wins; walls, range, cursor release, the console and an active crossing suppress unavailable actions. Holding an object selects Drop. Prompt selection and both E handlers share one target, so one keypress cannot both pick up clutter and start door travel. A vanished target does not activate another door.

## Authored clutter

Pickup now resolves collider children to the authored dynamic-body root, follows its streamed-cell parent coordinates, and wakes it on drop. The existing debug tankard route remains available through T. This adds grab/drop interaction, not inventory persistence or universal physics.

The native stationary probe loaded 39 dynamic clutter bodies and 1,136 authored static colliders. There were 165 partially represented static collisions and 1,064 placements with no authored static collider. A missing collider is counted rather than replaced with an invented proxy. Containers, activators, doors and movable statics are not dynamic-clutter candidates. Bodies start asleep. Controller impulses remain disabled because the upstream Rapier transfer path can panic; walking into an asleep object is not a useful pickup test.

## Loading measurements and repair

The user's original running session was preserved until they closed it. A short live stack sample did not capture a hitch. The controlled probes below use fresh processes, the same repaired pack, Apple M1 Pro Metal, grid (5,-11), default streaming settings, VSync, 60 warmup frames and 15 measured seconds. No adaptive controller is enabled. They are warm filesystem-cache stationary startup probes, not a travel benchmark or a comparison to the user's older version.

Ordinary benchmark mode omits interactive physics and uses a different camera height. The new `--benchmark-world-physics` option includes the actual player/collision/door setup in bounded, visible stationary runs. It rejects headless runs, automated camera routes and fixtures. Profile metadata records whether world physics was active.

Three pre-selector-repair probes loaded all 25 cells and 184 LOD chunks, with no asset, material, transform, terrain, water or streaming invariant failures. They measured 43.8–44.9 FPS, frame p99 50.8–62.7 ms and worst measured frames 71.4–76.1 ms. LOD visibility refresh p95 was 7.32–7.46 ms and its maximum about 8.9 ms. Scene spawning reached about 14 ms, including frames with one large LOD scene and no nearby model backlog; render extract/asset preparation reached about 40 ms. Database query maximum was about 1.4 ms. These stages overlap and nested spans must not be added together.

The selector repair reuses three scratch tables and uses Bevy's integer-key hasher. It retains the existing semantic invalidation, tier decisions, source/batch masks and upload ordering. Ordinary streaming defaults are unchanged. It addresses repeated CPU lookup work; it does not split an indivisible LOD scene spawn or solve render upload work.

Three repaired probes reduced LOD visibility refresh p95 to **1.27–1.35 ms**, an approximately **82%** reduction in that stage. Ready/visible terrain remained 65,224 / 44,644 quadrants, with the same 39 clutter bodies and 1,136 static colliders and no recorded functional failure counts. Whole-frame performance remains mixed: 44.4–45.5 FPS, p99 46.6–60.9 ms and worst 63.7–104.2 ms. World-ready time was 2.31–2.50 seconds after versus 2.09–2.15 seconds before. This is a confirmed CPU-stage improvement, not an accepted overall loading or FPS improvement.

| Probe | FPS | Frame p99 (ms) | Worst measured frame (ms) | LOD refresh p95 (ms) |
| --- | ---: | ---: | ---: | ---: |
| plain-1 | 44.9 | 57.7 | 76.1 | 7.32 |
| before-2 | 43.8 | 50.8 | 73.4 | 7.39 |
| before-3 | 44.3 | 62.7 | 71.4 | 7.46 |
| after-1 | 44.4 | 60.9 | 104.2 | 1.35 |
| after-2 | 45.5 | 46.6 | 63.7 | 1.27 |
| after-3 | 44.8 | 48.1 | 67.0 | 1.30 |

The [comparison receipt](runtime-native-comparison.json) records settings, binary and pack hashes, counts and scope limits. Raw reports and frame traces remain under `target/runtime-physics-*-profile`. The original renderer-only baseline is `target/runtime-plain-before.json`; its different camera/physics path excludes it from this comparison. Probe order was before-1, before-2, after-1, after-2, before-3, after-3.

GPU timestamps were supported by the adapter but unavailable in these reports. Render-thread time and waiting are not proof of GPU execution time. Async decode and disk costs are not fully attributed by these probes.

## Validation and remaining gates

The full engine library suite passed 728 tests with one ignored, followed by a passing shared-prompt door activation/vanished-target regression. The converter library suite passed 554 tests with 18 ignored, followed by a passing in-house reciprocal named-door export regression. The legacy door fixture passed three tests, including winning-link removal and atomic rollback on corrupt canonical data; five in-house world-extras tests also passed. Strict Clippy for engine, converter and launcher across all targets/features, formatting and diff checks pass.

The quick-profile engine, converter, launcher and door annotation helper are built. Unit/fixture evidence covers prompt text, reach/occlusion, compound pickup/drop and transitions. Manual visual prompt placement and retail interior roundtrips remain open. Door timeout/failed-return recovery also needs retail qualification: ending a hold is not proof of collision-ready placement. Sustained travel and the 60 FPS / 16.67 ms performance gate remain open.

Future merges must keep door export, database-cache invalidation and runtime optional-table compatibility together; preserve the shared interaction target when adding E actions; and retain compound-body ownership and parent-space transforms. Terrain batching still retains source assets and generated buffers outside the streaming reservation ledger, so admission modes keep the original terrain representation until those resources are accounted for. Large initial LOD scene instantiation remains a separate cost from later batching.
