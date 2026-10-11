# Apple Metal: terrain workload, draw encoding and frame waits

This is a historical investigation of a separate integrated development build. It does
not establish performance or coverage acceptance for the diagnostics split or a later
batching candidate. See the [split protocol](terrain-batching-matched-protocol-20261009.md)
and [tool source manifest](../../scripts/profiling/source-manifest.json) for provenance.

The 2026-10-08 Riverwood investigation found a concrete workload behind the CPU hotspots: tens of
thousands of tiny, distinct terrain LOD meshes are scanned for visibility; queued mesh bins produce
indirect records that Metal encodes individually. Drawable acquisition also delays the render schedule
and the main thread. This explains mechanisms present in the slow frame; it does not establish
the FPS gain of an unimplemented optimization or prove that GPU execution is the sole limit.

The [native profiling guide](../contributing/native-profiling.md) documents Samply 0.13.1, Firefox
Profiler, release symbols, Metal HUD and interpretation. This note adds source-level findings.

## Captures and scope

The source baseline was `f40ca1df770eab4ace83b335b6b38168bd83af60`, with local terrain-sampler,
asset-root and converter-fixture fixes. Dependencies were Bevy 0.19.0 and wgpu 29.0.4. Hardware was
Apple M1 Pro, 10 CPU/16 GPU cores, 32 GiB, macOS 26.6.2, on AC power.

The optimized release CPU capture sampled all threads at 1,000 Hz. Ordinary play rendered worldspace
`0x3c`, grid `(5,-12)`, radius 2, at 3200×1800 with FIFO presentation and normal shadows, reflection
and LOD. Its stationary, default NOCLIP scene averaged 49.56 presented FPS; the steady CPU crop was
58.509 seconds, consuming 109.681 CPU seconds. Walking and input workloads were not exercised.
The separate fixed-camera benchmark used Immediate presentation, an elevated camera and disabled
player physics; it is inventory evidence, not a controlled comparison with ordinary play.

An additional ordinary-scene observation logged workload counts at main/render frame 1500,
2026-10-08 19:43:12 UTC. That probe used `quick` solely to count objects and allocated records;
its FPS was not compared with release. Graphics behavior was unchanged. Both cameras were active,
with 3200×1800 main and 1024×576 reflection targets, and one sun with four cascades.

| Frame-1500 observation | Count |
|---|---:|
| `Mesh3d` entities | 67,049 |
| Distinct mesh handles referenced by those entities | 64,037 |
| Inherited-visible meshes | 48,517 |
| Visibility/transform query population | 175,466 |
| Indexed records, `Opaque3d` | 23,670 |
| Indexed records, `Opaque3dPrepass` | 23,670 |
| Indexed records, `AlphaMask3d` + `AlphaMask3dPrepass` | 336 |
| Indexed records, `Shadow` | 1,224 |
| Indexed records, `Transparent3d` | 9 |
| Total allocated indexed records | 48,909 |

The query population counts entities with `InheritedVisibility`, `ViewVisibility` and
`GlobalTransform`; Bevy's complete query has exclusion filters. Main and render observations have
separate counters in a pipelined renderer. Allocated records are not native draw calls: ranges can
be unused or visited by more than one pass. The adapter reported
`MULTI_DRAW_INDIRECT_COUNT=false`. Do not treat the saved renderer's 264 batch sets as 264 draws;
those renderer-proof fields are cumulative maxima.

Raw profiles, symbol maps, logs, the FPS dashboard, probe source/binary and `workload.json` are retained
in the local external bundle `../mudcrab-profiles/2026-10-08-riverwood/`. They are not repository
fixtures. Temporary instrumentation was removed and the preexisting source hashes/diff and quick
binary were restored.

## Why camera and shadow visibility consume CPU

The [terrain LOD converter](../../crates/converter/src/lod/terrain.rs) emits a distinct glTF mesh
primitive per source-cell quadrant at every tier. Each has just
[65 vertices and 64 triangles](../../crates/shared/src/lod.rs). The saved benchmark retained 63,176
ready quadrants and selected 44,644 by distance/coverage. Selection sets inherited visibility;
it does not perform camera-frustum culling.

