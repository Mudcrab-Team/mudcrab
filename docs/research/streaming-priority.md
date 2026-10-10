# Streaming priority and scene admission

This implements queue priority and an initial admission bound from [issue #202](https://github.com/Mudcrab-Team/mudcrab/issues/202), phases 2 and 3. Both are opt-in. It does not yet implement the startup/cruise controller, memory reservations, a downstream backlog limit or a bounded residency cache.

## Switches and limits

| Option | Default | Effect |
| --- | --- | --- |
| `--prioritize-streaming` | Off | Order cell requests, unique scene dispatch and ready placement activation by relevant demand. |
| `--max-scene-loads <count>` | `0` | Limit outstanding unique `Scene(0)` jobs. `0` means unlimited. |
| `--max-model-spawns-per-frame <count>` | `32` | Limit ready placements handed to the scene spawner per frame. `0` means unlimited. |
| `--max-upload-mib-per-frame <mib>` | `16` | Keep the existing soft render-asset preparation allowance. `0` means unlimited. |

With both new switches at their defaults, the existing loading and FIFO activation path remains active. Either new switch enables shared scene admission for full-cell models and coarse terrain LOD. In that mode, both kinds of placement use the existing model activation quota. Setting a scene cap alone keeps sequence-based dispatch and FIFO ready activation; it does not enable spatial priority. Priority alone leaves scene admission unlimited.

The scene cap counts jobs still awaiting root or recursive CPU dependency completion. It does not limit individual GLB dependencies, decode scratch, queued ready placements, GPU uploads, resident bytes or frame duration. A count of 32 jobs is therefore neither a 32-asset memory bound nor a guarantee against running out of memory. The upload allowance remains soft because preparation starts whole assets.

## Request and activation order

Priority mode requests full-cell database metadata in increasing squared grid distance from the streaming anchor, with stable cell-key ties. Pending LOD metadata is also ordered by chunk distance with stable tier and anchor ties. These are request orders; asynchronous query and asset completion can still differ.

Scene dispatch and ready placement activation recompute priority from the current camera transform, including turns. Bounds and camera position use stable runtime Y-up world coordinates so negative grids and render-origin rebases do not change their meaning. Within the current relevant demand, ordinary choices rank:

1. Static collision candidates whose bounds surface is within one exterior cell, 4,096 runtime units, including candidates behind the camera.
2. Full-detail placements before optional coarse terrain chunks.
3. View-facing bounds before bounds behind the camera.
4. Larger projected bounding-sphere radius, then shorter distance to the bounds surface.
5. Older request sequence and stable coordinate ties.

The footprint estimate uses a forward hemisphere expanded by the bounding sphere. It is not a frustum test, pixel coverage measurement or file-size estimate. Large buildings can rank ahead of smaller nearby clutter when their bounds occupy more of the view. Coarse chunks have a separate optional lane so their broad horizontal bounds cannot dominate detailed buildings. Unknown bounds use point distance and direction without a size estimate.

Collision candidate priority applies to `STAT`, `TREE` and `FURN` references when interactive world physics is enabled. Automated benchmarks, screenshots and headless runs do not enable that physics path, so their scheduling captures do not exercise collision protection.

Every eighth completed dispatch choice services the oldest queued scene demand. Every eighth completed activation choice similarly services the oldest ready placement. These counters advance when work is served, independently for the two stages, rather than once per frame. This also gives coarse or unknown-bound demand service. Collision protection is priority, not proof that a collider exists, is registered or prevents unsafe movement; aged service can select another job.

Unstarted requests in retiring full cells cannot dispatch or activate. Revival makes the retained requests eligible again. Existing LOD generation and unload checks remove irrelevant chunk roots. Removing the last placement subscriber cancels queued scene demand, but an already dispatched job keeps its handle and occupies a slot until the loader reports a terminal state. An absent loader state alone does not confirm cancellation.

## Shared resources and failure ownership

One resource record represents a canonical converted scene path within the active asset pack. Its identity includes the absolute asset-root path and SHA-256 of `conversion-manifest.json`; LOD records also include the chunk content hash. Fixture packs without a manifest use a root-scoped `unmanifested` identity. These identities are captured at startup and assume the pack remains immutable during the run.

Placements subscribe to the record and receive the same strong scene handle. Shared scene priority uses the best relevant placement and the oldest subscriber's age. Sharing a resource does not merge placement entities, transforms, FormIDs, cell identity or LOD generation. This registry coordinates admission; Bevy already shares asset loading by path.

Once every request has a handle, no jobs are pending and the exact subscriber entity set is unchanged, admission skips rebuilding its ownership records. Request paths and identities remain immutable for each root's lifetime. Future in-place mutation support must invalidate the request key and settled ownership snapshot; camera turns still re-rank pending activation independently.

Successful root and recursive CPU dependency completion release a job's slot. Root or recursive failure also releases the slot while retaining the failed handle for live subscribers. Repeated ordinary demand does not restart a failed record. Unowned loading records remain tracked; unowned queued or terminal records can be removed. Ready records remain while placements own them, rather than entering a separately budgeted cache.

LOD keeps its existing build/content validation, hash verification, bounded retry policy and terrain fallback handoff. Hash verification starts after scene admission. An explicit retry can reset a failed resource only when none of its previous subscribers remains live. If the root loaded but a recursive dependency failed, the retry waits for the old loader record to leave before issuing a fresh request: an ordinary `AssetServer::load` can otherwise return the same failed root. This wait is observable and does not count as a newly dispatched job.

## Trace fields

The existing [streaming trace](streaming-trace.md) receives an additive `scene_admission` object when either new switch enables admission; the default fixed path records `null`. The trace retains `format_version: 1`, benchmark format 7 and the existing measured-frame CSV layout.

| Field | Meaning |
| --- | --- |
| `configured_job_limit` | Configured scene cap; `0` means unlimited. |
| `prioritization_enabled` | Whether spatial priority is enabled. |
| `active_jobs`, `queued_jobs` | Current unique dispatched nonterminal jobs and unadmitted scene jobs. |
| `records`, `subscribers` | Current shared records and their placement subscribers. |
| `orphan_active_jobs` | Dispatched nonterminal jobs with no remaining subscriber. |
| `dispatched_total`, `completed_total`, `failed_total`, `canceled_total` | Lifetime job transitions; completion means recursive CPU readiness. |
| `peak_active` | Largest observed outstanding-job count. |

Equivalent counts are exposed as `admission/*` gauges. `admission/cleanup_waiting_jobs` reports unique scenes waiting for failed-root cleanup. `streaming/admit_scenes` measures synchronous admission work, including reconciliation and state polling. `streaming/scene_admission_wait` records elapsed queue time when a subscriber receives its handle; it does not measure scene completion. Admission and CPU readiness do not establish GPU preparation, successful drawing or collision readiness.

The current production path does not report confirmed loader cancellation, so `canceled_total` remains zero. Orphaned loads stay active until success or failure is observed.

## Comparing modes

Use the same built binary, immutable pack, scene, graphics settings and upload/activation limits for both runs. Replace the three paths/revision below with the actual artifact and record its binary hash separately. Use distinct profile directories and preserve each invocation. A fresh process does not establish a cold filesystem cache.

```sh
STREAMING_ENGINE=/absolute/path/to/engine
STREAMING_ASSETS=/absolute/path/to/modern_assets
STREAMING_REVISION=replace-with-built-commit

"$STREAMING_ENGINE" --assets "$STREAMING_ASSETS" \
  --worldspace 0x3c --grid-x 5 --grid-y -12 --stream-radius 2 \
  --max-model-spawns-per-frame 32 --max-upload-mib-per-frame 16 \
  --max-scene-loads 0 \
  --benchmark-warmup-frames 600 --benchmark-frames 120 \
  --benchmark-output /absolute/path/to/profiles/fixed-report.json \
  --profile-output /absolute/path/to/profiles/fixed \
  --profile-scenario streaming-startup --profile-run-id fixed-1 \
  --profile-commit "$STREAMING_REVISION"

"$STREAMING_ENGINE" --assets "$STREAMING_ASSETS" \
  --worldspace 0x3c --grid-x 5 --grid-y -12 --stream-radius 2 \
  --max-model-spawns-per-frame 32 --max-upload-mib-per-frame 16 \
  --prioritize-streaming --max-scene-loads 32 \
  --benchmark-warmup-frames 600 --benchmark-frames 120 \
  --benchmark-output /absolute/path/to/profiles/priority-report.json \
  --profile-output /absolute/path/to/profiles/priority \
  --profile-scenario streaming-startup --profile-run-id priority-1 \
  --profile-commit "$STREAMING_REVISION"
```

The candidate cap of 32 is an example, not a selected default or a measured optimum. Add `--profile-dirty-worktree` when the binary includes uncommitted changes. The frame trace includes warmup and measured frames, so startup can be analyzed without discarding its first 600 frames. Keep the existing benchmark acceptance gates and inspect failures separately from functional completion. Remove `--prioritize-streaming` and set `--max-scene-loads 0` to restore the fixed path.

Repeat startup and movement captures, retain settled complete-load and visual checks, and report missing-building duration alongside frame pacing. Camera flight does not validate walking collision. This document describes scheduling behavior and makes no performance claim.

## Remaining work and bypasses

The shared job cap covers referenced full-cell GLB scenes and coarse terrain LOD scenes. Full-detail terrain material images and water textures still load directly; generated terrain/water meshes and terrain collider construction occur during cell commitment. Scene dependency requests and later per-placement collision work also sit outside this job-count bound.

Phase 3 still needs downstream backlog control and a bounded cache with release accounting. Phase 4 needs converter cost metadata and reservations covering dependencies, decode scratch, ECS/collision, generated terrain, render staging and residency, including these bypasses. Phase 5 adds the adaptive startup/cruise/recovery controller after those contracts and measurements exist. Current cell commit, retirement, model activation and upload settings remain independent fixed controls.
