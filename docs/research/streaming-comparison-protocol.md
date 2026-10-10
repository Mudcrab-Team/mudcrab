# Native streaming comparison protocol

This protocol is fixed before the main comparison runs. It tests whether adaptive streaming should replace the current static launch configuration on the Mac test machine. The primary comparison measures the complete configuration change; matched-cap runs then separate the effects of safeguards and adaptive pacing. Earlier startup captures inform this design but are excluded from its results.

The campaign uses one newly instrumented `quick` binary, including the native shared Metal patch, for every arm. This answers a relative policy question for the installed test build. The `quick` profile is not a release performance benchmark; a selected default still needs release-build qualification. Record the source revision, dirty-source state, binary SHA-256 and native overlay identity before the first run.

## Configurations and run order

All arms enable `--prioritize-streaming` and use the same immutable converted pack, cost catalog, graphics settings, physical window size, initial camera pose and route. Use `AutoVsync`, matching the installed app, through `--benchmark-vsync`. Previous `AutoNoVsync` captures are not controls for this campaign. Keep screenshots and GPU inventory scans disabled in pacing runs.

| Mode | Scene jobs | Activations/frame | Upload MiB/frame | Adaptive | Backlog limit | Estimated memory MiB |
| --- | ---: | ---: | ---: | --- | ---: | ---: |
| `fixed64` | 64 | 32 | 16 | Off | 0 | 0 |
| `adaptive128` | 128 | 96 | 32 | On | 256 | 16,384 |
| `controlled64` | 64 | 32 | 16 | Off | 256 | 16,384 |
| `adaptive64` | 64 | 32 | 16 | On | 256 | 16,384 |

Zero backlog and memory settings disable those controls in `fixed64`. The controlled arms share the same explicit cost catalog and 2,048 MiB physical-memory headroom setting. These are finite configuration ceilings: adaptive decisions can choose less, and whole-asset preparation can exceed the soft upload allowance. `fixed64` is the current static test configuration, not a description of every crate default.

Run four smoke launches first: one stationary pair and one short-route pair. Keep their artifacts, but exclude them from campaign estimates. The main schedule is:

- Five `fixed64` / `adaptive128` pairs on the normal route: 370 units/s for 45 seconds, followed by a 10-second stationary tail.
- Five pairs on the stress route: 3,000 units/s for 20 seconds, followed by the same 10-second tail.
- Three normal-route triplets containing `fixed64`, `controlled64` and `adaptive64`.

There are 29 main launches. Alternate AB/BA order within each primary scenario, with the initial order chosen by recorded seed `20261010`. Interleave normal and stress pairs. Rotate triplet order to give each mode each position once. Start each run in a fresh process and leave the same three-second idle interval between runs. Startup is measured within both primary scenarios and reported separately by scenario; no additional stationary campaign is needed.

The primary contrast changes caps, safeguards and adaptation together. `fixed64` versus `controlled64` measures the safeguard package at fixed caps. `controlled64` versus `adaptive64` measures adaptive pacing with caps and safeguards held constant. A high-cap static arm is a possible follow-up if attribution remains unclear; it is not added after inspecting results to rescue a preferred outcome.

## Common startup gate and camera workload

Pin worldspace `0x3c`, grid `(5, -12)` and stream radius 2. For this exact pack and starting window, the CPU gate expects 25 resident cells, 2,029 validated model placements, 176 resident LOD chunks, 100 validated terrain patches and 25 validated water surfaces. It also requires empty cell, scene, placement, LOD, raw-response and deferred-response queues, reconciled database requests, no retirement work, no recorded failures, and the intended renderer path. It does not consult the adaptive controller's startup mode or usefulness heuristic.

The camera waits for this common gate to remain true continuously for five seconds. Loss of CPU settlement resets the wait. Movement starts afterward, so startup frames cannot improve or worsen the movement result merely because one mode loads sooner. This wait does not certify GPU preparation, drawing or equal pixel coverage.

Use the opt-in `--streaming-benchmark-route-speed` driver. It computes position from absolute `Time<Real>` elapsed time, keeps orientation fixed, moves along world `-Z`, and preserves global coordinates through render-origin rebases. Both arms start 6,000 units above the initial terrain center with the same elevated view. Record the initial pose, phase transitions, route distance and end position. Expected travel is 16,650 units for normal and 60,000 for stress; a long frame must not shorten the route through capped virtual time.

The normal speed matches the converted pack's forward-run value of 370 units/s in [the movement record map](../player-movement-record-map.md). This remains camera flight at an elevated view. It does not exercise the player, terrain collision, building collision or walking safety. The stress route measures rapid turnover, rather than normal play.

Exit after the prescribed tail. The common external timeout is 240 seconds per launch. Keep failed, incomplete and timed-out runs with their logs and counters; unresolved demand at the deadline is a result, not permission to extend that arm's warmup.

## Measurements

Use [streaming frame traces](streaming-trace.md) with real frame intervals. Report startup, movement and tail separately. For each phase, retain frame p50, p95, p99 and worst, plus counts and rates strictly above 33.33, 50 and 100 ms. Include long stalls. Assign boundary-crossing intervals by the recorded phase at their ending frame consistently in both arms and retain the complete trace for inspection.

