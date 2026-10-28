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
- Metadata records scenario, run, hardware label, worldspace and grid, and the run's fingerprint
  (`fingerprint`, format version 2). The fingerprint is written once, in both the bundle and the
  benchmark report, and the fields it carries (commit, dirty-worktree state, build profile, window
  resolution, engine and Bevy versions, stream radius) are no longer repeated beside it.

## Run fingerprint, per-pass GPU time and memory

A report is only comparable to another run that ran the same thing. These three sections, and the
`fingerprint` object in `metadata.json`, say what a run actually was; all of them are installed only
when the run records measurements (`--benchmark-frames` or `--benchmark-duration`, or
`--profile-output`). An ordinary play session adds no startup system, no diagnostic plugin and no
per-frame sampling for any of them.

### Fingerprint

`fingerprint` appears in the benchmark report (report `format_version` 11) and in the bundle's
`metadata.json` (format version 2):

- `commit`, `dirty_worktree` — the `--profile-commit` / `--profile-dirty-worktree` values.
- `engine_version`, `bevy_version`, `build_profile` — `release` or `debug`, from the build itself
  (`cfg!(debug_assertions)`) and nothing finer: cargo's `quick` profile inherits `release` (Cargo.toml
  marks it "not for measuring"), so a `quick` build reports `release`, and an engine report from any
  other cargo profile that says `release` cannot be told apart from a real `release` run — although
  `phase2-baseline.ps1` only ever runs the `release` engine binary, so a `quick` build is never
  measured by that command.
- `renderer` — the adapter Bevy selected: `backend` (`Vulkan`, `Dx12`, `Metal`, …), `adapter`,
  `driver`, `driver_info`, `device_type` (`DiscreteGpu`, …), and the device features the GPU
  diagnostics depend on (`timestamp_queries_supported`, `pipeline_statistics_supported`). `null`
  for a run with no renderer.
- `resolution` — the primary window's physical pixels as the run actually opened it, `null` for a
  headless run. Compare runs at the same resolution: it changes every pixel-count-bound number.
- `world_database_schema` — what this engine build expects (`shared::WORLD_DATABASE_SCHEMA_VERSION`),
  and `opened_world_database_schema` — what the database the run opened actually says in its
  `schema_info` table (`null` when the run opened none, e.g. the synthetic benchmark).
  `cell_cache_version` is the layout `cell_cache.rkyv` must match; a cache of any other version
  fails to open.
- `graphics` — the effective settings, after `--ini` files and the command line have both been
  applied: `stream_radius`, `unload_radius`, the three terrain LOD block distances and
  `split_distance_mult`, `max_upload_mib_per_frame`, `lights`, `headless`, and the window's
  `present_mode`. This engine keeps no separate graphics-settings type; `EngineConfig` is where a
  run's render-affecting settings live.

### `gpu_passes`

