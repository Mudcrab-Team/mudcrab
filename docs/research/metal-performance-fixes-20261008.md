# Metal performance fixes — 2026-10-08

This is a historical investigation of a separate integrated development build. It does
not establish performance or coverage acceptance for the diagnostics split or a later
batching candidate. See the [split protocol](terrain-batching-matched-protocol-20261009.md)
and [tool source manifest](../../scripts/profiling/source-manifest.json) for provenance.

The [native profile](metal-function-profiling-20261008.md) led to changes in terrain submission, visibility and component updates. Shadows, reflections, depth prepasses, GPU culling, terrain detail and wgpu validation remain enabled.

## Terrain submission

Converted terrain keeps its original GLB contract. The runtime groups compatible quadrant meshes into at most 2×2-cell tiles within each chunk. It concatenates every vertex attribute without welding or changing values, and offsets the original triangle indices. Material, axis transform, render layers, tags and shadow flags remain separate grouping keys. Unsupported animation, transforms, transparency, picking or other scene behavior retains the original hierarchy.

The existing selector still chooses the tier for each source cell and quadrant. A batch's indices contain exactly those selected triangles; dormant vertices do not add triangles. Selected CPU bounds are explicit and carry `NoAutoAabb`, preventing Bevy's asset refresh from overwriting them.

Mesh generations use immutable asset IDs. The original hierarchy remains drawable until the render world acknowledges preparation of both its retained immutable source meshes and its selected replacement meshes. Later selection changes draw the exact selected source quadrants while new batch uploads wait, then swap them atomically. Superseded requests are canceled. Chunk removal frees generated assets, and shared source assets remain independent of each root.

Preparation and initial activation share a single chunk allowance per frame and consume the streaming budget. Current camera, readiness and removal changes update selection before activation. CPU conversion and upload completion mark submission work separately; they do not rebuild the global selection map. The commit timer ends after selection and activation. The budget stops new work after its elapsed allowance; one whole chunk is still atomic and can exceed that allowance. Profiles record its cost and any budget overrun. A headless run keeps the original terrain path.

Larger batches reduce mesh/entity checks and the fixed indirect records Metal encodes. Their coarser GPU bounds can submit more selected geometry outside a view or behind an occluder. Sources remain resident for exact upload fallback, adding merged vertex storage, index metadata and GPU buffers. Measure memory and GPU work as well as CPU time.

## Visibility and reflection updates

Empty streamed hierarchy nodes use Bevy's change-driven `NoCpuCulling` path while retaining visibility inheritance. Meshes, bounds, visibility classes, lights, probes, cameras, gizmos and picking targets keep ordinary CPU culling. Adding any of those components restores the CPU path before that frame's visibility checks. The plugin owns its marker and leaves pre-existing explicit opt-outs alone.

The reflection camera now writes its transform, activation and exposure only when their values change. Motion, water visibility and exposure changes still update in the same frame; an unchanged view no longer creates component-change work.

## Measurement and validation

[Measurement validity notes](profiling-measurement-validity-20261008.md) describe diagnostic timestamp deduplication, observed physical resolution and GPU timing availability. Optional `MUDCRAB_PROFILE_DRAW_COUNTS=1` instrumentation counts submitted mesh API calls and fixed indirect slots, reports phase coverage and saves completed-render-frame deltas. Its capture is separate from performance comparisons because the counters add overhead.

Moving LOD validation can use the full Riverwood dataset with `--auto-fly-speed 1000 --benchmark-duration 90 --benchmark-warmup-frames 1200` and profile/frame-time output. Auto-flight travels along the initial horizontal forward axis and reverses at four cells from its start: about 65.5 seconds per round trip at that speed. Omit `--acceptance-screenshot`: that option anchors streaming to the start-cell origin, disables origin rebasing and pauses automatic movement until the PNG exists. Moving-run checks should include nonzero origin rebases, terrain batch updates beyond initialization, zero streaming invariant or asset failures, and a drained initial upload queue. Record moving timings separately from the stationary native comparison; the benchmark uses the automated camera and its presentation mode.

