# Physics: spud collision, character controller and ragdoll API

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Spud `src/physics`: Jolt-backed static collision streaming, player controller, queries and ragdoll interfaces.

## Summary
- Spud provides Jolt-backed static collision streaming, a player character controller, ray queries, dynamic object hooks, and ragdoll interfaces.
- Scope is 7 files and 3,993 lines, measured with `find` and `wc -l`; `docs/MOVEMENT.md` documents user-facing limits.
- Maturity is strong for static world collision and walking; dynamic simulation and actor interaction are partial.
- Key features: streamed terrain/reference collision; deterministic body IDs; capsule-like player controller; collision masks; raycasts; object/animated-body hooks; ragdoll APIs.
- Constraints and triggers are represented in imported collision data, but broad runtime use is absent.
- Largest liberties: clutter remains static; swimming is absent; moving platforms, doors, and actor collision are absent.
- Several movement values are explicit placeholders rather than Skyrim-derived values.
- Worth borrowing: collision streaming radius, stable IDs, and a nonblocking loader/simulation boundary.

## Feature map
| id | feature | what vanilla Skyrim does (1 line) | how spud does it (approach, algorithm, key types) | where (file:line, symbols) | evidence | liberties | worth borrowing |
|---|---|---|---|---|---|---|---|
| physics-static-stream | Static cell collision | Loads Havok collision around the player and unloads distant cells. | Jolt world streams collision on a smaller radius than rendering; shapes are built on loader threads and installed on simulation thread. | [`src/physics/world.cpp:275`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/world.cpp#L275) `World::Impl`; [`:1617`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/world.cpp#L1617) `World::update`; `docs/MOVEMENT.md` Collision streaming | impl | different backend (Jolt); documented 64 m in/112 m out policy | yes, bound work by proximity |
| physics-terrain | Terrain collision | Uses land height data for ground contact. | Jolt heightfield from 33×33 land samples per cell. | [`src/physics/world.cpp:90-97`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/world.cpp#L90-L97); `docs/MOVEMENT.md` | impl | quantized heightfield described as under 2 mm error | yes, direct heightfield |
| physics-shape-import | Static mesh shapes and layers | Applies Havok shape hierarchy, material, and collision filtering. | Builds Jolt shapes from `.osp` with a character collision mask and selected layers. | [`src/physics/world.cpp:142`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/world.cpp#L142) `build_shape`; [`src/physics/physics.hpp:59`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/physics.hpp#L59) `character_layers` | impl | unsupported shape formats possible; LE packed strips are untested per docs | yes, filter before allocation |
| physics-controller | Walking/jumping controller | Character controller handles ground, air, jump, slope and step response. | `Character` uses custom step/contact logic and collision hull, with `ControllerInput`/`CharacterState`. | [`src/physics/physics.hpp:242-307`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/physics.hpp#L242-L307); [`src/physics/character.cpp:809-874`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/character.cpp#L809-L874) | impl | acceleration, air control, step/slope values include placeholders; no sneak/swim/fall damage | yes, separate state/input structs |
| physics-determinism | Stable collision/tick readiness | Havok simulation responds to loaded collision and game ticks. | Stable cell/reference body IDs and readiness gate hold player until near collision is present. | `docs/MOVEMENT.md` Determinism; [`src/physics/world.cpp:1617-1624`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/world.cpp#L1617-L1624) | impl | custom determinism policy, not evidence of full Havok equivalence | yes, deterministic ordering |
| physics-rays | Raycasts | Uses raycasts for controller, interaction, and IK queries. | World exposes ray hits; foot IK uses casts through character collision access. | [`src/physics/physics.hpp:50`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/physics.hpp#L50) `RayHit`; [`src/anim/modifiers.cpp:206`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/anim/modifiers.cpp#L206) | impl | supported query filters constrain caller behavior | yes, explicit hit record |
| physics-rigid-objects | Movable rigid bodies | Havok rigid bodies respond to forces and collisions. | World has object add/remove and animated reference entry points; docs state movable clutter is not simulated. | [`src/physics/world.cpp:1693-1718`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/world.cpp#L1693-L1718); `docs/MOVEMENT.md` Known gaps | partial | plates/baskets are static obstacles; motion type retained but not simulated | maybe, prioritize body lifecycle |
| physics-constraints | Constraints | Joints and constraints connect bodies, actors, and ragdolls. | Ragdoll constraint specifications exist; general constraint simulation coverage not established. | [`src/physics/physics.hpp:83-99`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/physics.hpp#L83-L99); `docs/MOVEMENT.md` Known gaps | partial | docs explicitly say constraints are not imported/simulated in world flow | maybe, typed constraint spec |
| physics-triggers | Trigger volumes | Phantoms/triggers drive gameplay events. | Trigger/water/weapon/projectile/pick layers are retained in `.osp` but not built into world collision. | `docs/MOVEMENT.md` Collision streaming; source layer handling [`src/physics/world.cpp:86`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/world.cpp#L86) | partial | trigger/phantom behavior absent | no, first define event contract |
| physics-ragdolls | Ragdoll bodies and constraints | Actors enter physics-driven ragdoll state. | API adds/removes ragdolls, reads body states, and ray-tests bodies. | [`src/physics/physics.hpp:64-99`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/physics.hpp#L64-L99); [`src/physics/world.cpp:1654-1691`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/world.cpp#L1654-L1691) | partial | docs say ragdolls are not imported/simulated as a gameplay path | maybe, bridge API only after lifecycle support |
| physics-cloth-hair | Cloth and hair | Uses Havok cloth/hair where assets and actors require it. | No cloth or hair simulation implementation was located in the 7-file subsystem. | `src/physics` file inventory and symbol search | unverified | likely omitted, no runtime proof either way | no, establish asset requirements |

## Data and file formats
- Reads imported `.osp` collision shapes and `.osw` world/land data; formats are documented in `docs/formats/osp.md` and `docs/formats/osw.md` (format references inferred from MOVEMENT doc).
- Converts Skyrim units to metres at the physics boundary ([`src/physics/character.cpp:48`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/character.cpp#L48)).

## Cross-area dependencies
- Needs importer collision and world cell data, plus engine player position and movement rules.
- Exposes ray hits and contact/manifold data to animation IK, player logic, and future interaction systems.
- Animation supplies ragdoll descriptors; audio receives material collisions from engine/physics integration.

## Tests, tooling and gates
- `docs/MOVEMENT.md` documents offline mesh/collision alignment analysis over 151 Whiterun meshes and same-script byte-identical traces at two resolutions.
- The report gives findings but not reproducible commands for those checks. No tests were run.
- Automation supports `move`, `jump`, `trace`, and state inspection (`docs/AUTOMATION.md`, referenced by MOVEMENT).

## Liberties and deviations
1. **Visible/gameplay correctness:** movable clutter remains static, so plates and baskets do not respond physically. Evidence: `docs/MOVEMENT.md` Known gaps; [`src/physics/world.cpp:1693`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/world.cpp#L1693) exposes object creation only.
2. **Visible/gameplay correctness:** water is not a movement medium; player can walk on the bottom. Evidence: `docs/MOVEMENT.md` Known gaps.
3. **Gameplay correctness:** no moving platforms/keyframed doors, ragdoll/phantom runtime, or character-vs-character collision. Evidence: `docs/MOVEMENT.md` Known gaps.
4. **Gameplay correctness:** acceleration, air control, step height, and slope limit use placeholders; gravity and speeds partly derive from data. Evidence: `docs/MOVEMENT.md` Values and where they come from.
5. **Internal and compatibility:** Jolt replaces Havok runtime while importing selected Havok collision shapes. Evidence: `src/physics/jolt.hpp` and [`src/physics/world.cpp:142`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/physics/world.cpp#L142).

## Claims versus code
- MOVEMENT claims streaming, controller, stable IDs, tick gating, and reports trace/alignment checks; source has corresponding world/controller APIs.
- The same doc clearly discloses placeholders and missing gameplay systems; static object APIs should not be read as proof of dynamic simulation.
- LE packed-strip decoding is documented as untested; classify its runtime support as unverified.

## Open questions
- Which `.osp` Havok shape variants have real asset coverage? Cheapest check: compare importer shape tags with `build_shape` switch and import diagnostics.
- Are ragdoll specs reached by a live engine caller? Cheapest check: search engine call sites for `add_ragdoll` and `ragdoll_state`.
- Do collision alignment measurements have retained input/output receipts? Cheapest check: locate the documented scan command and audit artifact.
