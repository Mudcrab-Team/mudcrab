# Phase 2 Profiling

The profiling stage is implemented as an opt-in, reproducible campaign around the release engine.
It does not require or redistribute Skyrim assets for the synthetic scenario. World scenarios require
the user's converted, legally owned asset set.

## Captured data

Each run writes a self-contained directory containing `metadata.json`, `frame-metrics.json`,
`cpu-spans.json`, `gpu-passes.json`, `streaming.json`, `memory.json`, and `summary.md`.
Profiling runs also write [`streaming-frames.json`](../research/streaming-trace.md), a bounded
frame trace covering warmup, measurement and capture settlement. It separates synchronous CPU
spans from reported completion latencies and keeps render observations' source-frame IDs.

- CPU spans cover camera/world work, streaming planning, database queue/query/total latency, cell
  commit and spawn, terrain mesh generation, asset readiness, origin rebasing, and water systems.
- GPU data is sourced from Bevy 0.19 render diagnostics. Timestamp and pipeline-statistics support
  is recorded per run; counters the active backend cannot expose are listed under `unavailable`
  instead of being estimated.
- Renderer proof records whether GPU preprocessing, GPU culling and indirect drawing became active,
  plus the maximum occlusion-culling views, HZB views, indirect phase buffers, indirect batch sets
  and the number of proof frames. Bevy 0.19 does not expose native indirect draw counts or rejected
  instance counts to the main world; those fields remain explicitly `unavailable`, never zero-filled.
- Streaming includes aggregate counts plus request, stale-discard, unload, origin-rebase and commit
  events. It also records the per-frame commit budget and its raw worst value. A wall-clock overrun
  is classified as a violation only after a fixed 1 ms Windows scheduler tolerance; the independent
  frame-P95 acceptance threshold remains exactly 16.67 ms.
- Memory includes periodic process samples and the derived GiB/minute slope. The growth gate compares
  the first and last samples after a steady-state settling window equal to 10% of the scenario
  duration, capped at 60 seconds. Peak memory still covers the entire run. This excludes one-time
  asset and pipeline warm-up without hiding sustained growth in the long acceptance scenarios.
- Metadata records scenario, run, commit, dirty-worktree state, build profile and machine details.

## Reproducible campaign

Run the synthetic renderer without proprietary assets:

```powershell
./scripts/phase2-profile.ps1 -Scenario synthetic -Repetitions 3
```

The acceptance runner also executes `--streaming-fixture`: a proprietary-free database/cache scene
that performs rapid traversal, teleports, repeated origin rebasing and a return to the initial cell.
It rejects duplicate, orphaned or missing cell roots, stale work that was not discarded, incomplete
unload, worker shutdown failures, and per-frame commit-budget violations.

Run every scenario with converted assets and coordinates selected during integration:

```powershell
./scripts/phase2-profile.ps1 -Scenario all -Assets D:\SkyrimConverted `
  -RuralGridX 0 -RuralGridY 0 -DenseGridX 12 -DenseGridY 8 `
  -WaterGridX 7 -WaterGridY -2 -Repetitions 3
```

Results go to `target/profiling/<timestamp>-<cpu>-<gpu>`. The runner uses medians to reduce noise.
Pass `-UpdateBaseline` to write `profiling-baseline.json`. Against an existing baseline, material
regressions over 5% are warnings and regressions over 10% fail the campaign. Fixed noise floors of
5 FPS, 1.5 ms and 0.05 GiB prevent sub-millisecond scheduler jitter and sample granularity from being
amplified into false percentage regressions. Higher FPS is better; lower frame latency and memory
are better.
The profiling runner uses Windows `AboveNormal` process priority, inherited by the engine, to reduce
desktop-process preemption without using unsafe real-time scheduling.

`-Quick` reduces a campaign to one short repetition for plumbing checks. Stability defaults to 30
minutes. The dedicated `GPU Profiling` workflow is manually dispatched on a Windows GPU runner and
retains the complete bundles as CI artifacts.

## World loading and pacing metrics

These measurements show where world-loading time goes; they change nothing about what the engine
loads or in what order. They are fields of the benchmark report (`--benchmark-output`, report
`format_version` 8) and spans and events in the profiling bundle. They are installed only when the
run measures pacing (`EngineConfig::measures_pacing`: a benchmark run, or a jump target with no
frame limit); an ordinary play session adds none of the marker schedules, asset-arrival counts or
window scans.

