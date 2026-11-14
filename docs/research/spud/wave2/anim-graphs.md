# anim-graphs: Havok behaviour graphs, transitions, events and IK in spud, and the Mudcrab slice

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** anim-graphs: Havok behaviour graphs, transitions, events and IK in spud, and the Mudcrab slice

Topic: which Havok behaviour node classes, transitions, events, annotations, blend modes, IK and creature clips
work end to end in spud, and which are unsupported or defaulted.


## Scope read

Read in full or in the ranges cited. Line counts are `wc -l`, measured.

| file | lines | what it settled |
|---|---|---|
| `src/anim/graph.hpp` | 395 | Project/Behavior/Character/Graph/Record model, node registration API, foot-IK and sync-scene interfaces |
| `src/anim/node.hpp` | 206 | Node contract (activate/update/handle_event/deactivate/children), `SyncInfo`, `ActiveClip`, `Generator`, `Modifier` |
| `src/anim/clip.hpp` | 58 | `Skeleton`, `Clip`, `ClipMotion`, `BoneWeightArray` |
| `src/anim/clip.cpp` | 379 | spline decode, 6 rotation compression types, sampling |
| `src/anim/graph.cpp` | 1576, read 340-579, 1240-1560 | project/behaviour loader, unknown-class fallback, bindings, update/generate/modify loop, event queue, root motion |
| `src/anim/graph_nodes.cpp` | 217 | the 5 structural classes: behaviour reference, modifier generator/list, manual selector, event-driven modifier |
| `src/anim/state_machine.hpp` | 322 | transition flags (14), `StateMachine` fields, blending effect flags |
| `src/anim/state_machine.cpp` | 2267, read 40-110, 280-560, 1564-2150 | expression ops/functions/parser, transition selection, priority, delayed and global transitions, state link |
| `src/anim/generators.cpp` | 2654, read 300-570, 1001-1350, 1440-1650, 1651-1700, 1748-1790, 1956-2050, 2163-2320, 2642-2651 | clip generator, blender, pose matching, sync clips, sequences, registration table |
| `src/anim/modifiers.cpp` | 2682, read 282-340, 968-1560, 1596-2462 (in ranges), 2455-2680 | foot IK solver/driver, look-at, limb IK, timers, inert ragdoll/get-up modifiers, registration table |
| `src/anim/ragdoll.hpp` | 72 | ragdoll body/constraint/mapping types and the two pose mappers |
| `src/anim/ragdoll.cpp` | 242 | grepped only: `read_ragdoll`, `bodies_from_pose`, `pose_from_bodies` signatures and call sites |
| `src/importer/convert/hkx_import.cpp` | 1235, read 1-60, 380-520, 1150-1235 | HKX → `.osb`/`.osk`/`.osn` conversion, accepted clip kinds, motion import |
| `src/importer/convert/hkx_classes.inc` | 1141 | 224-class HKX reflection schema; counted class table at 916+ |
| `docs/formats/osb.md` | 45 | behaviour-data container and value kinds |
| `docs/ROADMAP.md`, `docs/AUTOMATION.md` | — | grepped for animation claims and actor graph commands |
| Mudcrab `crates/converter/src/animation/{mod,classes,spline,glb}.rs` | 668, 247, 1324, 865 | Mudcrab's HKX clip decoding and glTF emission |
| Mudcrab `crates/engine/src/actor_animation.rs` | 979 | gait clip playback today |
| Mudcrab `crates/shared/src/{actor_idle_catalog,creature_idle}.rs` | 287, 80 | idle binding catalog and the two creature idles |
| Mudcrab `docs/roadmap/04-gameplay-and-physics.md` | 40+ | the only written animation plan found |
| Issues #470, #471, #399 and #400 | — | context issues |

Not read, and why:
- `src/anim/generators.cpp` lines 570-1000 (clip sampling internals) and `modifiers.cpp` 340-968 (foot-IK leg solve and
  `FootPlacement`): the behaviour, fields and arms are cited from the entry points; the inner arithmetic was sampled,
  not read line by line. Rows that depend on it say so.
- `src/engine/actors.cpp` (4447 lines): only the animation call sites were grepped (lines 511-535, 1803-1940, 2300-2400).
- `docs/formats/oda.md`, `osk.md`, `osn.md`, `osr.md`, `osw.md`, `ose.md`, `osp.md`: grepped for animation fields; the
  container formats outside `osb` were not needed for a node-class answer.
- Issue #557 had not been read when this report was written, so its content is unknown here.

## Architecture

Spud compiles Havok behaviour files into its own object container (`.osb`) at import time, then runs a small
interpreter of registered C++ node classes per character. There is no Havok runtime and no script layer: each class
spud implements is a hand-written mirror of one `hkb*`/`BS*` class, and a class that is not mirrored is dropped at load.

Layers:

```
Skyrim Data/meshes/**/*.hkx
  importer/convert/hkx_import.cpp      reflection over hkx_classes.inc (224 classes)
    ├─ behaviour / project file → animations/<path>.osb      (osb::write, whole object graph)
    ├─ skeleton (hkaSkeleton)   → <path>.osk                (convert_skeleton)
    ├─ clip (hkaSplineCompressed | interleaved) → <path>.osn (convert_clip)
    ├─ ragdoll (hkaRagdollInstance) → <path>.ragdoll.osb     (plus NIF bodies/joints)
    └─ clip motion             → animations/motion/<project>.osb (import_motion)
  anim::Project::load        graph.cpp:367-425
    ├─ hkbProjectData → character[0] → hkbCharacterData        :371-402
    ├─ rigName → Skeleton (.osk)                               :396-397
    ├─ <rig>.ragdoll.osb → Ragdoll                             :398-401
    ├─ mirroredSkeletonInfo → MirroredSkeleton                 :403-413
    ├─ footIkDriverInfo → FootIkInfo                           :414-417
    ├─ load_behavior(): every referenced behaviour, templates  :485-568
    ├─ link(): external event ids, variables, expressions      :572-670
    └─ animationNames → Clip; motion table                     :421-424
  anim::Graph per character    graph.hpp:277-363, actors.cpp:1828
    update(dt): collect active set → timesteps → per-record update → drain events (5 rounds)
    generate(output): one Output per record, children first, root last
    run_foot_ik_driver(character, output, dt)
```

