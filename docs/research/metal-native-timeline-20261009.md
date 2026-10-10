# Native Metal GPU and presentation investigation

This is a historical investigation of a separate integrated development build. It does
not establish performance or coverage acceptance for the diagnostics split or a later
batching candidate. See the [split protocol](terrain-batching-matched-protocol-20261009.md)
and [tool source manifest](../../scripts/profiling/source-manifest.json) for provenance.

The largest measured GPU stage in the fixed Riverwood scene is opaque fragment
shading. The primary opaque encoder in the saved last-frame snapshot took
13.48 ms: 12.57 ms fragment work and 0.91 ms vertex work. This narrows the GPU
investigation, but does not yet identify the responsible material or shader
function within that encoder.

This follows the [CPU profiling and fixes](metal-performance-fixes-20261008.md).
Measurements use the current built-in display, Apple M1 Pro, macOS 26.6.2,
Bevy 0.19.0 and wgpu 29.0.4. The benchmark renders Riverwood at worldspace
`0x3c`, grid `(5, -12)`, streaming radius 2, fixed camera offset
`0,6000,8000`, and a measured physical surface of 3200 × 1802. Presentation is
Immediate. Shadows, reflection, terrain layers, normal maps, LOD and 16×
anisotropy remain enabled. Each accepted run warmed up for 1,200 frames and
measured 20 seconds using pinned optimized executables. Builds and timing runs
from this investigation were sequential; unrelated work on the machine must
also be checked before accepting a comparison.

## Recovering native GPU encoder measurements

