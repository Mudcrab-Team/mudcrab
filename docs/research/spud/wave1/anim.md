# Animation: spud skeletons, clips, behaviour graphs and IK

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Spud `src/anim`: skeleton and pose sampling, Havok behaviour-graph loading, state machines, clip blending, annotation events and foot IK.

## Summary
- Spud evaluates Skyrim Havok behaviour graphs, samples animation clips, blends poses, and drives character modifiers.
- Scope is 14 files and 11,147 lines, measured with `find` and `wc -l`.
- Maturity is substantial implementation; no area-specific test suite was located in the inspected tree.
- Core features: skeleton/pose sampling; HKX behaviour project loading; state machines and expressions; clip blending and sync; annotation events; foot IK.
- It also handles mirrored skeletons, ragdoll pose exchange, and graph variables/events.
- Top liberties: facial/lip-sync is outside this subsystem; ragdoll animation-to-physics integration is a separate bridge; only registered Havok nodes work.
- Creature and humanoid support depends on real asset/graph coverage, not generic rig inference.
- Ideas worth borrowing include explicit graph binding, annotation trigger extraction, and bounded node registration.

## Feature map
| id | feature | what vanilla Skyrim does (1 line) | how spud does it (approach, algorithm, key types) | where (file:line, symbols) | evidence | liberties | worth borrowing |
|---|---|---|---|---|---|---|---|
| anim-skeleton-pose | Skeleton and pose | Applies hierarchical local transforms to the actor rig. | `Skeleton`, `Qs`, and `Output` retain bind/reference transforms and animation tracks. | [`src/anim/clip.hpp:17`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/clip.hpp#L17) `Skeleton`; [`src/anim/pose.hpp:15`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/pose.hpp#L15) `Qs` | impl | uses its own runtime pose representation; none seen in transform intent | yes, explicit game-unit boundary |
| anim-hkx-project | HKX behaviour loading | Loads character behaviour graphs and referenced projects. | Project loader parses Havok graph objects, resolves member paths and node registrations. | [`src/anim/graph.cpp:362`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L362) `ProjectLoader`; [`src/anim/graph.hpp:166`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L166) `Project` | impl | supported registered class set may omit graph types | yes, reflection-style binding |
| anim-hkx-clip | HKX animation clip sampling | Samples compressed animation tracks over time. | `Clip` splines bind to skeleton tracks and generators sample them with crop, loop, and root motion. | [`src/anim/clip.hpp:47`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/clip.hpp#L47) `Clip`; [`src/anim/generators.cpp:325`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L325) `ClipGenerator` | impl | unsupported clip encodings remain unverified | yes, separated clip data/runtime |
| anim-state-machine | State machines | Selects states from events, variables, conditions, and transitions. | `StateMachine`, transition records, compiled expressions, and blending effects. | [`src/anim/state_machine.hpp:94`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L94) `TransitionInfo`; [`:192`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L192) `StateMachine` | impl | only supported condition forms execute | yes, transition-local state |
| anim-blending-sync | Blending and synchronized clips | Blends generators and coordinates paired animation timing. | Generator nodes blend transforms and use sync marks/scene coordination. | [`src/anim/graph.hpp:56`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L56) `SyncMarks`; [`:65`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L65) `SyncScene`; [`src/anim/generators.cpp:304`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L304) | impl | implementation is a compatible subset of Havok graph behaviour | maybe, sync abstraction |
| anim-events-annotations | Animation events and annotations | Emits event names and sound/footstep annotations at clip times. | Clip triggers and binding annotations raise behavior events at active times. | [`src/anim/generators.cpp:306`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L306) `ClipTrigger`; [`:365`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L365) annotation binding | impl | event delivery depends on loaded behavior binding | yes, trigger binding |
| anim-modifiers | Graph modifiers | Applies procedural pose changes after generator evaluation. | Registered modifier nodes mutate output pose and variable state. | [`src/anim/graph.hpp:387`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L387) registration; [`src/anim/modifiers.cpp:114`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L114) `fill_missing` | impl | only implemented modifier classes supported | maybe, explicit registration |
| anim-ik-foot | Foot IK | Adjusts feet and legs for ground contact and slope. | `FootIkDriver`, bone-chain solving and ray casts adjust model pose. | [`src/anim/graph.hpp:393`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L393); [`src/anim/modifiers.cpp:206`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L206) `cast`; [`:282`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L282) `FootSolver` | impl | depends on collision queries; exact Havok edge behavior unverified | yes, isolate solver from graph |
| anim-mirror | Mirrored animation | Uses mirrored clips/rig mapping for alternate directions. | Builds mirrored skeleton mapping and transforms sampled tracks. | [`src/anim/graph.hpp:378`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L378) `make_mirrored_skeleton`; [`src/anim/generators.cpp:297`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L297) | impl | mapping follows runtime skeleton data | yes, precomputed mapping |
| anim-ragdoll-links | Ragdoll rig linkage | Maps animation bones to physics bodies and constraints. | Reads ragdoll body/constraint descriptors and exchanges poses with physics. | [`src/anim/ragdoll.hpp:17`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/ragdoll.hpp#L17) `RagdollBody`; [`:53`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/ragdoll.hpp#L53) `Ragdoll`; [`src/anim/graph.hpp:84`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L84) | partial | physics docs say ragdolls are not imported/simulated in ordinary world flow; bridge exists but coverage is limited | maybe, typed bridge |
| anim-facial-lipsync | Facial animation and lip sync | Drives face bones/morphs from facial graph and dialogue lip data. | No dedicated facial/lip-sync subsystem found under `src/anim`; audio docs assign dialogue lip driving elsewhere. | `src/anim` search; `docs/AUDIO.md` dialogue scope | unverified | no evidence of face graph/runtime support in this area | no, investigate importer/engine |
| anim-creature-rigs | Creature rigs | Runs creature-specific skeletons and behaviours. | Shared skeleton/graph abstractions can represent rigs; no dedicated creature path established. | [`src/anim/graph.hpp:102`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L102) `Character`; [`src/anim/clip.hpp:17`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/clip.hpp#L17) | unverified | creature asset coverage not established by source structure | maybe, test representative creature assets |

## Data and file formats
- Reads HKX behavior graph and animation data through project/clip loaders (`src/anim/graph.cpp`, `src/anim/clip.cpp`).
- Uses runtime skeleton and pose structures; the engine owns file/cache integration. Exact supported HKX variants were not fully enumerated.

## Cross-area dependencies
- Needs importer-provided animation/behavior assets and actor skeletons.
- Foot IK queries physics collision; ragdoll descriptors exchange with `physics::World`.
- Exposes pose, events, active clips, and footstep annotations to engine and audio.

## Tests, tooling and gates
- No anim-specific tests were identified in the inspected repository listing. This is unverified beyond the search performed.
- `docs/AUTOMATION.md` describes actor event, idle, and graph-variable commands for runtime inspection; no command was run in this read-only review.

## Liberties and deviations
1. **Gameplay correctness, potentially visible:** only registered behavior node/modifier classes are executable; missing Havok classes can change or stop graphs. Evidence: [`src/anim/graph.hpp:366-387`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L366-L387) registration API and implementations in `graph_nodes.cpp`, `generators.cpp`, `modifiers.cpp`.
2. **Visible:** no facial/lip-sync implementation was established in `src/anim`; audio docs place lip driving outside audio. Evidence: subsystem search and `docs/AUDIO.md` scope paragraph.
3. **Gameplay correctness:** ragdoll linkage exists, but general physics docs identify ragdolls as not imported/simulated. Evidence: [`src/anim/ragdoll.hpp:53-70`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/ragdoll.hpp#L53-L70); `docs/MOVEMENT.md` Known gaps.
4. **Unverified:** creature rig completeness cannot be inferred from generic graph structures; representative asset coverage is needed.

## Claims versus code
- Code supports graph loading, state machines, generators, modifiers, annotation binding, and foot IK, beyond a simple clip player.
- Dedicated facial/lip-sync and broad creature coverage were not established in this directory.
- Docs describe animation event consumers, but area-specific test evidence was not found.

## Open questions
- Which Havok node classes and clip compression variants are exercised by installed Skyrim assets? Cheapest check: inventory node registrations and import logs for a representative human, creature, and facial graph.
- Does ragdoll pose exchange reach live physics bodies? Cheapest check: trace engine call sites for `add_ragdoll` and `ragdoll_state`.