Per-pass GPU time over the measured frames (after the warm-up), from Bevy's render diagnostics:
`mean`, `p95`, `worst` and `total` (with `count`, `p50`, `p99`) for each pass path,
`measured_frames` (the frames of the measured window the numbers rest on), plus
`timestamp_queries_supported` and `pipeline_statistics_supported` from the device. The GPU
readback lags a frame or two behind the frame that ran the pass, so the first measured samples
can describe passes of the last warm-up frames (a measurement already in flight when the window
opens is kept; the warm-up's own samples are consumed and dropped). Every observation Bevy
records is taken exactly once, not resampled from its latest value, so a pass's `count` is its
real sample count: larger than `measured_frames` when several cameras ran the pass, smaller when
the GPU readback lagged or the pass did not run every frame; `phase2-baseline.ps1` marks a pass
whose count is below its window's `measured_frames` for exactly that reason.

A path is Bevy's own: `render/<pass>/elapsed_gpu`, where `<pass>` is the label the render pass was
created with — `main_opaque_pass_3d`, `tonemapping`, `upscaling`, `ssao`, `clustering`, and so on.
Two things to know when reading one:

- Several cameras that run the same pass write the same path, so a row covers every camera that ran
  it, not one camera. Compare cameras by rendering them in separate runs.
- Shadow views are labelled per light and cascade
  (`shadow_directional_light_<light>_cascade_<cascade>`, `shadow_point_light_<light>_<face>`,
  `shadow_spot_light_<light>`), but Bevy's standard shadow node does not open a diagnostic span for
  them (only its meshlet path does), so those passes are normally absent from the report rather than
  zero. A missing row means "not measured here". Bevy 0.20 (0.20.0 was released 2026-10-08) adds more
  GPU tracing spans (bevy PR #25398, merged 2026-08-19): one around the auxiliary-view loop (a point
  light's six shadow views, a spot light's one) and a `per_view_shadow_pass` span whose documentation
  says it renders the directional light shadows. This engine is on Bevy 0.19 and measures none of
  them yet, so cascade and point/spot shadow GPU time needs a frame capture (RenderDoc or PIX) until
  it moves to 0.20.

When the device cannot time GPU work — any backend but Vulkan and DX12 — `passes` is empty and
`unavailable.elapsed_gpu` carries the reason; a run with no renderer at all gets "no renderer in
this run" for both `elapsed_gpu` and `pipeline_statistics` instead of a backend complaint. An empty
`passes` on a device that *can* time GPU work says no pass was recorded, which is also a reason
rather than a zero. Pipeline statistics (the per-pass shader-invocation counters the bundle keeps in
`gpu-passes.json`) are a separate device
feature and are described under `unavailable.pipeline_statistics` when absent. Never diff numbers
across backends.

### `memory`

Split by where the memory is:

- `cpu.mesh_bytes` and `cpu.image_bytes` — the CPU-side bytes of the mesh and image assets, summed
  once at the end of the run (not per frame): each mesh's vertex attributes (element size times
  vertex count) plus its indices, and each image's pixel data. `meshes` and `images` say how many
  assets were counted, and `meshes_without_cpu_data` how many meshes Bevy extracted to the render
  world without keeping a main-world copy (a `RenderAssetUsages::RENDER_WORLD` mesh whose GPU data
  was taken). Bevy 0.19's glTF loader keeps meshes in both worlds by default
  (`GltfLoaderSettings::load_meshes` is `RenderAssetUsages::default()`) and the engine does not
  override it, so streamed models do keep main-world mesh data; only those render-world-only meshes
  contribute no bytes to `mesh_bytes`.
- `gpu.mesh_allocator_slab_bytes`, `gpu.mesh_allocator_slabs`, `gpu.mesh_allocator_allocations` —
  Bevy's mesh allocator: the byte size of its slabs (the one true byte figure here) and how many
  slabs and mesh allocations are live.
- `gpu.render_asset_counts` — how many GPU-side assets are live, by type: `mesh`, `image`,
  `material`. These are **counts, not bytes**: wgpu reports the number of live resources and the
  engine can read no vendor VRAM counter. `material` is one shared count, not one per material
  type: every `MeshMaterial3d<M>` prepares into the same `PreparedMaterial` map, so Bevy reports
  the same number for any material type it is asked about.
- `gpu.unavailable` — a reason for every counter that was not sampled; the mesh allocator and
  render-asset counters stay `null` until the render world has actually measured them (Bevy publishes
  their atomics in main-world `PreUpdate` from the first frame, at zero), `gpu.measured_frames` says
  on how many measured frames they were sampled, and a run with no renderer says "no renderer in
  this run" rather than claiming the diagnostic was not sampled (a run that rendered no frame, for
  instance). Counters are never zero-filled.