The previous all-zero Bevy GPU pass values have a source-level explanation.
[Bevy deliberately omits command-encoder timestamp writes on macOS](https://github.com/bevyengine/bevy/blob/v0.19.0/crates/bevy_render/src/diagnostic/internal.rs#L749-L758)
to avoid [Tahoe flickering](https://github.com/bevyengine/bevy/issues/22257).
The diagnostic machinery still requests timestamp-query features and allocates
counter sample buffers. Apple's HUD encoder timing cannot operate alongside
application-owned counter sample buffers.

A separate diagnostic build omitted `TIMESTAMP_QUERY`,
`TIMESTAMP_QUERY_INSIDE_PASSES` and `TIMESTAMP_QUERY_INSIDE_ENCODERS` from
`WgpuSettings.features`, and explicitly excluded them through
`disabled_features`, when `MUDCRAB_NATIVE_GPU_DIAGNOSTICS` was present. It kept
`RenderDiagnosticsPlugin`, CPU spans and the complete graphics configuration.
The default production executable was restored after the build. This is a
measurement configuration, with no claimed FPS improvement.

The diagnostic process created the HUD's counter buffer and no Bevy timestamp
buffers. It produced a saved native HTML report using
`MTL_HUD_ENABLED=1`, `MTL_HUD_LOG_ENABLED=1` and
`MTL_HUD_ENCODER_TIMING_ENABLED=1`. `MTL_HUD_REPORT_URL` takes an absolute
filesystem path; a `file://` URI did not save a report in this investigation.
The environment variable chooses the destination, but does not start a report.
Select the HUD menu's **5 Seconds** report action. The existing public AppKit
menu action also worked from this locally built engine process.
[Apple describes the report workflow and counter-buffer restriction](https://developer.apple.com/documentation/xcode/generating-performance-reports-with-metal-performance-hud).

The verified report contains 234 frame intervals over 5.00 seconds, from HUD
frames 1352–1586. Its mean interval is 21.34 ms. Scope matters:

| Measurement | Native GPU time | Scope |
| --- | ---: | --- |
| Fragment stages | 16.66 ms | Report-window average |
| Vertex stages | 2.21 ms | Report-window average |
| Compute encoders | 3.06 ms | Report-window average |
| Blit encoders | 1.46 ms | Report-window average |
| Primary opaque encoder | 13.48 ms; fragment 12.57 ms | Last-frame snapshot |
| Reflection opaque encoder | 1.25 ms; fragment 1.19 ms | Last-frame snapshot |
| Tonemapping encoder | 594.80 µs | Longer, pooled label average |
| Indirect validation encoder | 6.99 µs | Longer, pooled label average |

GPU categories overlap: their sum exceeds the 21.07 ms encoder GPU aggregate.
Do not add them as serial frame time or call the result GPU utilization. The
label table has 3,178 opaque and 1,589 tonemapping occurrences; it covers a
longer range than the 234-frame report window and pools two opaque views.
The reset boundary is not specified by the HTML.

Tonemapping's last encoder took 0.61 ms, while its command buffer spanned
13.42 ms. The buffer interval includes dependencies and overlap; it is not the
tonemapping shader's cost. The HUD's CPU encoder intervals measure open-to-close
wall time, so a pending-write blit encoder held open during other work can look
expensive while consuming little CPU. Use native thread CPU clocks or sampling
to distinguish those cases.

## Locating the drawable wait

An opt-in Objective-C observer recorded `CAMetalLayer.nextDrawable`, thread CPU
time, command-buffer submission and completion, GPU start/end timestamps,
render attachment identity, and each drawable's `presentedTime`. Events use the
same monotonic clock. Callback records contain numeric identifiers rather than
retaining buffers, textures or drawables. A bounded queue writes events off the
render path and records lost events explicitly.

The complete full-observer capture had 936 acquired and presented drawables,
130,176 submitted/completed buffers, and zero reported drops, exceptions or
hook failures. Its interior acquisition range contains 841 observations:
7.724 ms mean wall wait and 0.034 ms mean thread CPU time.

Matching each returned texture to its previous final upscaling buffer and
presentation gives 837 reuse pairs across three texture pointers. Of summed
paired acquisition wait, 89.77% occurs after that texture's matching render
buffer ends, and 81.69% occurs after its reported onscreen time. These fractions
overlap. They locate a delay beyond the prior texture's rendering, but do not
observe Core Animation's exact release event or prove that unrelated GPU work
has ended. [`GPUStartTime`/`GPUEndTime`](https://developer.apple.com/documentation/metal/mtlcommandbuffer/gpuendtime)
and [`presentedTime`](https://developer.apple.com/documentation/metal/mtldrawable/presentedtime)
are separate endpoints.

Presented-handler delivery is also separate: 788 of 837 handlers arrived after
the next acquisition returned. Handler arrival cannot substitute for onscreen
time. A bounded eight-buffer sample found Present completion handlers arriving
0.020–0.057 ms after GPU end, followed by 6.303–7.575 ms before texture reuse.
That small sample excludes a long Present completion-handler delay there;
other observer effects remain possible.

| Full-scene run | Mean FPS | Mean acquisition wall time | Acquisition thread CPU |
| --- | ---: | ---: | ---: |
| No observer | 47.20 | Unmeasured | Unmeasured |
| Full native observer | 46.80 | 7.724 ms | 0.034 ms |
| Acquisition-only observer | 46.81 | 9.162 ms | 0.031 ms |
| Native display synchronization enabled | 46.96 | 7.984 ms | 0.039 ms |

The acquisition-only control installs no device, queue, command-buffer or
presentation hooks. The long wait survives their removal. Enabling native
display synchronization did not demonstrate an FPS gain. These are individual
captures; the differences do not establish an exact observer overhead or a
general presentation fix. The first acquisition-only attempt failed two
startup streaming commit-budget checks and was excluded from accepted
performance comparisons.

## Drawable ownership and earlier cleanup

[Metal surface acquisition](https://github.com/gfx-rs/wgpu/blob/v29.0.4/wgpu-hal/src/metal/surface.rs#L160-L185)
uses a bounded autorelease pool and retains the returned drawable and texture.
Core presentation removes the acquired HAL surface texture; HAL presentation
consumes it and releases the application's drawable reference after its local
pool drains. An unbounded drawable autorelease pool is not supported by this
source chain.

Other owners remain. Bevy's old `ViewTarget` output texture view survives until
target replacement after the next acquisition. Completed core submissions can
retain texture/view trackers until device maintenance. Native pending command
buffers have their own ownership. Texture-only references preventing Core
Animation reuse are not established by the public drawable documentation.

An isolated experiment calls `RenderDevice.poll(PollType::Poll)` after surface
creation and before `prepare_windows`, enabled only by
`MUDCRAB_METAL_POLL_BEFORE_ACQUIRE=1`. The public poll path drains completed core
submission trackers and deferred destruction without waiting for GPU
completion; it can still acquire CPU locks and run callbacks. It leaves old
Bevy view targets and the HAL's separate pending-buffer list intact. System
ordering places it before acquisition, with no guarantee that unrelated
systems cannot run between them.

The same pinned binary is tested with the flag off and on, full graphics,
production timestamp-query settings, the standard HUD and acquisition-only
observation. Cumulative poll calls, status, errors and wall time are logged
every 300 calls. This tests earlier core cleanup; it does not test every native
owner or identify compositor release directly.

Both control and enabled captures passed streaming/renderer checks and retained
nonblack, varied scene images. Their native interior ranges report:

| Same-binary setting | FPS | Mean acquire wait | Median acquire wait | Acquire thread CPU |
| --- | ---: | ---: | ---: | ---: |
| Poll off | 46.10 | 8.472 ms | 9.203 ms | 0.031 ms |
| Poll on | 46.79 | 7.339 ms | 8.879 ms | 0.030 ms |

Acquisition p95 remained about 10.7 ms. The logged poll range, calls 1200–2100,
contains 900 calls, zero errors, one `QueueEmpty` result and 0.266 ms mean poll
wall time. The reported 5.584 ms maximum includes loading and warmup; a
steady-only maximum is unavailable. Poll wall time is not thread CPU time.

The mean wait fell more than the median, and the added poll moves some work
before acquisition. Timestamped host snapshots prove compiler activity inside
the control's native crop and a LOD probe in 17 snapshots inside the enabled
crop. One pair does not establish a net production gain or an exact
cause of the remaining wait. A nonempty poll can drain older completed
submissions while newer ones remain. The result does not reject other native
owners, later completions or compositor lifetime. The patch remains isolated.

## Capture files and tool limits

Public `MTLCaptureManager` produced a 1.98 GB `.gputrace` after launching with
`MTL_CAPTURE_ENABLED=1`. Capture was scoped to the active layer's device,
started before acquisition 1200, and stopped after the final targeted Present
buffer completed. Although three acquisitions were bracketed, the capture
metadata reports one captured frame. Distinct decoded GPU frames have not been
verified. Capture-file writing lowered measured FPS; that run is excluded from
performance comparisons.
[Apple documents programmatic capture](https://developer.apple.com/documentation/xcode/capturing-a-metal-workload-programmatically).

Command Line Tools alone provide neither Instruments nor the full Xcode GPU
capture viewer. The trace is preserved for that viewer, but has not been
decoded into source-function timings. The native HUD encoder counters above
are measured evidence independent of that undecoded file.

Reusable experimental source and analyzer commands are in
[`scripts/profiling`](../../scripts/profiling/README.md). The source review
checked callback ownership, process-lifetime hooks, clocks and buffering. The
analyzers reject unusable inputs and distinguish sequential encoder instances
that reuse the same native pointer. Ten small analyzer checks and twelve native
configuration/clock checks passed. Native compilation uses warnings as errors.

Artifacts are outside the checkout in
`../mudcrab-profiles/2026-10-09-metal/`: `hud-report-5.html`, native observer
source and analyzers, raw NDJSON captures, correlation pairs, pinned binary
receipts, `riverwood-three-frames.gputrace`, and
`fps-dashboard-metal-inline.html`. The 18 native run bundles are also archived
under `runs/metal-native-*`, with file hashes in `run-archive-receipt.json`, so
they survive cleaning `target`. Game assets and raw GPU captures are not
repository fixtures.

## Full-quality shader experiment

The near terrain shader always samples six diffuse textures, even when a
quadrant declares fewer layers. The existing material uniform contains the
layer count, and absent weight-field slots are zero. In this 25-cell scene,
100 quadrants declare 464 active layers across 600 available slots. That is an
unweighted count, not a pixel-weighted GPU cost estimate. Compiled distant LOD
uses `StandardMaterial` and is outside this custom shader change.

An isolated shader experiment uses material-uniform branches to skip unused
diffuse samples and packed weight reads. Active blending order, normal sampling,
UV tiling and 16× anisotropy are preserved. Invalid and legacy count-zero
materials retain the six-slot path. Runtime compilation, image comparison and
matched native measurements determine whether this should become a fix.
The first runtime capture passed its streaming and renderer checks and
averaged 47.85 FPS. Its native report recorded 16.16 ms mean fragment time,
compared with 16.66 ms in the earlier baseline. The image was visually intact;
a 160,734-pixel grid comparison had 0.188 mean absolute channel error on the
0–255 scale, with 8.76% of sampled pixels differing. The images are not
byte-identical, and time-dependent foliage and water are not isolated by this
comparison.

A fresh baseline attempt failed a startup streaming commit-budget check and
had large CPU/frame spikes. It is retained as failed evidence and excluded
from gain comparisons. An unrelated Mudcrab worktree was building immediately
after that capture; there is no timestamped proof here that it caused the
earlier spikes. A second passing baseline and shader capture recorded 46.86
and 47.79 FPS, with mean fragment times of 16.61 and 15.98 ms. Host telemetry
confirms overlapping compilation in both. The consistent direction is
promising, but these captures do not establish a quiet-machine production gain.

A HUD-off baseline failed its startup budget check. The HUD-off shader capture
reported 92.17 update FPS while losing its primary drawable; it is invalid
rendering evidence and excluded. The shader remains isolated rather than being
promoted on these FPS results.

Fog's directional scattering also fetches directional shadows after ordinary
PBR lighting has fetched them. The lookups use different normals; their cost
and whether they can be shared safely remain unmeasured. Keep fog and shadow
filtering intact while investigating this path.

The next graphics-preserving observation is a sparse snapshot of each view's
eligible opaque entities, material types, cached pipeline descriptors and
shader definitions, paired with issued draw-call counters. Existing far-LOD
root markers distinguish its `StandardMaterial` meshes from other objects.
This can confirm the actual fog/filtering paths and shader mix. It cannot
assign GPU milliseconds to materials: a multidraw representative may cover
mixed-source slots, and eligible entities are not executed pixel counts.

[Apple's counter guide](https://developer.apple.com/documentation/metal/sampling-gpu-data-into-counter-sample-buffers)
describes stage-boundary timing on Apple silicon. Whole opaque-stage counters
do not split terrain and object shader costs within that encoder. Actual device
sampling capabilities still need to be recorded before a counter experiment.
Splitting the render pass would change its work and is outside this observation
plan. The preserved GPU capture remains available for full Xcode inspection.