Diagram source lines: [`src/anim/graph.cpp:367-425`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L367-L425); [`src/anim/graph.cpp:371-402`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L371-L402); [`src/anim/graph.cpp:396-397`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L396-L397); [`src/anim/graph.cpp:398-401`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L398-L401); [`src/anim/graph.cpp:403-413`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L403-L413); [`src/anim/graph.cpp:414-417`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L414-L417); [`src/anim/graph.cpp:485-568`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L485-L568); [`src/anim/graph.cpp:572-670`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L572-L670); [`src/anim/graph.cpp:421-424`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L421-L424); [`src/anim/graph.hpp:277-363`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L277-L363); [`src/engine/actors.cpp:1828`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/actors.cpp#L1828).

Ownership and threading:
- `Project` is immutable after load, shared by `shared_ptr`, loaded off-thread. [`actors.cpp:511-535`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/actors.cpp#L511-L535) wraps a load in
  `std::promise`/`std::shared_future` so one project loads once for all actors.
- Templates live in `Project::m_nodes`; a character clones a node when it first becomes active and drops the clone on
  deactivation ([`node.hpp:5-9`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/node.hpp#L5-L9), [`graph.cpp:912-1013`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L912-L1013)). State never outlives a deactivation.
- `Graph` owns the character's records, values and event queue. `Character&` is the callback surface into the game:
  ray casts, sync scenes, ragdoll poses, sequence playback, event notification ([`graph.hpp:102-148`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L102-L148)).
- A frame is strictly ordered: `Graph::update` then `Graph::generate` then the foot-IK driver ([`graph.hpp:285-287`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L285-L287),
  `actors.cpp` call sites around 1864-1940). Graphs of different actors are independent; `std::lock_guard` around the
  event sink is the only shared mutation ([`actors.cpp:1916`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/actors.cpp#L1916)).

Node-class execution model: a node's `children()` lists templates; the root walks them, clones the ones that became
active, orders records leaves-first, and calls `update`, `handle_event`, `generate`/`modify` from the root only
([`node.hpp:9`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/node.hpp#L9)). So a node never drives its own children, which is why unknown classes disappear quietly rather than
corrupting a traversal.

**Conversion-to-player path, end to end:**
1. `import_hkx` ([`hkx_import.cpp:1155-1235`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/hkx_import.cpp#L1155-L1235)) lists `meshes/**.hkx`, reads each with `read_hkx`, sorts by variant:
   `hkaAnimationContainer` with animations → `.osn`; with skeletons only → `.osk` (+ `.ragdoll.osb`); otherwise the
   whole graph → `.osb`. Freshness is per output extension and converter version ([`:1172-1184`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/hkx_import.cpp#L1172-L1184)).
2. `convert_clip` keeps the first animation and first binding of the container, rejects anything that is not
   `hkaSplineCompressedAnimation` or `hkaInterleavedUncompressedAnimation` ([`:445-451`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/hkx_import.cpp#L445-L451)), and copies annotation track
   text and times into the clip ([`:494-508`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/hkx_import.cpp#L494-L508)).
3. The race's `behavior_graph` field names the project ([`actors.cpp:1318-1339`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/actors.cpp#L1318-L1339)). `Project::load` resolves the
   character, rig, ragdoll, foot-IK info, clips and motion.
4. `ActorAnimation` makes a `Graph` with initial variable values ([`actors.cpp:1803-1829`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/actors.cpp#L1803-L1829)), wires the character
   callbacks (ray cast, ragdoll contact, on_event), and each frame updates, generates, then runs foot IK.
5. Gameplay pokes the graph only through events and variables: `notify("IdleStop")`, `notify("DeathAnim")`,
   `set_float`, root motion, active clip list ([`graph.hpp:289-302`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L289-L302); [`actors.cpp:2391-2407`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/actors.cpp#L2391-L2407)).

## Feature ledger

Evidence levels: `impl` (implemented, no anim-specific test found anywhere in the repo), `impl+cited vanilla quirk`
(implemented and the code states the vanilla rule it copies), `partial`, `stub`, `unverified`. No test file targets
`src/anim` (`rg -c 'TEST|catch2|doctest' src/anim/*.cpp` → no matches), so nothing below is `impl+tests`.

The "vanilla Skyrim rule" column comes from Havok behaviour-graph conventions and from comments in spud's own code, not
from an independent dump of vanilla behaviour data. Where the rule is only my expectation it says so in the cell.

### A. Project, character and file load

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| anim-load-project | project → character → rig → behaviour chain | Project names characters; each character names a rig and a behaviour file | `hkbProjectData.characterFilenames[0]`, `hkbCharacterData`, `rigName` → `.osk`, `behaviorFilename` → recursive load | [graph.cpp:367-425](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L367-L425), [:485-568](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L485-L568) | impl | only character 0 is loaded; other project characters are ignored | missing project/character throws `ParseError`; missing rig file throws |
| anim-load-clips | clip table from `animationNames` | Behaviour's clips come from the character's animation list | every name loaded as `Clip`; also addressable by file path | [graph.cpp:421-423](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L421-L423), [graph.hpp:181-182](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L181-L182) | impl | a clip not in `animationNames` but named by a clip generator resolves only by path | unknown name → `project->clip()` returns null, generator samples nothing |
| anim-load-motion | per-generator root motion | Extracted motion objects drive root motion for some clips | separate `animations/motion/<project>.osb` keyed by clip-generator name | [graph.cpp:424](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L424); [graph.hpp:183-184](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L183-L184), [:300-302](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L300-L302) | impl | motion is a second file the importer must produce; a missing entry means no root motion | no entry → `root_motion` returns false |
| anim-load-skeleton | animation skeleton | `hkaSkeleton` bones, parents, reference pose | `.osk` bones with parent index, reference `Qs`, names lower-cased for lookup | [clip.hpp:17-27](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/clip.hpp#L17-L27); [hkx_import.cpp:397-431](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/hkx_import.cpp#L397-L431) | impl | rotations converted to a basis at import (importer edge), units to metres | bone absent → `bone()` returns -1 |
| anim-load-ragdoll | ragdoll descriptor beside the rig | `hkaRagdollInstance` bodies, constraints, bone map | `<rig>.ragdoll.osb`, physics objects kept whole, NIF bodies/joints written over them | [graph.cpp:398-401](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L398-L401); [hkx_import.cpp:1194-1200](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/hkx_import.cpp#L1194-L1200) | impl | ragdoll file exists only when the HKX had `hkaRagdollInstance` | no file → `Project::ragdoll()` null, `PoseMatchingGenerator` stays inert |
| anim-load-mirror | mirrored skeleton | `hkbMirroredSkeletonInfo` axis + bone pairs | `make_mirrored_skeleton(axis, bonePairMap)` builds per-bone rotations | [graph.cpp:403-413](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L403-L413); [graph.hpp:158-163](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L158-L163), [:201](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L201) | impl | axis and pairs read straight from the file | no info → identity mirror table, mirroring does nothing |
| anim-load-footikinfo | foot-IK driver info | `hkbCharacterData.footIkDriverInfo` gains per leg | `make_foot_ik_info()` loaded like any bindable | [graph.cpp:414-417](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L414-L417); [modifiers.cpp:2639-2641](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2639-L2641) | impl | — | no info → `foot_ik` never made, foot IK inert |
| anim-load-behaviour | behaviour data tables | events, variables, types, flags, initial values | name/type/flag vectors, `VariableValues`, per-behaviour id maps | [graph.cpp:485-523](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L485-L523), [:470-483](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L470-L483) | impl | types above quaternion are not modelled (`VariableType` 0-8) | malformed behaviour throws; missing `hkbBehaviorGraph` throws |
| anim-load-references | referenced behaviours | A behaviour may reference another by name | depth-first load with a path-keyed cache that also breaks cycles | [graph.cpp:485-494](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L485-L494), [:562-568](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L562-L568) | impl | `hkbBehaviorReferenceGenerator.userData` forced to 1 on resolve | reference to a missing file throws the whole project load |
| anim-unknown-node | unregistered class | Game loads every class in the file | factory lookup miss → debug log, template null, `made[index]=nullptr` | [graph.cpp:525-552](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L525-L552) | impl | **silent drop**: the node is not a failure, it is absent | a null template keeps its place in a child list and is skipped |
| anim-link-ids | event/variable id unification | external event ids shared across behaviours | first-seen-wins `event_to_external`/`event_from_external`; root variables grow | [graph.cpp:572-670](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L572-L670); [graph.hpp:44-52](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L44-L52) | impl | `-1` means "this behaviour has no such event" | unknown external id → `event_internal` -1, node skipped |
| anim-bindings | member bindings | `hkbVariableBindingSet` writes members to/from variables | member path resolved once per clone; `copy_in`/`copy_back` per frame | [graph.cpp:764-910](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L764-L910); [graph.hpp:240-249](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L240-L249) | impl | output bindings only copied back when the record reports `has_outputs` | unresolved path → binding dropped (no log seen in the read range) |
| anim-var-api | event/variable entry points | Game sets variables and sends events by name | `notify`, `set_bool/int/float/vector`, `get_*`, case-insensitive | [graph.cpp:1530-1560](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1530-L1560), [:350-360](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L350-L360), [:1390-1398](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1390-L1398) | impl | `set_*` on an unknown name returns false without logging | unknown name → false, nothing written |
| anim-annotation-bind | annotation → trigger conversion | Clip annotations raise the behaviour event of the same name | `bind_annotations` maps annotation text to a behaviour-local event id, payload kept | [graph.cpp:377](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L377); [graph.hpp:150-156](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L150-L156); [generators.cpp:376-390](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L376-L390) | impl | an event the behaviour lacks is skipped silently (vanilla logs it) | unmapped annotation → no trigger |

### B. Graph runtime

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| anim-runtime-collect | active set maintenance | Nodes activate when they enter the active set, deactivate on leave | root walks `children()`, clones on first activation, keeps records leaves-first, root last | [graph.cpp:912-1013](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L912-L1013), [:1015-1136](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1015-L1136) | impl | cloning is per character, so node state is per actor by construction | inactive child → template stays, no clone, no update |
| anim-runtime-lifecycle | activate/deactivate | Node state does not survive deactivation | `activate()` on entry, `deactivate()` on exit, clone freed | [graph.cpp:721-727](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L721-L727), [:1015-1136](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1015-L1136) | impl | state loss is deliberate and documented | — |
| anim-runtime-timestep | per-record timestep | Variable timestep nodes subscribe to `dt` | `timesteps(dt)` distributes dt to records that asked | [graph.cpp:1181-1226](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1181-L1226); [graph.hpp:269](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L269) | impl | fixed-step and variable-step nodes share one clock | no subscription → timestep 0 |
| anim-runtime-update | frame order | update all, then generate | collect → timesteps → per-record `update` (generator) or `update_modifier` → `update_sync` → `drain` | [graph.cpp:1253-1290](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1253-L1290) | impl+cited | re-collects after events that changed the active set | `user_data & 1` → node's update skipped |
| anim-runtime-events | event queue | Events raised during a frame are delivered after it | queue of (external id, payload), drained in up to 5 rounds, duplicates of same id+payload collapsed | [graph.cpp:1292-1302](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1292-L1302), [:1354-1388](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1354-L1388) | impl | 5-round cap is invented; vanilla has no fixed cap | after 5 rounds the queue is cleared, events lost |
| anim-runtime-deliver | event dispatch | Every active node sees the event; behaviours without it skip | per record: behaviour-local id, `ignore_events` skip, modifiers activated on demand | [graph.cpp:1308-1353](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1308-L1353) | impl | a node whose behaviour lacks the event never sees it | `m_discard_raised` → events raised by an ignore-events subtree are dropped |
| anim-runtime-global | global transitions | Wildcard transitions fire anywhere in the graph | global table built at link, fired when no local transition started | [graph.cpp:1345-1349](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1345-L1349); [state_machine.cpp:2061-2083](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L2061-L2083), [:2150-2267](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L2150-L2267) | impl | one entry per state; first machine found owns an ambiguous state (comment says the design does not say) | no table entry → event does not climb |
| anim-runtime-generate | generate pass | Generators produce one output, root last | one `Output` per record, children gathered in `children()` order | [graph.cpp:1400-1432](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1400-L1432); [node.hpp:181-196](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/node.hpp#L181-L196) | impl | pass-through nodes copy their child; recycling is memory-only | child not a generator → excluded from the kids span |
| anim-runtime-fill | reference-pose fill | Missing bones take the reference pose | after generate, `present[] == 0` bones take `skeleton.reference`, quaternions normalised | [graph.cpp:1433-1449](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1433-L1449) | impl | only when `pose_fraction > 0` | clip not dense → bones fall back to reference pose |
| anim-runtime-modify | modify pass | Modifiers run on the generator output | `modify_step` per child modifier, enable binding honoured, activate on demand | [graph.cpp:1484-1517](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1484-L1517) | impl | `user_data & 2` skips modify | modifier record absent → triggers a re-collect and returns |
| anim-runtime-rootmotion | root motion | Extracted motion drives the character | weighted sum of per-clip deltas over the active clip list, mirror flips x and yaw/roll | [graph.cpp:1452-1482](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1452-L1482) | impl | weights are products down the tree, normalised by total | no motion for any active clip → false |
| anim-runtime-activeclips | active clip list | Game reads the active clip set each frame | walk from root, `child_weight()` supplies blend and motion weight | [graph.hpp:168-179](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L168-L179), [:321-322](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L321-L322) | impl | — | clip without `clip_state` → not listed |
| anim-runtime-vars | variables and properties | Variables and character properties read by expression | `words` for bool/int/real, `quads` for vector/quaternion; properties by index | [graph.hpp:25-31](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L25-L31), [:179-194](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L179-L194); [graph.cpp:470-483](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L470-L483) | impl | REAL stored as float bits in an int32 word | index out of range → read returns 0 |

### C. State machine, transitions

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| anim-sm-states | state ids | State ids need not be unique or ordered; id 0 renumbered | `link()` renumbers a later state with id 0 to its index (stated as the game's quirk) | [state_machine.cpp:2089-2096](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L2089-L2096) | impl+cited | — | unknown `toStateId` → transition rejected (`state_index` -1) |
| anim-sm-flags | 14 transition flags | Each flag changes when/how a transition fires | all 14 declared and consulted in `passes`/`ready`/`process`/`start` | [state_machine.hpp:100-121](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L100-L121); [state_machine.cpp:1665-1710](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1665-L1710), [:1813-1905](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1813-L1905) | impl | `flag_abut_at_end_of_from_generator` is implemented but called dead in vanilla by the comment | any flag not set → the plain path |
| anim-sm-startmode | start state selection | Default, sync, random, chooser | `StartStateMode` 0-3, `choose_start_state` | [state_machine.hpp:186-187](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L186-L187) | impl | chooser mode depends on the behaviour's own variables | no states → state stays 0 |
| anim-sm-condition | condition evaluation | `hkbExpressionCondition`; string conditions possible | expression compiled once at link; `conditionString` non-empty defeats compiling | [state_machine.cpp:2105-2115](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L2105-L2115); [state_machine.hpp:75-90](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L75-L90) | partial | **string conditions are not evaluated**: only the expression is; a condition object with a string is never true unless the expression compiled | compile failure → empty program → condition false |
| anim-sm-priority | priority | Highest priority eligible transition wins | `gather_list` keeps the highest priority; equal priority prefers a `ready` one, else queues | [state_machine.cpp:1720-1735](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1720-L1735), [:1760-1785](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1760-L1785) | impl | — | no eligible transition → nothing happens |
| anim-sm-wildcard | wildcard transitions | Local and global wildcards | local wildcard requires `is_local_wildcard`; global handled by the table | [state_machine.cpp:1687-1700](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1687-L1700) | impl | a wildcard without LOCAL exists only as a global | — |
| anim-sm-delayed | delayed transitions | `initiateInterval` not met yet → transition waits | `m_delayed`; uninterruptible-while-delayed protects; fired when the interval arrives | [state_machine.cpp:1813-1862](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1813-L1862), [:1594-1610](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1594-L1610) | impl | — | delayed list non-empty blocks lower-priority starts |
| anim-sm-blend | blending transition effect | Two-input blend over duration, curve, self-transition mode | `BlendingTransitionEffect`: duration, curve, event mode, self-transition flags, input wired to the previous effect | [state_machine.hpp:19-64](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L19-L64); [state_machine.cpp:1864-1905](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1864-L1905), [:1039-1143](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1039-L1143) | impl | each transition plays its own copy because transitions share effect objects | no transition effect on the transition → immediate state change |
| anim-sm-interrupt | interruption | A newer transition can blend from the old one | new effect's `from` is the previous effect, chained; `retire_transitions` merges | [state_machine.cpp:1578-1592](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1578-L1592), [:1884-1889](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1884-L1889) | impl | blend-from-a-blend can nest up to `maxSimultaneousTransitions` | at 32, oldest ended first |
| anim-sm-maxsim | simultaneous transitions | `maxSimultaneousTransitions` | enforced before pushing | [state_machine.cpp:1880-1882](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1880-L1882); [state_machine.hpp:210](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L210) | impl | int8 field, default 32 | — |
| anim-sm-events | enter/exit event delivery | States raise enter/exit events; a machine can raise one on change | `send_enter`/`send_exit`, `eventToSendWhenStateOrTransitionChanges` | [state_machine.cpp:1973-2017](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1973-L2017), [:1571-1577](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1571-L1577) | impl | — | event id -1 → `raise` drops it |
| anim-sm-return | return to previous state | `returnToPreviousStateEventId` | matched in `passes`, `return_to_previous_of` | [state_machine.cpp:1787-1793](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1787-L1793), [:1765-1772](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1765-L1772) | impl | — | previous == current → condition fails |
| anim-sm-higherlower | next higher/lower state | `transitionToNextHigher/LowerStateEventId`, wrap flag | scans state ids, wraps only when `wrapAroundStateId` | [state_machine.cpp:1624-1646](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1624-L1646) | impl | — | no higher/lower id and no wrap → nothing |
| anim-sm-random | random transition | `randomTransitionEventId` picks a weighted target | gathers all passing transitions, `weighted_choice` by per-state probability | [state_machine.cpp:1734-1755](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1734-L1755) | impl | — | no passing transition → nothing |
| anim-sm-echo | echo / restart under fade | A generator restarted under a fading echo | `start_echo` + `m_echo_next_update`, self-transition mode decides | [state_machine.cpp:1594-1610](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1594-L1610); [generators.cpp:534-570](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L534-L570) | impl | echo count capped at 2 | no self-transition mode → no restart |
| anim-sm-nested | nested machines | A state may hold another machine | nesting table, `set_start_override`, `from/toNestedStateId`, `notify_nested` | [state_machine.cpp:1999-2060](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1999-L2060), [:1864-1878](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1864-L1878) | impl | — | nested id invalid → override ignored |
| anim-sm-nopoll | conditions are event-driven only | Vanilla never polls transition conditions | stated in a comment: "Transitions are never polled. A condition is only evaluated when an event arrives" | [state_machine.cpp:1569-1571](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1569-L1571) | impl+cited | — | a variable changing alone never triggers a transition |

### D. Expressions (`hkbExpressionData`, conditions)

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| anim-expr-parser | shunting-yard compile | Havok expression grammar with its own precedence | 46 op tokens; precedence is the game's: logical operators bind tighter than comparisons and share a level | [state_machine.cpp:48-56](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L48-L56), [:287-530](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L287-L530) | impl+cited | deliberately non-C precedence, needed by `blockbehavior.hkx` | invalid token → warn, program truncated |
| anim-expr-functions | 27 functions | `rand01`, `randRange`, trig with degree variants, `clamp`, `lerp`, `clerp`, `cond`, `pow`, `sign`, ... | table of 27 names with arity, prefix-matched longest-first | [state_machine.cpp:72-82](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L72-L82) | impl | `clerp` takes 5 operands; `cond` 3 | unknown function name → token rejected, expression dropped |
| anim-expr-vars | operands | Variables and character properties | name → root variable index, else character property index | [state_machine.cpp:515-528](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L515-L528) | impl | property lookup is case-insensitive | neither → warn and fail compile |
| anim-expr-compile-fail | failed compile | Game logs and skips | tokens cut back to the start, entry kept so later indices stay valid | [state_machine.cpp:541-553](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L541-L553) | impl | — | empty program evaluates false |
| anim-expr-modifier | `hkbEvaluateExpressionModifier` | Evaluates expressions, assigns to variables | `ExpressionData` list with assignment variable and flags | [state_machine.cpp:558-712](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L558-L712) | impl | — | no assignment target → result discarded |

### E. Generators

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| anim-gen-clip | `hkbClipGenerator` | Plays a clip with crop, speed, mode, triggers | 4 modes (single, looping, user-controlled, ping-pong), crop start/end, start time, enforced duration | [generators.cpp:325-340](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L325-L340), [:388-470](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L388-L470) | impl | zero playback speed is coerced to ±0.001 (holds nearly still, not still; the exact vanilla behaviour is unverified) | clip name not found → nothing sampled, bones unset |
| anim-gen-clip-triggers | clip triggers | Time-based events inside the clip | file triggers + annotation-derived triggers; relative-to-end and acyclic handled | [generators.cpp:306-321](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L306-L321), [:376-390](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L376-L390), [:471-490](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L471-L490) | impl | duplicated triggers are not deduplicated across rounds (drain does) | event id not in behaviour → trigger dropped at activation |
| anim-gen-clip-syncinfo | clip sync info | Sync points, base frequency, mirrored flag | period from cropped duration divided by the absolute speed, sync points from events flagged as sync points, cap 8 | [generators.cpp:474-512](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L474-L512), [:26](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L26) | impl | `mirrored` read from the generator flag; the id-table writer was never found (comment) | no sync points → point_count 0 |
| anim-gen-clip-echo | clip echo | Echo of a previous cycle under a fade | up to 2 echoes with weight ramp, offset wraps for looping | [generators.cpp:534-600](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L534-L600), [:898](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L898) | impl | cap 2 is spud's | echo weight ≤ 0 → slot removed |
| anim-gen-blender | `hkbBlenderGenerator` | Weighted blend of children | 6 flags, per-child weight clamped 0-1, smoothstep when `smooth_weights`, threshold fill for unreached bones | [generators.cpp:1001-1090](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1001-L1090), [:1274-1284](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1274-L1284), [:1155-1260](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1155-L1260) | impl | single child with threshold 0 is a pass-through fast path | zero total weight → reference pose fill |
| anim-gen-parametric | parametric blend | Children anchored on a parameter line, cyclic option | sort anchors, find interval, lerp between neighbours, wrap when cyclic | [generators.cpp:1286-1330](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1286-L1330) | impl | — | fewer than 2 children → no interval, weight 0 |
| anim-gen-additive | additive blending | Additive children added after normal ones; subtractive last child | two passes; `subtractLastChild` removes the last child's contribution | [generators.cpp:1160-1270](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1160-L1270), [:1117-1130](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1117-L1130) | impl | all-additive output starts from identity | additive flag only when every child is additive |
| anim-gen-boneweights | per-child bone weights | `hkbBoneWeightArray` per child | weights scale the child's contribution; `finalise` fills bones under `threshold` from the reference pose | [generators.cpp:1014-1026](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1014-L1026), [:1156-1200](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1156-L1200) | unverified | exact per-bone accumulation (`blend_bones`) not read line by line | empty array → every bone weight 1 |
| anim-gen-syncmaster | sync master child | One child drives the blend's sync | `indexOfSyncMasterChild` short-circuits sync combining | [generators.cpp:1126-1141](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1126-L1141) | impl | — | no master → weighted `combine_sync` |
| anim-gen-posematch | `hkbPoseMatchingGenerator` | Picks the closest start pose using the ragdoll | builds a signature per child from ragdoll bone positions, dot-product error, hysteresis by switch time and pelvis speed | [generators.cpp:1447-1630](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1447-L1630) | impl | requires a ragdoll pose source; signatures rebuilt per activation | no ragdoll, or invalid bone indices → inert blender |
| anim-gen-refpose | `hkbReferencePoseGenerator` | Emits the skeleton's reference pose | fills every bone from the skeleton | [generators.cpp:1632-1650](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1632-L1650) | impl | — | empty output → nothing |
| anim-gen-statetag | `BSiStateTaggingGenerator` | Tags a state id over its child | passes the child through, records `iStateToSetAs`, sync copied | [generators.cpp:1651-1672](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1651-L1672) | impl | the tag itself has no consumer in this read range | no child → empty output |
| anim-gen-boneswitch | `BSBoneSwitchGenerator` | Swaps children per bone by weight | default generator plus weighted entries | [generators.cpp:1673-1747](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1673-L1747) | impl | — | no entries → default generator |
| anim-gen-cyclic | `BSCyclicBlendTransitionGenerator` | Freeze / cross-blend a blender on events | 5 modes (inactive/default/frozen/blending/waiting), clones a plain blender | [generators.cpp:1748-1955](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1748-L1955) | impl | `eBlendCurve` is declared and never read (comment) | blender not a plain `BlenderGenerator` → no transition copy, no blending |
| anim-gen-offset | `BSOffsetAnimationGenerator` | Offsets the pose by an additive clip keyed on a variable | samples the offset clip between its first and last frame, composes offsets | [generators.cpp:1956-2069](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1956-L2069) | impl | `fOffsetRangeStart` is not subtracted (comment says the game does the same) | offset clip not additive → log error, zero offsets |
| anim-gen-sequence | `BGSGamebryoSequenceGenerator` | Plays an object's `.kf` text-key sequences | `sequence_start/update/stop` callbacks, percent, play mode, blend | [generators.cpp:2400-2640](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L2400-L2640) | impl | requires the game to supply `.kf`; sequences are not converted by this repo's importer | sequence missing → start returns false, no playback |
| anim-gen-syncclip | `BSSynchronizedClipGenerator` | Paired animation: leader/follower, sync marks | per-character scene, `getToMarkTime` lerp, prefix-mapped events, local track binding | [generators.cpp:2163-2398](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L2163-L2398) | partial | `bSyncClipIgnoreMarkPlacement` is declared and never read (comment) | no scene → `DefaultSyncScene` plays alone |
| anim-gen-syncscene | `hkbSynchronizedAnimationScene` | Participants wait for all marks, then run on one clock | `SyncScene` interface: `all_in_scene`, `all_at_marks`, join/leave/left_early, `on_all_left` → PairEnd | [graph.hpp:63-81](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L63-L81), [generators.cpp:2070-2162](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L2070-L2162) | impl | — | no participants → default scene, no waiting |
| anim-gen-sequence-tracknames | annotation track names | Clips carry multi-actor track names | `track_names()` per clip from the `.osn` | [graph.hpp:202-207](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L202-L207); [hkx_import.cpp:494-500](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/hkx_import.cpp#L494-L500) | impl | names are empty when the source clip had no annotation tracks | no names → local binding by bone name only |

### F. Modifiers

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| anim-mod-isactive | `BSIsActiveModifier` | Publishes five "is active" flags | `activate` sets each flag to `!invert`, `deactivate` writes `invert` back | [modifiers.cpp:1067-1090](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1067-L1090) | impl | — | flags are outputs only; nothing reads them inside the node |
| anim-mod-lookat | `BSLookAtModifier` | Turns listed bones toward a target within a limit | target from `targetLocation` or hard-coded camera position, per-bone gains, limit + threshold, out-of-limit event | [modifiers.cpp:1112-1296](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1112-L1296) | impl | the camera position is a member, not a live camera; `lookAtCamera` uses it verbatim | bones empty or bone index invalid → returns untouched |
| anim-mod-directat | `BSDirectAtModifier` | Points the upper body at a game-supplied target | heading and pitch from a target, clamped by limits, offsets, gain damping, spread over the chain | [modifiers.cpp:1832-1965](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1832-L1965) | impl | target comes from a `Character` callback; `userInfo 1` falls back to the look target | no callback and no look target → blend back to zero and return |
| anim-mod-twist | `hkbTwistModifier` | Twists a bone chain about an axis | chain from `startBone..endBone`, parents collected, angle distributed | [modifiers.cpp:1967-2054](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1967-L2054) | impl | — | invalid bone indices → nothing |
| anim-mod-limbik | `BSLimbIKModifier` | Raises a limb above nearby geometry | box test, ray per bone in the index range, best hit wins, gain-damped angle applied to the start bone | [modifiers.cpp:2306-2410](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2306-L2410) | impl | hard-coded `k_scale = 69.991249` and collision layer 43 | no `shapes_in_box` or `cast_ray` → no-op; no hit → angle decays to 0 |
| anim-mod-transformvector | `hkbTransformVectorModifier` | Transforms a vector by a rotation/translation | `compute()` on activate and/or modify, rotate-only and inverse modes | [modifiers.cpp:2411-2462](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2411-L2462) | impl | — | neither flag set → `vectorOut` 0 |
| anim-mod-damping | `hkbDampingModifier` | PID smoothing of a scalar and a vector | per-generate PID with kP/kI/kD, error sums, previous error | [modifiers.cpp:1421-1476](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1421-L1476) | impl | **no timestep**: runs once per generate, so gains are frame-rate dependent | both enable flags false → nothing |
| anim-mod-speedsampler | `BSSpeedSamplerModifier` | Samples the movement speed for a state and direction | `sample_speed` callback, else `goalSpeed` | [modifiers.cpp:1477-1497](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1477-L1497) | impl | the fallback silently reports goal speed | no callback → `speedOut = goalSpeed` |
| anim-mod-interpvalue | `BSInterpValueModifier` | Smooths a value toward a target | gain-fraction lerp per update | [modifiers.cpp:1498-1522](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1498-L1522) | impl | `source` is not advanced; smoothing needs a binding onto both ends | no binding → `result` follows the frozen `source` |
| anim-mod-modifyonce | `BSModifyOnceModifier` | Runs modifier children once on activation | clones the child, activate/update(0)/deactivate, never adds it to the active set | [modifiers.cpp:1523-1565](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1523-L1565) | impl+cited | children's `modify` never runs (comment says vanilla's are unbound expression modifiers) | clone flag set → `children()` reports none |
| anim-mod-timers | `hkbTimerModifier`, `BSTimerModifier` | Raise an alarm event after N seconds | elapsed accumulation, fires on crossing, BSTimer optionally resets | [modifiers.cpp:1368-1420](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1368-L1420) | impl | hkb timer does not reset; BS timer resets even with no event | alarm time ≤ 0 → fires on the first frame |
| anim-mod-eventeveryn | `BSEventEveryNEventsModifier` | Send an event every N of another event | counter modulo, randomisation between min and N with the graph's LCG | [modifiers.cpp:1309-1367](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1309-L1367) | impl | randomisation redraws after each send | N ≤ 0 → coerced to 1 |
| anim-mod-eventsfromrange | `hkbEventsFromRangeModifier` | Send events when a value enters a range | stacked ranges, send-on-enter vs send-while-in-range | [modifiers.cpp:2162-2198](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2162-L2198) | impl | at most one range matches (ranges stack) | no matching range → all was-active flags clear |
| anim-mod-gettimestep | `BSGetTimeStepModifier` | Publishes the frame timestep | writes `timeStep` output each update | [modifiers.cpp:2225-2236](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2225-L2236) | impl | — | unbound → value never read |
| anim-mod-passby | `BSPassByTargetTriggerModifier` | Raises an event when the actor passes a target | target from callback, radius test, direction dot test, edge-triggered | [modifiers.cpp:2237-2280](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2237-L2280) | impl | without the callback nothing fires; the node writes `targetPosition` from the game | no callback → clears the passed flag, no event |
| anim-mod-decompose | `BSDecomposeVectorModifier` | Splits a vector4 into four floats | copies components each update | [modifiers.cpp:2281-2305](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2281-L2305) | impl | — | — |
| anim-mod-events | `BSEventOnDeactivateModifier`, `BSEventOnFalseToTrueModifier` | Raise an event on deactivation, or on a false→true edge | `deactivate` raises; three slots with edge detection | [modifiers.cpp:1297-1308](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1297-L1308), [:1798-1831](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1798-L1831) | impl | the three slots' booleans are the bindings, so the edge is on the bound value | event id -1 → dropped |
| anim-mod-footikcontrols | `hkbFootIkControlsModifier` | Publishes foot IK gains and per-leg results | writes `foot_ik_gains`/`fraction`, copies driver legs back, raises ungrounded events | [modifiers.cpp:1587-1640](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1587-L1640) | impl | — | no driver info → gains written but no leg data, `copy_legs` writes zeros past the driver's legs |
| anim-mod-footik | `hkbFootIkModifier` | The in-graph foot IK node | full leg solve through `FootPlacement`, gains, track data, align-with-ground rotation out | [modifiers.cpp:1661-1797](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1661-L1797) | impl | gain of exactly 1 means "at once" here, unlike the driver | no `cast_ray`, no legs, or bone out of range → returns, `alignWithGroundRotation` identity |
| anim-mod-rotate | `hkbRotateCharacterModifier` | Rotates the character root at a rate | accumulates degrees × speed × dt, applies to `world_from_model`, resets after modify | [modifiers.cpp:2055-2091](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2055-L2091) | impl | — | speed 0 → no rotation, angle reset |
| anim-mod-tweener | `BSTweenerModifier` | Tweens position/rotation to a target | start captured on activate, duration from member or sync frequency, lerp / shortest-arc quaternion | [modifiers.cpp:2092-2144](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2092-L2144) | impl | uses `modify_sync` to learn the duration | duration 0 → immediately at the target on the first modify |
| anim-mod-inert-ragdoll | `hkbPoweredRagdollControlsModifier`, `hkbRigidBodyRagdollControlsModifier`, `hkbGetUpModifier`, `hkbKeyframeBonesModifier` | Drive ragdoll motors, get-up, keyframed bones | members declared so bindings resolve; **no `modify`, no `update`** | [modifiers.cpp:2494-2541](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2494-L2541), [:2583-2596](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2583-L2596), [:2616-2630](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2616-L2630), [:2458-2461](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2458-L2461) comment | stub | deliberate placeholder: comment says "The ragdoll workstream gives them their behaviour" | always a no-op |
| anim-mod-istate | `BSIStateManagerModifier` | Writes the current state machine state to a variable | declared inert with comment "nothing in the game attaches its listener" | [modifiers.cpp:2199-2224](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2199-L2224) | stub | — | `iStateVar` keeps its bound value |
| anim-mod-contact | `BSRagdollContactListenerModifier` | Emits an event when ragdoll bones touch something | `update` asks the host `ragdoll_touching(bones)` and raises | [modifiers.cpp:2597-2615](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2597-L2615) | impl | depends entirely on the game's contact query | no callback → never fires |

### G. IK, ragdoll bridge, creature coverage

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| anim-ik-driver | `hkbFootIkDriver` | Post-generate pass places feet on the ground | `run_foot_ik_driver` after generate; params from `hkbFootIkDriverInfo`; on/off fraction gate; scale moved out of the pose and back | [graph.hpp:391-393](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L391-L393); [modifiers.cpp:968-1060](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L968-L1060), [:2643-2649](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2643-L2649) | impl | runs only when the project has driver info and the game allows it | `foot_ik_fraction ≤ 0` → all leg state dropped |
| anim-ik-legparam | foot leg parameters | Per-leg hip/knee/ankle, heights, knee limits, ray lengths | `FootLegData` fields drive `make_leg` | [modifiers.cpp:514](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L514) (range not finished in the original note), [`:282-300`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L282-L300) | unverified | exact clamping constants inside the solver not read | bone out of range → the whole driver returns |
| anim-ik-cast | foot ray cast | Ray up/down at ankle and foot end, higher ground wins | `cast_rays` compares two fractions, keeps the larger ground fraction and its normal | [modifiers.cpp:305-340](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L305-L340) | impl | assumes the world up is the project's `worldUpWS` | no hit either ray → leg keeps previous state |
| anim-ik-limb | limb IK | See `anim-mod-limbik` | — | [modifiers.cpp:2306-2410](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2306-L2410) | impl | — | — |
| anim-ragdoll-poses | ragdoll ↔ animation exchange | `animationToRagdollSkeletonMapper`, live ragdoll pose | `RagdollPoses`: `map_pose`, `current_pose`, `body_speed`, implemented by the host | [graph.hpp:84-93](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L84-L93); [ragdoll.hpp:17-72](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/ragdoll.hpp#L17-L72) | impl | interface only in `anim`; the physics side is another area | no ragdoll → `ragdoll` null, pose matching and ragdoll modifiers inert |
| anim-creature | creature rigs | Each creature has its own skeleton and behaviour | nothing creature-specific: same Skeleton/Project/Graph path | [graph.cpp:396-425](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L396-L425); [clip.hpp:17](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/clip.hpp#L17) | unverified | coverage depends on the asset's behaviour using only registered classes | a creature behaviour with unregistered nodes loses those nodes silently |
| anim-facial | facial / lip-sync | Face graph, visemes, expression morphs | no facial code found in `src/anim` | — | doc-only | absent from this subsystem | no face animation at all |

## Support matrix

### Node classes: registered, partial and stub

Counted with:
`rg -c 'register_node' src/anim/*.cpp` → [`generators.cpp:10`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L10)`,` [`graph_nodes.cpp:5`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph_nodes.cpp#L5)`,` [`modifiers.cpp:29`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L29)`,` [`state_machine.cpp:3`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L3)
(47 total; [`graph.cpp:1`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1) is the definition). Class names to `comm` were taken from
`rg -o 'register_node<[A-Za-z]+>\("[A-Za-z0-9_]+"\)'` and from the schema table at [`hkx_classes.inc:916`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/hkx_classes.inc#L916)`+`.

Method note: the two name lists were diffed with `comm` on temporary files that held class names only.

Totals (measured): **47 registered**, of which **35 supported**, **7 partial**, **5 stub**.
The importer schema lists **224 classes**, of which exactly **48** are named `*Generator`, `*Modifier` or
`*TransitionEffect`; 3 of those 48 are the abstract bases (`hkbGenerator`, `hkbModifier`, `hkbTransitionEffect`), and
all **45 concrete** ones are registered. So no concrete node class in spud's own schema lacks a runtime class.

| # | class | status | note |
|---|---|---|---|
| 1 | hkbBehaviorReferenceGenerator | supported | transparent link to another file's root ([graph_nodes.cpp:13-36](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph_nodes.cpp#L13-L36)) |
| 2 | hkbModifierGenerator | supported | modifier applied to the wrapped generator ([graph_nodes.cpp:38-69](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph_nodes.cpp#L38-L69)) |
| 3 | hkbModifierList | supported | runs each modifier in order ([graph_nodes.cpp:71-102](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph_nodes.cpp#L71-L102)) |
| 4 | hkbManualSelectorGenerator | supported | `selectedGeneratorIndex` clamped, re-collect on change ([graph_nodes.cpp:104-153](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph_nodes.cpp#L104-L153)) |
| 5 | hkbEventDrivenModifier | supported | activate/deactivate events, `activeByDefault` ([graph_nodes.cpp:155-203](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph_nodes.cpp#L155-L203)) |
| 6 | hkbClipGenerator | supported | 4 modes, crop, speed, triggers, echoes ([generators.cpp:325-1000](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L325-L1000)) |
| 7 | hkbBlenderGenerator | supported | weighted, parametric, additive, subtractive, bone weights ([generators.cpp:1001-1446](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1001-L1446)) |
| 8 | hkbPoseMatchingGenerator | partial | inert without a ragdoll pose source ([generators.cpp:1447-1630](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1447-L1630)) |
| 9 | hkbReferencePoseGenerator | supported | ([generators.cpp:1632-1650](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1632-L1650)) |
| 10 | BSBoneSwitchGenerator | supported | ([generators.cpp:1673-1747](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1673-L1747)) |
| 11 | BSCyclicBlendTransitionGenerator | partial | only a plain `BlenderGenerator` child is cloned; `eBlendCurve` unread ([generators.cpp:1748-1955](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1748-L1955)) |
| 12 | BSiStateTaggingGenerator | partial | tag written, no consumer found in the read range ([generators.cpp:1651-1672](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1651-L1672)) |
| 13 | BSSynchronizedClipGenerator | partial | `bSyncClipIgnoreMarkPlacement` declared and unread ([generators.cpp:2163-2398](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L2163-L2398)) |
| 14 | BSOffsetAnimationGenerator | supported | needs an additive offset clip ([generators.cpp:1956-2069](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1956-L2069)) |
| 15 | BGSGamebryoSequenceGenerator | supported | needs `.kf` playback from the host ([generators.cpp:2400-2640](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L2400-L2640)) |
| 16 | hkbStateMachine | supported | states, transitions, conditions, globals, nesting (state_machine.cpp) |
| 17 | hkbBlendingTransitionEffect | supported | duration, curve, self-transition, event mode ([state_machine.hpp:19-64](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L19-L64)) |
| 18 | hkbEvaluateExpressionModifier | supported | compiled expressions with assignment ([state_machine.cpp:558-712](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L558-L712)) |
| 19 | BSIsActiveModifier | supported | ([modifiers.cpp:1067-1090](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1067-L1090)) |
| 20 | BSLookAtModifier | supported | ([modifiers.cpp:1112-1296](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1112-L1296)) |
| 21 | BSEventOnDeactivateModifier | supported | ([modifiers.cpp:1297-1308](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1297-L1308)) |
| 22 | hkbPoweredRagdollControlsModifier | **stub** | members only ([modifiers.cpp:2494-2514](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2494-L2514)) |
| 23 | hkbKeyframeBonesModifier | **stub** | members only ([modifiers.cpp:2529-2542](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2529-L2542)) |
| 24 | BSEventEveryNEventsModifier | supported | ([modifiers.cpp:1309-1367](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1309-L1367)) |
| 25 | hkbTimerModifier | supported | ([modifiers.cpp:1368-1391](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1368-L1391)) |
| 26 | hkbRigidBodyRagdollControlsModifier | **stub** | members only ([modifiers.cpp:2583-2596](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2583-L2596)) |
| 27 | BSRagdollContactListenerModifier | partial | needs the host's contact query ([modifiers.cpp:2597-2615](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2597-L2615)) |
| 28 | hkbGetUpModifier | **stub** | members only ([modifiers.cpp:2616-2630](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2616-L2630)) |
| 29 | hkbDampingModifier | supported | PID per generate, no timestep ([modifiers.cpp:1421-1476](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1421-L1476)) |
| 30 | BSSpeedSamplerModifier | supported | falls back to `goalSpeed` ([modifiers.cpp:1477-1497](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1477-L1497)) |
| 31 | BSInterpValueModifier | supported | ([modifiers.cpp:1498-1522](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1498-L1522)) |
| 32 | BSModifyOnceModifier | supported | children never reach the active set ([modifiers.cpp:1523-1565](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1523-L1565)) |
| 33 | hkbFootIkControlsModifier | supported | gains + leg read-back ([modifiers.cpp:1587-1640](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1587-L1640)) |
| 34 | BSEventOnFalseToTrueModifier | supported | 3 slots ([modifiers.cpp:1798-1831](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1798-L1831)) |
| 35 | BSDirectAtModifier | supported | ([modifiers.cpp:1832-1965](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1832-L1965)) |
| 36 | hkbTwistModifier | supported | ([modifiers.cpp:1967-2054](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1967-L2054)) |
| 37 | hkbRotateCharacterModifier | supported | ([modifiers.cpp:2055-2091](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2055-L2091)) |
| 38 | BSTweenerModifier | supported | ([modifiers.cpp:2092-2144](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2092-L2144)) |
| 39 | hkbEventsFromRangeModifier | supported | ([modifiers.cpp:2162-2198](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2162-L2198)) |
| 40 | BSIStateManagerModifier | **stub** | inert by comment ([modifiers.cpp:2199-2224](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2199-L2224)) |
| 41 | hkbFootIkModifier | supported | full in-graph foot solve ([modifiers.cpp:1661-1797](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1661-L1797)) |
| 42 | BSGetTimeStepModifier | supported | ([modifiers.cpp:2225-2236](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2225-L2236)) |
| 43 | BSPassByTargetTriggerModifier | partial | inert without the host callback ([modifiers.cpp:2237-2280](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2237-L2280)) |
| 44 | BSDecomposeVectorModifier | supported | ([modifiers.cpp:2281-2305](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2281-L2305)) |
| 45 | BSTimerModifier | supported | resets optionally ([modifiers.cpp:1392-1420](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1392-L1420)) |
| 46 | BSLimbIKModifier | partial | needs `shapes_in_box` and `cast_ray` ([modifiers.cpp:2306-2410](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2306-L2410)) |
| 47 | hkbTransformVectorModifier | supported | ([modifiers.cpp:2458-2462](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2458-L2462) comment; 2411-2462 code) |

**Unsupported by construction:** any class absent from `hkx_classes.inc` is absent from both the `.osb` and the
runtime ([graph.cpp:530-535](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L530-L535)). Its occurrence in a real behaviour file is a silent node loss, visible only as a
`debug`-level log line. The class table's provenance is not in the repository (no generator script under `tools/` or
`scripts/`), so whether 224 classes is the complete Skyrim SE set is **unverified**.

**Defaulted rather than unsupported:** the 4 ragdoll/get-up modifiers and `BSIStateManagerModifier` load, bind and do
nothing; 4 generators degrade (pose matching, cyclic blend curve, state tag, sync-clip mark placement); 3 nodes depend
entirely on host callbacks (`ragdoll_touching`, `shapes_in_box`/`cast_ray`, `pass_by_target`).

### Everything else, counted

| dimension | supported | unsupported / defaulted | command |
|---|---|---|---|
| clip encodings | `hkaSplineCompressedAnimation`, `hkaInterleavedUncompressedAnimation` | delta, wavelet, predictive → `ParseError("clip of an unknown kind")`, counted as failed | `rg -n 'hkaSplineCompressed\|hkaInterleaved' importer/convert/hkx_import.cpp` |
| clips per container | first animation + first binding | extra animations in one container ignored | [hkx_import.cpp:445-451](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/hkx_import.cpp#L445-L451) |
| expression functions | 27 | none | `rg -c '{"' state_machine.cpp` over k_functions |
| expression operators | 46 op tokens | bitwise operators absent (Havok has none) | [state_machine.cpp:48-56](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L48-L56) |
| transition flags | 14 of 14 | `flag_abut_at_end_of_from_generator` implemented but called dead by vanilla validator | [state_machine.hpp:100-121](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L100-L121) |
| transition condition kinds | expression | string condition never evaluates true | [state_machine.cpp:2105-2115](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L2105-L2115) |
| clip rotation compression | 6 types (0-5) | unknown type byte → falls through to the raw-float branch | [clip.cpp:49-96](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/clip.cpp#L49-L96) (the original cited 20-21 and 38-104) |
| sync points per generator | 16 (array), 8 from clips | more than 8 clip sync points dropped | [node.hpp:43](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/node.hpp#L43); [generators.cpp:26](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L26), [:498](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L498) |
| echoes per clip | 2 | 3rd echo ignored unless a slot frees | [generators.cpp:27](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L27), [:552-570](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L552-L570) |
| graph variable types | 9 (bool, int8/16/32, real, pointer, vector3/4, quaternion) | no matrix or string variable | [graph.hpp:25](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L25) |
| facial / lip-sync | none | no implementation in `src/anim` | `rg -in 'viseme\|facial\|facegraph' src/anim` → no real match (only `clip` substring hits) |
| ragdoll feedback | 5 of 5 modifier classes load | 4 of them do nothing | [modifiers.cpp:2458-2462](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2458-L2462) |

## Mudcrab mapping

Mudcrab today has a clip pipeline and a three-clip playback graph. It has no behaviour graph, no state machine, no
events, no annotations, no blending beyond Bevy's transitions, and no IK.

| ledger group | Mudcrab today | gap | owner |
|---|---|---|---|
| A. project/character/behaviour load | RACE `behavior_graph` is not read by the converter. `actor_idle_catalog` carries `project`, `skeleton`, `body_scene` and per-clip binding counts (`crates/shared/src/actor_idle_catalog.rs:24-46`) | no behaviour decode, no `.osb`-equivalent, no event/variable tables | converter (`crates/converter/src/animation/`) + new behaviour module |
| B. graph runtime | `crates/engine/src/actor_animation.rs` installs one `AnimationPlayer` per actor with idle/walk/run (`:1-16`, `:44-57`, `:231`); a missing clip leaves bind pose by design (`:10-12`) | no active set, no node lifecycle, no event queue, no root motion from clips | engine, new `actor_behaviour` module |
| C. state machine | none. Gait selection is `Gait` enum logic in `actor_drive.rs` | no transitions, priorities, wildcards, conditions, globals | engine, same module |
| D. expressions | none | no compiler, no variables | shared (pure function, testable headless) |
| E. generators | only flat clip playback. Clips emitted are exactly three per sex (`crates/converter/src/pipeline.rs:2838-2856`), path `meshes/actors/character/animations/{sex}/{mt_idle,mt_walkforward,mt_runforward}.glb` (`:2848`) | no blender weights, no additive, no sync clips, no parametric blend | engine |
| E. sequences (`BGSGamebryoSequenceGenerator`) | none; `.kf` text keys are not converted | whole path missing (also blocks furniture/activator sequences) | converter + engine |
| F. modifiers | none | all 29 classes absent | engine (subset first) |
| G. foot/limb IK | none | needs ray casts from physics (`crates/engine/src/physics.rs`); API fit unverified | engine, `actor_foot_ik` |
| G. ragdoll bridge | none. `#400` keeps ragdolls as a later, separate lane and warns that a serialized class is not graph execution | ragdoll modifiers stay out of the first slice | physics + engine |

What the converter must emit, per behaviour project, and what already exists to build on:

- HKX reading: `crates/converter/src/animation/packfile.rs` (642 lines) already parses the 2010.2 64-bit packfile;
  `classes.rs` (247 lines) models only the fields the clip decoder reads (`:1-5`). A behaviour decoder needs a member
  table, which spud solves with a 1141-line generated offset table (`hkx_classes.inc`). Mudcrab has no such table; the
  choice is either a generated table (spud's technique) or `serde-hkx`'s class definitions (`#400`).
- Clips: `mod.rs:199-200` rejects everything except `hkaSplineCompressedAnimation`. Spud accepts interleaved as well
  ([`hkx_import.cpp:445-451`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/hkx_import.cpp#L445-L451)). Adding interleaved is small and unlocks clips the current converter fails.
- Annotations: Mudcrab's decoder does not read `annotationTracks` at all (`rg -n annotation crates/converter/src/animation` → none). Footstep and sound events therefore cannot exist today.
- Output shape: a new versioned pack under `meshes/behaviors/<project>.glb`-style key space, or a JSON sidecar, with a
  typed contract in `crates/shared` (precedent: `actor_idle_catalog.rs:1-8`, `PATH = "meshes/actor-idles/catalog.json"`,
  `VERSION = 1`, and its own `clip_issues`/`unresolved` lists).

Fit with Mudcrab design:
- Rust ownership maps cleanly onto spud's template/clone split: `Project` becomes an immutable `Arc<Behaviour>`,
  per-character state becomes a `Vec<Record>` rebuilt when the active set changes. No `unsafe` is needed.
- Bevy: one `AnimationPlayer` per participating entity is already how paired actors would work; blending must be done
  by the behaviour module writing weights, because Bevy's `AnimationTransitions` has no sync marks or parametric blend.
- Luau: events and variables are the natural scripting surface (`Graph::notify`/`set_*`, [graph.hpp:289-298](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L289-L298)); expose them
  as engine-side functions rather than letting Luau own the graph.
- Docs: the only written plan is `docs/roadmap/04-gameplay-and-physics.md:14-16` ("animation state blending tree (idle,
  walk, run, sprint, attack)"). No ADR covers behaviour graphs, HKX class tables, or IK. That is a decision gap.

Issue context:
- **#470** (child NPCs in T-pose, missing hands/feet): the T-pose half matches the documented Mudcrab fallback — a
  missing or unbound idle clip leaves bind pose (`actor_animation.rs:10-12`); spud shows the same outcome when a clip
  name does not resolve ([`generators.cpp:360`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L360), and an unbound generator samples nothing, [`generators.cpp:639-641`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L639-L641);
  unset bones are filled with the reference pose at [`graph.cpp:1433-1449`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1433-L1449)).
  A behaviour graph is not required to fix it; a clip/asset fix is. The missing hands/feet are a separate body-part
  composition issue.
- **#471** (animals idle-less): `creature_idle.rs:1-4` is explicitly "idle-only ... not behavior-graph discovery or
  retargeting", with two bodies (cow, chicken) at `:28-40`. Spud's evidence says creatures work only when their own
  behaviour's classes are all registered and their own skeleton/clip names line up (no retargeting anywhere in
  `src/anim`). So per-species discovery, not a shared humanoid path, is the correct shape — which is what Mudcrab chose.
- **#399** recommends trialling "decode-only HKX with bone-binding fixtures first". That is exactly the first slice
  below: decode + bind + one transition, no simulation.
- **#400** supplies the ragdoll lane and the warning that a parsed class is not execution. Keep the four ragdoll
  modifiers out of scope until physics owns them.
- **#557** had not been read when this report was written, so its requirements are not covered here.

## Port notes

Pitfalls, in the order they will bite:
1. **One missing class silently removes a node.** [`graph.cpp:530-535`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L530-L535) logs at `debug` and continues. A Rust port must
   turn this into a counted, reported diagnostic, or a graph will "run" with a state machine that never transitions.
2. **Node state must not outlive deactivation** ([`node.hpp:5-9`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/node.hpp#L5-L9)). Cloning on activation is not an optimisation detail:
   `BlenderGenerator` and `StateMachine` rely on a fresh `m_active`/`m_current` each time.
3. **Retired transition effects must stay allocated** until the next collection, because records key on the object
   address ([`state_machine.hpp:309-313`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L309-L313)). Rust: use an id/generational arena, not a raw pointer key.
4. **Frame order is fixed**: collect → timesteps → update → `update_sync` → drain (≤5 rounds) → generate → foot IK
   ([`graph.cpp:1253-1290`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1253-L1290), `1400-1450`, [`graph.hpp:285-287`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.hpp#L285-L287)). Foot IK after generate, in world space, with the pose
   scale temporarily moved out ([`modifiers.cpp:1015-1040`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1015-L1040)).
5. **Conditions are never polled** ([`state_machine.cpp:1569-1571`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L1569-L1571)). Evaluate only when an event arrives, or states will
   flicker compared with vanilla.
6. **Expression precedence is Havok's, not C's**: logical operators bind tighter than comparisons and share one level
   ([`state_machine.cpp:287-289`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L287-L289)). A stock Pratt parser gives different results on `a == 1 && b == 2`.
7. **Event dedupe is by (id, payload pointer) within one round** ([`graph.cpp:1362-1368`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/graph.cpp#L1362-L1368)); the 5-round cap is spud's own
   invention, not vanilla. Decide deliberately.
8. **Constants worth copying**: 8 clip sync points / 16 sync slots ([`generators.cpp:26`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L26), [`node.hpp:43`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/node.hpp#L43)), 2 echoes
   ([`generators.cpp:27`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L27)), 32 simultaneous transitions ([`state_machine.hpp:210`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.hpp#L210)), limb-IK layer 43 and
   `k_scale = 69.991249` ([`modifiers.cpp:2340-2341`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2340-L2341)), "left early" below 0.2 of the clip ([`generators.cpp:2261`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L2261)).
9. **A zero playback speed is coerced to ±0.001** ([`generators.cpp:394`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L394)), so the pose creeps instead of holding; whether
   vanilla holds still is unverified but very likely. Any clip authored with speed 0 drifts.
10. **Damping has no timestep** ([`modifiers.cpp:1460`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L1460)) so it is frame-rate dependent; do not copy that.
11. **Paired animations need a shared clock and marks**, not Bevy transitions: `SyncScene` gates clip time on
    `all_at_marks` and lerps to the mark over `getToMarkTime` ([`generators.cpp:2219-2237`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L2219-L2237)).
12. **Ragdoll modifiers are placeholders by design** ([`modifiers.cpp:2458-2461`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L2458-L2461)). Do not read their presence as parity.
13. **Creature rigs get no retargeting**: a creature's clip must be bound to that creature's own bone names; spud's only
    generality is the shared `Skeleton`/`Project` type.

Minimal vertical slice that proves the subsystem (Mudcrab):
1. Pick the humanoid behaviour project named by `NordRace`'s `behavior_graph` (same field spud reads,
   [`actors.cpp:1318-1339`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/actors.cpp#L1318-L1339)). Extract it with the existing archive reader.
2. Implement only: the HKX member table for the classes that file contains, then `hkbStateMachine`, `hkbClipGenerator`,
   `hkbBlenderGenerator`, `hkbModifierList` (as a no-op list), `hkbBehaviorReferenceGenerator`.
3. Emit a versioned pack with: project → character → skeleton, clip table (reuse the existing three gait glTFs), node
   table, state ids, transition list, event names, variable table.
4. In the engine, drive one spawned actor: `IdleStop`-style event transitions the machine from idle to its stop state,
   the active clip's name changes, and the pose at frame N differs from frame 0.
5. Exact check, headless: a fixture JSON of `(event sequence) → (expected state id, expected active clip name)` compared
   after each event, plus a per-frame pose checksum for 30 frames. No window, no GPU, no assets beyond the extracted
   fixture.
6. Second slice, IK: `hkbFootIkDriverInfo` + `hkbFootIkModifier` on a flat plane; assert ankle height equals the ground
   within 1 unit and that the root does not drift.

## Test ideas

Deterministic and headless; all of these can run on fixtures extracted once from vanilla data.

1. **Node-class census** (highest value, cheapest): run the converter over every behaviour HKX and tally the class name
   of every object. Assert every class is in the supported set; print the residual list. This settles "which classes work
   end to end" with data instead of a claim, and it also settles the 224-class provenance question.
2. **Transition golden tests**: hand-written fixtures (no assets) of a state machine plus an event script, asserting state
   id after each event. Cover: priority ties, wildcard vs state list, delayed transition, uninterruptible-while-playing,
   return-to-previous, higher/lower with and without wrap, disable-condition flag.
3. **Expression parity**: extract every `hkbExpressionCondition` string from the behaviour set, evaluate over a fixed
   variable vector, compare against a reference table produced once (spud's op table or a Havok-side dump). Include
   `a == 1 && b == 2` to pin the precedence rule.
4. **Clip sampling parity**: for one clip, compare the Rust decoder's sample at frame boundaries against `hkaSpline`'s
   own control points; include the 6 rotation compression types ([clip.cpp:38-104](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/clip.cpp#L38-L104)) so a corpus gap is visible.
5. **Annotation → event mapping**: fixture clip with two annotations and a behaviour with one of the two event names;
   assert exactly one trigger fires and the unmapped one is dropped without an error.
6. **Blend weights**: three-child parametric blender, blend parameter swept 0→1 in 11 steps; assert weights sum to 1 and
   the middle child peaks at its anchor. Catches the cyclic-window arithmetic ([`generators.cpp:1286-1330`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/generators.cpp#L1286-L1330)).
7. **Event drain**: a node that raises the same event it handles; assert exactly 5 rounds then stop (pins the invention)
   or assert no infinite loop with your chosen cap.
8. **Foot IK on a step**: flat plane plus a 20-unit step under one foot; assert planted foot height and that the other
   foot stays down. Deterministic if the ray source is a fixture, not Rapier.
9. **Creature idle binding**: cow and chicken (the two bodies in `creature_idle.rs`); assert the clip's tracks resolve to
   that skeleton's bones and that a missing track is reported, not silently dropped (`skipped_tracks` already exists in
   `actor_idle_catalog.rs:33`).

## Open questions

1. Is the 224-class table the complete set of classes in Skyrim SE behaviour files? Settle by test idea 1; no generator
   script exists in the repo to prove provenance.
2. Do any vanilla behaviour files use a string condition (`conditionString`) or a fixed-time transition interval? The
   code compiles only expressions and warns about fixed times ([`state_machine.cpp:2105-2115`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/state_machine.cpp#L2105-L2115)); whether real data hits
   those paths is unverified.
3. Is `BSiStateTaggingGenerator.iStateToSetAs` consumed anywhere (engine or another class)? No consumer was found in
   `src/anim`.
4. Does anything read `BSSynchronizedClipGenerator.bSyncClipIgnoreMarkPlacement`, and how wrong is the result when it is
   set but ignored?
5. What is the real rule behind the 5-round event drain and the 2-echo cap? They look like guards, not vanilla values.
6. How often does `SampleSpeed`'s fallback (`goalSpeed`) diverge from Havok's speed sampler in practice?
7. Do the four ragdoll stubs need to be implemented before death animations look correct, or does the physics ragdoll
   path hide them? Needs a live ragdoll comparison, which is out of this read-only audit.
8. Which creature behaviours use classes outside the 45 registered? A census (test idea 1) answers this per species and
   would directly inform #471.
9. Issue #557's requirements are unknown here; if it scopes behaviour-graph work, this report should be reconciled
   against it.