- **Time to a loaded world.** "Loaded" means every cell of the stream window is resident, no cell is
  loading, no database request is in flight, the model arming queue is empty, and no model or
  terrain/water surface is still pending. A cell that failed (map edge, missing data) is never
  retried and never becomes resident, so it counts as settled: the predicate is ready when resident
  plus failed cells cover the window. The predicate must hold on two consecutive frames (a cell
  that just committed is resident while its references are still being created, so one frame can be
  early). `time_to_world_ready_ms` and `frames_to_world_ready` are measured from the first frame to
  the first of those two frames, warm-up included, and `failed_window_cells_at_ready` says how many
  failed cells were in the window on the latch frame (`null` when the world never became ready).
  When the predicate never holds, `world_ready_reached` is `false` and the times are `null`. A run
  with no streaming (the synthetic benchmark) has none of these fields.
- **A jump.** `--benchmark-jump <grid-x>,<grid-y>` (same worldspace, each coordinate within
  ±512) moves the camera to the middle of that cell, at its ground height plus only the height (Y)
  of the start offset (`setup_world`'s camera offset, Y 1200 by default; the cell centre supplies X
  and Z), on the first frame the world is ready. The report names the target in `jump_target`.
  `time_to_world_ready_after_jump_ms` and `frames_to_world_ready_after_jump` are measured from the
  jump to the start of the next two-frame ready run (`jump_issued` says whether it happened). A
  target cell with no terrain logs a warning and uses ground height 0. A coordinate outside ±512 is
  dropped with a warning when the arguments are parsed. The jump is ignored, with a warning, when
  `--auto-fly-speed`, `--streaming-fixture`, `--shots` or `--acceptance-screenshot` is also set,
  because the first two keep driving the camera, a shots run poses it for each shot and the
  screenshot anchors streaming on the start cell. It is also ignored, with a warning, in a synthetic
  `--benchmark-only` run, which loads no world to jump in, and in a `--material-fixture`,
  `--terrain-water-fixture`, `--transform-bounds-fixture`, `--renderer-fixture` or
  `--physics-fixture` run, which creates no streaming runtime. Without `--benchmark-frames` or
  `--benchmark-duration` the jump still runs but no report is written, so it warns that it will not
  appear in one. A jump run is an automated camera path either way: it has the free camera and no
  walking player, whose camera follow would put the camera back at the body.
- **Falling behind at speed.** With `--auto-fly-speed` set, `fly_lag` records the horizontal distance
  from the camera at which each model finished loading: `models_ready`, `ready_within_one_cell`
  (within 4096 units), and `ready_distance_p5` and `ready_distance_min` in units. A model that
  finishes close to the camera arrived late. The count and the minimum cover every model; the
  percentile covers the first 200,000 distances, and `p5_sample_size` says how many it used. `peak_arming_queue_depth` is the largest number of
  models waiting to be armed at once.
- **Frame times inside the loading windows.** `frame_ms_worst` mixes one-off startup frames (Bevy
  blocking on its upscaling pipeline, 150 ms and more) with the hitches that matter, those while cells
  load during play. Two windows separate them, each as `{ frames, p99_ms, worst_ms, over_33ms,
  over_50ms }` (warm-up frames included; `null` for a window
  with no frames). These two windows take their frame deltas from the real (wall) clock, not the
  virtual `Time` the report's own `frame_ms_*` use: Bevy clamps the virtual delta to 250 ms by
  default, so a one-second hitch would read as 250 ms, while the real delta is unclamped. A frame's
  delta is the interval that ended at that frame, so a window's first
  entry covers the work that led up to the frame that opened it. `frames_after_ready` covers every
  frame after the first world-ready latch, so startup is excluded; in a jump run it covers the jump's
  loading window too, since the jump is issued on the latch frame. It applies to fly runs too.
  `jump_load_window` covers the frames from the one
  the jump is issued on up to and including the frame the world is ready again after it (to the end of
  the run if it never is); it is `null` when no jump was issued. Both windows are kept in benchmark runs only
  (`--benchmark-frames` or `--benchmark-duration`) and are `null` otherwise. The same numbers appear in the profile
  `summary.md` beside the other headline lines.
- **Per-stage commit costs.** Spans `streaming/spawn_references`, `streaming/terrain_validation` and
  `streaming/terrain_seam_weld`, next to `streaming/spawn_cell` and `streaming/cell_commit`. The
  meaning of `streaming/terrain_mesh` changed in this PR: it used to be recorded once per quadrant
  against a clock started on the line before it, so it always read zero; it is now one real sample
  per committed cell covering all four terrain quadrant meshes and their colliders (a cell without
  terrain records nothing). Each
  committed cell also logs a `spawned references=N model_loads=M` timeline event, followed by its
  `committed` event with the whole commit time.