Each cell/tier record also creates parent cell, terrain-group and quadrant nodes, plus primitive
children. Bevy gives glTF nodes visibility components. Thus camera queries traverse a much larger
population than renderable meshes: the probe found 175,466 visibility/transform entities versus
67,049 meshes. The distinct ready quadrants correspond to 15,794 cell/tier records and at least
157,940 hierarchy/primitive entities before chunk roots.

Bevy's `check_visibility_cpu_culling` scans a broad visibility/transform query for every active view,
including non-renderable nodes. It rejects hidden/layer/range mismatches, tests bounded entities,
and rebuilds visible lists. With two active views the broad traversal is repeated. Its sampled
nonblocking endpoint group costs 95.42 CPU ms/s, approximately 1.93 summed CPU ms/frame; query
iteration and its closure account for about 77% of that group. This is substantial traversal work,
not just frustum-plane arithmetic.

`check_dir_light_mesh_visibility` scans candidate meshes per view and tests eligible bounded meshes
against each cascade. The probe counted 48,516 inherited-visible meshes without `NotShadowCaster`
or `NoCpuCulling`. Two views and four cascades expose a large potential test population; layer/range
rejection and other branches prevent treating that product as an actual test count. The shadow
group costs 120.25 CPU ms/s, approximately 2.43 summed CPU ms/frame. Its `Frustum::intersects_obb`
clone at RVA `0xb8d9cc` accounts for about 60% of the group. That function transforms the box center
and computes projected radii against frustum planes. The same-named camera clone at `0x3edf5c`
is a separate physical function and must not be merged into the shadow cost.

The project's [LOD tier selection](../../crates/engine/src/streaming/lod.rs) already skips unchanged
grid/readiness states; its steady interactive CPU attribution was only 1.608 ms across the crop.
The expensive repetition occurs downstream in Bevy. Bevy rebuilds directional cascades and frusta
each frame even though the project sun has a fixed pose. The
[reflection update](../../crates/engine/src/render.rs) writes its transform each frame, so change
flags alone may invalidate an otherwise reusable result.

The relevant Bevy 0.19 sources are `bevy_camera/src/visibility/mod.rs:748`,
`bevy_camera/src/primitives.rs:271`, `bevy_light/src/lib.rs:336`, `bevy_light/src/cascade.rs:195`, and
`bevy_light/src/directional_light.rs:217`. They are available in Cargo's downloaded crate sources.

## Why batching still reaches expensive Metal state functions

Bevy groups meshes into batch sets by pipeline, material bind group and allocator slabs, but mesh
asset identity separates bins within those sets. Distinct terrain patch handles therefore cannot
become one instanced draw merely because they share a material. GPU preprocessing reserves an
indirect record for each bin.

In wgpu 29.0.4's Metal backend, `draw_indexed_indirect` loops over `draw_count` and issues one native
`drawIndexedPrimitives:…indirectBuffer:…` call per record. There is no implemented count-buffer
multi-draw path on this backend. Bevy's fixed-slot fallback can write `instance_count=0` for a
GPU-rejected bin, but the CPU still validates and encodes that record. GPU culling saves geometry
work without necessarily removing CPU draw submission.

The saved stacks independently corroborate this mechanism: 99.85% of self CPU attributed to AGX
`encodeAndEmitRenderState`, and 99.95% attributed to `extractProgramVariantArgumentDirtyBits`, has
indexed-indirect draw ancestry. These routines cost 77.72 and 31.89 CPU ms/s respectively. They are
driver CPU work while encoding draws; their names do not establish shader execution, descriptor
reallocation or pipeline compilation. Bind-group tracking suppresses unchanged bindings, and its
sampled cost is much smaller than native indirect-draw encoding.

`DrawBatcher::add` is wgpu's indirect validation bookkeeping. Each pass constructs a new batcher,
allocates destination ranges, builds per-record metadata and looks up validation batches. Finishing
the pass writes staging metadata, copies it, dispatches validation compute work and transitions
validated arguments back to indirect use. Buffers can be pooled while metadata and dispatches are
still rebuilt each pass/frame. Its self CPU is 26.41 CPU ms/s; native CPU sampling does not time the
validation shader's GPU work.

wgpu defers native encoding until `CommandEncoder::finish` calls `encode_commands`. The stacks place
that work under Bevy's `RenderContextState::queue`/deferred command processing. Consequently, a tiny
Bevy CPU pass-recording span does not mean the later Metal encoding is cheap. `encode_commands`
receives 354.18 inclusive CPU ms/s, approximately 7.15 summed CPU ms/frame, including descendants.
It is not an additional cost to add to the driver and validation rows.