`gpu.mesh_allocator_*` and `gpu.render_asset_counts` come from Bevy's mesh-allocator and
render-asset diagnostic plugins, which are installed only for measured runs. They are gauges, not
distributions: the report keeps their last value in the measured window, and only once the render
world has measured them (until then they are `null`, as above). The process-level memory figures stay where they were
(`peak_process_memory_gib`, `process_memory_growth_gib` and the bundle's `memory.json`).

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
  -RuralGridX 0 -RuralGridY 0 -DenseGridX 3 -DenseGridY -12 `
  -WaterGridX 7 -WaterGridY -2 -Repetitions 3
```

Results go to `target/profiling/<timestamp>-<cpu>-<gpu>`. The runner uses medians to reduce noise.
Pass `-UpdateBaseline` to write `profiling-baseline.json`, which stores each scenario's `place`
beside its medians — its worldspace and grid, or `synthetic` for a scene that runs in no world
place. A stored entry with no place (a baseline written before places were recorded) or one measured
in another place describes a different scene — `dense` moved from 18,-5 to Riverwood — so that
scenario's comparison is skipped with one warning line naming the scenario and the reason, and the
campaign is not failed for it; re-run once with `-UpdateBaseline` to record places. Against an
existing baseline, material
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
`format_version` 11) and spans and events in the profiling bundle. They are installed only when the
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
  jump to the start of the next two-frame ready run (`jump_issued` says whether it happened). A jump
  into a cell whose window is already resident still latches only after those two ready frames, so it
  reports a near-zero reload; that is correct and not a measurement bug. A target cell with no terrain
  logs a warning and uses ground height 0. A coordinate outside ±512 is
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
  with no frames). These two windows take their frame deltas from the real (wall) clock. Bevy
  clamps the virtual delta to 250 ms by default, so a one-second hitch would read as 250 ms, while
  the real delta is unclamped. A frame's
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

## Scene counts and CPU time in the report (format version 11)

