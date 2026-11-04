# Performance loop: measure, change, compare

`scripts/perf/perf.py` runs the engine on fixed scenarios, compares a base binary with a candidate
binary statistically, and compares screenshots, so a "measure, then optimise" loop can run without
a person watching. It is Python 3.11, standard library only. It does not change the engine: the
reports it reads are the ones [the profiling bundle](../roadmap/02-profiling.md) already writes.

```sh
python3 scripts/perf/perf.py scenarios          # list scenarios
python3 scripts/perf/perf.py preflight          # is this machine fit for timing runs
```

## Scenarios

`scripts/perf/scenarios.json` holds the scenarios of `scripts/phase2-baseline.ps1` with the same
flags and grids (`synthetic`, `rural`, `dense` = Riverwood at grid 3,-12, `dense-legacy`, `water`,
`stress`, `stability`, `travel`, `check`), plus `terrain-water` (`--terrain-water-fixture`) and
`stress-still` (the stress stream radius without the fly-through). `synthetic` and `terrain-water`
need no converted data; the others need `--assets`, and the harness adds
`--allow-incomplete-assets` because the real pack's manifest says `complete=false`. Route
scenarios run at least as long as the route takes plus 60 s, as the baseline script does. The
harness adds the benchmark, profile and acceptance flags itself and neutralises the acceptance
thresholds, so a slow frame never turns a measurement into a failed run.

World scenarios default to 300 warm-up frames: on this Mac Riverwood is ready after about 90
frames, and the default of 60 would put the world load in the measured window. A run whose world
became ready after the warm-up, or never did, carries that as a warning.

## One run

```sh
python3 scripts/perf/perf.py run --engine bins/engine-main-64108a1c --scenario dense \
  --assets modern_assets_real --out evidence/dense-1 --duration 20
```

The output directory gets `report.json` (the benchmark report), `bundle/` (the profile bundle),
`stdout.log`, `stderr.log` and `run.json`: the command line, the binary's SHA-256, how the run
ended and any warnings. Timing runs pass `--hidden-window` (`--visible` turns that off).

The engine sometimes writes its report and then never exits (issue #525, a hang in Bevy's
pipelined-render teardown; the same run can also exit non-zero after a complete report). The
watchdog polls the process, treats a parseable report as "the run is done", gives the process
`--grace` seconds (20) to exit and then kills its process group. A run that never writes a report
is killed at `--timeout` (duration + 120 s). Classes:

| class | meaning | usable |
| :-- | :-- | :-- |
| `ok` | the process exited after a complete report | yes |
| `hung_after_report` | the report was complete, the process had to be killed | yes |
| `timeout_no_report` | killed at the timeout without a report | no |
| `failed` | exited, or could not start, without a report | no |

A non-zero exit after a complete report stays `ok` with a warning. The engine exits non-zero when
one of its own gates fails (for example `commit_budget_respected`), and the numbers are still a
measurement.

## Comparing two binaries

```sh
python3 scripts/perf/perf.py compare --base bins/engine-main-<sha> --cand bins/engine-<branch>-<sha> \
  --scenarios synthetic,dense --pairs 5 --assets modern_assets_real --duration 30 \
  --out evidence/perf/<topic>
```

Runs are interleaved in pairs and the side that goes first alternates (base, candidate; candidate,
base; ...), so thermal drift and background load land on both sides. For each metric the harness
takes the candidate/base ratio inside each pair, the median of those ratios, and a seeded
bootstrap 95% interval of that median (10,000 resamples of whole pairs, `--seed`).

A verdict is `improved` or `regressed` only when the interval excludes 1.0 **and** the median
effect is at least `--min-effect` (0.02). Otherwise it is `noise`. Times are lower-is-better;
`average_fps` is higher-is-better. Metrics: frame time mean, p50, p95, p99 and fps; the mean of
every GPU pass that reports a non-zero time (Metal's timestamp queries often read all zero, and
those passes are skipped); the means of the eight most expensive CPU spans and schedules; process
memory peak when the report has one. The scenario's summary verdict is drawn from the four frame
time metrics only; the others are listed when they are not noise. A pair is dropped when either
run is not usable, and a verdict needs at least three usable pairs.