Auto-flight turns on frame-sized steps, so separately timed runs can finish at different camera positions. `--shots` supplies exact poses and return views, but its existing settle rule tracks full-cell work without checking LOD requests, chunks or the CPU/GPU batching queues. A `settled` line in `shots.log` therefore does not prove far terrain is complete. Before comparing a final pose with the #200 candidate, independently confirm that `lod/pending_terrain_batch_chunks`, `lod/pending_initial_terrain_upload_chunks` and `lod/pending_terrain_selection_uploads` are all zero and that the initial completion log has been emitted; also check LOD loading, query counts and specialization retries. The selection-upload gauge counts pending immutable batch meshes after initial activation; neither the initial completion log nor zero gauges prove image-frame matching or visible coverage. The seam/handoff fixtures and deferred-upload tests cover exact geometry and selection while these live runs exercise streaming and origin rebasing.

Raw artifacts stay outside `target` under `../mudcrab-profiles/2026-10-08-fixes`. The preserved dashboard keeps the original ordinary-game timeline and adds separate matched captures.

## Initial integrated revision — provisional

The first integrated revision passed 1,121 workspace tests; 18 existing fixture tests were ignored. The eight batching tests passed again after the retained-source GPU guard, and workspace Clippy passed with warnings denied. Release captures used thin LTO and line-table symbols on the Apple M1 Pro, with full graphics and validation enabled.

Three fresh baseline runs and three initial-fix runs used the same Riverwood camera, 3200×1802 physical surface, Immediate presentation, 1,200 warmup frames, 30 measured seconds and all-thread Samply at 1,000 Hz. The earlier baseline-1 capture is retained as a reference and excluded from these medians.

| Metric | Fresh baseline median | Initial fixes median |
| --- | ---: | ---: |
| FPS | 46.91 | 47.68 |
| Main-world span | 5.745 ms | 3.726 ms |
| Waiting for render thread | 15.044 ms | 16.618 ms |
| Render-thread span | 20.854 ms | 20.548 ms |
| Swapchain acquisition span | 9.061 ms | 8.786 ms |
| Render preparation span | 4.204 ms | 4.264 ms |
| Graph and presentation span | 6.218 ms | 6.151 ms |
| Sampled peak process RSS | 2.446 GiB | 2.753 GiB |

The FPS difference is 1.63%; fixed runs range from 47.48 to 52.12 FPS. These few observations do not establish a reliable FPS gain. Most main-world savings become render wait. The spans overlap and must not be added. Process RSS is sampled once per second and includes loading; it is not total Metal allocation.

Samply estimates for exclusive nonblocking endpoint groups show shadow culling falling from 2.352 to 0.346 CPU ms/frame (85.3%), and camera culling from 1.449 to 0.115 (92.1%). Narrow physical functions also improve: AGX render-state emission falls 28.5%, argument dirty-bit extraction 39.1%, Metal indexed-indirect encoding 29.3% inclusive, and wgpu's `DrawBatcher::add` 41.3% self. Inclusive paths overlap. These are sampled CPU interval attributions, not wall-clock frame costs or GPU activity. The broader driver and validation groups retain per-pass compute, blit, buffer and resource-tracking work.

All static runs retain 44,644 selected LOD quadrants and 2,857,216 selected LOD triangles. The initial fixes use 4,032 resident batches, 2,952 selected batches and 7,905 mesh entities, versus 67,049 mesh entities before. Total entities fall from 176,228 to 21,969. All 176 chunks prepare and activate without unsupported-scene fallback, and both CPU and initial GPU queues drain. Verified scene screenshots are saved beside the reports.

The separate draw-count capture contains 470 complete frames in a final ten-second draw-clock interval. Each frame issues 335 fixed indexed mesh API calls covering 2,867 argument slots. All eleven material mesh variants are wrapped; three transparent gizmo variants remain outside mesh coverage. Slots include zero-instance records. The historical 48,909 allocated records are not an issued-call baseline and cannot support a reduction percentage.

Normal 16 MiB and constrained 1 MiB upload-budget moving runs each record 120 measured seconds, 61 origin rebases and 305 cell unloads. Both have zero streaming invariant, asset, material and renderer failures. The normal run ends with 45 ordinary asset instances still loading; the constrained run ends with none. Neither is a fully settled static acceptance run. No commit-budget violations occur, but this revision's selector runs after the commit timer, so that result does not bound complete transition cost.

The initial revision rebuilds LOD selection during representation changes, adding about five seconds of selector work over loading and warmup. The final correction separates submission handoffs from changes to camera, readiness or terrain coverage. It rechecks current selections before retiring the source hierarchy and suppresses only the exact removal events generated by a successful internal transfer.