Source anchors: `bevy_pbr/src/material.rs:1267`,
`bevy_render/src/batching/gpu_preprocessing.rs:2237`,
`bevy_pbr/src/render/build_indirect_params.wgsl:68`,
`wgpu-hal/src/metal/command.rs:1574`, `wgpu-core/src/indirect_validation/draw.rs:977`, and
`wgpu-core/src/command/mod.rs:1225`. These identify mechanisms, not sampled line costs.

## Why the main thread waits

Three exact serialized sampling timestamps show a worker in `nextDrawable`, the render thread
waiting for scheduled work, and the main thread waiting for the returned render world:
18:46:08.601507, 18:46:08.685673 and 18:46:09.210242 UTC. The dependency is
`prepare_windows → Metal acquire_texture/nextDrawable → render schedule completion → renderer_extract`.
The channel exposes that delay; it is not evidence that the channel implementation is slow.

Workers have 7.904 seconds of zero-reported-CPU drawable-acquisition residence in the 58.509-second
crop; the main render-world handover has 25.693 seconds. The main wait also covers CPU encoding and
other renderer work. These overlapping durations are not additive. Normal Metal submit commits
asynchronously rather than waiting synchronously for GPU completion.

FIFO enables display synchronization; maximum frame latency 2 creates a three-drawable pool in this
wgpu backend. `nextDrawable` waits for an available pool entry, which can depend on GPU progress
and Core Animation/presentation scheduling. The HUD's 16.67/25 ms present cadence and mean 17.62 ms
command-buffer GPU span support rendering/presentation pressure but cannot identify which event
released each drawable. See [Apple's drawable contract](https://developer.apple.com/documentation/quartzcore/cametallayer/nextdrawable())
and [HUD metric definitions](https://developer.apple.com/documentation/xcode/understanding-metal-performance-hud-metrics).

## Measurements that can test output-preserving changes

| Candidate | Mechanism it would address | Evidence needed before claiming a gain |
|---|---|---|
| Conservative chunk rejection before quadrant culling | Repeated broad traversal and plane tests | Per-view/cascade rows and rejection counters; exact visible/caster membership comparison |
| Flatten redundant terrain hierarchy nodes | Camera queries scanning non-renderable nodes | Query rows by renderability; preserved transforms, hierarchy visibility and LOD handoff |
| Cache unchanged visibility/cascade results | Rebuilding memberships for unchanged geometry and views | Actual matrix/state changes; correct replay after per-frame visibility reset |
| Group terrain geometry or use a shared patch representation | Distinct mesh bins and per-record native draws | Issued records/state changes per pass; unchanged geometry/materials and quadrant coverage |
| Optimize bounds reuse or axis-aligned terrain tests | Repeated center transforms and projected radii | Box/plane counters; identical near/far and boundary classifications |

Visibility caches must invalidate on camera activation and camera/projection/light/cascade matrices,
transforms, bounds, layers, visibility/ranges/classes, culling and shadow-caster markers, shadow
enablement, readiness and origin rebases. Skipping a culling system without replaying its
results conflicts with Bevy's per-frame visibility reset. Grouping meshes can reduce culling
granularity or increase overdraw; that tradeoff needs measurement even when rendered detail stays
the same. Disabling shadows, reflections, culling or validation is not the experiment proposed here.

Count issued native indirect records and zero-instance records per pass to distinguish allocation
from execution. Correlate CPU submission, GPU start/end, acquisition and presentation timestamps
to determine whether a frame waits for CPU encoding, GPU work or display release. A Metal System
Trace provides the full timeline. Without full Xcode, the Metal HUD encoder report can supply
per-encoder evidence, provided its timing mode does not conflict with the app's counter buffers.
Those measurements would quantify recoverable frame time; the current captures establish workload
and call-path mechanisms rather than an optimization's FPS outcome.

The subsequent [fix series and repeated validation](metal-performance-fixes-20261008.md) implements terrain
batching, empty-hierarchy culling hints and unchanged reflection-camera updates. It records timing
outcomes separately from the historical capture above. [Measurement validity](profiling-measurement-validity-20261008.md)
explains the new issued-draw counters and GPU diagnostic availability.
