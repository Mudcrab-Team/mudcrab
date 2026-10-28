# Metal submission and measurement validity — 2026-10-08

This is a historical investigation of a separate integrated development build. It does
not establish performance or coverage acceptance for the diagnostics split or a later
batching candidate. See the [split protocol](terrain-batching-matched-protocol-20261009.md)
and [tool source manifest](../../scripts/profiling/source-manifest.json) for provenance.

The current Metal profile attributes substantial CPU work to indexed indirect command encoding,
driver render-state emission and wgpu indirect validation. The sampled totals attribute CPU
intervals to stack endpoints; parked endpoints are reported separately from executing functions,
and inclusive rows overlap. They do not establish GPU utilization or the frame's
critical path. The original capture and workload are described in
[Metal function profiling](metal-function-profiling-20261008.md).

## What the submission path permits

Bevy 0.19's binned material phases allocate indirect argument records for prepared mesh bins.
Its mesh draw command uses the
GPU-written command count only when `MULTI_DRAW_INDIRECT_COUNT` is available; otherwise it submits
the full prepared range. wgpu 29.0.4's Metal backend implements that range as a CPU loop calling
Metal once per indirect record. A record whose GPU-written instance count is zero still occupies
one iteration. Reducing distinct mesh bins while preserving geometry therefore addresses both
CPU submission and validation work. Combining records into one API range already happens and
does not eliminate the backend loop. See the pinned sources for
[Bevy's mesh draw command](https://github.com/bevyengine/bevy/blob/v0.19.0/crates/bevy_pbr/src/render/mesh.rs),
[its phase ranges](https://github.com/bevyengine/bevy/blob/v0.19.0/crates/bevy_render/src/render_phase/mod.rs),
and [wgpu's Metal implementation](https://github.com/gfx-rs/wgpu/blob/v29.0.4/wgpu-hal/src/metal/command.rs).

wgpu pools its validation buffers and bind groups, but reconstructs per-draw CPU validation
metadata for each render pass. Each record carries buffer offsets and the current vertex/index
and instance limits. The validated argument output depends on the GPU-written input, so caching
old output across frames or passes would need a proof that both inputs and bounds are unchanged.
The public project API provides no validation-metadata cache. This investigation leaves validation
enabled and does not change the dependency source. The relevant implementation is in
[render command replay](https://github.com/gfx-rs/wgpu/blob/v29.0.4/wgpu-core/src/command/render.rs)
and [indirect draw validation](https://github.com/gfx-rs/wgpu/blob/v29.0.4/wgpu-core/src/indirect_validation/draw.rs).

Allocated indirect records, batch sets, render-phase items and issued native draw commands are
different quantities. Buffer lengths establish allocation, not issuance. A counter at the mesh
draw command can establish submitted API calls and argument ranges; GPU-counted ranges still
require separate treatment because their actual command count is determined on the GPU.

Set `MUDCRAB_PROFILE_DRAW_COUNTS=1` for a separate bounded observation capture with the normal
`--profile-output` options. This adds `draw-submission.json`. The plugin keeps every original
draw-function ID and command tuple member, replacing only the terminal `DrawMesh` with a wrapper
that delegates to it unchanged. Counts are recorded only when that original command returns
success, after issuing its API draw. Earlier pipeline/bind-group skips never reach the wrapper;
mesh-command skips are recorded separately. StandardMaterial, native NIF materials and the
terrain/water extended materials share these draw paths in Bevy 0.19.

The report separates direct calls, fixed indexed/non-indexed argument slots and GPU-counted
range upper bounds across opaque, alpha-mask, transparent, transmissive, prepass, deferred and
shadow phases. It also identifies missing phase registries and unwrapped draw variants. This
coverage excludes fullscreen/non-mesh render nodes. Fixed slots describe submitted arguments,
including zero-instance records, rather than the number of records producing visible pixels.

Cumulative totals include loading and warmup. Each completed render schedule also publishes
per-phase count deltas with a render-frame sequence and elapsed capture time. Crop those frames
to a stated steady interval before comparing workload; the render sequence is independent of
the main-world benchmark frame counter. Counters are sampled after render cleanup, and the main
world drains a queue so asynchronous delivery retains every published frame. Queue/history
storage is capped at 20,000 frames and reports discarded samples. Keep this instrumentation off
for matched timing profiles because it adds CPU work.

## Changes to the profiling bundle

`ProfilingState` now records each enabled render diagnostic measurement once using its timestamp.
It consumes all unseen measurements still retained by Bevy, so asynchronous delivery neither
duplicates the previous value on every main frame nor discards a newly delivered history entry.
Equal values with different timestamps remain separate observations. Rejected nonfinite values
are counted separately.

The bundle reports enabled device timestamp/statistics features independently of timing values.
Each GPU pass is marked unobserved, all zero, positive, or invalid; the aggregate can be mixed.
All-zero and negative pass values remain in the raw metric summaries but are excluded from the
GPU timing table. Positive values establish availability, not independent validation of clock
accuracy or attribution. Bevy's macOS command-encoder timestamp writer returns early to avoid
a documented rendering issue, while its render/compute pass writers still issue timestamps. See
[the diagnostic writer](https://github.com/bevyengine/bevy/blob/v0.19.0/crates/bevy_render/src/diagnostic/internal.rs).

Metadata format 2 records the observed primary window's physical resolution, logical resolution
and scale factor. `resolution` is physical pixels and is null when no primary window was observed;
it no longer invents a nominal 1600×900 size. The metric summary adds a minimum value so negative
elapsed observations remain detectable even when most observations are positive.

## Native GPU evidence

Apple's standard HUD GPU duration spans command buffers and may include idle gaps between
encoders. Its encoder timing measures vertex, fragment and compute work, requires that the app
does not use Metal counter sample buffers, and can increase HUD CPU overhead. An instrumented
run may omit Bevy's diagnostics plugin to free those buffers while retaining rendering features;
label that run separately. The HUD also reports command-buffer and encoder counts and CPU
encoding intervals. These are not individual draw counts.
[Apple's HUD documentation](https://developer.apple.com/documentation/xcode/monitoring-your-metal-apps-graphics-performance)
describes these restrictions.

The HUD performance report includes command-buffer/encoder timing and encoding order for its
final frame, plus aggregate metrics and labelled expensive encoders. `MTL_HUD_REPORT_URL` chooses
an app-writable destination; the documented duration selection uses the HUD menu.
[Generating performance reports](https://developer.apple.com/documentation/xcode/generating-performance-reports-with-metal-performance-hud)
describes the report contents and controls. No GPU-bound conclusion follows from the CPU profile
or zero Bevy GPU pass values alone.

## Visibility and upload boundaries

The visibility optimization adds an owned `NoCpuCulling` marker only to empty nodes beneath
streamed cell or LOD roots. Bevy gives these nodes their inherited visibility through its
change-driven path. Their `Visibility` and `InheritedVisibility` components remain in place, so
children still receive normal visibility propagation. Nodes with a visibility class, bounds,
mesh, camera, light, probe, picking target or collider keep CPU visibility checks. The restoration
pass runs after bounds calculation and before visibility propagation/reset; participating
components present at that pass restore ordinary culling that frame. Existing explicit opt-outs
are left alone. Terrain hierarchy flattening removes some of the same nodes, so the two changes'
population reductions overlap.

Terrain upload acknowledgments check requested mesh IDs against `RenderAssets<RenderMesh>` and
their resident `MeshAllocator` ranges after mesh preparation. Vertex ranges must cover the
descriptor's vertex count; nonempty indexed meshes also need index ranges covering their index
count. They establish preparation and buffer availability for submission, not completed GPU execution.
Every changed batch selection uses a new immutable mesh ID. Initial flattening waits for both the
selected generated buffers and every retained source buffer; later selection changes draw the
exact selected source primitives while replacements upload. Generated and fallback entities use
`NoAutoAabb` with explicit selected CPU bounds. Bevy's extracted mesh metadata can still contain
bounds covering dormant vertices, so this does not establish selected-geometry GPU bounds.

The bridge retains IDs rather than asset handles. Activation forgets generated acknowledgments;
superseding a selection cancels its old ID; removing a batch root cancels its generated and source
IDs and removes its generated mesh assets. Source handles remain with the root for fallback use.
Without a render backend, batching keeps the original hierarchy. A canceled source ID shared by
another pending root can require another acknowledgment; that root keeps its source hierarchy
during the extra wait.

## Moving-streaming validation

A stationary profile cannot validate origin rebasing or deferred selection uploads. Run the
current integrated binary through several crossings and reversals, first at the normal 16 MiB
upload budget, then at 1 MiB to extend upload waits. This changes upload pacing while keeping
geometry, shadows, reflections and render diagnostics enabled. Treat the 1 MiB capture as a
stress scenario rather than a matched performance comparison.

Do not add `--acceptance-screenshot` to this protocol: its presence fixes the render origin and
anchors the streaming center to the start cell. Observe the visible window and capture transition
frames externally. `--auto-fly-speed 2048` follows a horizontal path with a four-cell half-span;
the first turn takes about eight seconds and an end-to-end traverse about sixteen seconds.
Movement also runs during warmup. A 120-second measured interval covers several reversals. This
is automated camera flight; it does not exercise the interactive player physics path.

Run these commands from the repository after the integrated release build is complete. Keep the
window unobscured and record its actual physical resolution from the bundle. Benchmarks request
`AutoNoVsync`; interactive runs request `AutoVsync`. Preserve the native surface log to establish
the resolved mode, which is not currently a metadata field. The commands retain the normal
acceptance thresholds and save each exit code, including failures. The dirty-worktree
flag is added only when Git reports staged, unstaged or untracked changes.

```sh
# Run from the repository root; keep artifacts outside ignored target/.
ARTIFACT_ROOT=${ARTIFACT_ROOT:-../mudcrab-profiles/2026-10-08-riverwood/runs}
capture_stamp=$(date -u +%Y%m%dT%H%M%SZ)
profile_commit=$(git rev-parse HEAD)
profile_status=$(git status --porcelain)
set --
if [ -n "$profile_status" ]; then
  set -- --profile-dirty-worktree
fi
for upload_mib in 16 1; do
  moving_run="${ARTIFACT_ROOT}/moving-${upload_mib}mib-${capture_stamp}"
  mkdir -p "$moving_run"
  shasum -a 256 target/release/engine > "$moving_run/binary.sha256"
  env -u MUDCRAB_PROFILE_DRAW_COUNTS \
      -u MUDCRAB_PROFILE_NO_SHADOWS \
      -u MUDCRAB_PROFILE_NO_REFLECTIONS \
      -u MUDCRAB_PROFILE_NO_RENDER_DIAGNOSTICS \
      -u MUDCRAB_PROFILE_FREEZE_CAMERA \
      -u MUDCRAB_PROFILE_RESOLUTION \
      -u MUDCRAB_PROFILE_PRESENT_MODE \
      -u MTL_HUD_ENCODER_TIMING_ENABLED \
      MTL_HUD_ENABLED=1 MTL_HUD_LOG_ENABLED=1 \
      RUST_LOG=info,bevy_render::renderer=debug,wgpu_hal::metal::surface=debug \
      ./target/release/engine \
      --assets "$PWD/modern_assets" --worldspace 0x3c --grid-x 5 --grid-y -12 \
      --stream-radius 2 --auto-fly-speed 2048 \
      --max-upload-mib-per-frame "$upload_mib" \
      --benchmark-duration 120 --benchmark-warmup-frames 1200 \
      --benchmark-output "$moving_run/benchmark-report.json" \
      --benchmark-frame-times "$moving_run/frame-times.csv" \
      --run-label "Riverwood moving ${upload_mib} MiB" \
      --profile-output "$moving_run/profile" \
      --profile-scenario riverwood-moving-streaming \
      --profile-run-id "moving-${upload_mib}mib-${capture_stamp}" \
      --profile-commit "$profile_commit" "$@" \
      --profile-hardware 'Apple M1 Pro' \
      > "$moving_run/stdout.log" 2> "$moving_run/stderr.log"
  run_exit=$?
  printf '%s\n' "$run_exit" > "$moving_run/exit-code.txt"
done
```

Review each report's `streaming.origin_rebases`, `unloaded_cells` and LOD activity to establish that
movement exercised the lifecycle. Require zero duplicate, missing, orphaned or out-of-range cell
roots and zero streaming invariant failures. Preserve and inspect runtime validation failures,
commit-budget violations, p95/p99/worst frame time and memory growth; an exit code alone does not
identify which gate failed. Watch terrain boundaries, water reflections and shadows during
crossings and reversals. The current aggregate metrics do not provide a per-frame terrain coverage
oracle, so they cannot certify the absence of a visible transition hole.

In `cpu-spans.json`, inspect `lod/terrain_batch_gpu_poll`, `lod/terrain_batch_commit`,
`lod/terrain_batch_activation` and `lod/visibility`. The source-readiness guard checks members of
pending roots, and changed selections clone complete batch vertex streams before upload; these
costs can differ substantially from a stationary interval. The gauges
`lod/pending_terrain_batch_chunks` and `lod/pending_initial_terrain_upload_chunks`, plus
`initial_uploads_drained` events in `streaming.json`, distinguish CPU preparation from initial GPU
activation. For the #200 candidate, also require `lod/pending_terrain_selection_uploads`
to be zero before accepting a settled checkpoint; it counts pending immutable batch
meshes after initial activation. Check loading/query work and specialization retries
separately. Gauges report the latest value, not a history. Compare `memory.json` samples over
repeated traversals and inspect the final `assets/meshes` and scene counts for continued growth.
These commands are a validation protocol; this document does not claim they have passed.
