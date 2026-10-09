# Matched terrain batching protocol

PR #200 stays draft. The archived result is not acceptance evidence: fixed-view FPS
was 47.315 → 47.453, ordinary play was one unmatched pair at 48.448 → 59.971,
and static peak RSS was 2.428 → 2.770 GiB. The 14.1% increase does not prove a leak.

## Build identities and dependencies

Use integration `c39449b5a8b6a62c7c3c43bb60164e8ba6911839` plus the root agent's
corrected #205 sampler, #201 generation retry and #191 early config-root branches
as the common base. Those corrections are owned separately. Until their actual
merge commits are available, source equivalents are provisional and must be named
by exact commit/tree in each receipt. Do not close #197 or retarget #200 before
#205 actually merges.

The baseline contains diagnostics and those prerequisites, with batching disabled
by source selection. The candidate adds only batching, change-driven LOD/visibility
and reflection updates. Both sides need the same capture implementation. Do not
compare the old archive binary with an integrated candidate as if batching were
the only difference.

Build both with `--locked --release`, thin LTO, one codegen unit and the same debug
setting. Record command/environment, commit, tree, dirty diff, executable SHA-256,
OS/GPU/power/display state and asset/INI identities. A profile's `release` string
also covers `quick` builds, so it cannot replace a build receipt. Keep binaries,
retail assets, raw logs, native profiles and new images outside Git.

## Existing acceptance limits

Use the project's [acceptance policy](../roadmap/02-acceptance.md): average FPS ≥60,
p95 frame time ≤16.67 ms, post-settle process-memory growth ≤0.5 GiB, zero runtime,
LOD and lifecycle failures, and respected commit budgets. Missing memory is an
unavailable gate even though the current engine report can pass without it.

The comparison in `scripts/phase2-acceptance.ps1` warns above 5% and fails above
10%, after absolute noise floors of 5 FPS, 1.5 ms for latency and 0.05 GiB for
memory. It compares peak process memory and memory growth. The old peak-RSS increase
exceeds that comparison threshold if reproduced in a qualifying matched campaign;
it is not an agreed exception. No fixed absolute RSS cap or numerical pixel-difference
budget was found. An exception needs an explicit maintainer decision, supported by
repeat captures. Do not substitute the historical capture's relaxed 0 FPS/1000 ms/
4 GiB thresholds for acceptance.

Use at least three fresh processes per side, interleaved B-C, C-B, B-C. First run a
120-second pilot, then the documented 300-second world, 600-second stress and
1800-second stability campaigns. Include loading in full-run RSS; compare the
same settled ranges separately. Run timing captures when other builds and GPU
captures have finished. Preserve failed runs.

## Both upload budgets

Run the full pair/repeat matrix at both settings:

| Setting | CLI |
| --- | --- |
| Normal byte budget, explicit scene arming limit | `--max-model-spawns-per-frame 16 --max-upload-mib-per-frame 16` |
| Constrained byte budget, same arming limit | `--max-model-spawns-per-frame 16 --max-upload-mib-per-frame 1` |

The second setting is 1,048,576 bytes per frame. The model option limits scene
spawns, not GPU model uploads. The byte budget allows an oversized single asset;
record actual upload and retry state rather than inferring it from the limit.

## Fixed views and pose-jump coverage

The checked-in [handoff shots](../../scripts/profiling/fixtures/riverwood-terrain-handoffs.json)
contain 24 exact Creation poses, three repeated trips across a cell/tile boundary,
reverse visits and returns. Use the same 1600×900 frame, HFOV, exposure, shadows,
reflections, validation, culling, asset pack and INI on both sides. This is a
pose-jump coverage test; it does not time continuous traversal.

Set `MUDCRAB_PROFILE_ASSETS` to the absolute path of the matched converted pack.

```sh
MUDCRAB_PROFILE_SCENE_EVIDENCE=1 "$engine" \
  --assets "$MUDCRAB_PROFILE_ASSETS" \
  --worldspace 0x3c --stream-radius 2 \
  --max-model-spawns-per-frame 16 --max-upload-mib-per-frame "$budget_mib" \
  --shots scripts/profiling/fixtures/riverwood-terrain-handoffs.json \
  --shots-out "$run/shots" --profile-output "$run/profile"
```

