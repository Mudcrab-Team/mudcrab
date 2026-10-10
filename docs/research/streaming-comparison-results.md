# Static versus adaptive streaming: Mac results, October 10, 2026

Keep **static 64** for the current Mac test app. Adaptive mode consistently delayed complete CPU settlement and nearby streaming work. It reduced long-frame excess during rapid travel, but did not demonstrate a normal-travel benefit. It fails the default-selection rules fixed before the campaign.

The launcher's current settings already use static 64, with spatial priority enabled. Adaptive pacing remains available for experiments. The primary comparison uses static ceilings of 64 scene jobs, 32 activations and 16 MiB uploads per frame, versus adaptive ceilings of 128, 96 and 32 MiB with the safeguard package enabled. Actual adaptive budgets can be smaller.

We completed 32 main native launches. The analysis uses five matched normal-route pairs, five fast-route pairs and three clean three-mode control blocks. Three original control runs remain visible but excluded because another Cargo build overlapped one arm. Four earlier smoke launches are separate. See the [protocol](streaming-comparison-protocol.md), [analysis notes](streaming-comparison-analysis-notes.md), [per-run data](streaming-comparison-runs-20261010.csv) and [interactive results](streaming-comparison-results-20261010.html).

## Normal travel

This route uses the pack's forward-run speed of 370 units/s for 45 seconds, then a 10-second stationary tail. Both modes start after the same CPU gate and five uninterrupted quiet seconds. All comparisons use the same native binary, pack, view and route.

| Measure | Baseline median | Treatment median | Paired difference, 95% CI | Pairs |
| --- | ---: | ---: | --- | ---: |
| Launch to CPU settlement | 5.38 s | 8.24 s | 2.93 [2.61, 3.19] s | 5 |
| Long-frame excess above 33.33 ms | 0.627 ms/s | 0.782 ms/s | 0.430 [-0.340, 20.082] ms/s | 5 |
| Movement frame p99 | 28.29 ms | 27.73 ms | -0.34 [-0.80, 17.55] ms | 5 |
| Sampled peak process RSS | 2.538 GiB | 2.692 GiB | 0.154 [0.106, 0.181] GiB | 5 |
| Missing committed-cell exposure | 1.341 cell·s | 6.328 cell·s | 4.701 [3.410, 10.571] cell·s | 5 |
| Pending model exposure | 81.4 model·s | 619.5 model·s | 540.2 [430.7, 614.8] model·s | 5 |

Adaptive startup was 55.1% slower by the median paired percentage, with a pointwise 95% interval of 46.7–59.4%. Its slightly smaller median p99 does not establish smoother normal travel: the paired interval crosses zero and includes a substantial regression.

Two valid adaptive runs had movement p99 values of 45.70 and 43.55 ms, with worst frames of 148.55 and 208.18 ms. They remain in the estimates. Their causes are unproven, and they make the normal-pacing intervals wide. The matching static runs had p99 values of 28.16 and 30.54 ms. We have no evidence to attribute these outliers specifically to CPU, GPU, storage or the controller.

Exposure measures delayed CPU work: integrate the missing or waiting count over real seconds. One cell-second can mean one desired cell awaiting commitment for one second. These observations do not establish drawing or pixel coverage. Using ratios of the displayed medians, adaptive produced about 4.7 times the missing-cell exposure and 7.6 times the pending-model exposure. Every normal pair increased both measures.

## Rapid travel

The stress route travels at 3,000 units/s for 20 seconds, then waits for 10 seconds.

| Measure | Baseline median | Treatment median | Paired difference, 95% CI | Pairs |
| --- | ---: | ---: | --- | ---: |
| Launch to CPU settlement | 5.29 s | 7.78 s | 2.49 [1.85, 2.76] s | 5 |
| Long-frame excess above 33.33 ms | 7.950 ms/s | 3.464 ms/s | -5.105 [-32.772, -0.417] ms/s | 5 |
| Movement frame p99 | 36.38 ms | 33.11 ms | -3.10 [-8.43, -1.55] ms | 5 |
| Sampled peak process RSS | 2.757 GiB | 2.854 GiB | 0.106 [0.071, 0.130] GiB | 4 |
| Missing committed-cell exposure | 4.983 cell·s | 61.763 cell·s | 56.909 [43.211, 70.180] cell·s | 5 |
| Pending model exposure | 395.9 model·s | 5190.5 model·s | 4746.3 [3867.0, 5976.6] model·s | 5 |

