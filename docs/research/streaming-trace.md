# Streaming trace semantics

Phase 1 of #202 adds opt-in observations of the existing streaming pipeline. It does not change asset admission, instance quotas, upload budgets, terrain handoff or movement policy. Enable CPU, queue, camera and completion-latency collection with `--profile-output <dir>`. GPU inventory is disabled by default; enable the separate expensive diagnostic with `--profile-gpu-inventory --profile-output <dir>`. The GPU flag alone does not enable collection.

The new artifact is `streaming-frames.json`, with `format_version: 1`. It is separate from the existing benchmark report (version 7), metadata (version 1) and two-column measured-only frame CSV. The bundle is written when a benchmark finishes; use `--benchmark-frames` or `--benchmark-duration` to export the trace. Collection outside a benchmark currently remains in memory. Preserve the profile bundle's run provenance when comparing captures: candidate revision, asset-pack identity, engine/backend, settings, hardware and cache conditions. A fresh process does not establish a cold filesystem cache.

## Frame correlation

Each sample identifies a `main_frame`, elapsed time, actual frame delta and benchmark window. The frame clock advances in `First`, before streaming work; timeline events and the sample taken in `Last` use that ID. `delta_ms` is the real interval since the preceding frame boundary, including warmup and intervals above virtual time's cap. It is not the duration of the enclosing frame's CPU spans. Warmup, measured, settlement and outside-benchmark frames remain distinguishable. Camera motion is an observation, not an inferred controller state. Camera positions use stable runtime Y-up coordinates alongside the render-origin grid; a rebase must not appear as player movement.

`cpu_spans_ms` sums recorded synchronous durations by name within the main frame. Parent and child spans can overlap, so summing every name does not produce total CPU time. These durations do not measure asynchronous I/O completion or GPU execution.

`completion_latencies_ms` separately groups reported asynchronous durations, including database queue wait, query and total request latency, and `assets/model_ready`. The enclosing frame is when the duration was reported, not the interval in which the work executed. Summed latencies are neither an end-to-end critical path nor a total of concurrent work. Existing aggregate CPU-span reports retain their prior meanings.

An embedded GPU observation has its own `source_main_frame` and `render_observation_seq`. The source frame is the request extracted into the render world; the sequence counts actual inventory observations. Requests retain a per-frame clock, but inventories are sampled only at source frame 1 and multiples of 8. The latest observation can lag by both this cadence and pipelined rendering, and successive main samples can contain the same observation. Compare source identities before calculating changes. Do not align render observations or shader timing to the enclosing main frame without evidence.

`collection_ms` records CPU elapsed time for the inventory scan and observation preparation, before bridge publication. It is not GPU execution time or the enclosing main-frame duration. Headless observations leave it unavailable. This measures the collector's own cost and does not establish zero profiling overhead.

## Counts and readiness

With GPU inventory disabled, the sample's GPU value is `null`; this means no observation was collected, not that zero resources were ready. The report's `gpu_inventory_enabled` field records whether the diagnostic was enabled.

Scene counts refer to observed CPU scene asset IDs. Repeated placements can share one scene ID. These counts are not unique GLB file counts, dependency counts, decode-job counts or placement counts. CPU-loaded means the observed scene and recursive CPU dependencies loaded; it does not prove scene validation, GPU preparation or drawing. An inactive ID means its load state is absent, not that physical memory was released. Requests reconcile current load state, asset messages update affected IDs and pending scenes are polled for recursive failures. Bevy does not emit a typed scene failure event for every dependency failure. Terminal dependency states are not continuously reconciled during hot reload; use this trace for fixed-load campaigns rather than as a complete live dependency monitor.

Streaming snapshots retain the existing counters' meanings. Lifetime validated placement totals can exceed current residency after unloading. Queue depths and resident counts are current observations; they should not be compared as though they were cumulative totals. Attached static-collider counts do not prove Rapier backend registration, active collision or safe movement.

GPU fields describe aggregate render-resource inventories sampled after material bind-group preparation:

| Field | Meaning |
| --- | --- |
| `mesh_descriptors` | Present `RenderMesh` records |
| `meshes_allocator_resident` | Records whose vertex and required index allocator ranges cover their logical counts |
| `prepared_images` | Present `GpuImage` records |
| `prepared_material_records` | Present prepared-material records |
| `material_bind_groups_ready` | Material records with an available allocator slab and bind group |

Inventories include resources beyond one streaming placement and may include engine-generated or shared resources. No inventory count establishes which placement is ready, successful draw coverage, pipeline compilation, completed GPU execution or resident byte usage. Exact placement coverage and collision readiness remain separate contracts.

`null` counts mean a required observation resource was unavailable. Zero means the collector observed an empty inventory. `render_available: false` reports the absence of the render sub-app; headless operation does not become GPU-ready. Before the first observation, the entire GPU value can be absent. Do not replace these unknown or unavailable signals with zero-ready conclusions.

## Bounds and overhead

A Fiji debug diagnostic with approximately 64,041 mesh descriptors measured inventory scan p95 around 25–28 ms and a maximum around 54 ms. These observations explain why GPU inventory requires a separate switch even with an eight-source-frame cadence. Enabling it can change frame pacing; captures with it enabled must not be used as though they were uninstrumented FPS comparisons. CPU trace collection also has overhead.

The frame and lifetime scene-ID capacities are each 20,000. The report records `sample_capacity`, `scene_capacity`, `dropped_samples` and scene identity-tracking completeness. Samples stop accumulating at their cap; retained samples are a prefix of the run, not a rolling window. `dropped_samples` continues increasing after the frame cap. When new IDs cannot enter the full ledger, `identity_tracking_complete` becomes false. Rejected observation calls can repeat the same ID, so their count is not the number of distinct missing identities. Incomplete identity tracking must remain visible in analysis.

The GPU bridge retains one request and one latest observation. It holds no asset handles and does not prolong resource residency. Sampling scans render-resource inventories rather than placement hierarchies. Its CPU cost grows with inventory size; enabling profiling therefore has overhead and is not proof of unchanged performance. Count bounds limit retained records, not a precise byte allocation ceiling.

Retain existing settled visual and complete-load checks alongside startup traces. Evaluate frame pacing together with useful coverage and collision evidence: lower frame times accompanied by longer missing buildings do not establish an improvement. This phase provides measurements for later controller work and claims no performance gain.

[PR #122](https://github.com/Mudcrab-Team/mudcrab/pull/122) covers ready-window aggregate pacing. This trace is a separately versioned addition and retains benchmark format 7 until that work merges; reconcile the overlapping measurements when integrating it.