Native CPU comparison ranges end at the last periodic drawable proof with zero measured missing frames. Several final summaries include one missing drawable during window shutdown; those unproven tails are excluded from CPU attribution. The 30-second main-world benchmark reports remain preserved separately.

Metal HUD encoder timing was launched in a separate observation with only the app's render diagnostics omitted, as Apple requires for counter-buffer compatibility. macOS denied menu inspection and screen capture, so no per-encoder report was extracted. Full Xcode is unavailable on this machine. The data therefore does not identify active shader time or distinguish GPU completion from compositor release as the cause of each drawable wait.

## Second integrated revision — provisional

The second revision passes 1,125 workspace tests, with 18 existing ignored tests, and workspace Clippy with warnings denied. Formatting and whitespace checks pass. Four additional terrain tests exercise current-mask activation with delayed acknowledgments, internal removal filtering and upload-only handoffs. Native release validation uses a new pinned binary and is kept separate from the initial revision. The moving captures exposed further allocation work, addressed by the third revision below.

## Third revision — quarantined

**V3 is quarantined after visual QA failed.** Static run 1, static run 4 and the production smoke PNG show substantial terrain holes and floating buildings; static run 2 shows full ground. Automated tests, identical selected-quadrant counts and CPU geometry checks missed intermittent failures in actual rendering. The timings below remain raw diagnostic evidence. V3 performance and quality preservation are not accepted; investigation and replacement captures are required before it can be presented as a completed fix.

Further isolation also reproduces holes with V2 batching plus the tier bitmask, while a serial rerun of the frozen baseline shows full ground. All integrated timing comparisons remain provisional until this rendering issue is resolved; earlier full-looking images do not establish that the current integration preserves coverage reliably.

Tier readiness uses a three-bit set in place of per-quadrant heap-allocated sets. Changing a batch selection to empty cancels its upload and hides its retained immutable mesh; it does not clone vertex attributes or enqueue an empty replacement. Initial preparation still builds full-vertex dormant meshes for later use. Returning to a nonempty selection still creates a new immutable generation and draws exact source quadrants until preparation is acknowledged.

The final source passes **1,130 workspace tests**, with 18 existing ignores, plus workspace Clippy with warnings denied, formatting and whitespace checks. The fifteen terrain-batching tests include exact attributes/indices, upload delays, stale generations, empty transitions, origin shifts and cleanup. The tier tests compare all eight subsets against the prior set-based selector, including reach boundaries and rebased negative coordinates.

Five fresh baseline captures (2–6) and three completed final captures (1, 2 and 4) use the same fixed Riverwood scene and all-thread Samply settings. Baseline 6 is interleaved with final captures. Final capture 3 was interrupted by macOS monitor removal and window closure before benchmark completion; its raw evidence is retained and excluded. The restarted capture uses the same 3200×1802 Immediate surface.

| Fixed-scene metric | Baseline median, n=5 | Final median, n=3 |
| --- | ---: | ---: |
| FPS | 47.05 | 47.83 |
| FPS range | 46.90–47.60 | 47.20–49.02 |
| Mean frame time | 21.252 ms | 20.908 ms |
| p95 frame time | 22.814 ms | 22.215 ms |
| Main-world span | 5.789 ms | 3.775 ms |
| Waiting for render thread | 14.951 ms | 16.469 ms |
| Render-thread span | 20.801 ms | 20.498 ms |
| Swapchain acquisition span | 9.061 ms | 9.041 ms |
| Render preparation span | 4.204 ms | 4.102 ms |
| Graph and presentation span | 6.218 ms | 5.826 ms |
| Sampled peak process RSS | 2.446 GiB | 2.752 GiB |

The median FPS difference is 1.65%, with overlapping ranges. These captures do not establish a reliable fixed-scene FPS gain. Main-world savings mostly become render wait. The stage spans overlap and must not be added. Peak RSS includes loading and warmup, is sampled once per second, and is not Metal allocated memory. Its median increase is about 0.31 GiB (12.5%).

Exclusive nonblocking native endpoint groups fall from 2.374 to 0.368 CPU ms/frame for directional-shadow culling (84.5%), and 1.436 to 0.105 for camera culling (92.7%). Total recorded process CPU averages 1.402 core equivalents before and 1.115 after. The driver and validation groups improve less: 1.038→0.943 and 0.327→0.301 CPU ms/frame. These are sampled interval attributions across threads, not wall-clock frame costs. CPU intervals ending at recognized parked functions remain separate; they are not time executing wait syscalls.