The benchmark report (`acceptance.json`, and the same value as the bundle's `frame-metrics.json`)
carries two more sections, and every field in them is `null` plus a reason rather than a zero when
the run could not measure it.

- **`scene`** — what the frame drew. `entity_count` is Bevy's own entity-count diagnostic;
  `mesh_entities`, `terrain_patches` and `water_surfaces` are the engine's scene inventory, sampled
  every 60th frame and kept only from a frame inside the measured window (a count never sampled
  there is `null`, never the warm-up's number, so a synthetic run's real zero terrain patches and a
  missing sample are told apart). `pipeline_statistics_supported` is the
  device's answer, and on a device that has pipeline statistics (Vulkan; DX12 offers none) the
  section lists per pass `vertex_shader_invocations` and `clipper_primitives_out`, each a
  distribution over the measured frames, with `measured_frames` giving the frames those numbers rest
  on; a run with no renderer gets "no renderer in this run" as the `pipeline_statistics` reason.
  `draw_calls` is always `null`:
  `unavailable["draw_calls"]` says "Bevy 0.19 exposes no draw-call count", because no Bevy, wgpu or
  engine API counts them (bevy issue #17445 asks for one; Bevy 0.20 does not add it).
- **`cpu`** — where the frame's CPU time went, in milliseconds. `schedules` is one summary per Bevy
  main schedule (`main_schedule/<name>`, the eight `schedule_timing` times), `spans` is one per
  span the engine records by hand, the `streaming/*` spans among them (the schedule summaries are
  not repeated in `spans`, and the `assets_added/*` counts are in neither: they are counts, not
  milliseconds, and stay in the bundle's `cpu-spans.json`). Each summary carries count, total,
  mean, p50, p95, p99 and worst. `render_world_prep` is always `null`: Bevy 0.19 has no diagnostic
  that times the render app's schedules (`bevy_diagnostic` ships no schedule timer and
  `RenderDiagnosticsPlugin` records only per-pass spans), so the report says so instead of
  inventing one. What does exist for render-world work is the report's own `render_world` section
  and the `render_world/*` spans in `cpu.spans` (main world, extract, the wait for the render
  thread, the render thread's phases), and per pass `render/<pass>/elapsed_cpu` in the bundle's
  `gpu-passes.json`. The two kinds of span cover different windows: the `streaming/*` spans are
  recorded on every frame of the run, warm-up frames included, while the `render_world/*` spans come
  from the render-timing collector, which records only while the measured window is open, so those
  describe the measured frames alone.

## The baseline command

`scripts/phase2-baseline.ps1` runs the fixed scenes on whatever branch is checked out and writes a
small `baseline.json` and `baseline.md` for it:

```powershell
pwsh -File scripts/phase2-baseline.ps1 -Assets D:\SkyrimConverted            # 3 repeats of the six baseline scenes
pwsh -File scripts/phase2-baseline.ps1 -Assets D:\SkyrimConverted -Scenarios stress -Repeats 5 -DurationSeconds 180
pwsh -File scripts/phase2-baseline.ps1                                       # synthetic only: no -Assets needed
```

The world scenes use the same places as `docs/roadmap/02-acceptance.md` in Tamriel (`0x3c`), with
the references within two cells of each: rural at grid 0,0 (about 1,050), dense at 3,-12 (Riverwood,
about 2,900, the densest cell neighbourhood in Tamriel) and water at 7,-2 (about 1,580).
`-RuralGridX`, `-DenseGridX`, `-WaterGridX` and their `Y` partners override them. Rural and dense
are the same default in all three commands; water is not: `phase2-baseline.ps1` defaults it to 7,-2,
and the acceptance campaign's documented command line passes 7,-2, while `phase2-profile.ps1` keeps
its own water default of grid 0,0 and `phase2-acceptance.ps1` shares that 0,0 default — the place
its own rural scene already measures. A water run is this standard place only when the command it
runs under is given these coordinates, and a stored baseline for another place is not compared (see
**Comparing baselines** below).

`dense-legacy` is the old dense place, 18,-5 (about 1,530 references; `-DenseLegacyGridX`/`Y`). It
is accepted by every script but is not in `-Scenario all` or the default sets, and is kept for one
release, then removed. Baselines and
`profiling-baseline.json` results made before this change are not comparable for `dense`, which is
now a different, about twice as dense place. The scripts do not map an old `dense` entry to
`dense-legacy`: compare an old `dense` median with a new `dense-legacy` median by hand.

Both scripts need PowerShell 7 (`pwsh`): they read reports with `ConvertFrom-Json -AsHashtable`.
The command builds the engine once (`cargo build --release --bin engine`, skipped with `-NoBuild`,
and the binary is looked for under `CARGO_TARGET_DIR` when that is set), prints the number of
windows and the expected duration before starting, and reuses `phase2-profile.ps1`'s scenario
definitions and engine invocation (that file is dot-sourced with `-AsLibrary`, so the two commands
cannot drift apart).

- **Windows.** The route scenarios `travel` and `check` get one window, the moving one, which runs
  for the longer of `-DurationSeconds` and the route's travel time plus a 60 s allowance for the
  world to load before the route starts (the route cannot begin before that). Every
  other scenario gets a standing-still window. A scenario that moves — `stress` and
  `stability`, which fly the camera — gets a second, streaming window: the same scene once with the
  camera left in place and once as the scenario defines it. That is how steady frames and streaming
  frames are reported separately; nothing in the engine's CLI measures both in one run. Every
  engine run is labelled `--run-label "baseline <scenario> <n>/<repeats>"`, and the streaming
  window adds `streaming`.
- **Scenarios.** The default set is `synthetic`, `rural`, `dense`, `water`, `stress`, plus `travel`
  when `-Assets` is given (see "Travelling and the frame budget" below). `stability` is left out
  because it is a 30-minute soak, not a baseline, and `check` because it is the held-out route;
  `-Scenarios all` includes both, and `-Scenarios` takes any of the names `phase2-profile.ps1` knows
  and the two route scenarios `travel` and `check`. Without `-Assets` the bare
  default runs `synthetic` only, the one scene that needs no converted data, because the default is
  a convenience rather than a request. An explicit request is not narrowed that way: naming a world
  scenario without `-Assets` fails, and `-Scenarios all` without `-Assets` fails with that reason
  instead of quietly running the synthetic scene alone.
- **Fresh reports.** Every campaign writes its bundles to its own timestamped directory,
  `-Out/runs/<timestamp>-<hardware>/`, so a `run-N` is never a file an earlier campaign left
  behind. A repeat counts only when the engine wrote a report this invocation can identify as its
  own — the format version this script reads, a timestamp at or after the run started, measured
  frames, and the run id and scenario in the bundle's `metadata.json` — and the engine runs from
  the repository root, as the profiling campaign always has. Anything else throws with the
  scenario, repeat and directory rather than being aggregated.
- **`baseline.json`** (`format_version` 3). The campaign level carries what every window shares —
  `commit`, the machine, the repeats, the window lengths, the run directory, and a `fingerprint`
  holding only the fields that must be the same in every window (engine and Bevy versions, build
  profile, resolution, renderer, world-database and cell-cache schemas). The script throws when two
  windows disagree on any of those. Each scenario window then carries its own full `fingerprint`
  and its `scenario_inputs` — worldspace, grid, effective stream radius, duration, warm-up frames, and the
  workload itself: the `fly_speed` the scenario flew at (`null` when the camera was left standing
  still), the `synthetic_instances` it rendered, and the resolved full path of the `assets` set it
  loaded. The window comparison checks every one of them, so a rerun at another flight speed,
  another instance count or against another asset set is refused rather than scored against a
  different workload. The data set's identity is among them: `assets_identity` holds the size and
  last-write time of `skyrim_world.db`, the size of `cell_cache.rkyv` (a re-conversion or another
  plugin set rewrites both) and the `configuration_hash` and `schema_version` of the
  `conversion-manifest.json` (the converter's settings). They are `null`, with the reason in
  `assets_identity_unavailable`, when the window loaded no data set or a file could not be read, so
  two runs whose `assets` path is the same but whose data is not are refused too. Format 3 added those fields and the `measured_frames` below; a
  format-2 file does not carry them and cannot be compared with a run of this script — nor can a
  format-3 file written before `assets_identity` existed, which the window comparison refuses as a
  missing input.
  The measurements follow: frame metrics (count, mean, p50, p95, p99, worst, average FPS) as
  median/min/max over the repeats, `gpu_passes.passes` (per pass, the median over the repeats)
  with `gpu_passes.measured_frames` (the frames the pass samples rest on, median/min/max over the
  repeats), `gpu_passes.pass_samples` (each pass's own sample count, so a count below
  `measured_frames` — GPU readback lag, or a pass that did not run every frame — is visible in the
  file) and `gpu_passes.unavailable`, `memory` (the process figures, `cpu` asset bytes, and the
  `gpu` counters with their own `measured_frames`) with `memory.gpu.unavailable`, `scene` (the
  counts, `measured_frames`, and the per-pass pipeline-statistics counters) with
  `scene.unavailable`, and `cpu` (per schedule and per span) with `cpu.unavailable`. Every
  map takes the union of its keys across the repeats and each entry keeps `samples`, the number of
  repeats that measured it, and `runs`, the expected repeat count. A pass or span only one repeat
  recorded is still visible with its partial coverage. A window a
  scenario does not have is `null` with the reason in the scenario's `streaming_unavailable`.
  Nothing is zero-filled here either: a counter the report left `null` stays `null`. When the engine
  ends a repeat on a failed acceptance gate the window records `acceptance`: `runs_failed_gates` (the
  repeats whose report says `passed: false`), `runs` (the repeat count) and `failed_gates` (the sorted
  union of the `thresholds` names whose value is the boolean `false`); a baseline run measures through
  an engine gate rather than failing on it, because the report a gated run wrote is still a
  measurement, and only a report that cannot be shown to be this repeat's (missing, stale, malformed,
  zero frames, another run's) throws.
- **`baseline.md`.** One short table per scenario window, whose "median (min-max over N runs)"
  cells are exactly what `baseline.json` holds, with `samples/runs` beside each measured value,
  including GPU passes and the lagging-pass list; a short memory table (CPU asset bytes in MiB, the
  GPU counters as counts with their unit); the slowest GPU passes and CPU spans; and every
  counter a window could not measure with the reason the report gave for it. When GPU pass times
  are missing the row prints the GPU reason, not the CPU-schedule one. The measured frame counts
  are printed beside those tables — "Measured frames" for the GPU passes, the pipeline statistics
  and the GPU counters — and every pass whose sample count is below its window's measured frames is
  listed under the table as "fewer samples than frames (GPU readback lag or a pass that did not run
  every frame)". Under the table of a window whose `acceptance.runs_failed_gates` is above zero, one
  line names the gates and the runs: "Acceptance gates failed in N of M runs: <names>. The numbers
  above are still measurements of those runs." — the numbers stand, they are just read with the
  failed gate in view.
- **Comparing baselines.** With `-CompareTo <baseline.json>` (or, without it, an existing
  `baseline.json` in `-Out`; a `-CompareTo` path that does not exist is refused rather than
  ignored), the new numbers are set against the old ones using the profiling campaign's noise
  floors. The baseline picked up from `-Out` was not asked for, so it is checked before the first
  window and, when it is unreadable, of an older format or otherwise unusable, it is skipped with
  one warning line and the campaign runs without a comparison; `-CompareTo` names a comparison the
  user asked for, so a file like that is refused there instead. A requested comparison is refused
  before the remaining windows are run when the two runs differ in any field that changes the
  numbers: the machine — the campaign-level `hardware` label (CPU, GPU and driver) and the
  fingerprint's renderer backend, adapter and driver — resolution, build
  profile, graphics settings, world-database and cell-cache schemas, or any window's scenario
  inputs (flight speed, synthetic instance count and the data set included). The data set is checked
  twice: as the resolved `assets` path, and as its own identity (`assets_identity`, above), so a
  re-conversion or an added mod at the same path is refused rather than scored as the same workload.
  An asset-backed window with `assets_identity_unavailable` on either side is also refused,
  even when both paths and unavailable reasons match. This check runs before timed windows:
  explicit `-CompareTo` throws, while an implicit baseline is dropped with a warning. Synthetic
  windows without assets need no data-set identity.
  The identity is a file identity, not a content hash: copying the same data set to another folder
  keeps the sizes but changes the last-write time, so the copy is refused too, which is the safe
  direction; rerun the baseline there to compare. A `baseline.json` written before its windows
  carried `assets_identity` is refused by that check rather than compared against; rerun the
  baseline once to write a new one. A different **commit**
  is expected and allowed — comparing a branch against the baseline
  it was measured from is the point of the command. `-Out` defaults to `target/phase2-baseline`, so
  a rerun compares with the previous baseline by default; each window's report bundle stays under
  `-Out/runs/`, and `baseline.json`'s `run_directory` says which campaign's bundles belong to it.
  A metric is regression-scored only when both sides measured it in every repeat (`samples`
  equals that campaign's `repeats`), including complete one-repeat campaigns. Partial medians
  remain visible under the additive `comparison.unscored` map and as `n/a` comparison rows with
  coverage and a reason; they produce no regression verdict. A metric no repeat measured stays
  omitted, without a fabricated zero. Existing `comparison.metrics` keys remain unchanged;
  `comparison.unscored` groups bare metric names by scenario/window.
- **CI.** Nothing here runs in CI: the `performance` job is converter-side and has no GPU, and only
  the manual self-hosted runner can run even the synthetic scene. A baseline is a local,
  same-machine measurement.

## Travelling and the frame budget

**The goal.** The portal demo should hold 160 frames per second (6.25 ms a frame) while the player
travels. A mean or a p95 cannot show that: one 40 ms hitch every few seconds hides in both. The
baseline therefore measures a moving scene and reports the frame-time distribution against a budget.

**The pass bar (proposed).** At least 99.9% of the frames of a travel window are within the budget
(6.25 ms) and no frame is over twice the budget (12.5 ms). `--frame-budget-ms` changes the budget
(default 6.25); the report says whether the window met the bar (`meets_pass_bar`) for whatever budget
it was given. The bar is a proposal, not a decision.

**The routes.** `--route <file>` makes the camera follow a route file (`scripts/routes/`): waypoints in
Skyrim world units, a speed in units per second (Skyrim running is about 400), an eye height above
the terrain (default 120), optionally `"loop": true` (the last leg returns to the first waypoint).
The camera moves along straight legs at that speed, faces the direction of travel, and stays the
eye height above the terrain. The terrain height is read from the converted height field of the
cells along the route before the run, so following the ground costs nothing while measuring; a
frame with no terrain under the camera keeps the previous height and is counted in the report's
`route.route_frames_without_ground`. (It is the height field the terrain mesh is built from, and
not a physics ray: a benchmark run builds no terrain colliders, and adding them would change what is
measured.) The route starts when the world is first ready, so the walk is not the cold load of the
start cell (`"start": "immediately"` skips the wait). The start frame stands at the first waypoint,
so the time spent loading moves nothing; the walk begins on the next frame. From then on the runner
never waits for loading, never slows down and hides nothing. The route ends the benchmark window when its last
waypoint is reached, so the report covers the route, and the distance travelled follows the real
clock, so a hitch costs the camera its distance. Frame times are scored from the first full frame on the
route (the benchmark's warm-up frames are not trimmed on top of that). The engine knows no route: the files are data, and `-Scenarios travel` or `check` only names a
file.

- `scripts/routes/travel.json` (`-Scenarios travel`): about 23,800 units at 400 units per second, a
  minute, from the start of the Riverwood road in grid 4,-12 north-east over the hills towards
  Whiterun, across 8 cell borders (9 cells). Its waypoints come from the world database's terrain
  and water heights: every point is above its cell's water level, and the route avoids a river bed
  that the straight line along the road would cross.
- `scripts/routes/check.json` (`-Scenarios check`): a different place of about the same length, the
  farmland around Rorikstead in the west (grid -13,-10 north to -14,-5, 6 cell borders). **It is the
  held-out check route: nobody tunes against it.** It is run and reported next to `travel`, and it is
  never used to choose a setting, a threshold or a budget; if a change improves `travel` and worsens
  `check`, that is the finding. Its waypoints change only if the converted data changes under them.

**The report.** Every benchmark report (report format 11; the new fields are additive) carries
`frame_budget` for the measured window (a route run: the route's frames) and `route` (`null` without
a route). The frame times `frame_budget` scores are real-clock deltas. The older `frame_ms_*` fields
(with `average_fps` and the duration limit) keep their clock: Bevy's virtual clock, which clamps a
frame at 250 ms, on a run without a route, and the real clock on a route run, which differs from the
virtual one only for frames longer than 250 ms.
A `--route` file run measures the route's own frames: the older `frame_ms_*` fields and
`frame_budget` both cover the frames from the first full moving frame on the route (the report names
that window in `frame_budget.scope`), so the wait for the world to become ready and the frame the
route starts on are in neither. A run whose route has not finished when its window closes (its frame
limit, its time limit, or the real-time deadline, which counts from the start of the process) reports
`route.completed` as false and fails acceptance. A route run's `frame_ms_*` fields read the real clock
(`Time<Real>`); a run without a route keeps the virtual clock it always had. `frame_budget` has: `budget_ms`,
`frames`, `window_seconds`, `p50_ms`,
`p99_ms`, `p99_9_ms` (nearest rank: the smallest frame time with at least that share of frames at or
below it; the older `frame_ms_p99` takes the higher of the two ranks around `(n - 1) * 0.99`, so the
two can differ by one rank), `worst_ms`, `frames_over_budget`, `share_within_budget`,
`over_budget_per_minute`, `frames_over_double_budget`, `meets_pass_bar`, and
`over_budget_owners`, the frames over the budget by owner. `baseline.json` and `baseline.md` carry
the same numbers as median/min/max over the repeats; the owner counts are per run, and
`runs_meeting_pass_bar` counts the repeats that met the bar. `baseline.json` stays at format 3: the
new fields are additive, and `scenario_inputs` gains a `route` entry (file name and SHA-256 of the
file) that is `null` for every other scenario, so older baselines still compare. `-FrameBudgetMs`
passes `--frame-budget-ms` to every window.

**Who owned an over-budget frame.** A frame time is the time from the start of one main-thread frame
to the start of the next, and with pipelined rendering (the Bevy default) the main thread runs frame
`n + 1` while the render thread renders frame `n`, then waits for it, so a frame is roughly the
larger of the two sides plus the extract. The engine keeps one small record per main-thread frame
(`render_timing::CycleSides`) with the main schedule, the extract, the render schedule and the
swapchain wait that ran in that cycle, and files the render frame under the cycle it ran in (the
next one when pipelined). The owner is the side with the largest time in the frame's cycle:

| Owner | What it is |
|---|---|
| `main_world` | the main thread: the main schedule plus the extract, less the streaming commit |
| `upload` | the streaming commit time of the frame (`streaming/frame_commit`), measured on the main thread, only on frames that committed a cell or LOD chunk |
| `render_thread` | the render schedule's CPU time, less the swapchain wait |
| `gpu` | the render thread's wait for a swapchain image (`render/swapchain_acquire`), where a GPU-bound or presentation-bound frame shows up on the CPU |
| `unknown` | no owner can be named: the cycle has no main-thread or render-thread record (the first measured frame, a run with no renderer), or no side reaches half of the frame (for example main-thread work and streaming upload each under half) |

Ties go to the first in that order. The limits are real. `gpu` is a lower bound: a render thread that
blocks in submit or present rather than in the swapchain acquire is counted as `render_thread`,
because those waits are not split out of `render/graph_and_present`. GPU pass timestamps
(`gpu_passes`) are not used for a per-frame owner: Bevy publishes them from a readback that arrives
a frame or more late with no frame number on it, so matching one to a frame would be a guess, and
the report does not guess. `upload` covers the streaming commit only; the renderer's own asset
preparation is on the render thread. The owner counts of a window sum to its `frames_over_budget`,
and `unknown` is counted, not hidden.

`over_budget_main_thread_wait` is a diagnostic beside the owners, not an owner: for the frames over
the budget that have the measurement (pipelined rendering only), the median and the largest time the
main thread waited for the render thread to hand the render world back
(`render_timing`'s `wait_for_render_thread`, filed under the main frame that waited), and how many
frames it covers. A frame owned by `render_thread` with a large wait is a busy render thread holding
the main thread; the same owner with a small wait means the main thread was not blocked on it. It is
`null` when no over-budget frame has the measurement.

## Interpretation

Compare the run fingerprints first: two runs whose `fingerprint` objects differ in anything but the
commit (different adapter, resolution, graphics settings or world-database schema) are not
comparable, and the fingerprint is written precisely so that difference is visible. A different
commit is not a difference in what was measured: it is the change a baseline comparison is meant to
detect, and `phase2-baseline.ps1` refuses to compare only when the machine, the build or the
workload differs. Then, with the scenario inputs, resolution, build profile and hardware the same,
start with frame P95/P99 and inspect the top CPU spans, GPU passes and streaming timeline in the
same run. A missing GPU counter means unsupported instrumentation, not a zero value. Real-asset and
target-hardware sign-off remains an execution result, not something the repository can pre-certify.

For native function names, library/driver stacks and thread waits, see the
[native profiling guide](../contributing/native-profiling.md). It covers Samply, Firefox Profiler,
Metal HUD captures and the limits of CPU/GPU timing interpretation.

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
