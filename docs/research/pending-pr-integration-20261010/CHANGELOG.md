> Written with an AI assistant (Codex).

# What to expect from the pending-PR build

This changelog covers the 31 captured PRs integrated into `t3/merge-pending-prs`, plus the repairs that make them work together. It describes changes relative to the captured upstream `main`, not a finished Skyrim replacement. Twelve included PRs were drafts. Their features are available for evaluation; inclusion does not establish that their acceptance work is complete.

## Launch this build

```sh
cd /Users/taylor/.t3/worktrees/mudcrab/t3-366adc03
./target/quick/engine --assets "$PWD/target/runtime-assets-27" --grid-x 5 --grid-y -11
```

Use this fresh pack rather than `~/Projects/mudcrab/modern_assets`. The integrated LOD contract requires converter producer **27** and world database **9**. The older pack fails that startup check.

## Changes you can see and try

### Developer console and teleporting

Press the backtick key, below Escape, to open the console. Opening it pauses the game. Commands are case-insensitive, Tab completes command names, and misspellings can produce suggestions. `Help` lists the commands; `Help collision` filters the list; `ClearConsole` clears its output.

Commands to try:

| Command | Behavior |
| --- | --- |
| `tcl` | Toggle collision. |
| `tcg` | Toggle collision geometry visualization. |
| `coe 5 -11` | Move to exterior grid cell `(5, -11)` in the current worldspace. |
| `coc <cell editor id>` | Move to a named exterior cell. Interior destinations are not supported by this command yet. |