All three V3 static scenes report 44,644 selected LOD quadrants and 2,857,216 selected triangles. They have 4,032 resident terrain batches, 2,952 selected batches, 7,905 mesh entities and 21,969 total entities. All 176 chunks prepare and activate; both batching queues drain with zero source, asset, material, streaming or renderer failures. Geometry fixtures preserve original CPU vertex values, selected indices, materials, transforms and render layers. Those counters and fixtures do not prove submitted GPU geometry or complete image coverage: saved images exposed missing terrain despite the matching metrics.

The ordinary-game result is a separate observation: **54.17→60.78 presented FPS**, or 18.459→16.453 ms mean interval, on matching 3200×1800 Fifo surfaces. The second revision also observed 60.82 FPS. These are one baseline and one final capture, with a separate intermediate observation, rather than a repeated controlled ordinary-play experiment. p95 and p99 remain 25 ms. Validated HUD doublets preserve equal observations: the final analysis has 3,429 observations, phase 1, one leading boundary trim and no trailing trim or interior drops. Its command-buffer GPU span averages 14.841 ms versus 16.370 ms before; that span can include idle and overlap, so it does not identify active GPU savings.

The ordinary camera is lower and uses the normal player path; the fixed benchmark uses a raised camera with player physics disabled and Immediate presentation. Native ordinary profiles show lower culling, indirect validation and driver work, consistent with its faster presentation. Neither view's result establishes the other view's critical path or identifies shader costs. CPU selection starts at logged render frame 1,200 plus two seconds; HUD selection starts at capture time plus thirty seconds. Both end at the last healthy periodic drawable proof, and their starts differ.

Both final moving captures cover 120 measured seconds, 60 origin rebases and 300 cell unloads. They retain shadows, reflections, detail, GPU culling and validation. The normal 16 MiB run passes its recorded acceptance gates, with zero streaming-budget violations and a maximum complete streaming commit of 13.047 ms. It ends with 38 ordinary asset instances pending and both terrain batching queues empty.

The constrained 1 MiB run **fails its acceptance gate**: three startup commits take 20.631, 22.746 and 21.116 ms. They occur at frames 225–227 before the measured interval; there are zero measured-window budget violations. It ends with seven initial terrain uploads and 158 ordinary asset instances pending. Source terrain remains available during those waits. Both runs record zero source, renderer, material, asset and streaming-invariant failures. They are moving lifecycle tests, not fully settled static comparisons. Their relaxed controller FPS, p95 and memory gates are smoke thresholds; passing them is not a 60 FPS acceptance claim.

The stress bursts validate and mark 10, 11 and 9 LOD chunks ready in consecutive frames. `track_lod_readiness` validates pending scenes and queues per-quadrant coverage/readiness changes in one pass, followed by true selection. This arrival burst remains a startup cost. The recorded ready-marker gaps and whole-record inner span maxima cannot be added or assigned as exclusive costs to those exact frames. A future budget change must preserve coverage while pacing that validation and its deferred ECS work.

V2 and V3 use the same expanded streaming timer. Whole-record static selection mean per refresh falls about 28.6%; static mask changes are zero, supporting cheaper tier bookkeeping. Moving mask choices remain about 11,000 per capture while per-refresh costs fall. The logical update counter includes empty transitions; it is not a mesh-generation counter. No separated generation span proves how much of the remaining gain belongs to skipped empty uploads. LOD summary spans lack per-sample timestamps, so loading and measured LOD totals cannot be split retrospectively.

The final draw-count observation crops an explicit ten-second interval on the render recorder's clock, excluding its final second. All 470 complete schedules issue **334 fixed indexed mesh API calls** covering **2,907 argument slots per frame**, with no skips, failures, direct calls, dynamic ranges or dropped frames. All eleven material mesh variants are wrapped; the three transparent gizmo variants remain outside that coverage. Counts describe API submissions and slots, including zero-instance records. They do not measure visible draws or shader work, and the historical allocated-record count remains unsuitable as an issued baseline.

The remaining fixed-scene path still spends about nine milliseconds acquiring a drawable. Native worker waits and render-world handoff waits overlap. Metal must encode each fixed indirect slot when GPU-counted multi-draw is unavailable, and driver resource tracking, compute/blit setup and per-pass validation remain after mesh reduction. The current evidence cannot distinguish GPU completion from compositor release for each drawable wait. Full Xcode's Metal timeline and a valid encoder report are still needed to attribute active GPU work.