Adaptive reduced hitch excess in every stress pair; the median paired reduction was 5.11 ms/s. It also delayed startup in every pair. Missing-cell exposure rose about 12.4 times and pending-model exposure about 13.1 times by the displayed medians. The stress pacing benefit therefore comes with a readiness penalty that blocks adoption under the protocol.

All runs cleared their recorded CPU queues and desired-cell deficits by the end of the prescribed tail. The delays were temporary; this benchmark does not test persistent transparent pixels.

## What the controls explain

The primary contrast changes caps, safeguards and adaptation together. The control blocks use the same 64 scene-job, 32 activation and 16 MiB upload ceilings throughout. `controlled64` adds the cost catalog, estimated memory reservations, backlog gates and preparation observer; `adaptive64` adds the adaptive policy to that package.

For **static 64 → static 64 with safeguards**:

| Measure | Baseline median | Treatment median | Paired difference, 95% CI | Pairs |
| --- | ---: | ---: | --- | ---: |
| Launch to CPU settlement | 5.21 s | 6.35 s | 1.09 [1.04, 1.31] s | 3 |
| Long-frame excess above 33.33 ms | 0.464 ms/s | 1.264 ms/s | 0.800 [0.530, 1.829] ms/s | 3 |
| Movement frame p99 | 28.25 ms | 27.97 ms | -0.28 [-1.33, 1.09] ms | 3 |
| Sampled peak process RSS | 2.536 GiB | 2.730 GiB | 0.195 [0.139, 0.221] GiB | 3 |

For **static 64 with safeguards → adaptive 64**:

| Measure | Baseline median | Treatment median | Paired difference, 95% CI | Pairs |
| --- | ---: | ---: | --- | ---: |
| Launch to CPU settlement | 6.35 s | 9.09 s | 2.74 [2.29, 2.89] s | 3 |
| Long-frame excess above 33.33 ms | 1.264 ms/s | 0.333 ms/s | -0.932 [-1.988, -0.504] ms/s | 3 |
| Movement frame p99 | 27.97 ms | 27.84 ms | -0.25 [-1.47, 0.70] ms | 3 |
| Sampled peak process RSS | 2.730 GiB | 2.685 GiB | -0.045 [-0.066, -0.035] GiB | 3 |
| Missing committed-cell exposure | 1.579 cell·s | 7.299 cell·s | 5.628 [1.404, 6.023] cell·s | 3 |
| Pending model exposure | 82.4 model·s | 545.3 model·s | 463.0 [367.5, 471.0] model·s | 3 |

The safeguards added about 1.09 seconds to startup and 0.195 GiB to sampled peak RSS by the paired medians. Adaptation added another 2.74 seconds at identical caps. It reduced hitch excess relative to that safeguard-only control, while further delaying cell and model work. Three pairs support this attribution for the tested package; they do not establish a universal effect for either feature independently.

In the first clean control block, the final scene job dispatched at roughly 4.98 seconds for plain static, 5.30 seconds with safeguards and 8.67 seconds with adaptation on the profiler clock. Spawning the full model and LOD set finished only 16–75 ms before full trace CPU readiness, so this selected delay was not predominantly a validation tail.

The first main pair shows why partial readiness can look encouraging. From the initial request, adaptive reached 95% model readiness in 1.94 seconds and static in 1.88 seconds. The remaining 5% took another 5.22 seconds with adaptation, versus 0.055 seconds with static. Most models reached CPU readiness quickly in both modes, while adaptive left the last portion behind.

Adaptive 64 paused ordinary intake for 5.83 seconds before full CPU settlement in that block. For 4.45 seconds it had queued scene demand and no active scene jobs. It repeatedly left recovery with small drain budgets as the two-second upward ramp restarted. This records policy backoff. Whether reduced upload service causes a preparation-backlog feedback loop remains a hypothesis; actual GPU execution and upload bytes were not measured. No recorded memory denial or memory-pressure frame explains that selected startup delay.

## Decision and evidence limits

Adaptive fails both startup and nearby-readiness guards. Normal hitch excess does not meet the required benefit threshold, while the faster stress pacing cannot substitute for normal travel. Splitting phase-boundary frames against the exact real-time route gives the same decision.