`scene-observations.ndjson` records actual absolute camera/projection, render origin,
queue state, boundary crossings and reversals even in shots-only runs. It is opt-in,
bounded and flushed as records are written. A file-open/write failure or cap makes
the evidence incomplete. The regular benchmark bundle is not exported by shots-only
runs; preserve this sidecar and `shots.log` instead.

Each expected image must exist, have the requested dimensions and pass visual review
against its same-name baseline. Retain terrain/water/detail masks and review seams,
missing surfaces and duplicate geometry. Never count an optional screenshot gate
without a requested image as coverage proof. The existing `settled` log checks ordinary
queues and ten quiet frames; it omits pending LOD/batch selection uploads and retries.
Therefore `settled` alone is insufficient to accept the batching candidate.

## Continuous crossing, reversal and repeated traversal

The [route fixture](../../scripts/profiling/fixtures/matched-route.json) fixes
Riverwood start `[22528,-47104,6000]`, yaw 90°, pitch 20°, HFOV 90° and a horizontal
route: start → +4 cells east → −4 cells east → start, repeated three times. Advance
through the same 64-unit positions on both sides: 3072 movement steps. Reapply absolute
poses after origin rebases. Match timing by route-step ranges and steady checkpoint
poses, not an equal number of frames or equal elapsed seconds at different positions.

Before acceptance, an opt-in route driver must execute that list, freeze exact poses
for screenshots, retain main/render frame associations, and track selected terrain
coverage and every outstanding transfer. This split supplies the fixture and observation
sidecar, not that driver. Current auto-flight uses frame-sized elapsed-time steps,
reverses before the crossing step and moves during frame-count warmup. It is suitable
for a provisional stress run, not an exact matched comparator.

For a bounded provisional stress run, use `--auto-fly-speed 4000
--benchmark-warmup-frames 120 --benchmark-duration 120` with the chosen budget and
`--profile-output`, `--benchmark-output` and `--benchmark-frame-times`. Omit
`--acceptance-screenshot`: it changes the camera, anchors streaming and pauses flight.
`MUDCRAB_PROFILE_HANDOFF_IMAGES=1` requests images at cell crossings and reversals
only when scene evidence is enabled. These requests are asynchronous; their stored
pose is the request pose, not a certified captured-frame pose. Use them to inspect
holes, preserve their limitation, and exclude this image run from timing comparisons.

Settled checkpoints must wait for ten consecutive quiet frames with zero ordinary
queues, pending LOD queries/chunks, CPU batch work, initial replacement transfers,
subsequent immutable selection uploads and specialization retries. Enforce the
existing 30-second settle timeout; a timeout is failed/incomplete coverage. In #200,
both batch `pending_count()` implementations count retained source hierarchies only,
so zero initial queue gauges do not certify later selection uploads. Expose those
states before accepting a movement or repeated-return result.

Sample RSS externally at one-second intervals from the confirmed engine PID with
monotonic and UTC timestamps. Compare full-run peak, post-settle growth and repeated
returns to the same pose. Track meshes/entities/resident cells and queued resources
on returns; growth alone does not establish a leak. Do not accept timing if camera,
coverage, dimensions, backend, presentation mode or queue state is unmatched.

## Native attribution and CI

Keep uninstrumented FPS runs separate from count, native-observer and Xcode captures.
Acquire-only and full-observer controls quantify instrumentation effects. Preserve
raw native events, health/drop/hook records, symbol coverage and guarded crop bounds.
GPU elapsed intervals include waits and overlap; they are not shader active utilization.

Full Xcode is installed here although Command Line Tools is the selected default.
A command-scoped developer directory exposes `Metal System Trace`:

```sh
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun xctrace record \
  --template 'Metal System Trace' --time-limit 30s \
  --output "$run/metal.trace" --launch -- "$engine" ...
```

A trace file alone does not attribute the limiting boundary. Correlate CPU submission,
GPU completion, drawable acquisition and presentation on the same timeline before
making GPU or compositor claims. Keep this gate open if launch, recording, export or
inspection fails.

At pinned head `0650c70987cb4056eb3f08525ab9329f2e8cf179`, live read-only GitHub
queries on 2026-10-09 found zero check runs and zero Actions runs. CodeRabbit's green
status is not test CI. Run required fmt, workspace Clippy/tests, audit and release
performance checks after integration, and obtain actual CI at the publishable heads.
Historical 1141-test receipts do not validate this split.