## Final production results: late mesh preparation

V4 adds a targeted retry for meshes whose render descriptor arrives after their first specialization attempt. The final source passes **1,141 workspace tests**, with zero failures and 18 existing ignores across 48 summary targets. Workspace Clippy with warnings denied, formatting and whitespace checks pass. The production binary `local bin/engine-production-v4` (retained in the external archive) uses line-table debug information, thin LTO and one codegen unit; its SHA-256 is `298342a422c9b43907b6f78125baa9f59ab6b2011a8486137ffb33bb0b93816e`. Guarded static and ordinary analyses, moving lifecycle checks and bounded submission counts are complete, with their visual-validation scopes kept separate. The [production build receipt](evidence/metal-20261008/production-binary.json) records the archived binary; the binary itself is not published.

The earlier accepted retry proof image (`runs/terrain-isolation-preparation-retry-2/frame.png`, retained in the external archive) shows intact ground. Its native log (`runs/terrain-isolation-preparation-retry-2/engine.stderr.log`, retained in the external archive) records 106 delayed meshes tracked and resumed, a peak of 74 pending, zero final pending and zero cancellations. This quick proof used the first retry binary, before the late shadow-view observer; final static captures use the reviewed observer.

Rendered images exposed a gap that CPU fixtures and coverage counters missed. The failing stationary captures still selected 44,644 quadrants and 2,857,216 triangles, with no recorded batch selection updates. The render-world audit log (`runs/terrain-isolation-residency-audit-1/engine.stderr.log`, retained in the external archive) reports zero cached input-uniform mismatches among 3,933 resident instances at render frame 600: vertex offsets, index offsets and counts matched their resident allocator ranges despite holes in the saved image. The [audit implementation](../../crates/engine/src/mesh_residency_audit.rs#L135) checks those fields, not phase membership or pixels. Correct mesh metadata therefore did not prove that every selected terrain surface reached a render pass.

Bevy 0.19 retains missing materials and mesh instances in pending queues, but a missing `RenderMesh` exits specialization without adding a retry in the [main pass](https://github.com/bevyengine/bevy/blob/v0.19.0/crates/bevy_pbr/src/material.rs#L1020), [prepass](https://github.com/bevyengine/bevy/blob/v0.19.0/crates/bevy_pbr/src/prepass/mod.rs#L1011) and [shadow pass](https://github.com/bevyengine/bevy/blob/v0.19.0/crates/bevy_pbr/src/render/light.rs#L2408). Specialization normally revisits changed, newly visible or already pending entities. A descriptor arriving later can leave an unchanged entity outside those queues. The [isolation record](evidence/metal-20261008/terrain-visual-isolation.json) shows that allocator-aware upload acknowledgment alone still left holes, while a one-time mesh change restored full ground. Those observations support testing a missing-specialization retry; they do not by themselves prove every remaining rendering path is correct.

The [project retry plugin](https://github.com/Mudcrab-Team/mudcrab/blob/0650c70987cb4056eb3f08525ab9329f2e8cf179/crates/engine/src/mesh_preparation_retry.rs#L35) tracks only relevant change, visibility and pending-queue candidates that lack a descriptor. It retains the exact entity and mesh asset generations, without keeping asset handles alive. Once the descriptor and its required vertex and index ranges are resident, it marks that entity for Bevy's ordinary specialization and extraction paths. Replacement or unloaded generations are canceled. Shadow view changes can arise during specialization, so a second observation pass records those late candidates for the next frame's retry. Steady meshes do not receive repeated component changes or global re-specialization.

The [terrain upload bridge](https://github.com/Mudcrab-Team/mudcrab/blob/0650c70987cb4056eb3f08525ab9329f2e8cf179/crates/engine/src/terrain_upload.rs#L130) also requires more than descriptor existence. Both retained source meshes and immutable replacement meshes need resident vertex ranges large enough for the descriptor's vertex count, plus resident index ranges covering every nonempty indexed mesh. An empty selection still needs its vertex allocation but requires no index elements. This acknowledgment proves preparation and submission buffer availability after mesh preparation; it does not fence GPU execution, prove phase membership or establish complete image coverage.

The V3 dormant-empty optimization is withdrawn from the final production revision. That branch never ran in the failing stationary captures, and rolling batching back to its V2 behavior still reproduced holes. Withdrawal reduces the final change set; it is not evidence that empty transitions caused the regression. The tier bitmask retains the same tier and coverage decisions.

The [guarded V4 result archive](evidence/metal-20261008/final-results-v4.json) compares one fresh baseline capture, `fixes-baseline-7`, with three V4 captures. All four native Metal surface logs show 3200×1802 Immediate at the same Riverwood camera. Earlier captures are kept separately because display topology may have changed; their five-run baseline cohort is not pooled into this comparison. Three final manual PNG reviews show full ground. Each final capture also has a healthy periodic drawable proof with zero measured misses, all 176 LOD chunks prepared and GPU-ready, 44,644 selected quadrants, 2,857,216 selected triangles and 100 near terrain patches. All final queues drain and recorded renderer, asset and streaming errors remain zero. The image reviews establish saved-frame terrain coverage separately from those counters.

The published [baseline PNG](evidence/metal-20261008/before.png) and [V4 run 1 PNG](evidence/metal-20261008/after.png) are the original saved frames from these stationary captures.

| Fixed-scene metric | Fresh baseline, n=1 | V4 median, n=3 |
| --- | ---: | ---: |
| FPS | 47.315 | 47.453 |
| FPS run range | 47.315 | 47.125–47.839 |
| Mean frame interval | 21.135 ms | 21.074 ms |
| p95 frame interval | 22.867 ms | 22.810 ms |
| p99 frame interval | 24.258 ms | 24.318 ms |
| Main-world mean span | 5.778 ms | 3.767 ms |
| Main waiting for render thread | 14.855 ms | 16.848 ms |
| Render-thread mean span | 20.666 ms | 20.635 ms |
| Render Prepare mean span | 4.185 ms | 4.321 ms |
| Render graph / presentation mean span | 6.241 ms | 6.270 ms |
| Swapchain acquire mean span | 8.867 ms | 8.660 ms |

The observed FPS difference is **+0.29%**, within the final run range; one fresh baseline and three final runs do not establish a reliable gain or statistical equality. Main-world work falls about two milliseconds while the main thread spends almost two milliseconds longer waiting for rendering. The render thread remains nearly unchanged. These overlapping elapsed spans support a render-limited fixed view; they cannot be added or used to identify active GPU work.

Guarded native CPU crops last 23.37 seconds before and 23.09–23.49 seconds after. They begin after warmup plus two seconds and end at the last healthy periodic drawable proof, excluding the unproven tail and teardown. Total sampled process CPU falls from 1.411 to a median 1.177 core equivalents, **−16.6%**. Directional-shadow visibility falls from 112.258 to 18.344 CPU ms/s (**−83.7%**), and camera visibility from 67.969 to 5.935 CPU ms/s (**−91.3%**). The sampled Metal-driver group stays near 47.786→47.494 CPU ms/s, and indirect draw validation near 16.080→16.421 CPU ms/s. These are exclusive endpoint estimates across threads, with recognized waits reported separately; they are not syscall execution times. Per-frame CPU values use the full 30-second benchmark FPS denominator for the smaller crop and remain approximate.

The [ordinary-game comparison](evidence/metal-20261008/ordinary-v4-comparison.json) uses the fresh current-display capture `fixes-ordinary-baseline-2` and `fixes-ordinary-final-v4-1`, separately from historical Fifo observations. Both actual Metal surfaces are 3200×1800 Fifo. Mean presented HUD interval changes from **20.641 to 16.675 ms**, or **48.448 to 59.971 FPS**, an observed **+23.78%**. There is one capture per side; this difference does not establish repeatability or a reliable gain. p95 and p99 change from 25.000 to 16.670 ms. Contiguous-doublet validation retains 2,788 baseline and 3,460 final observations across 114 complete HUD batches each, with boundary-only trimming and no interior drops. Native CPU uses the same 57.60-second baseline and 58.07-second final HUD clocks, reporting 1.936→1.629 core equivalents (**−15.86%**). Mean HUD command-buffer GPU duration changes from 17.722 to 15.859 ms; those elapsed timestamps can include idle and overlap and do not measure active GPU work or utilization.

In those ordinary CPU windows, directional-shadow visibility changes from 116.063 to 25.784 CPU ms/s, camera visibility from 90.253 to 8.016, Metal-driver endpoints from 195.893 to 68.057, indirect validation from 63.309 to 26.778 and other render encoding from 77.896 to 65.462. These sampled endpoint groups are separate from recognized waits. Ordinary play uses the normal player camera and Fifo path; its observed driver reduction does not establish the same reduction for the raised Immediate benchmark, whose driver group remains nearly unchanged.

Ordinary quality settings are supported by the actual surface DEBUG log, pinned binary hash, reviewed source and owner build receipt, plus the default production CLI. The old environment's ignored AlwaysOnTop control does not establish effective window state. The [final ordinary manifest](evidence/metal-20261008/runs/fixes-ordinary-final-v4-1/run.json) preserves that provenance; binary/source identity was not independently proven. Loading queues are unobserved, so the capture-plus-thirty-second cutoff does not prove loading complete. There is no matched ordinary PNG comparison. Validated HUD observations, renderer counters and enabled settings do not prove complete ordinary-scene terrain coverage; the three accepted PNG reviews above apply to the stationary benchmark.

Full-capture sampled peak RSS rises from 2.428 GiB to a median **2.770 GiB**, a final range of 2.764–2.787 GiB and an observed increase of **14.1%**. Those once-per-second samples include loading and warmup and can miss peaks. They measure process residency, not Metal allocation size; the engine peak-memory field is unavailable on macOS. Final retry logs track and resume 169, 128 and 127 delayed meshes, with zero pending before warmup completes. Whole-record retry and loading counters are not measured-window function costs.

The [final native summary](evidence/metal-20261008/final-native-summary.json) records the 16 MiB moving run separately, without a matched before comparator. Its 120-second benchmark reports 70.292 FPS, p95 20.685 ms and p99 23.964 ms. Whole-record lifecycle checks cover 60 origin rebases, 300 cell unloads and 157,056 despawned entities, with zero commit overruns and a maximum complete commit of 12.894 ms. Renderer, asset and streaming failure counters remain zero. All terrain upload, chunk and surface queues end empty, while six model instances and an arming depth of two remain. The retry resumes 101 meshes with zero final pending or canceled. Sampled peak RSS is 3.226 GiB. These endpoint and whole-record observations do not describe a fully settled scene.

Guarded moving CPU covers 117.286 seconds and reports 1.944 core equivalents. It ends at healthy periodic render frame 9,600, `2026-10-09T02:18:30.177914Z`, with zero cumulative and measured drawable misses. The final Drop reports one teardown miss after benchmark completion; that raw evidence is retained and excluded from the guarded crop. Moving quality remains counter-only: there is no moving PNG or route image oracle. Permissive smoke thresholds and this changing workload do not establish a target-FPS gate or a stationary speedup.

The separate final draw-count capture covers 468 completed render schedules, frames 1,153–1,620, in the explicit draw-clock interval **24.100423208–34.100423208 seconds**. That ten-second crop includes roughly one second of the warmup tail and is not a benchmark-only interval. Every selected schedule issues **327 covered fixed indexed mesh API calls**, covering **2,851 indexed argument slots**, with zero skips, failures, frame gaps or dropped samples. These replace the quarantined V3 observation of 334 calls and 2,907 slots for the final revision; the historical observation remains separate.

| Covered mesh phase | API calls per schedule | Indexed slots per schedule |
| --- | ---: | ---: |
| Opaque | 171 | 565 |
| Alpha mask | 36 | 259 |
| Transparent | 20 | 22 |
| Opaque prepass | 16 | 1,062 |
| Alpha-mask prepass | 70 | 502 |
| Shadow | 14 | 441 |

All eleven expected standard and extended mesh variants are wrapped. The transparent registry also contains three unwrapped gizmo variants whose actual submissions are unknown, so whole-registry coverage is incomplete; non-mesh render nodes are outside this instrumentation. Covered direct, nonindexed, dynamic and unclassified submissions are zero. Argument slots include zero-instance records and do not count GPU-visible draws. Historical allocated indirect records are not an issued-slot baseline. Draw instrumentation adds overhead, so its FPS is excluded from matched timing evidence. Its manually reviewed static PNG shows intact ground at that view and does not validate the moving route.

Selected masks, triangle totals, prepared buffer ranges and retry counters remain useful diagnostics, but none substitutes for visible ground, buildings, seams and shadows. Ordinary and moving evidence retains its image-validation limits rather than inheriting the stationary PNG result. Earlier integrated timings remain provisional; V3 stays quarantined.
