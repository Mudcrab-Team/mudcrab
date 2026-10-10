# Adaptive streaming and pipeline backpressure

This extends [scene priority and shared admission](streaming-priority.md) with downstream backpressure, estimated memory reservations and an opt-in startup/walking controller from [issue #202](https://github.com/Mudcrab-Team/mudcrab/issues/202). The fixed path remains the default.

## Controls

| Option | Default | Effect |
| --- | --- | --- |
| `--adaptive-streaming` | Off | Enable finite startup, cruise, quiet and recovery budgets; also enable spatial priority. |
| `--max-streaming-backlog <count>` | `0` | Set the downstream queue high watermark. `0` disables backpressure outside adaptive mode; adaptive mode uses `256`. |
| `--streaming-memory-mib <mib>` | `0` | Set the allowance for estimated streaming allocations. `0` disables reservations outside adaptive mode; adaptive mode selects half detected RAM, capped at `16384` MiB, with an `8192` MiB fallback. |
| `--streaming-costs <file>` | Pack metadata | Override the converter's `streaming-costs.json` file. This switch alone does not enable admission. |
| `--streaming-headroom-mib <mib>` | `2048` | Keep this much observed system memory available outside new streaming reservations. `0` requests no additional reserve. |
| `--streaming-frame-ms <ms>` | `16.67` | Set a positive finite controller frame target. Benchmark acceptance thresholds remain separate. |

Memory and queue controls can be enabled individually without adaptive pacing or spatial priority. In adaptive mode, zero memory/backlog values select the finite automatic allowances; they do not disable those protections. A nonzero memory allowance overrides the automatic choice. The configured system headroom can further lower the automatic allowance on smaller systems. MiB values that overflow the byte representation are rejected.

Existing scene, activation, cell commitment and upload limits remain ceilings. Legacy zero-as-unlimited settings are translated into finite controller ceilings when pipeline controls are enabled. Inside the controller, a zero count means paused work. An intake pause must never be passed to a legacy API as zero-as-unlimited. A wall-clock allowance is soft: a dispatch attempt can cross it so reconciliation does not prevent all progress.

## Intake and downstream work

The controller observes ready placements, already-spawned work, collision work, returned cell responses and available GPU preparation backlog. Cell-response counts include both the worker channel and deferred responses. Controlled cell requests are nearest first, limited per frame, and bounded across outstanding generations. Canceled queries retain their generated-cell reservation until their terminal response is consumed; revisiting the same cell waits for that generation to drain. Reaching a high watermark pauses ordinary new scene intake and new cell requests. Intake resumes only after observed queues fall below their low watermarks and remain there through a recovery hold.

Responses and loads already dispatched continue toward completion. Activation, collision work, cell commitment and uploads retain finite drain budgets. Queue pressure alone preserves useful drain throughput; it does not force a healthy renderer to process a large ready backlog two placements at a time. Timing or memory pressure reduces drain budgets until those signals recover through a separate hold. Healthy drain budgets then return even while a large queue keeps intake paused.

A watermark is a feedback threshold, not a strict maximum queue length. Already-dispatched jobs can finish together, and one shared scene can supply many placements. The job count, downstream queue feedback and byte reservations therefore serve different purposes.

Nearby collision demand receives bounded service while ordinary intake is paused. That service still obeys the finite job cap and resource reservations; it cannot bypass memory pressure. It is scheduling policy, not proof that a collider is installed or that walking is safe. Existing movement/collision guards and acceptance checks remain necessary.

## Memory estimates and ownership

Converter metadata identifies shared scene geometry and textures separately from per-placement ECS and collision allocations. Estimates include retained allocations and additional temporary work such as decoding, build scratch and CPU/GPU copies. Generated full-detail terrain, terrain collision and water require their own accounting; external shared textures are charged separately.

The catalog is tied to the exact conversion manifest fingerprint and assumes an immutable pack. Estimates carry their quality and assumptions. Missing, invalid or mismatched metadata is logged and replaced with unknown estimates. Each unknown resource or placement cost receives at least 64 MiB of retained allowance and another 64 MiB of temporary allowance rather than being treated as free. Runtime admission reads the metadata; it does not decode every model to discover its size. An old pack with many unknown assets can exhaust the estimated allowance before all scenery loads; generate matching metadata rather than treating missing costs as zero. Valid conservative estimates can also exceed the allowance: required resident work can remain missing until ownership releases memory or the budget changes. This implementation does not evict live scenery to make an undersized allowance converge.

Before dispatch, admission reserves both shared resources and all current placement ECS/collision costs. This prevents decoded scenes from consuming the entire allowance and leaving no space to instantiate them. A later subscriber must also fit its placement reservation before receiving the shared handle or entering activation. Failed reservations keep the request queued and cannot start asset I/O. Dispatch scans are bounded; failed candidates rotate through retries so a large request cannot repeatedly block cheaper requests behind it.

One shared resource reservation can serve several placements. A placement still owns its own transform, scene hierarchy, collision and lifetime. Removing the final placement does not release an in-flight job's allowance: orphaned work remains charged until completion, failure or confirmed cancellation. An absent loader state alone is not cancellation evidence. CPU completion and resource release are also distinct; retained assets and render work need continued ownership accounting. Abandoned resource reservations require confirmed loader/render absence before release. Temporary allowance is released only when every live owner has completed preparation.

The MiB allowance bounds the ledger's estimates. It is not a certified process-RSS, unified-memory or dedicated-VRAM limit. Allocator overhead, platform-specific copies, driver behavior and unmodelled generated work can differ from metadata. Available system-memory and process-RSS diagnostics provide another admission gate. Renderer preparation observations retain their source frame so stale results can be ignored. Unavailable GPU timing or preparation information remains unknown; an empty observation is not proof of zero GPU work.

## Startup, movement and recovery

Healthy startup uses the available finite burst ceilings immediately. It exits initial-startup mode when the initial usefulness heuristic or current demand settlement persists through a short hold. The current heuristic requires at least four validated terrain patches, at least 75% of admitted and queued scene jobs marked prepared and fewer than 64 CPU-ready placements waiting for activation. The readiness hold does not advance during recovery. It does not certify nearby drawing or collision readiness. A new midgame loading spike does not restart initial startup.

Stationary initial startup allows a larger CPU burst: moderate pressure requires streaming work of at least half the frame target, and streaming work at or above the whole target triggers immediate backoff. Walking, turning, the motion hold and all later loading retain the smaller quarter-target and half-target thresholds. Healthy stationary startup also receives a CPU allowance of at least half the configured frame target, bounded by its stage ceilings. This avoids limiting initial loading to the small headroom left over after normal rendering. Timing or memory pressure still selects the reduced drain budgets. Queue, memory and severe frame-spike gates still apply during startup.

Walking or turning immediately selects cruise ceilings. A motion hold prevents single stationary frames from raising the budget. After the camera remains still, quiet-mode budgets increase gradually. A teleport or camera/world replacement resets timing history and the quiet ramp; a render-origin rebase alone does not count as camera motion or a teleport.

The pure controller can smooth CPU and fresh GPU timing samples, while large current spikes can trigger immediate backoff. Current runtime integration supplies full-frame elapsed time and measured streaming CPU spans; separate GPU execution timing remains unavailable to the controller. Streaming CPU work is separated from recent non-streaming frame cost. A bounded rolling median supplies the recent baseline, so isolated short asynchronous frames do not hold its estimate below normal rendering cost. A bounded recent upper percentile supplies a separate tolerance for frame-only spikes, including the upper tail of asynchronous presentation timing. Moderate frame pressure requires current streaming CPU work, preventing stale CPU averages from attributing ordinary rendering variation to loading. The controller compares loading spikes with the recent baseline as well as the configured target, so a GPU-bound scene that naturally renders in 40 ms does not indefinitely stop loading because a 16.67 ms target is unattainable. Recovery waits for the pressure signals that caused entry to stay clear through the hold, instead of requiring the renderer to beat its normal baseline. The configured target and effective scheduling reference are both reported; benchmark acceptance is unchanged.

When frame pressure persists but downstream queues are low and memory allows admission, periodic one-job progress probes prevent ordinary scene loading from starving forever. These probes do not bypass memory or queue gates. Finite drain floors can allow work even when the base frame exceeds target; they are not a frame-time guarantee. Whole cell commits, model instantiation, collider construction and render-asset preparation remain indivisible at their existing operation boundaries.

## Validation and comparison

Compare the same built binary, immutable asset pack, camera path and graphics settings with controls disabled, independent backpressure/memory controls enabled, and full adaptive mode. Keep fresh-process invocations, binary hashes, estimated/reserved bytes, unknown-cost counts, actual process memory, controller decisions, queue depths and frame distributions. Report startup usefulness and complete settlement separately from benchmark acceptance.

Repeat both startup and movement runs. Inspect settled images and missing-building duration rather than inferring drawing from CPU readiness or renderer-path counters. Camera flight does not establish walking collision readiness. Retain black captures, timing outliers, failures and cases where a request cannot fit the budget; do not weaken acceptance thresholds to make the controller appear successful.

The deterministic controller tests cover high/low hysteresis, pressure recovery, hard ceilings, drain progress, camera motion, quiet ramps, stale/invalid timing, an unattainable base frame target and bounded collision/progress service. Those tests establish policy behavior. They do not establish performance gains, memory-estimate accuracy or runtime drawing/collision safety.