The primary movement pacing measure is hitch excess:

```text
hitch_excess_ms_per_second = sum(max(0, real_frame_ms - 33.33)) / observed_movement_seconds
```

Also report excess above the configured 16.67 ms frame target as a secondary measure. That target excess includes ordinary rendering cost when the machine cannot sustain 60 FPS; it should not be presented as isolated streaming cost. CPU span distributions help identify work, but nested spans cannot be summed into a CPU critical path. Real frame intervals include GPU waits and instrumentation overhead and do not isolate GPU execution time.

Measure process-launch-to-CPU-settlement as the primary startup latency, including cost-catalog initialization. Anchor the report's generated wall time and route elapsed time to the recorded process launch. Check wall versus monotonic elapsed time; a clock shift makes that startup measurement unavailable. Also report initial-request-to-model readiness, LOD readiness and complete CPU settlement, so initialization and pipeline progress remain distinguishable. CPU readiness is not evidence that every placement drew.

Record demand coverage alongside pacing. A low pending queue can result from paused intake. Measure missing desired-cell exposure against the actual camera window and pack inventory, pending placement exposure, peak deficits, and unresolved work after the tail. Desired-cell exposure is missing-cell count integrated over real seconds; pending-placement exposure is an aggregate queue proxy, not a pixel measurement. If the trace cannot establish current demand or visibility, report that measurement as unavailable. Do not infer complete rendering from cumulative readiness counts or a startup screenshot.

Sample the owned process's OS RSS externally at 1 Hz in every arm. Report sampled peak, movement/tail distributions and end-of-tail change. The sampled peak is a lower bound; RSS does not establish GPU memory usage or a physical-memory guarantee. Compare measured RSS across arms, never fixed-mode RSS against adaptive reservation estimates. For controlled runs, report estimated resident/transient peaks, memory refusals, pressure reasons and terminal transient/orphan charges separately. Zero temporary charges alone do not prove that all demand was served.

## Host conditions and retained evidence

Preserve filesystem caches, swap state and unrelated host workloads. Do not purge caches, reboot, stop services or terminate another player's process. Input hashing itself can warm metadata. Describe these as fresh-process, cache-preserved runs, rather than cold-cache runs.

Record AC/battery state, swap usage, load and available thermal status before the campaign and each launch, periodically during a run, and afterward. Unsupported thermal readings stay unknown. Run no concurrent Cargo build, converter or second game within the campaign; if one is already active, defer the launch and preserve that process. Record background changes and order effects instead of selectively discarding slow runs.

Retain the expanded launch arguments, per-run before/after binary and input hashes, stdout/stderr, report, frame trace, route state, RSS samples and host snapshots. Hash or route mismatches, truncated required traces and missing clock anchors invalidate the affected estimate, with the reason retained. Functional failures and deadline misses remain in the failure rate; completed-run performance estimates cannot erase them. A repeat receives a new run identifier and does not replace its original artifact.

## Analysis and default decision

Use runs as replicates. For each matched pair, report absolute and percentage differences, then the median paired difference and its 95% percentile bootstrap interval from 10,000 resamples with recorded seed `20261010`. Resample complete pairs within a scenario, not individual frames. Keep all pair values and run order visible. Five pairs give limited uncertainty estimates, particularly for rare stalls; pairing does not remove cache, thermal or background-work effects. The intervals are pointwise, with no adjustment for the two permitted benefit endpoints or secondary comparisons. They do not provide a family-wide confidence guarantee. Three ablation triplets support attribution but cannot establish broad hardware parity.

Apply this rule before choosing a default:

1. A candidate must pass the functional and recorded memory-safety checks. Crashes, renderer/lifecycle failures, persistent missing demand, unresolved tail work or confirmed harmful memory pressure block adoption. Missing coverage or memory evidence leaves that part of the decision open.
2. `adaptive128` earns the default only if its median paired reduction is at least 20% and 0.5 seconds in process-launch CPU settlement, or at least 25% and 1 ms/s in normal-route hitch excess. The other primary measure must not regress by more than 10%. Startup results from normal and stress runs must be reported separately and agree in direction; do not pick the faster scenario after the run.
3. Normal/stress readiness deficits and stress hitch excess must not materially regress: use the same 10% relative margin, with absolute differences shown. When the static baseline is zero, percentages are undefined; require no added readiness exposure, unresolved demand or hitch excess for that guard. Faster frames achieved by withholding nearby work do not qualify.
4. Confidence must support the benefit and the absence of a material regression. Show whether benefit intervals exclude no improvement and whether regression intervals remain within the 10% margin. Wide or contradictory intervals leave the choice inconclusive; they do not force a winner. The absolute cutoffs are project decision thresholds, not universal perceptual limits.

When both configurations pass and the evidence is tied or inconclusive, keep the simpler static configuration. If static fails a safety or functional gate, that tie rule cannot qualify it; evaluate the controlled static alternative on its own evidence. Publish functional completion, pacing, measured memory and drawing/collision limits separately. A result from this Mac and quick build supports that scoped choice, not an unconditional performance claim for release builds or other machines.
