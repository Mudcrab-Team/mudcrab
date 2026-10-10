# Phase 2 Profiling

The profiling stage is implemented as an opt-in, reproducible campaign around the release engine.
It does not require or redistribute Skyrim assets for the synthetic scenario. World scenarios require
the user's converted, legally owned asset set.

## Captured data

Each run writes a self-contained directory containing `metadata.json`, `frame-metrics.json`,
`cpu-spans.json`, `gpu-passes.json`, `streaming.json`, `memory.json`, and `summary.md`.

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