These are the implemented commands, not the complete Skyrim console. Teleports follow the current worldspace after door travel and are rejected while a door crossing is pending. (#123, #124)

### Linked load doors

Look at a usable linked door and press **E** with the mouse captured. A crosshair prompt names the destination, such as **E · Enter Sleeping Giant Inn**. The transition fades out, loads the destination and waits for landing readiness; failure and timeout paths can return or end the hold. Exiting an interior clears its streaming state; later streaming and console movement use the new active space. (#128 and runtime repair)

The first integrated pack lacked `door_links`, so those doors could not activate. The repaired pack now has 2,263 links, including seven exterior doors in Riverwood: Sleeping Giant Inn, Riverwood Trader (two), Faendal’s House, Alvor and Sigrid’s House, Hod and Gerdur’s House, and Sven and Hilde’s House. The converter now exports the projection from winning XTEL records for both readers. A fixture verifies reciprocal arrival coordinates and names; this is separate from manually completing each retail interior visit.

### Expanded collision and dynamic clutter

More authored collision data survives conversion, including sphere and cylinder shapes and additional physical layers. Eligible clutter receives dynamic rigid bodies from authored data. Look at a supported object within reach: **E · Pick up** appears; press E to hold it, then E to drop it. Pickup now resolves compound collider children to the owning body, and dropping wakes the body. (#125, #126 and runtime repair)

The native stationary Riverwood probe loaded **39 dynamic clutter bodies** and **1,136 authored static colliders**. Examples in the surrounding cells include buckets, tankards, kettles, ingots and cabbages. Containers, activators, doors and movable statics are excluded from the dynamic-clutter path; scenery is not universally movable. Authored bodies start asleep. Walking into one does not apply a controller impulse: that path remains disabled because of a Rapier manifold-transfer failure. **T** spawns a debug tankard when terrain is ready; it is separate from authored clutter.

### Rendering and scene corrections

- Authored decals have a depth-handling fix intended to stop flickering against their supporting surfaces. Check signs, overlays and other decals while moving the camera. (#205)
- Terrain shares its sampler on Apple Metal to stay within the platform's sampler limit. (#197)
- Initially disabled references are filtered before spawning, avoiding objects that should start hidden or absent. This is not a complete implementation of later quest/script enable-state changes. (#189)
- Meshes delayed by the GPU upload budget are retried rather than left permanently missing because they were not ready on the first attempt. (#201)
- Relative asset paths now resolve to one absolute root shared by the database and asset loader. The launch command above remains explicit. (#191)
- Terrain LOD batching is included as a rendering optimization. It is suppressed when request/admission controls are enabled because its retained resources are not yet fully accounted for by that ledger. Its presence does not establish a frame-rate improvement. (#200)

## Optional streaming behavior

The ordinary launch command does **not** enable adaptive streaming or explicit spatial priority. To try the adaptive controller with this pack:

```sh
./target/quick/engine --assets "$PWD/target/runtime-assets-27" --grid-x 5 --grid-y -11 --adaptive-streaming
```

- `--prioritize-streaming` favors nearby collision demand and view-facing bounds. `--max-scene-loads <count>` limits outstanding unique scene loads; its ordinary default is unlimited. (#210)
- `--adaptive-streaming` enables spatial priority and adjusts startup/walking budgets using frame, backlog and estimated memory pressure. It supplies a default backlog limit of 256 and an estimated memory allowance of half detected RAM, capped at 16 GiB. (#211)
- Backlog, memory allowance, free-memory headroom and frame target can be configured separately. These are admission estimates, not a guarantee of total process memory usage.
- Door landing upload relief survives controller updates, dynamic collision participates in reservations, and native material image dependencies must actually be ready before reservations are released.

Compare these modes on the same route and settings. The runtime repair reduces repeated LOD visibility lookup work without changing ordinary streaming defaults. Native probes still fail the 60 FPS / 16.67 ms acceptance threshold. Large LOD scene instantiation and render asset preparation remain measured loading costs; sustained travel performance is unresolved. See [runtime repair evidence](RUNTIME-REPAIRS.md).

## Converter and launcher changes

These affect the generated world and future conversions more than the game window:

- **Mod Organizer 2 support:** the launcher and converter can use MO2 profiles and resolve their winning inputs. Legacy asset compatibility also improves. The current runtime pack was converted from the staged Skyrim `Data` directory, not an MO2 profile. (#154)
- **Localized names:** NPC names are resolved from string banks, with source precedence and language included in the conversion/cache contract. This improves exported data; it does not add NPC conversations or a new name-display interface. (#198)
- **Record identity fixes:** NPC faction IDs and additional single-FormID fields are remapped correctly. (#171)
- **In-house record reader:** a schema-driven reader is available through `--record-reader inhouse`; the fresh runtime pack uses it. The legacy reader remains a separate option. (#204)
- **Input discovery failures are errors:** inaccessible or otherwise failed discovery should no longer silently produce a partial input set. (#190)
- **Terrain LOD reuse:** verified compatible chunks can be reused, with redundant compression removed. The fresh pack rebuilt its terrain LOD; reuse benefits subsequent compatible conversions. (#167)
- **GPU LOD and batched archive ingestion:** GPU LOD encoding is the new default, with bounded archive processing. (#208)
- **Less warm-conversion work:** extraction/cache improvements reduce redundant work, including deduplicated payload handling and improved restoration. This build has not had a matched cold/warm performance comparison. (#209)
- **Cache recovery and durability:** typed input selection agrees with coverage recipes; sealed-pack recovery verifies content; v1/v2 pack compatibility and atomic blob repair are retained. New runs default to archive-level durability, while legacy serialized configurations keep their per-file default. Warm restoration also honors a change from no durability to per-file durability. (#213 and integration repairs)
- **Explicit stale-pack rejection:** incompatible LOD producer identities fail startup instead of being accepted by broader database compatibility alone. Old producer bytes are not relabeled as current output.

## Diagnostics, tests and developer tooling

| PR | Included change | What it means when using this build |
| --- | --- | --- |
| #122 | World-load and loading-frame timing | Benchmark reports can distinguish load time from frame pacing during loading. |
| #161 | Skyrim SE corpus and schema validation | Adds source/schema validation infrastructure; no new player-facing feature. |
| #163 | Offline schema explorer and P0 evidence | Adds a browsable schema research artifact; browser behavior still needs its current interaction check. |
| #173 | Stricter real-output timing validation | Invalid artifacts cannot be reported as successful timing results. |
| #174 | External Mutagen winning-reference comparison test | Adds an independent comparison route; inclusion does not mean its external oracle was run here. |
| #175 | GPU UASTC encode/decode tests on lavapipe | Adds software-Vulkan CI coverage; this is separate from native Metal qualification. |
| #206 | Bounded streaming frame traces | Diagnostics can record frame-level streaming behavior without unbounded trace growth. |
| #207 | Metal diagnostics separated from historical evidence | Expensive GPU inventory is explicit through `--profile-gpu-inventory` with `--profile-output`; older benchmark evidence retains its original scope. |
| #212 | Static/adaptive real-time route comparisons | Adds repeatable comparison infrastructure; it does not supply a passed comparison for this binary. |

Benchmark routes own their camera motion, and timing instrumentation is shared across the combined benchmark options. Capture preflight requires the current producer/world contracts. CI checks now cover stacked PR bases, owning-source changes and manual integration dispatches.

## What has been verified

- The integrated code passed **1,869 Rust tests** across 86 targets, with 28 ignored, and **301 Python tests**, plus workspace checks, strict Clippy and formatting.
- The native quick-profile engine, launcher and converter were built.
- The fresh real-data pack completed conversion with producer 27, world schema 9, no conversion failures and a passing integration report.
- Its **76,213 files / 13.7 GB** passed full size and hash validation.
- The engine passed the stale-producer gate and initialized on Apple M1 Pro Metal with this pack. The bounded headless probe recorded no benchmark frames and failed benchmark acceptance; it is startup evidence only.

The follow-up native physics probe loaded all 25 nearby cells and 184 LOD chunks with no asset, transform, material, terrain or streaming invariant failures. Headless regressions cover named/occluded prompts, authored compound pickup/drop and door transitions. Manual retail door roundtrips, visual prompt placement and sustained travel performance remain open gates. The source contains missing references and some unbounded effects; conversion records those limitations rather than creating missing game content. This integration does not claim new quests, complete NPC gameplay, a complete Skyrim console, universal mod compatibility or a guaranteed FPS gain.

For exact PR heads, draft flags, test evidence, repair comments and future merge risks, see [the integration report](README.md) and [the inventory](inventory.json).