This result applies to an Apple M1 Pro with 32 GiB RAM, AC power, requested VSync and a 3200×1802 physical output. Thermal samples were nominal; sampled swap usage stayed at 965.75 MiB. The host was preserved, including unrelated workloads. The resumed runs had no observed build or converter overlap in the process monitor, sampled about every two seconds.

Each launch used a fresh process with OS caches preserved. Smoke runs and input hashing warmed some inputs. These are not cold-cache or first-shader-launch results. The `quick` binary and existing Metal terrain compatibility patch are identical in every arm; this is a relative test of the current app build. Release performance, other hardware and a player camera with collision need separate qualification.

RSS was sampled at 1 Hz, so peaks are lower bounds and full GPU memory is not measured separately. One stress adaptive run had a zero terminal RSS sample; the strict analyzer leaves its RSS estimate unavailable, giving four stress RSS pairs. An original excluded control run had the same issue. Memory safety outside this tested route is not established.

Intervals resample whole run pairs 10,000 times with seed `20261010`. Five primary pairs and three control pairs give coarse pointwise intervals, with no adjustment for multiple endpoints. Differences are treatment minus baseline. The paired median difference can differ from the difference between the two run medians shown in the tables.

Independent validation covered all 110,799 trace frames: matching binary/input identities, no trace drops or recorded functional failures, correct real-time routes and rebases, the common startup gate, and empty terminal queues. The maximum route-position error was 0.000244 world units. Engine tests passed (500); comparison tests passed (24); formatting and engine Clippy with warnings denied passed.

## Reproduce and inspect

The measured native binary has SHA-256 `f79b1db0eb14e25155c9812759c705b03366cf822e4dd9a3281212de327227a7`. Its receipt records base commit `5ac1bc92b7018f8f627c1ff8c4d2d27a5e24d697`, source overlays and the native sampler patch. Raw logs, reports, full traces, RSS, host snapshots, the frozen protocol/runner and the preserved original manifest remain in `/private/tmp/mudcrab-streaming-comparison-main-20261010`. A verified copy of the main and smoke artifacts, native build sources and binary, cost catalog and thermal-probe source is saved locally at `target/streaming-comparison/evidence-20261010.tar.gz`. The archive is ignored by Git; its SHA-256 is `9c0592b34fb0e17836de12dbc0b2bbaba107562d6d465eeaaa0058ba64dd9e59`. The report, selected CSV and interactive results are versioned separately.

Build cleanup removes the temporary compiler caches, staged app copies and `/private/tmp/mudcrab-streaming-comparison-build-20261010`; it preserves the installed app and evidence archive. To restore the measured binary, extract that archive member into its original directory. The retained run manifests use absolute paths. If the raw bundles, cost catalog or probe source are also absent, restore their corresponding members first.

```sh
tar -xzf target/streaming-comparison/evidence-20261010.tar.gz \
  -C /private/tmp mudcrab-streaming-comparison-build-20261010

swiftc /private/tmp/mudcrab-thermal-state.swift \
  -o /private/tmp/mudcrab-thermal-state
```

The archive includes the thermal probe's source, not its compiled executable. The results describe the archived binary; subsequent parent integrations and documentation changes are checked separately and were not benchmarked again.

To repeat on this Mac, use a new output directory and the matching pack/catalog:

```sh
python3 -B scripts/compare-streaming-modes.py \
  --engine /private/tmp/mudcrab-streaming-comparison-build-20261010/engine \
  --assets /Users/taylor/Projects/mudcrab/modern_assets \
  --costs /private/tmp/mudcrab-streaming-costs-20261010-02.json \
  --output /private/tmp/mudcrab-streaming-comparison-repeat \
  --commit 5ac1bc92b7018f8f627c1ff8c4d2d27a5e24d697 \
  --thermal-probe /private/tmp/mudcrab-thermal-state
```

To regenerate the analysis:

```sh
python3 -B scripts/analyze-streaming-comparison.py \
  --experiment /private/tmp/mudcrab-streaming-comparison-main-20261010/experiment.json \
  --output-dir /private/tmp/mudcrab-streaming-comparison-main-20261010/analysis \
  --bootstrap-samples 10000 --seed 20261010
```

The archive receipt is `target/streaming-comparison/evidence-20261010.tar.gz.receipt.json`; it records 2,170 members and 78,022,816 bytes.