`verdict.json` has everything (including per-pair ratios and every run's class); `summary.md` is
the table to paste. Exit code 3 means refused.

### Refusals

The comparison refuses a scenario, with a named error in `verdict.json`, when:

- `fingerprint_mismatch`: any run's fingerprint differs from the others in anything except
  `commit` and `dirty_worktree` (resolution, backend, adapter, graphics settings, schema versions,
  build profile). Mixing a hidden and a visible window changes the resolution, so both sides always
  use the same mode.
- `inventory_mismatch`: the median mesh entities, terrain patches or water surfaces differ by more
  than 2% (`--inventory-tolerance`, with a slack of 2 for tiny counts). A candidate that draws less
  is not faster.
- `too_few_valid_pairs`: fewer than two pairs had a usable run on both sides.

`preflight` refuses the whole run (exit 3) unless `--force`: on battery, a thermal speed limit,
Low Power Mode, a running `cargo` or `rustc`, another engine process, or under 5 GiB free. On
other systems the machine checks are skipped. A non-empty `--out` also needs `--force`.

### Noise floor

Run `compare` with the same binary as base and candidate. Every verdict must be `noise`; the width
of the intervals is the smallest effect that setup can resolve. Do this once per machine state and
scenario, and again whenever the numbers look odd.

On an M4 Pro with a 20 s window, 3 pairs and 300 warm-up frames, the Riverwood `dense` interval for
`frame_ms_mean` was 0.86 to 1.07 and for `synthetic` 0.94 to 1.06: with three pairs only changes of
about 10% or more can be told from noise. The cure is more pairs (5 to 9) and longer windows, not a
smaller `--min-effect`. Of three pairs of `synthetic`, one run was `hung_after_report`, and one
`dense` run never reached world-ready; both are in the run warnings, so check them before trusting
a close call.

## Screenshots

```sh
python3 scripts/perf/perf.py shots --engine bins/engine-main-<sha> --shots scripts/perf/shots/riverwood.json \
  --assets modern_assets_real --out evidence/shots/main
python3 scripts/perf/perf.py shots --engine bins/engine-<branch>-<sha> ... --out evidence/shots/branch
python3 scripts/perf/perf.py visual --base evidence/shots/main --cand evidence/shots/branch \
  --out evidence/shots/diff [--noise-floor evidence/shots/noise/visual.json]
```

`shots` always uses a visible window. On macOS `--hidden-window` can render uniformly black
frames; expect a window to appear for about ten seconds. `scripts/perf/shots/riverwood.json` has
four 1280x720 poses around Riverwood (Tamriel, grid 5,-12 and 5,-11). The file format is in
[reference shots](../specs/engine/reference-shots.md).

`visual` compares matching PNGs. It uses NVIDIA FLIP (mean, p99, fraction of pixels over 0.1, and a
heat map beside `visual.json`) when `flip-evaluator` and numpy are importable, and otherwise its
own metric: mean absolute difference on the 0-255 scale and the fraction of colour samples that
differ by more than 8, with the PNG decoded in pure Python (8-bit, non-interlaced). `visual.json`
records which metric ran. The pure-Python path takes about a second per 1280x720 pair.

A shot fails when a frame is blank (uniform, or near black), when it is missing or undecodable,
when the sizes differ, or when a metric exceeds its limit: the noise floor for that shot plus a
margin (0.5 and 0.005 for the fallback metric, 0.005 and 0.005 for FLIP; `--margin-mean`,
`--margin-fraction`). Capture the same binary twice and pass that `visual.json` as `--noise-floor`;
without one the margin is the whole limit. Identical file sizes across all shots of one side mark
a suspected black run and put every shot on the review list. `review` in `visual.json` is the list
to look at with your own eyes (or an image-reading agent) before accepting a change; a pass means
"within the noise", not "looks right". Water animates and has no fixed phase flag on this binary, so
expect water shots to need a noise floor.

Two captures of one binary on this Mac gave a mean difference of 0.001 to 0.005 on the 0-255 scale
and almost no pixels over the threshold.

## Building a candidate

```sh
python3 scripts/perf/perf.py build <ref> --worktree-root scratch/worktrees --bin-dir scratch/bins
```

Resolves the ref to a SHA and reuses `<bin-dir>/engine-<sha8>` when it exists. Otherwise it makes
a detached worktree, runs `CARGO_INCREMENTAL=0 cargo build --locked --release -p engine --bin
engine` there with the worktree's own target directory (any inherited `CARGO_TARGET_DIR` is
dropped), copies the binary and removes the worktree and its target (`--keep-worktree` keeps
them). It refuses with under 15 GiB free. `--dry-run` shows what it would do. A build takes a
while and `target/` is 9 to 20 GiB; check the disk first.

## Caveats

- Use AC power, nothing compiling, and the same Mac for base and candidate. `preflight` checks
  most of this.
- Hidden windows are fine for timing, not for pictures.
- Do not point two worktrees at one `CARGO_TARGET_DIR`.
- Compare binaries built the same way (release, same toolchain). The fingerprint's build profile
  is checked, the compiler is not.
- Each run is also a streaming run: a world scenario's first seconds load cells. The warm-up
  covers that; check the run warnings if you shorten it.

## Agent loop

1. Baseline. Build `main` into `bins/`, run the noise floor for the scenarios you care about, and
   keep `verdict.json`.
2. One hypothesis, from the profile bundle (`bundle/cpu-spans.json`, `gpu-passes.json`): "the
   `render/prepare` span costs 1.4 ms because ...".
3. One change, in its own worktree and branch, with its own target directory.
4. Build it with `perf.py build`, then `compare` against the base binary, at least the scenario the
   change targets plus one it should not touch.
5. If `compare` refuses, read the error. A scene-inventory refusal usually means the change
   altered what is drawn.
6. If the verdict is `improved` or `noise` with a plausible win, run `shots` for both binaries and
   `visual` with a noise floor; look at everything on the `review` list.
7. Record the result: scenario, pairs, medians, interval, verdict, visual outcome, commit. Keep
   failed and noise results as well as wins; a note that something made no difference saves the
   next attempt.
8. Only then open a PR, with `summary.md` and the before/after images as evidence.