- **Main-schedule time and asset arrivals.** Spans `main_schedule/<Name>` time each Bevy main
  schedule, and `assets_added/{mesh,image,standard_material,world_asset}` count the `Added` asset
  events seen that frame. Both are kept as distributions (the bundle summarises the per-frame
  samples away), so the largest burst and the worst frame can be compared but not matched frame by
  frame. Installed only when the run measures pacing: a benchmark run, or a `--benchmark-jump` run.
- **Pipelines on first use.** `render_pipelines` counts, per render frame, the pipelines (render and
  compute: Bevy's `PipelineCache` holds both) newly queued
  in the cache (`created`) and the ones that stopped waiting (`became_ready`). The
  cache's waiting set holds pipelines that are queued and pipelines still being created
  asynchronously, so `became_ready` counts pipelines that finished creating and pipelines that
  failed for good; a pipeline that is retried stays waiting. It gives the mean render thread time on
  frames with pipeline activity against the rest, and the ten slowest render frames with their
  counts, so a spike frame can be matched with pipeline creation. Render frames are recorded only
  after the warm-up (`--benchmark-warmup-frames`, 60 by default), so the burst of pipelines built
  while the world first loads is not in them. To see it, run with `--benchmark-warmup-frames 0`.
  The cache is counted only when the run measures pacing, like the schedule timings.

Examples: a jump to a far cell (first load included in the render frames), and a fast fly:

```powershell
cargo run --release -p engine -- --assets D:\SkyrimConverted --grid-x 0 --grid-y 0 `
  --benchmark-jump 40,12 --benchmark-warmup-frames 0 --benchmark-duration 30 `
  --benchmark-output target/pacing-jump.json --run-label pacing-jump

cargo run --release -p engine -- --assets D:\SkyrimConverted --grid-x 0 --grid-y 0 `
  --auto-fly-speed 5000 --benchmark-duration 30 `
  --benchmark-output target/pacing-fly.json --run-label pacing-fly
```

## Interpretation

For native function names, library/driver stacks and thread waits, see the
[native profiling guide](../contributing/native-profiling.md). It covers Samply, Firefox Profiler,
Metal HUD captures and the limits of CPU/GPU timing interpretation.

Compare identical scenario, resolution, release profile and hardware. Start with frame P95/P99,
then inspect the top CPU spans, GPU passes and streaming timeline in the same run. A missing GPU
counter means unsupported instrumentation, not a zero value. Real-asset and target-hardware sign-off
remains an execution result, not something the repository can pre-certify.

## Load speed defaults: models per frame and IO threads

Once a converted model has loaded, the engine hands it to Bevy's scene spawner, at most
`--max-model-spawns-per-frame` models a frame (default 32, `0` = no limit). The asset IO pool keeps
its automatic size: a quarter of the hardware threads, at least 1 and at most 4
(`--io-threads <n>` overrides it). The arming limit was 4 a frame before.

Measured on a Ryzen 7 5800X (16 threads, release build) on a full conversion made with upstream
main's converter on 2026-10-02, with a benchmark that jumps the camera to cell 4,-21 once the start
area is loaded (the jump and the loading-window frame times come from the separate pacing
measurement change). "Worst frame while loading" is the worst frame from the jump until the world is
ready again.

| IO threads, models a frame | runs | world ready | ready after jump | worst frame while loading |
| --- | --- | --- | --- | --- |
| 4, 4 (before) | 4 | 1.77-2.00 s | 2.36-2.80 s | 14.7-20.4 ms |
| 4, 16 | 5 | 0.85-1.03 s | 1.10-1.14 s | 15.5-20.2 ms |
| 4, 32 (default) | 5 | 0.75-0.82 s | 0.82-1.01 s | 15.2-18.4 ms |
| 4, no limit | 2 | 0.75-0.76 s | 0.92-0.94 s | 16.2-17.0 ms |
| 8, 4 | 2 | 1.71-2.01 s | 2.42-2.73 s | 27.5-33.6 ms |
| 8, 16 | 3 | 0.80-0.95 s | 0.93-1.18 s | 26.3-38.0 ms |

A fly-through at 5000 units/s with stream radius 3 shows the same: the mean wait from a model's
load request to the model appearing falls from 692-704 ms at 4 a frame to 168 ms at 32, while the
worst frame (20.5 -> 22.2 ms), p95 (10.2 -> 10.8 ms) and peak memory (1.51 GiB both) barely move.
More IO threads did not load faster (the ranges overlap) and lengthened the worst loading frame, so
the automatic size stays; the load was waiting on the arming limit, not on IO.

The [archived 2026-10-08 Metal investigation](../research/metal-performance-fixes-20261008.md)
records a separate batching candidate, not a speedup established by this diagnostics change.
See [measurement validity](../research/profiling-measurement-validity-20261008.md) for observed
resolution, diagnostic timestamps and optional completed-frame draw counts.
