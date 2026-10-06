# SPEC

## §G GOAL

Build Skyrim-derived player movement in phases. First milestone: retain working Riverwood WALK/NOCLIP controller, drive walk/run speeds from resolved game records in Creation units, and keep basic jump working while resolving its record-backed parameters. Expand record conversion and behavior only when later phases need them; full controller parity remains long-term goal.


Dynamic ingestion initiative: newest Steam Skyrim SE/AE `1.7.104.0` / build `24914197` first; 100% complete catalog/variant/field interpretation + lossless source + Mudcrab-owned canonical data + current-game compatibility. Earlier SE backwards & VR only after P6 acceptance, then LE, then other games. [Approved plan](docs/roadmap/dynamic-schema-initiative.md) defines gates. Movement goal/statuses above remain owned by movement work.

## §C CONSTRAINTS

- Initial target: Skyrim Special Edition master records in current converted asset set; preserve plugin order and winning override. Runtime oracle capture is optional, not a gate for basic walk/run/jump.
- CommonLibSSE-NG headers establish native structures, names, and record links; they do not establish update equations or prove behavioral use of every field. Derive expected flat-ground distance from verified `MOVT` speed units and elapsed fixed-step time; use selected `GMST` values only after identifying their units and role. Label project-derived formulas as such, without claiming exact executable parity.
- Engine/Rust owns input dispatch, camera, actor state, movement solver, collision, animation state requests, fixed-step integration, and native Papyrus implementations. Records supply immutable definitions; runtime state supplies mutable actor values, active effects, inventory, control locks, and save data. Papyrus invokes native operations and reacts to events; script source does not implement contact resolution or per-frame locomotion.
- FormID/load-order resolution, overrides, sex/race selection, missing-form policy, and record provenance precede tuning. Retail Player `NordRace` has no `WKMV`/`RNMV`; Q2 selects `NPC_Default_MT` FormID `0x0003580D` as explicit first-pass profile pending native selection-path evidence. Never infer a value from an unrelated GMST name or copy a header offset as a record layout.
- Legacy P1–P3, V1–V34, and T1–T15 document prototype behavior/history. `MovementTuning` remains responsible for working WALK physics; Q2 replaces only verified walk/run/jump inputs. Capsule, eye height, slopes, and other guesses stay provisional for later phases.
- Riverwood primary manual physics test area; primitive fixture retained for automated regression only.
- Prototype stack: Bevy `0.19.0`; Rapier 3D `0.36.x`. Q2 reuses current Rapier controller and fixed-step integration; later replacement requires a concrete mismatch and Riverwood regression gate.
- Linux build/test → `devenv shell` supplies Bevy Wayland pkg-config libraries.
- Rust release upgrades deliberate: `rust-toolchain.toml`, CI toolchain inputs & `RUSTUP_TOOLCHAIN` ! same fixed version; `devenv` reads toolchain file.
- Former P3 moves first, merged with thin WALK/tankard fixture from former P4; phases renumbered in execution order. Every collision gate uses same player capsule & dynamic tankard, not probes alone.
- Render terrain/collision terrain share validated 33×33 quadrant geometry, transforms, & streamed lifetime.
- Static collision source explicit per converted asset. Prefer original NIF/Havok collision for placed `STAT`/`TREE`/`FURN`; `FURN` gets no render proxy. Where unavailable, use declared render-triangle proxy only for verified `STAT`/`TREE` solids. Never use broad bounds boxes or treat all visuals as solid. Record proxy/skipped coverage.
- Converted GLB scene extras carry NIF collision provenance and supported Havok geometry; authored absence and unsupported extraction remain distinct from legacy GLBs.
- Database `statics` table also holds `MISC`/other movable model records; fixed collider eligibility requires base record type or equivalent authoritative metadata. Movable clutter cannot receive both fixed & dynamic colliders.
- Debug tankard uses visible cup/handle geometry & compound convex collider; converted tankard model absent in inspected asset set. Label it as physics fixture, not Skyrim asset parity. P1 auto-spawns in fixture; P2+ `T` spawns bounded debug tankards in loaded interactive world.
- Existing benchmark, visual fixtures, screenshot, headless, & `--auto-fly-speed` paths retain current camera behavior; interactive Riverwood owns controller.
- Physics uses Creation units throughout; WALK & dynamic tankards share gravity constant. Origin rebase moves Rapier body poses with Bevy transforms, preserving velocity; shifting visual transforms alone insufficient.
- First milestone excludes new camera states, Papyrus bindings, actor-value/effect stacking, mounts, swimming, sneak, combat, animation, and general movable clutter. `Alt` sprint uses a provisional 1.5× directional run factor with unlimited duration; debug tankards and pickup stay available. Later phases add only records needed by their behavior.
- Phases remain planned until gate evidence passes. Later phase work waits for predecessor acceptance.

id|state|capability|gate
Q0|accepted|record/value map|Player race, selected `MOVT`, candidate `GMST`, source hashes, unit assumptions and project formulas in `docs/player-movement-record-map.md`
Q1|accepted|minimal typed conversion|`NPC_` link + winning `RACE` validation, selected `MOVT`, candidate `GMST` fields load with overrides/provenance; converter/runtime tests pass
Q2|planned|record-backed walk/run + basic jump|existing WALK controller uses Q1 values where verified; flat-distance, jump, Riverwood collision/regression gates pass
Q3|deferred|modifiers and added gaits|add `AVIF`, `SPEL`, `MGEF`, `PERK` only for supported speed/jump/sprint/sneak rules
Q4|deferred|water and movable world|add `WRLD`, `CELL`, `LAND`, `REFR`, `WATR`, and required base object records for swim/contact
Q5|deferred|camera, animation, scripts|add equipment/animation forms and `QUST`/VMAD as corresponding behavior ships
Q6|deferred|special player states|mount, furniture, transformations, bleedout, dragon; identify record dependencies per state

Q0–Q2 form current delivery. Q3–Q6 are parked scope; none blocks basic WALK release.

id|state|capability|depends_on|gate
P1|planned|NOCLIP + WALK + dynamic fixture|-|mouse camera, `V`, walking capsule, debug tankards pass primitive slope/wall tests
P2|planned|streamed terrain collision|P1 accepted|player walks/jumps across hill/seams; tankards roll/rest on hill; unload/rebase pass
P3|planned|fixed static collision|P2 accepted|player blocked by rock/wall, passes doorway; tankards hit statics without tunneling


### Dynamic ingestion scope

- P0–P6 newest SE only; runtime exe SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`. Full official Data/Creation/locale/load-order manifest ! pin; unresolved inputs ⊥ guessed. `1.6.1170.0` static importer evidence ! transfer proof before applicability on 1.7.104.
- Movement-specific consumed-field restrictions above scope movement consumers; complete ingestion canonical fields include data unused by current engine. Existing movement goals/tasks/invariants unchanged.
- xEdit primary physical-layout evidence; independent Mutagen/native validation. Canonical API Mudcrab-owned, ⊥ clone upstream object models. Raw-only/unexplained/disputed fields block P6 100% gate.
- Extend current `converter::esm` owners after P0 gate; source immutable, resolved/runtime projections separate. No earlier-game reader/framework, blanket FormID rewrites or speculative API.
- Active `mudcrab-reverse-engineering` checkout/store/decompilation read-only for this slice; use isolated worktrees/build outputs. Existing oracle source reused with recorded digest; independently authored extension at validation owner. Runtime evidence later from pinned executable; ⊥ copy proprietary/decompiled source into Mudcrab.
- No archive/editing/parser adoption before scoped preservation/profile/identity contracts + P0 acceptance. Tool unavailability recorded, not silent success. P0 source inventory/qualification may proceed while native research runs.

## §I INTERFACES

- I.launcher_lod: implemented launcher LOD reporting; planned existing-assets build action; contract and gates in `docs/specs/modding/launcher-lod.md`. Fiji RC excludes build action.
- cmd: `--physics-fixture` → interactive primitive hill/wall arena, WALK/NOCLIP toggle, auto-spawned debug tankards; no Skyrim asset install required.
- runtime: P1+ interactive exterior → first-person NOCLIP at start-cell view; fixture supports both modes from P1. P2 enables WALK over streamed terrain.
- key: NOCLIP `W/A/S/D` fly relative to view; `Space` rise; `Ctrl` descend; `Shift` accelerate.
- input: mouse look; click viewport captures pointer; `Escape` or focus loss releases pointer; uncaptured pointer → no movement/look.
- debug: `NOCLIP: ON  [V]` or `NOCLIP: OFF  [V]`; `[V]` identifies toggle key, no player glyph; blocked WALK entry shows reason.
- debug: interactive Riverwood `[F3]` toggles Rapier collision outlines; status reports ON/OFF.
- key: `V` → toggle NOCLIP ↔ WALK once per press; NOCLIP default ON on interactive start.
- key: WALK `W/A/S/D` move relative to yaw; mouse look; `Space` jump; `Shift` walk slowly; `Alt` press while moving toggles sprint; no movement input, focus loss, or NOCLIP switch clears sprint. No stamina cost.
- key: P2+ `T` → spawn one debug tankard ahead of camera only when local terrain collider ready; bounded live count; fixture auto-spawns several in P1.
- key: P2+ `E` → pick aimed nearby debug tankard; next `E` drops it. Held item follows view with collision/gravity suspended; release restores dynamic physics.
- mode: NOCLIP→WALK in free space keeps position; overlap → bounded upward search for free capsule placement; missing collision/no safe placement → remain NOCLIP & show reason.

### Movement data flow

- source: winning `NPC_` Player record → `RACE`; absent `WKMV`/`RNMV` → explicit first-pass `NPC_Default_MT` `MOVT` selection. Separately inspect named `GMST`. Export typed consumed fields plus source FormID/plugin and raw record. Existing `MovementTuning` remains runtime owner; Q2 fills selected walk/run fields after asset load.
- unit: document raw field type, evidenced or unknown source unit, project conversion factor, and resulting Creation units/s or units/s² for each consumed value. First pass treats `MOVT` speed as Creation units/s with factor 1; unit remains inferred. At steady speed on flat ground, expected distance = effective speed × elapsed fixed-step time; acceleration/contact makes start-from-rest distance different.
- jump: select record-backed height/impulse/gravity only with established field semantics and units. If height + gravity are confirmed and no impulse field exists, project model uses `launch_speed = sqrt(2 × gravity × target_height)`; this is a derived initial rule, not a recovered Skyrim update equation. Keep current jump values labeled provisional until source mapping is established.
- runtime: reuse current WALK input, grounded check, fixed-step integration, camera, collision, `V` NOCLIP, `F3`, and tankard debug path. No new player state object, Papyrus binding, or native solver for Q2.
- later: when Q3+ needs actor values, effects, scripts, or save state, keep their mutable runtime state separate from immutable record definitions. Papyrus invokes engine-owned operations; scripts do not solve contacts.

### Record extraction contract

Each phase adds typed conversion for consumed fields only. Retain raw subrecords, winning override, source plugin/FormID, and actionable errors for malformed required links or values.

phase|record types|minimum new fields|consumer
Q1|`NPC_`|reuse existing Player race FormID; height/sex only if chosen value rule needs them|player-to-race link
Q1|`RACE`|inspect optional walk/run movement FormIDs (`WKMV`/`RNMV`); retail Player `NordRace` leaves both absent|race profile evidence
Q1|`MOVT`|directional walk/run speed data (`SPED`) and fields actually used by Q2|flat movement targets
Q1|`GMST`|allowlisted candidate names, type, value; consume only after role/units confirmed|jump parameters if source identified
Q3|`AVIF`, `SPEL`, `MGEF`, `PERK`|actor-value IDs and effect/condition links required for speed/jump modifiers|modified gaits
Q4|`WRLD`, `CELL`, `LAND`, `REFR`, `WATR`, `STAT`, `TREE`, `FURN`, `ACTI`, `DOOR`, `CONT`, `MISC`, `FLOR`|water/contact and placed-object fields beyond existing collision export|swim and movable contacts
Q5|`WEAP`, `ARMO`, `ARMA`, `KYWD`, `EQUP`, `ENCH`, `ALCH`, `INGR`, `IDLE`, `QUST`/VMAD|equip/effect/graph/script fields only as implemented|camera, animation, scripts
Q6|state-specific forms from earlier rows|identify exact links for mount, furniture, transformation, bleedout, dragon|special states

`CLAS`, `MATO`, and `MATT` remain conditional on a demonstrated movement/contact dependency. Core input mapping and INI values are external to plugin record extraction.


### Dynamic ingestion interfaces

- artifact: P0 corpus manifest → executable pin, ordered file identities/digests/sizes, observed TES4 metadata/masters, declared content/load-order/locale status & unresolved inputs. Installed source remains unmodified; proprietary bytes/retail record dumps remain local.
- artifact: qualified oracle JSONL → before-merge major-record observations, tool/input provenance, typed values/references where exposed, explicit unavailable observations & errors. Existing placed-record command retains behavior.
- document: `docs/roadmap/dynamic-schema-initiative.md` → complete phase/gate contracts; `docs/research/dynamic-schema/` → reproducible candidate evidence, not accepted schemas.

- artifact: `docs/research/dynamic-schema/schema-explorer.html` → offline searchable source catalog, current SQL tables/columns, selected field/native evidence & labeled unknowns; generated from pinned project/MCP facts.

## §R RESEARCH

id|topic|finding|src
R1|dependency|`bevy_rapier3d 0.36.0` declares Bevy `0.19.0` dependency|https://docs.rs/crate/bevy_rapier3d/0.36.0/source/Cargo.toml
R2|controller|Rapier kinematic controller exposes translation, slope climb/slide angles, autostep, & ground snap|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/control/struct.KinematicCharacterController.html
R3|controller result|Rapier output exposes grounded, effective translation, collisions, & slope sliding|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/control/struct.KinematicCharacterControllerOutput.html
R4|terrain collider|Rapier `Collider::trimesh` consumes vertices + triangle indices & returns `Result`; handle failure during cell commit|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/geometry/struct.Collider.html#method.trimesh
R5|Skyrim controller|reverse-engineered `bhkCharacterController` exposes gravity, jumpHeight, supportNorm, collisionBound, & step/jump flags; no native update body|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/B/bhkCharacterController.h
R6|Skyrim input|`hkpCharacterInput` includes movement, jump intent, gravity, velocity, & surface info; exact player values unmeasured|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/H/hkpCharacterContext.h
R7|Skyrim slope|`hkpCharacterProxy` names `maxSlopeCosine` & friction fields; player use/value of each field unmeasured|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/H/hkpCharacterProxy.h
R8|dynamic shape|Rapier supports compound colliders for multi-part debug tankard shape|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/geometry/struct.Collider.html#method.compound
R9|dynamic body|Rapier supports `RigidBody::Dynamic` for gravity-driven tankards|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/dynamics/enum.RigidBody.html
R10|contact|Rapier exposes CCD for fast dynamic bodies; enable only if gate observes tunneling|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/dynamics/struct.Ccd.html
R11|native input|`PlayerControls` holds movement/look/sprint/sneak/jump/POV handlers; `PlayerControlsData` stores move/look vectors and running/auto-move flags; handler algorithms absent|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/P/PlayerControls.h ; https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/P/PlayerControlsData.h
R12|native camera|`PlayerCamera` enumerates first person, third person, free, furniture, mount, transition, bleedout, dragon states; offsets/transition rules unproven by enum|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/P/PlayerCamera.h
R13|native state|`hkpCharacterState` enumerates ground, jump, air, climb, fly, swim and virtual `Update`/`Change`; `hkpCharacterInput` carries directional input, jump, surface, gravity, velocity, and step info|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/H/hkpCharacterState.h ; https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/H/hkpCharacterContext.h
R14|native controller|`bhkCharacterController` holds `gravity`, `jumpHeight`, `waterHeight`, `swimFloatHeight`, `actorHeight`, `supportNorm`, `collisionBound`, and step/support flags; no formula for deriving these from records|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/B/bhkCharacterController.h
R15|race|`TESRace::RACE_DATA` has sex-specific height, mass, acceleration/deceleration; race links six `BGSMovementType` forms and behavior graphs|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESRace.h
R16|movement form|`BGSMovementType` contains `Movement::TypeData` with directional walk/run speed table, rotation, and movement-speed fields; header alone does not establish units or update equation|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/B/BGSMovementType.h ; https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/M/Movement.h
R17|actor values|native `ActorValue` includes `SpeedMult` and `JumpingBonus`; `ActorValueOwner` has base/current/modification methods; `ActorValueInfo` is a form with enum name and abbreviation|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/A/ActorValues.h ; https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/A/ActorValueOwner.h ; https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/A/ActorValueInfo.h
R18|player base|`TESNPC` has height field; converter currently extracts NPC race/class/flags and retains raw records; runtime has no typed RACE/MOVT/GMST player resolver|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESNPC.h ; crates/converter/src/esm/exporter.rs
R19|Papyrus boundary|vanilla `Game.psc` declares native control/POV/settings functions; `Actor.psc` declares native actor-value/race functions; signatures do not implement movement physics|https://github.com/ianpatt/skse64/blob/25b72352adb6543fa6d0bd3795780672b2e238e0/scripts/vanilla/Game.psc ; https://github.com/ianpatt/skse64/blob/25b72352adb6543fa6d0bd3795780672b2e238e0/scripts/vanilla/Actor.psc
R20|effects|`EffectSetting` carries effect flags, associated form/skill and archetype; `BGSPerk` has entries/conditions; behavioral influence requires record and runtime evidence|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/E/EffectSetting.h ; https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/B/BGSPerk.h
R21|prototype implementation|`MovementTuning` currently hardcodes movement, gravity, capsule and eye values; `PlayerControlsPlugin` uses Rapier KCC; converter stores all raw records but typed `NPC_` subset only|crates/engine/src/physics.rs ; crates/converter/src/esm/exporter.rs
R22|retail movement values|winning Player `0x00000007` from `ccbgssse018-shadowrend.esl` RNAM `NordRace` `0x00013746`; no `WKMV`/`RNMV`; `NPC_Default_MT` `0x0003580D` SPED left 80.09/370, right 79.75/370, forward 80.10/370, back 71.93/205.25; unit/s inferred|docs/player-movement-record-map.md ; https://github.com/Mutagen-Modding/Mutagen/blob/dev/Mutagen.Bethesda.Skyrim/Records/Major%20Records/MovementType.xml
R23|retail GMST candidates|`fMoveCharWalkBase` `0x0001EC72` = 100; `fJumpHeightMin` `0x000ABEF6` = 76; no proven player-speed/impulse mapping or gravity GMST|docs/player-movement-record-map.md


R24|dynamic target|RE lock pins Steam Skyrim SE/AE 1.7.104.0/build 24914197; 1.6.1170 static importer-native; complete Data manifest pending|docs/roadmap/dynamic-schema-initiative.md#reverse-engineering-stack-and-existing-evidence
R25|source candidates|pinned xEdit/Mutagen scrape has 127 shared signatures + six xEdit-only candidates; broad release gates & unused/save-related declarations require review|docs/research/dynamic-schema/candidate-inventory.json ; docs/research/dynamic-schema/README.md
R26|existing oracle|RE `oracles/records` Mutagen 0.54.4/.NET SDK9.0.318 placed pilot; F0005 matching counts only, runtime consequences unverified|docs/roadmap/dynamic-schema-initiative.md#reverse-engineering-stack-and-existing-evidence

## §V INVARIANTS

V1: P2 ∀ valid visible terrain quadrants → matching fixed collider from same positions/indices & parent transform; invalid/hidden terrain → no collision; unload/reload → no stale/duplicate collider; player & tankard cross seams.
V2: P2 collider build error → reported asset/cell failure, no panic or invented floor; existing render/streaming acceptance behavior preserved.
V3: P3 ∀ eligible fixed placements → collider follows full instance/node transform & cell lifetime; source marked original or proxy; unsupported assets skipped & counted. Base record type distinguishes fixed statics from `MISC`/other movable clutter; no duplicate fixed/dynamic collider.
V4: P3 player blocks at rock/wall, passes doorway/opening & excluded visual; tankard hits same rock/wall without tunneling, falls through opening as geometry permits; unload/reload leaves no stale collider; P2 gate remains green.
V5: P1 interactive exterior → one streaming camera, NOCLIP ON, walking collider/gravity disabled; camera crosses fixture/world geometry; noninteractive paths unchanged.
V6: P1 mouse yaw/pitch controls view, pitch bounded; uncaptured/unfocused input cannot move or look; focus loss clears held keys; render-origin rebase preserves position & view.
V7: P1 debug overlay reports actual mode every frame with adjacent `[V]` key hint; overlay never obscures center view or screenshot path.
V8: P1 `V` press toggles once; NOCLIP→WALK at any height when capsule fits; failed overlap search leaves NOCLIP ON with visible reason; WALK→NOCLIP removes collision/gravity immediately.
V9: P1 WALK capsule upright independent of camera pitch; camera follows body at eye offset; yaw moves body, pitch moves view. P2+ render-origin rebase shifts Rapier body poses & Bevy transforms together; body/camera/tankards/world colliders stay aligned without added velocity.
V10: P1 WALK input normalized & camera-relative; acceleration/deceleration bounded; WALK < RUN < SPRINT; velocity frame-rate independent. Initial guesses: walk 160, run 300 Creation units/s, sprint 1.5× directional run; horizontal acceleration 1800 units/s².
V11: P1 gravity applies while airborne; jump requires grounded contact & new `Space` press; no air repeat. Initial guesses: gravity 900 units/s² downward, jump launch 340 units/s upward; fixture records actual apex/time.
V12: P1 capsule radius 28, standing height 100.8, eye height 89.6 Creation units; slope climb 50°, slide 55°, autostep height 36 units, ground snap 12 units; provisional values centralized. Fixture ground, wall, step, slope outcomes match configured behavior.
V13: NOCLIP→WALK requires free capsule placement, independent of ground distance; failed entry keeps NOCLIP with reason. Active airborne WALK applies gravity through absent ground collision; no synthetic floor or loading claim from ray miss. NOCLIP remains available.
V14: P1 toggling clears stale flight/walk velocities & jump state; mode/overlay/collision response agree after switch in air, on ground, or near obstacle.
V15: P1 dynamic debug tankards use 60 Hz Rapier fixed step, same downward 900 Creation units/s² gravity as WALK, & compound convex colliders; fixture cups tumble/roll downhill, contact ground/wall, settle without persistent penetration; P2+ `T` spawns only near loaded collider, capped at 32 live bodies; switch to NOCLIP leaves tankard physics active.
V16: ∀ collision phase gate P1–P3 → exercise actual WALK capsule & dynamic tankard against phase geometry; probe-only success insufficient. Record position/contact outcomes, frame-step settings, asset/collision provenance, & manual playtest result.
V17: Player WALK, terrain, fixed statics, & tankards share one Rapier physics context with matching collision groups; player & tankards contact surfaces, tankards contact player, NOCLIP camera contacts none.
V19: P1 character controller keeps `apply_impulse_to_dynamic_bodies=false` until upstream Rapier manifold-transfer panic fixed; hill gate walks slope with tankards nearby & asserts zero Rapier panics.
V18: Input sampled once/frame, movement integrated once/60 Hz physics tick; streamed collider commits & origin rebase reach Rapier before next physics tick. Same fixture inputs at 30/60/120 render fps → positions/contact outcomes within declared tolerance.
V20: Interactive Riverwood uses one controller camera; WALK yaw & pitch consume current `LookIntent`; mouse up/down changes view while capsule stays upright. Benchmark/screenshot/headless/auto-fly camera behavior preserved.
V21: P2 `E` picks aimed `DebugTankard` within 240 units only; held body follows camera, collision & gravity suspended; second `E` releases dynamic body without stale velocity. Unload/rebase/mode toggle leaves no dangling hold. Overlay shows `T`/`E` controls.
V22: Fiji Riverwood launcher with unset display vars + live `/run/user/<uid>/wayland-N` socket → set `XDG_RUNTIME_DIR` + `WAYLAND_DISPLAY` before engine; preserve explicit display vars; no socket → clear launcher error.
V23: P3 `RockCliff` `BLEND` rock faces → render proxy; `MASK` detail & unrelated blended visuals → no proxy. Fiji warning log ! `RockCliff` skipped for no eligible triangles.
V24: P3 legacy GLBs: `TREE` pine models & `STAT` pine logs/stumps, firewood piles, road ramps → fixed render proxy from supported primitives; `FLOR`, plant `TREE`, movable records → no fixed collider.
V25: P3 lumbermill walkway `MASK` primitive → proxy; masked roof/rope & unrelated masked primitives → no proxy.
V26: WALK crosses adjacent 15-unit floor rises without jump while existing wall/24-unit step tests remain green.
V27: P3 converted `STAT`/`TREE` with physical NIF Havok layer + supported shape → authored collider; absent collision or `NONCOLLIDABLE` layer → passable; unsupported shape → counted reason. Legacy GLBs alone use narrow proxy policy.
V28: P3 bridge/stair compressed mesh follows authored triangles, chunk transforms, body and node transforms; WALK crosses deck/treads without render-beam snag; tankard contacts deck. Invalid refs, indices, transforms → skipped reason, no invented collider.
V29: Converter output-contract change bumps producer schema; combined converter 24/world 7 includes native-BC DDS, authored emission/specular/normal/UV/alpha, rigid-body metadata, grass & LOD. Runtime accepts complete converter schemas 15–24 & world schemas 3–7; LOD package ! exact 24/7 identity; older/newer/incomplete assets fail startup.
V30: Historical converter schema 12–15 → 16 migration invalidated GLBs while preserving unchanged texture/script entries. Current producer 24 migration follows V73 & V112; older schema number alone grants no mesh/texture compatibility.
V31: Packaged Riverwood 5×5 and wider grid `x=-1..11,y=-18..-6` audits → zero unsupported fixed `STAT`/`TREE` models; bridge, stairs, lumbermill, pine solid & clover passable.
V32: P3 authored multi-shape placement → each mesh/primitive attached as child collider to one fixed body; ⊥ nested Rapier compounds; all child colliders use world groups and answer contact queries without panic.
V33: P3 placed `FURN` with supported authored NIF collision → fixed streamed collider; authored absence → passable; `FURN` gets no render proxy. MillLogPile contact blocks WALK and tankards. `MISC`/`FLOR` remain outside fixed path.
V34: Interactive Riverwood `F3` → toggle actual Rapier collider outlines; startup OFF, status matches toggle; entirely behind-camera collider bounds hidden, bounds crossing view plane shown; noninteractive camera paths retain prior behavior.
V35: Q0 source manifest ! master/plugin files and hashes, load order, winning Player `NPC_` FormID, linked `RACE` FormID, explicitly selected `MOVT` FormID and absent race links, selected `GMST` names and values. No runtime sampler required for Q0–Q2.
V36: Q0 field map distinguishes CommonLib header field, ESM subrecord, value type, source unit, conversion factor, resulting Creation unit, and Q2 consumer. Only mapped fields enter tuning.
V37: Q0 walk/run steady-state target for flat unobstructed motion = resolved effective `MOVT` speed in Creation units/s × elapsed seconds. Acceleration phase and contacts evaluated separately; header layout alone does not establish conversion factor or update formula.
V38: Q0 jump map identifies source of target height/impulse and gravity. When height and gravity have known units, `sqrt(2 × gravity × height)` defines project launch-speed calculation; no claim this is Skyrim's internal formula. If no confirmed record source exists, retain provisional jump tuning and report gap.
V39: Q1 typed extraction reuses `NPC_` race link, validates winning `RACE` and optional walk/run links, adds selected `MOVT` speed fields and allowlisted `GMST` candidates. `AVIF`/effects/quests/animation remain later-phase work.
V40: Q1 resolver applies winning override and FormID remap, preserves plugin/FormID provenance, validates Player `NPC_`→`RACE`, and rejects missing selected `MOVT` with record/field error. Absent retail `WKMV`/`RNMV` ! remain explicit; existing raw records remain available.
V41: Q1 decoded speed, gravity, jump values finite and in valid range; invalid type/length/value names exact source. No silent zero, guessed setting name, or arbitrary fallback in a package claiming record-backed movement.
V42: Q2 existing `MovementTuning`/Rapier fixed-step WALK path consumes resolved walk/run parameters; existing input, grounded jump guard, camera, collision, and NOCLIP remain authoritative. No second solver or speculative `PlayerRuntime` layer.
V43: Q2 forward/back/strafe speeds use resolved direction-specific `MOVT` fields where established; diagonal input keeps bounded normalization. Any movement-mode/direction without a confirmed field keeps labeled provisional tuning.
V44: Q2 deterministic empty-fixture steady walk/run displacement matches Q0 `speed × time` within max(1 Creation unit, 1% target) over 2 seconds at 60 Hz; start-from-rest checks current acceleration integration separately.
V45: Q2 jump still requires grounded press and no air repeat. When record-backed height/gravity exist, free-flight apex matches derived project target within max(2 Creation units, 2% height); otherwise current jump remains functional and marked provisional.
V46: Q2 Riverwood WALK traverses existing bridge, lumbermill, stairs, terrain seams and blocks at solid trees/buildings; mode toggle, origin rebase, tankard contact, and 30/60/120 render-fps fixture regressions stay green.
V47: Q2 record-backed values enter physics before fixed step; changing render fps does not alter movement distance, jump result, or contact category for equal simulated time. No per-frame tuning mutation or duplicate integration.
V48: Q2 diagnostic reports effective walk/run/jump/gravity values and source record/FormID or provisional constant once at startup; warning+ log records rejected/missing fields without flooding each tick.
V49: Q2 new complete packages with missing required walk/run records fail with actionable error; legacy packages may use existing provisional tuning only when explicitly identified as legacy. NOCLIP recovery remains available.
V50: Q3 adds actor-value/effect records and sprint/sneak only when their chosen behavior is implemented; immutable record definitions and mutable actor state remain separate.
V51: Q4 adds water/world and movable-object fields only as needed by swimming and contacts; existing authored collision/proxy provenance rules still apply.
V52: Q5 adds camera, animation and Papyrus records/functions only when corresponding behavior ships; CommonLib state names are not movement equations.
V53: Vanilla capture/measurement is optional diagnostic for unresolved formulas or later parity claims, never prerequisite to Q0–Q2 implementation. Static record calculation and engine regression are sufficient for this first milestone.
V54: Developer `V` NOCLIP, `F3`, `T`, `E`, fixture, overlay, benchmark and headless paths retain behavior while Q2 tuning changes; debug flight excluded from walk/run distance checks.
V55: Full Skyrim controller parity remains unclaimed until later phases validate every supported gait, camera, water, script-modified and special state. First milestone reports exactly which values are record-backed and which remain provisional.
V56: Fiji Riverwood package bundles glibc-matched `libdl.so.2` beside `libvulkan.so.1`; missing library → named launcher error; target startup → Vulkan `AdapterInfo` and runtime initialization.
V57: WALK `AltLeft` or `AltRight` new press with movement input toggles sprint latch; key release preserves latch; second press turns it off; zero movement input, uncaptured focus, or mode switch clears latch. Sprint has no stamina cost; provisional multiplier and fixed-step integration remain.
V58: Jump fixture measures `PlayerBody` pose from grounded rest through launch apex and landing; camera and other transforms cannot satisfy or fail the jump gate.
V59: Portable Riverwood launcher leaves host Vulkan ICD discovery intact and preserves caller `VK_ICD_FILENAMES`; bundled ALSA config hook resolves through package `lib` on target. Target smoke reaches `AdapterInfo` and runtime initialization.
V60: WALK overlay reports `Alt` latch and actual horizontal speed from Rapier `effective_translation` per fixed step, never target speed; under 5 Creation units/s contact jitter reads 0, vertical jump excluded. Controlled perspective gains 8° FOV only during moving WALK sprint, with 0.12 s exponential half-life back to base on stop/mode change; noninteractive cameras retain base FOV.
V61: Provisional sprint target = 1.5× selected directional run speed, including winning `MOVT` fields; packaged forward 370 → 555 Creation units/s. Collision-free fixed-step actual speed reaches targets; blocked speed remains 0 despite nonzero target.
V62: WALK entry has no ground-distance limit; active WALK keeps falling when ground lies far below or no ground ray hits.
V63: Compressed Havok mesh vertices scale by finite positive serialized quantization error; alternate valid scales preserve geometry.
V64: Selected `MOVT SPED` accepts 40- or 44-byte layouts; rejects other lengths and invalid speed fields.
V65: Package launcher resolves XKB data on Nix and standard Linux paths or reports missing data before engine start; configured valid `XKB_CONFIG_ROOT` survives.
V66: WALK view pitch follows `LookIntent.pitch`; body pitch stays zero.
V67: `Space` press remains latched across Update frames until WALK fixed tick consumes it; no repeat after consumption.
V68: Exactly one fixture mode selected per run; physics fixture benchmark passes only after validation with zero fixture failures.
V69: Active airborne WALK with no downward collision ray continues descending across fixed ticks; ground-ray miss alone never resets velocity or reports terrain loading.
V70: NOCLIP→WALK from far above ground enters WALK & descends when capsule fits; overlap rejection keeps NOCLIP with reason after bounded upward search.
V71: Runtime accepts passed integration report & world database schemas 3–7; grass-only world 5 loads without LOD tables; legacy world 6 LOD query remains valid; older/newer schemas rejected.

V72: ∀ staged GLB used by texture pruning or world integration → accepted current-run mesh artifact with current schema/configuration, source/dependency hash & output-byte proof; prune audit record alone grants no provenance. Removed source → no bounds update or published GLB; unavailable-source accounting preserved. Same-schema verified pruned resume retains mesh & audit record.

V73: Converter 24 migration from known legacy producers 12–23 rebuilds every GLB/KTX2 & world output; unchanged scripts/archive ingestion reuse ! source/configuration/output proof. Ambiguous schema 18 native-BC/specular identity grants no mesh/texture reuse. Current configuration hash ! encoder selection & GPU quality; batch size scheduling-only. Staged mesh/texture records ! exact current producer 24 & V72 proof; ⊥ relabel old bytes.

V74: Source-surface contract from producer 23 retained by combined 24; schema 22 meshes regenerate. Tangent normal convention, authored UV/clamp aliases & enabled vertex/alpha channels survive conversion and native loading; legacy producers ⊥ relabel to current; V72 provenance & V73 encoder identity retained.

V92: Launcher conversion runs existing terrain LOD stage exactly once before final validation/publication; LOD failure cannot report successful conversion. No second compiler or duplicate post-publication job.
V93: Launcher retains LOD chunk count & LOD warnings; zero chunks, partial world coverage & compiler failure remain distinct. LOD stage label/count accurate; no invented ETA or benchmark acceptance.
V94: Existing-assets LOD build verifies source manifest, retained asset hashes & matching plugin order/checksums; publishes schema-24/7 derived output to new disjoint directory. Source assets remain unchanged; metadata version relabeling forbidden.
V95: Existing-assets LOD build unavailable until cancellable metadata API & worker/state tests pass; cancellation checked between retained files & worlds and before publication; in-flight file/world allowed to finish; never reports resumable staging without journal support. Engine-running/path/asset-lock guards retained.
V96: Publication backups identified by full output name; record-owned recovery restores supported manifests with verified retained output sizes/hashes; incomplete status retained; invalid or symlink backups remain untouched.
V97: Equivalent existing asset paths share one canonical lock; acquisition fails on resolution errors; missing-tail paths normalized before report containment checks.
V98: LOD readiness counts unique queued, loading & scheduled retry work; current unrecovered failures block screenshot; recovered failures retain cumulative diagnostics; smoke capture still rejects historical failures.
V99: Shared commit cap unchanged; continuous near-cell work cannot starve queued LOD & LOD cannot starve near cells; center changes retain only in-world/in-range immutable metadata; queries retry at most three times with 1/2/4-second backoff.
V100: Publication recovery ! recorded destination/backup ownership, unchanged manifest & sealed generated artifacts; ambiguous legacy backups untouched. Missing/corrupt DB/cache/LOD prevents recovery. Symlink output rejected; one exclusive output guard spans prior-pack reads through publication.
V101: Shared lock opens existing read-only descriptor; missing lock on read-only parent fails closed. Terminal metadata errors never retry; transient SQLite busy/locked/IO errors retain bounded retries.
V102: Mesh producer reuse ! explicit compatible producer allowlist & source/dependency/output/configuration proof; older numeric identity alone insufficient. Tier residency bounded by tier distance + margin across query/admission/queue/unload; coarse inner fallback retained.
V103: Near/baked terrain use shared repeats-per-cell; tiling change participates in LOD build identity. World-local compiler content failure skips world with explicit warning; input mutation/cancellation/publication/DB errors fatal.
V104: Completed launcher summary & bounded conversion notices survive; post-run notices bounded separately. Generic smoke rejects historical query failures & pending queries. Atlas documentation states actual UASTC encoding & native loader limits.
V105: Interrupted publication cleanup ! validated replacement plus sealed generated files before deleting owned prior package; ownership record atomically published before directory rename.

V106: Record-owned publication recovery accepts sealed structurally valid incomplete packages at every directory-swap boundary; completeness/failures/integration pass status retained, never promoted; schema/hash/cache/DB/LOD checks unchanged.
V107: Legacy pre-prune GLB replay ! manifest-matched raw NIF & dependencies; unavailable raw source → actionable rejection before publication; retained source bytes unchanged.
V108: `--ini` retains strict CLI missing/option-shaped value rejection, help precedence & file-layer/CLI override order; INI loaded-grid radius ≤ `MAX_STREAM_RADIUS`.

V109: Script CLI drift checks cover utility commands, excluding script-test assertions; non-engine flags classified by owner; comma-separated values remain whole and PowerShell list delimiters still split.
V110: Serialized legacy `PipelineConfig` without `lod_origins` → empty map, no invented world origin; explicit signed origins retained; existing config fields/defaults unchanged.

V111: LOD producer 24/world 7 distinct from native-BC/lighting 22–23 & grass world 5; staged outputs ! exact current producer/configuration; older numeric staging identities rejected; grass-only world 5 loads without LOD tables.

V112: Combined converter 24/world 7 ! native-BC, lighting, grass & LOD contracts; normal mesh reuse only verified current producer 24, with source/dependency/configuration/output proof. Producer 23 shared before/after rigid-body dynamics export; ⊥ compatible legacy identity. All producers 12–23 regenerate meshes/textures; unchanged scripts/archive ingestion require exact source/configuration/output proof. Metadata-only retained bytes keep explicit original producer/configuration & original manifest/output hashes; ⊥ certify legacy bytes as producer 24.

V113: Launcher Play readiness & runtime startup share read-only world DB/LOD validation; actual schema ! report schema. Empty/corrupt/unsupported DB ⊥ ready; grass-only schema 5 requires no LOD tables. Advertised current LOD ! exact producer/world/build identity & chunk count; stale/missing/mismatched identity ⊥ ready.

V114: Serialized legacy PipelineReport without lod_chunks/lod_warnings/LOD-publication timings → zero/empty defaults; explicit values retained. Timings measured by monotonic clock; absent historical values ≠ measured zero.
V115: `--no-lod` & PipelineConfig.no_lod skip terrain compiler in normal/metadata routes; full-detail assets/world data remain; published LOD rows empty, manifest/payloads absent. Toggle preserves ordinary asset configuration/byte proof; resumed generated outputs rebuilt; source package unchanged.
V116: Launcher LOD bar ! labeled stage completion, no fabricated overall weight/ETA; shared overall estimate monotonic & restored after LOD.

V117: Dynamic ingestion: Complete SE structural verdict includes TES4, all groups/record/subrecord occurrences and complete byte accounting. Unknown semantics never block supported framing, but do not satisfy interpretation acceptance.
V118: Dynamic ingestion: No-op archive-reopened output equals input bytes; complete archive remains usable without original Data files.
V119: Dynamic ingestion: Resolution, interpretation and winner merging cannot mutate source bytes or remove source occurrences/tombstones.
V120: Dynamic ingestion: Compressed subrecord spans identify decoded address space; encoded stream and all original length/header encodings remain preserved.
V121: Dynamic ingestion: Structural bounds/zlib failures and resource limits have distinct bounded outcomes; per-record and aggregate decoded-byte/work budgets enforced; no partial output receives complete status.
V122: Dynamic ingestion: Layout selection uses declared context and record observations; ambiguous/unsupported selection never silently chooses another variant.
V123: Dynamic ingestion: Reference decoding belongs to evidenced layout fields/codecs; unknown payloads receive no guessed remapping.
V124: Dynamic ingestion: Source occurrence, owning identity and winner identity remain distinct; Skyrim override/deletion/reference and GMST/full/light-slot exceptions retain tested behavior; newest capability changes require explicit proof.
V125: Dynamic ingestion: Canonical outcomes distinguish absent, unknown, invalid and unresolved; field provenance includes source range, layout and evidence.
V126: Dynamic ingestion: Registry/generator output deterministic and offline; invalid selectors, spans, mappings and codec references fail validation.
V127: Dynamic ingestion: Coverage denominators and evidence tiers explicit; structural completeness and runtime readiness independent.
V128: Dynamic ingestion: Qualified xEdit SSE/Mutagen SE traverse the full acceptance corpus; missing observations require independent native resolution, never assumed agreement; unresolved discrepancies remain gaps; reused logic cannot corroborate itself.
V129: Dynamic ingestion: Runtime DB/cache share one effective view; existing supported outputs and legacy package acceptance retain parity.
V130: Dynamic ingestion: Source, layout, canonical and runtime artifact identities distinct; relevant changes invalidate affected products while unrelated verified assets retain reuse.
V131: Dynamic ingestion: Archive/output persistence atomic and hash-verified; scanner/index bound to exact retained snapshot; failed interpretation preserves successful raw import; asset-cache eviction cannot delete source authority.
V132: Dynamic ingestion: Projection, annotation and registry updates leave the required no-op round-trip byte-identical; canonical editing remains separately scoped.
V133: Dynamic ingestion: Localized IDs/table provenance retained; missing required tables remain unresolved and block affected interpretation acceptance; supplied bundles round-trip exactly.
V134: Dynamic ingestion: Profile acceptance requires pinned corpus/source/tool evidence and declared resource/performance results; unknown semantics do not imply supported gameplay.
V135: Dynamic ingestion: Delivery order is accepted newest SE (P6), then earlier SE/VR (P7), then LE (P8), then eventual other games (P9); future work cannot open before its predecessor gate.
V136: Dynamic ingestion: SE source/occurrence and owner identities never collapse distinct records; future native-key/source-namespace generalization waits for P9 and must preserve each game's own identities.
V137: Dynamic ingestion: SE coordinates/extents, terrain dimensions and text bytes retain native facts; future opened games retain theirs; cell splitting/resampling/record replacement have separate conversion contracts.
V138: Dynamic ingestion: Every reused module/algorithm/test vector records source pin, author/license/notice and adaptations; provenance and independent evidence remain visible in accepted layouts.
V139: Dynamic ingestion: Complete SE canonical fields, including data unused by current gameplay, survive persist/reopen/query/report; current SQL/cache columns never limit the interpretation contract.
V140: Dynamic ingestion: Newest-SE acceptance requires 100% catalog signatures, valid variants, fields/flags/ranges and valid corpus occurrences correctly interpreted; raw-only fallback, unexplained tails, disputed semantics and failed required references are blockers.
V141: Dynamic ingestion: Newest-SE runtime/distribution/data/tool pins and corpus/catalog denominators are explicit and fixed per acceptance run; newly discovered variants update the ledger and reopen completeness.
V142: Dynamic ingestion: P0–P6 have no earlier-SE/VR/LE/other-game implementation, corpus, native-key, adapter or framework prerequisite; newest-game-compatible older serialized forms remain part of the newest-SE input contract.
V143: P0 input manifest ! executable SHA-256 alignment, explicit missing inputs, digest/size + observed header/master metadata from same verified read; source drift → fail pin. Installed filename ≠ official-content proof; unresolved load order/locale remain explicit.
V144: P0 oracle observations ! exact package/runner + input hash; unavailable observations ≠ empty/absent; lazy major records materialized; failure/truncation → nonzero status & no completed verdict. Physical framing remains Mudcrab source authority.
V145: P0 fixtures ! positive header counts/hierarchy match physical occurrences; unknown/repeated subrecords, valid full/light identities, overrides/deletions, localization & malformed/truncated negative cases as applicable to each pilot. Pilot agreement ≠ full-catalog or native-runtime acceptance.
V146: P0 manifest dependency closure ! deep acyclic chains & cycles handled without Python stack recursion; missing/ambiguous/cyclic dependencies remain explicit failures. Long-chain & cycle fixtures guard traversal.
V147: P0 oracle artifacts/temp/build/cache ! outside every registered Mudcrab worktree, active RE checkout/store & supplied oracle/tool roots; verify worktree discovery & validate destination before any write; tool temp env artifact-local. Protected-path fixtures guard both CLI & runner.
V148: P0 oracle supervisor ! bounded external-command timeouts; timeout → nonzero/incomplete, retained diagnostics & no completed qualification verdict. Timeout fixtures guard status handling.

V149: Standalone schema/dotnet tool options ! classified by existing script-option audit; every actual engine option remains checked against parser. Existing script/help/doc regressions guard ownership.

V150: P0 corpus evidence ! bounded strict JSON + exact newest target + verified artifact hashes; supplied semantic claims remain unverified. Invalid/deep JSON → managed failure; output cannot overlap descriptor or evidence artifacts. Evidence CLI/deep-JSON regressions guard boundary.
V151: P0 tool reports ! exact Python decision-runner/helper/fixture source hashes separate from external binary/package pins; release metadata ≠ verified source-to-binary build. Startup-report provenance regression + final executable artifacts guard identity.
V152: P0 manifest API/CLI ! resolve containing directories before immediate-child checks; final executable/CCC filename remains unresolved so leaf symlinks rejected. Directory-alias/`..` and leaf-symlink regressions guard both routes.
V153: P0 runtime-loadable plugin set ! Data top level only; nested plugins separately hashed/reported, never satisfy masters/base presence/CCC loadable matches. Loose string tables remain recursively observed; nested-master/base/duplicate regressions guard partition.
V154: P0 Windows path/descriptor checks ! same device/inode/type/size; descriptor-before/after full metadata equality retained for drift. POSIX path/descriptor full metadata equality unchanged; platform-skew and existing drift regressions guard boundary.

V155: P0 Git provenance reads ! deadline + inspected revision/path context; timeout → controlled nonzero, no new/replaced source report. Revision/file deadline & prior-report preservation regressions guard boundary.
V156: P0 process output ! strict locale decoding before structured acceptance; invalid bytes → unavailable/incomplete, never replacement-text JSON. Oracle reports retain raw stream bytes/hash/size and failed case/startup; decode/timeout/report regressions guard availability.
V157: P0 repository/Data-relative evidence path keys ! POSIX separators on every host; issue/source joins & decision-runner identity use same canonical path spelling. Windows-path & nested-issue regressions guard serialization.
V158: P0 saved text logs/JSON ! explicit UTF-8 regardless host locale; strict child-output decoding separate. Non-CP1252 xEdit artifact regression guards writer encoding.

V159: Schema explorer ! pinned code-derived catalog/SQL facts, source citations & distinct declaration/static/current/unknown labels; inferred relationships ≠ SQLite foreign keys; selected field evidence ≠ complete interpretation; future ingestion visibly planned; offline navigation/search & embedded text safety verified.

V160: Explorer UTF-8 text identities ! explicit CRLF→LF normalization; LF/CRLF checkouts → identical facts/HTML; semantic source drift → managed failure. Hash labels state normalization; retail/archive byte identities retain existing raw-byte contracts.

## §T TASKS

id|status|task|cites
T1|x|P1 add Rapier fixed-step setup, `--physics-fixture` primitive slope/wall arena, debug tankard mesh + compound dynamic collider; capture existing camera regression baseline|V5,V15,V17,R1,R8,R9
T2|x|P1 add upright WALK capsule, camera follow, movement/jump/slope settings, & collision-safe toggle against fixture geometry|V8,V9,V10,V11,V12,V13,V14,V18
T3|x|P1 add mouse-look NOCLIP, `V` toggle, `NOCLIP: ON/OFF  [V]` overlay, cursor lifecycle; preserve noninteractive camera paths|V5,V6,V7,V8
T4|x|P1 test controller + tankards on primitive hill/wall, toggle/focus/rebase, 30/60/120 render fps, & camera regressions; record gate evidence|V5,V6,V7,V8,V9,V10,V11,V12,V14,V15,V16,V18,V19
T5|~|P2 attach validated terrain trimesh to quadrant lifetime; handle failures & missing-ground transition|V1,V2,V13,R4
T6|~|P2 enable bounded `T` tankard spawn; rebase Rapier poses with world; test player + tankards on real hill, seams, stream unload/reload; record gate evidence|V1,V2,V9,V13,V15,V16,V18
T7|x|P3 inventory NIF/Havok collision support, base record types, & representative statics; record original/proxy/skip policy|V3
T8|x|P3 convert/load eligible fixed colliders with full transforms, streamed lifetime, & source/skip counts; exclude movable records|V3
T9|~|P3 test player + tankards at rocks, walls, openings, excluded visuals & unload/reload; adjust CCD/contact only from measured failures|V3,V4,V15,V16,V17,V23,V24,V25,V26,R10
T10|.|P3 rerun P1/P2 gates, interactive playtest, screenshot/benchmark regression; record evidence, controls, static proxy limits|V1,V2,V3,V4,V5,V7,V15,V16
T11|x|P2 mount controller in Riverwood, fix WALK pitch, add `E` tankard pickup/drop; preserve noninteractive camera paths|V5,V6,V9,V20,V21
T12|x|Package Fiji launcher with display discovery; verify from shell with display vars unset|V22
T13|~|Extract NIF Havok collision into GLB, use it for fixed `STAT`/`TREE`, audit Riverwood coverage, package Fiji build and run WALK/tankard gate|V3,V4,V16,V27,V28,V29,V30,V31,V32
T14|x|Load authored furniture collision, shrink player height, add F3 collision view, package Fiji and test mill log pile|V12,V33,V34
T15|~|Cull F3 outlines fully behind camera, keep crossing bounds, package Fiji and check debug frame rate|V34
T16|x|Q0 inspect winning Player `NPC_`, linked `RACE`, selected walk/run `MOVT`, and candidate jump/gravity `GMST`; record FormIDs, absent links, overrides, field types and values|V35,V36,R15,R16,R18
T17|x|Q0 document speed/acceleration/jump units, conversion factors and project formulas; mark unknown GMST mapping rather than inventing it|V36,V37,V38,R14,R16,R21
T18|x|Q1 reuse NPC race link; validate winning `RACE`, optional `WKMV`/`RNMV`, and explicit retail absence before selected profile|V39,V40,V41,R15,R18
T19|x|Q1 convert needed `MOVT` directional speed fields; validate source values, unit conversion and override resolution|V36,V39,V40,V41,R16
T20|x|Q1 export allowlisted `GMST` candidates with type/value provenance; consume jump/gravity only when Q0 establishes role and units, otherwise retain provisional values|V38,V39,V41,R14
T21|x|Q1 validate Player→RACE, select `NPC_Default_MT` as first-pass profile when race links absent, keep GMST candidates unconsumed until mapped; test missing-form errors and legacy-package fallback|V40,V41,V42,V49
T22|x|Q2 feed resolved walk/run directional speeds into current fixed-step WALK path; keep input normalization, collision and debug modes|V42,V43,V47,V54
T23|x|Q2 feed confirmed jump/gravity values and derived launch speed when available; keep grounded/no-air-repeat behavior and report provisional gap otherwise|V38,V42,V45,V48
T24|~|Q2 audit packaged GLB collision coverage before handoff; package as soon as build passes for Riverwood manual test; run deterministic distance/jump and bridge/stairs/terrain/tankard regressions at 30/60/120 render fps in background, then report results|V31,V44,V45,V46,V47,V54,V56
T25|.|Q2 report effective values with source IDs, conversion formulas, unsupported mappings and first-milestone pass/fail; release only supported subset|V35,V36,V48,V49,V55
T26|.|Deferred Q3: add `AVIF`/`SPEL`/`MGEF`/`PERK` fields needed by selected speed/jump modifiers|V50
T27|.|Deferred Q3: add record-backed sprint/sneak gait rules after modifier/state dependencies are known|V50,R11,R15,R16
T28|.|Deferred Q4: add water/world fields and swimming transition behavior|V51,R13,R14
T29|.|Deferred Q4: add movable-object record types and authored dynamic contact beyond debug tankards|V51,V54
T30|.|Deferred Q5: add third-person camera and race-linked animation fields when implemented|V52,R12,R15
T31|.|Deferred Q5: bind relevant Papyrus native controls and script records to engine state|V52,R19
T32|.|Deferred Q6: resolve state-specific records and implement mount/furniture/transformation/bleedout/dragon paths; report scoped parity|V55,R12,R13
T33|x|Latch provisional unlimited WALK sprint on `Alt` press until movement stops; reset on focus loss and mode switch; test key edge and gait changes|V10,V14,V57
T34|~|Make sprint perceptible; display actual speed and latch; ease controlled camera FOV; package laptop and verify manual launch|V10,V57,V60,V61
T35|x|Launcher LOD A: retain chunk count/warnings in `RunReport`; label LOD world progress; preserve final summary & warning visibility; document automatic conversion-stage build|V92,V93,I.launcher_lod
T36|~|Launcher LOD A: focused report/status/worker tests; tiny conversion verifies payload/DB/manifest identities; stop/failure never enables new incomplete output; scripted launcher capture|V92,V93,I.launcher_lod
T37|.|Launcher LOD B: add cooperative metadata cancellation with typed failure & no-resume contract; tests cover preflight, retained-file copy, world boundary, pre-publication, source preservation|V94,V95,I.launcher_lod
T38|.|Launcher LOD B after T37: `Build LOD` action for selected converted source & new destination; reuse worker/messages/state ownership; verify reuse hashes, plugin mismatch, busy/engine/path guards & publication|V94,V95,I.launcher_lod
T39|x|Historical PR105 review batch: destination-bound recovery seals/locking, schema16 mesh reuse, tier residency, query classification, world-content failure policy, capture/launcher fixes, shared tiling & contract docs; 830 workspace tests, strict clippy/fmt, 37 Python tests, 4 release perf tests; frozen-source software captures|V93,V96,V97,V98,V99,V100,V101,V102,V103,V104,V105
T40|.|Deferred by owner 2026-10-03: PR105 final-head Fiji stationary/moving/recovery/launcher captures & wider native performance; no pre-merge native campaign required. PR95 merged & historical converter-17/world-5 range tests passed. Prior candidate approval ≠ final-head native evidence; native limits retained|V93,V98,V99,V100,V102,V103,V104

T41|~|Dynamic ingestion P0 pin newest SE, complete catalog/corpus ledger, consumer/time/RSS baselines, SE validator and archive contracts|V117,V118,V128,V134,V138,V141,V142,V143,V146,V150
T42|.|Dynamic ingestion P1 extend existing 24-byte Skyrim scanner/arena; preserve all headers/groups/spans/compression/XXXX and bound failures|V117,V119,V120,V121,V136,V142
T43|.|Dynamic ingestion P1 persist/reopen SE archive and no-op writer; prove structural diagnostics/failure/immutability gates|V118,V121,V131,V142
T44|.|Dynamic ingestion P2 implement newest-SE selection, registry/linter and independent pilot layouts/codecs|V122,V123,V126,V138,V141
T45|.|Dynamic ingestion P2 prove newest capability/encoding/variant/union/repetition cases and deterministic offline generated accessors|V120,V122,V126,V133,V142
T46|.|Dynamic ingestion P3 define canonical SE outcomes/references and separate occurrences/owners/winners; persist and query pilot data|V119,V123,V124,V125,V136,V139
T47|.|Dynamic ingestion P3 migrate current Skyrim SQL/cache/runtime consumers with identity/reference/native-geometry parity|V124,V125,V129,V137
T48|.|Dynamic ingestion P4 qualify whole-corpus xEdit/Mutagen differential checks and reviewed miners; independently resolve omitted/disputed observations|V127,V128,V134,V138,V140
T49|.|Dynamic ingestion P5 integrate complete SE canonical persistence/query, source/layout invalidation, atomic recovery and legacy product compatibility|V129,V130,V131,V139
T50|.|Dynamic ingestion P6 accept 100% newest-SE interpretation, current-game compatibility, no-op/bundle preservation and performance/resource campaign|V118,V121,V132,V133,V134,V135,V140,V141,V142
T51|.|Dynamic ingestion P4 close every newest-SE signature/variant/field/flag/reference gap through registry/codec work and controlled native research|V122,V123,V125,V127,V128,V139,V140,V141
T52|.|Dynamic ingestion Each opened phase record exact commands/hashes/raw evidence, automated verdict, accepted limits and dependency handoff|V134,V135,V138
T53|.|Dynamic ingestion Deferred P7 after P6: earlier SE backwards and VR, complete per-profile interpretation and newest-SE non-regression|V122,V128,V133,V134,V135,V140
T54|.|Dynamic ingestion Deferred P8 after P7: LE layouts/encoding/reference/corpus acceptance; preserve SE/VR compatibility|V122,V128,V133,V134,V135,V137
T55|.|Dynamic ingestion Eventual P9 after P8: open separate other-game native ingestion plans using retained tes4skyrim research and applicable validators|V117,V128,V134,V135,V136,V137,V138
T56|~|Dynamic ingestion P0 extend existing Mutagen pilot at validation owner; qualify synthetic observations/error/availability contract and xEdit runner; missing capability remains gap|V128,V134,V143,V144,V145,V147,V148,V151,R26

T57|x|P0 human schema explorer: compact offline HTML, searchable candidate catalog/current SQL, selected field/variant labels from project + read-only mudcrab-re MCP; verify source data, navigation, citations & unknowns|V134,V138,V140,V141,V159,V160

## §B BUGS

id|date|cause|fix
B1|2026-09-26|probe-only collision phases deferred WALK/dynamic validation until too late|V16
B2|2026-09-26|Rapier 0.35 controller panics slicing empty manifold vec when pushing dynamic bodies on slope|V19
B3|2026-09-26|WALK follow reused prior view pitch; interactive world omitted controller plugin|V20
B4|2026-09-27|Fiji launcher assumed graphical display variables inherited by terminal; winit panicked before startup|V22
B5|2026-09-26|direct Cargo test lacked Wayland pkg-config path|§C build env
B6|2026-09-27|static proxy draft used wrong Bevy iterator and Parry ray types; Clippy found non-idiomatic loops|compile + Clippy fixes
B7|2026-09-27|opaque-only proxy skipped `RockCliff` rock faces exported as `BLEND`|V23
B8|2026-09-27|fixed proxy eligibility omitted Riverwood pine, woodpile, & road-ramp models|V24
B9|2026-09-27|opaque-only proxy skipped lumbermill walkway exported as `MASK`|V25
B10|2026-09-27|24-unit autostep clearance blocked adjoining 15-unit floor rises|V26
B11|2026-09-27|path and render-material proxies missed bridge and made stair support beams physical|V27,V28
B12|2026-09-27|converter schema bump would reject existing Fiji schema-15 package at startup|V29
B13|2026-09-27|schema-16 bump bypassed earlier GLB-only cache migration, dropping reusable texture/script entries|V30
B14|2026-09-27|unsupported `bhkNiTriStripsShape` left two wider-area uprooted pines without collision|V31
B15|2026-09-27|multi-shape authored model wrapped trimesh in Rapier compound; Parry panicked on nested composite|V32
B16|2026-09-27|mill log pile was placed `FURN`; runtime fixed-collider gate admitted only `STAT`/`TREE` despite nine authored shapes|V33
B17|2026-09-27|frame-rate gate compared unequal physics durations; Bevy FixedUpdate retained 64 Hz default despite 60 Hz Rapier/player tuning|V18
B18|2026-09-28|Riverwood bundle omitted `libdl.so.2`; wgpu failed Vulkan entry-point loading and reported no GPU|V56
B19|2026-09-28|movement package copied legacy GLBs instead of authored collision GLBs; 239 Riverwood fixed models lost collision provenance|V31,V46
B20|2026-09-28|jump test sampled an arbitrary Transform, which selected camera height and falsely failed the apex gate|V58
B21|2026-09-28|Fiji launcher forced Radeon ICD on Intel laptop; bundled ALSA hook searched build-host plugin path and crashed|V59
B22|2026-09-28|fixed 420-unit sprint gave only 13.5% gain over packaged 370-unit forward run, with no FOV or actual-speed feedback|V60,V61
B23|2026-09-28|Rapier wall contact produced about 4 units/s lateral jitter, making a stationary speed indicator nonzero|V60
B24|2026-09-28|FOV ECS test used ambiguous generic `Time::default()` and failed to compile|explicit `Time<()>` fixture resource
B25|2026-09-29|legacy conversion test copied schema 4 integration report into schema 3 checkout|fixture uses `WORLD_DATABASE_SCHEMA_VERSION`
B26|2026-09-29|400-unit WALK entry search reused for active ground presence and froze long falls|V62
B27|2026-09-29|compressed mesh decoder ignored serialized quantization error and assumed 0.001|V63
B28|2026-09-29|MOVT parser rejected legacy 40-byte SPED despite same eight speed fields|V64
B29|2026-09-29|launcher searched XKB only in Nix store; standard Linux hosts could use stale build path|V65
B30|2026-09-29|collision audit compared normalized model key but reported raw-key status|normalize report status
B31|2026-09-29|launcher test expected `/bin/sh` to depend on libdl after glibc 2.34|readable preflight fixture
B32|2026-09-29|new converter loops failed CI Clippy under Rust 1.98|collapse node condition and use `as_chunks`
B33|2026-09-29|fixture WALK camera reused prior pitch instead of `LookIntent.pitch`|V66
B34|2026-09-29|Update overwrote jump press before next fixed tick at high render fps|V67
B35|2026-09-29|fixture flags selected conflicting owners; benchmark ignored physics validation|V68
B36|2026-09-29|fixture test reassigned field after Default; CI Clippy rejected it|struct initializer
B37|2026-09-29|record tuning and scene helper used patterns Clippy rejects under workspace warning gate|struct initializer and scoped argument allows
B38|2026-09-29|active WALK treated any downward ray miss as loading and reset fall state every tick|V69
B39|2026-09-29|NOCLIP→WALK rejected free high-altitude capsule because ground lay beyond 400-unit cast|V70
B40|2026-09-29|schema 3 runtime binary paired with schema 4 Riverwood assets; report & database gates rejected valid package|V71
B41|2026-10-01|floating CI `stable` upgraded 1.98.1→1.99.0; new macro warnings failed unchanged workspace under `-D warnings`|§C fixed Rust toolchain; restore 1.98.1
B42|2026-10-01|PR #102 merge retained local LAND tiling constant alongside shared import; engine failed E0255|reuse shared constant; workspace compile + Clippy gates
B43|2026-10-03|resume cleanup exempted historical prune-record GLBs without schema/source/output proof; removed NIF let unpublished stale mesh supply world bounds & hide unavailable source|V72
B44|2026-10-04|native-BC DDS & specular branches both used converter 18; numeric identity could accept incompatible output bytes|V73,V112
B45|2026-10-04|configuration hash omitted CPU/native-BC vs GPU/UASTC selection & GPU quality|V73
B46|2026-10-04|normal/UV/alpha source-surface slice originally producer 19 predates merged native-BC/specular producer 22; 22 mesh reuse omits new material contract|V74
B47|2026-09-30|launcher LOD report fields omitted from Play-availability fixture outside conversion module|migrate fixture; launcher compile/test gate
B48|2026-09-30|ignored layout performance test retained 12-file count after LOD sidecar addition|migrate count; assert sidecar output; preserve 10-second budget; no new invariant
B49|2026-10-01|world-inspect queried absent legacy LOD table; capture scripts passed unsupported log flag|legacy table guard; preserve malformed-table errors; existing stdout/stderr redirection
B50|2026-10-01|publication recovery adopted newest backup by name without validating completeness or outputs|V96
B51|2026-10-01|dotted outputs shared backup namespace; raw path aliases bypassed asset locks; completed notices trimmed results|V96,V97,V93
B52|2026-10-01|near cells exhausted shared budget; center changes dropped relevant LOD metadata; pending counts omitted queue/retry work; failed queries stayed requested|V98,V99
B53|2026-10-01|metadata LOD extraction let unmatched archives override package-matched plugin sources|filter unmatched archives before settings/diffuse extraction; record omission; preserve loose precedence
B54|2026-10-01|screenshot readiness treated recovered LOD failures as current errors|V98
B55|2026-10-01|first fair-budget draft derived reservation from shrinking remainder and released it after a near commit|V99; reserve from frame limit before collectors
B56|2026-10-01|backup validation omitted generated artifacts & destination ownership; output alias changed lock identity; publication lock checked too late|V100,V101
B57|2026-10-01|schema17 invalidated unchanged schema16 GLBs; all tiers loaded largest radius; deterministic metadata errors retried|V101,V102
B58|2026-10-01|generic capture omitted query failures; launcher replaced progress notices then allowed unbounded post-run growth; ADR misstated UASTC encoding; PR91 tiling diverged|V103,V104
B59|2026-10-01|new preflight IO used `?` across `PipelineFailure` boundary without `Report` conversion|named `WrapErr` context; compile oracle catches mechanical error; no new invariant
B60|2026-10-01|interrupted replacement cleanup trusted next manifest alone; missing generated files could discard last-good backup|V105
B61|2026-10-03|record-owned recovery applied Play-readiness completeness gate to structurally valid incomplete packs|V106
B62|2026-10-03|legacy prune replay assumed published packs retained raw `vfs/` NIFs; missing source produced opaque hash IO error|V107
B63|2026-10-03|INI integration bypassed strict CLI value contract & loaded-grid overflow bound; GPU result channel omitted fatal cleanup flag|V108,V103; preserve upstream parser & shared cleanup semantics
B64|2026-10-03|metadata payload validation retained old three-argument API after main parallelized validation; new recovery test used absent runner; compiler fixture retained removed export helper|pass configured CPU jobs; reuse existing runners/merged-record exporter; compile oracle, no new invariant
B65|2026-10-03|script drift scanner mistook audit/Cargo/Git flags & negative test assertions for engine arguments; comma splitting turned camera CSV into scalar|V109
B66|2026-10-03|conflict splice placed `notices` before LOD keys in sorted JSON snapshot|restore observed sorted order; fixture oracle, no new invariant

B67|2026-10-03|new `lod_origins` lacked serde default, rejecting legacy serialized configs|V110
B68|2026-10-03|schema docs claimed legacy `lod` removal while exporter retained unused table|document retained placeholder and external GLB payloads; documentation correction, no new invariant
B69|2026-10-03|V29/V71 retained pre-LOD current schema limits after runtime range expanded to converter 17/world 5|align existing invariants with supported ranges; existing runtime/launcher range tests, no new invariant
B70|2026-10-03|LOD & grass both stamped world 5; LOD & emission both stamped converter 17; range reuse hid distinct producer contracts|V111; allocate LOD 20/6; preserve explicit cache gates; refresh schema-dependent snapshot/error fixtures; downstream failure not reproduced
B71|2026-10-04|combined LOD engine fixture retained producer21 success expectation after current producer advanced24; runtime correctly rejected stale identity|V112; shared current constants & explicit21/22/23 rejection regressions; mechanical fixture migration, no new invariant
B72|2026-10-04|conflict splice dropped async regression test attribute; legacy producer regeneration function compiled without execution|V73,V112; restore test registration; strict Clippy unused-function gate, no new invariant
B73|2026-10-04|schema 23 retained across rigid-body GLB payload change; honest old source/config/output proof reused mesh without body dynamics|V73,V112; valid legacy GLB negative control, current24 unchanged resume
B74|2026-10-04|launcher readiness checked only world DB file existence; empty SQLite fixture enabled Play while runtime rejected|V113; shared read-only DB/LOD contract, real SQLite readiness fixtures
B75|2026-10-04|schema23 fixtures retained mesh-only count and normal-reuse allowlist after legacy mesh/texture regeneration became required|V73,V112; align output-byte/count assertions and current-only normal reuse; preserve honest metadata-only provenance; existing invariants sufficient
B76|2026-10-04|PipelineReport.lod_chunks required by Deserialize after optional LOD addition; older JSON rejected|V114; failing legacy-report regression, serde defaults
B77|2026-10-04|normal/metadata conversion always compiled LOD with no opt-out; uncalibrated overall bar frozen during LOD|V115,V116; --no-lod both routes, byte-preserving toggle/resume tests, labeled stage bar
B78|2026-10-04|report-projection fixture and serialized snapshot retained old report shape after timing fields added; compile/snapshot checks failed|V114; explicit fixture timing defaults & snapshot timing redactions; mechanical fixture updates, no new invariant

B79|2026-10-04|P0 manifest used recursive dependency DFS; 1500-node acyclic light-plugin chain raised `RecursionError`|V146; stdlib iterative dependency traversal + chain/cycle fixtures
B80|2026-10-04|P0 manifest success predicate omitted unresolved load order, locale & accepted corpus pins; valid base-only corpus could report success|V143; explicit completion blockers + matched-runtime base-only regression
B81|2026-10-04|custom CCC missing/drift diagnostics hard-coded `Skyrim.ccc` instead of supplied descriptor path|V143; shared source provenance + custom missing-descriptor regression
B82|2026-10-04|P0 hand-authored CELL fixtures omitted interior block/subblock & type-6 children groups; skipped references could mask invalid qualification inputs|V145; legal sourced hierarchy + exact expected typed record/reference checks
B83|2026-10-04|P0 localization fixture used unsupported `STAT/FULL`; absence could masquerade as a string-resolution observation|V144,V145; translated `ARMO/FULL` fixture + exact ID & explicit missing-table outcome
B84|2026-10-04|P0 inspect redirected library stdout but discarded captured diagnostics|V144; retain library output in observation/error evidence
B85|2026-10-04|P0 runner appended helper to Program.cs & also wrote helper `.cs` under SDK default compile glob|V144; keep one compiled helper definition
B86|2026-10-04|P0 runner guarded only current repo; CLI could create artifacts inside protected RE/store/source roots|V147; shared destination guard before writes + protected-path fixtures
B87|2026-10-04|P0 runner subprocess calls had no timeouts; hung tool could prevent qualification failure reporting|V148; bounded supervised calls + timeout fixture
B88|2026-10-04|renamed unknown-subrecord fixture expected original full-plugin FormKeys instead of actual source filename|V144,V145; current-plugin owner/reference expectations
B89|2026-10-04|negative-case classifier accepted exit `-9`/startup failure as expected malformed rejection|V144; require managed rejection exit 2 + diagnostic marker; signal/startup/timeout regressions
B90|2026-10-04|P0 runner saved parsed JSON before validation while reporting no completed verdict on rejection|V144; raw diagnostic logs first, validated observation JSON only after contract passes
B91|2026-10-04|P0 typed model-path check compared wire backslashes with Mutagen slash normalization|V144,V145; declared path comparison normalization; raw fixture bytes/digest unchanged
B92|2026-10-04|P0 localized check assumed typed printer exposed `FULL` ID; missing table yielded blank Name output|V144; fixture wire ID provenance separate from typed ID/text availability
B93|2026-10-04|optional typed StringsKey null could convert to empty string under available status|V144; nullable-key guard before reporting availability
B94|2026-10-04|P0 positive HEDR counts omitted GRUP headers; typed-reader success masked inconsistent source fixtures|V145; include major + group occurrences, independent physical count checks & regenerated golden digest
B95|2026-10-04|P0 report claimed typed localized-ID check even when both typed ID observations unavailable|V144; separate fixture wire-ID provenance, typed-ID availability & unresolved translated text
B96|2026-10-04|same-tick same-size fixture rewrite retained mtime/ctime; source-drift regression depended on host timestamp advancement|V143; explicit timestamp drift in test; document stat-checked read ≠ immutable snapshot
B97|2026-10-04|engine script-option audit treated standalone schema argparse/dotnet options as engine options; parent CI failed after 918/919 passing tests|V149; extend existing NON_ENGINE_FLAGS + script/help/doc regressions
B98|2026-10-04|supplied load-order descriptor normalized artifact under `source`; CLI indexed nonexistent `evidence` key|V150; responsible source key + supplied-evidence CLI regression
B99|2026-10-04|manifest output could alias or overlap external evidence input; source-only guard missed new descriptor artifacts|V150; resolved overlap guard before writes + alias/ancestor regressions
B100|2026-10-04|deep evidence JSON escaped decode handler with `RecursionError` despite byte limit|V150; managed invalid-JSON failure + 20000-level input regression
B101|2026-10-04|detached descendant retained stdout after process-group termination; final unbounded `communicate()` waited 60 seconds|V148; bounded final collection + pipe close; detached-pipe regression; escaped descendant cleanup separately limited
B102|2026-10-04|oracle artifact guard protected only current Mudcrab worktree; primary dirty checkout remained writable destination|V147; timed registered-worktree discovery before writes + every-checkout/failure regressions
B103|2026-10-04|xEdit console exited zero after Wine crash & tolerated malformed dump; exit code alone could masquerade as qualification|V144,V145; crash/completion/identity diagnostics; malformed dumps remain unqualified; original schema diagnostics retained
B104|2026-10-04|tool reports omitted Python classification source identity; xEdit inherited potentially protected temp paths|V147,V151; decision-source hashes + artifact-local Wine temp env + startup/provenance regression
B105|2026-10-04|static XESP summary omitted flag-write condition after zero-parent removes extra entry|native shard cross-check; flag write only when entry exists
B106|2026-10-04|oracle artifact guard omitted custom SDK/Wine executable directories; temporary outputs could enter supplied runtime roots|V147; resolve runtime & guard directory before writes in runner/CLI; custom-runtime regressions
B107|2026-10-05|P0 `.absolute()` normalized lexical `..` before resolving directory aliases; valid executable/CCC parents failed immediate-child checks|V152; normalize parent only, retain leaf symlink rejection; API/CLI regressions
B108|2026-10-05|recursive Data plugin discovery let nested masters/base names satisfy runtime-loadable closure/presence|V153; separate loadable top-level set and hashed nested observations; nested master/base/duplicate regressions
B109|2026-10-05|Windows lstat/fstat timestamp skew classified unchanged file as source drift|V154; cross-interface identity/size checks on Windows, full same-descriptor drift checks; synthetic platform regression + Windows CI
B110|2026-10-05|synthetic corpus `plugins or defaults` treated explicit empty mapping as five base plugins|V145; only None selects defaults; empty-corpus regression
B111|2026-10-05|P0 source evidence used unbounded Git reads; stalled child prevented controlled report outcome|V155; bounded reads with revision/path diagnostics; preserve previous report on timeout
B112|2026-10-05|locale text-mode subprocess decoding raised before xEdit saved incomplete report; replacement decoding could corrupt accepted typed JSON|V156; binary capture, strict decoding and fail-closed exception; xEdit raw-byte logs/incomplete report regressions
B113|2026-10-05|real timeout fixtures assumed Python startup/flush within 0.2 seconds; concurrent builds exposed empty diagnostic race|V148,V156; bounded post-flush readiness before supervised timeout; retain real-process diagnostic/deadline assertions; no new invariant
B114|2026-10-05|source miner could count Pascal call text inside literals; initial portable regressions used malformed fixtures|mask literals before call detection; correct fixture syntax/markers; source-miner regressions; existing evidence contract sufficient, no new invariant
B115|2026-10-05|alias fixture relied on POSIX symlink traversal before `..`; Windows canonicalization described different parent|V152; separate parent-alias and real-directory `..` API/CLI fixtures; leaf-link rejection retained
B116|2026-10-05|Data-relative issue paths used native Windows separators unlike hashed source keys|V157; POSIX serialization + nested plugin/string/archive issue regressions
B117|2026-10-05|parent final pipe drain unbounded; Windows reader-thread close can block; partial Unicode decode bypassed timeout diagnostics|V144,V148; bounded tree termination/drains/wait; POSIX-only pipe close; retain undecodable raw bytes; real detached-child + failed-Windows-cleanup + partial-Unicode regressions
B118|2026-10-05|parent issue-path fixture mocked archive stat helper removed by next layer's hashed archive owner|V157; next fixture targets existing hashed-file read + archive source-drift outcome; no new invariant
B119|2026-10-05|decision-runner provenance used Windows separators while report consumers keyed POSIX repository paths|V151,V157; POSIX relative serialization + PureWindowsPath and startup-report regressions
B120|2026-10-05|extensionless SDK/Wine fixtures failed Windows executable lookup before protected-directory assertions|V147; platform-valid `.exe` fixture names; runtime resolution/guard order retained
B121|2026-10-05|xEdit text artifact writes inherited host encoding; non-CP1252 diagnostics could prevent saved report|V158; explicit UTF-8 every text write + Unicode artifact regression; strict child decoding retained
B122|2026-10-05|source-span lexer classified Rust lifetimes as character literals; matcher missed generic lifetime declarations|V134; bounded char-literal recognition + lifetime/declaration/character-brace regressions; no new invariant
B123|2026-10-05|source report `--check` read errors escaped as tracebacks and mismatches used uncontrolled exit|V134,V155; managed exit 2 for missing/malformed reports and mismatches; report-read regression; no new invariant
B124|2026-10-05|next Mutagen callers handled timeout text only; shared binary decode errors lost raw artifacts at generic failure summary|V144,V156; persist raw streams/hash/size + unavailable decoded-output metadata for checked/case/legacy/SDK commands; partial-Unicode artifact regression; no new invariant
B125|2026-10-05|docs-owned source evidence tests placed under engine utility scan; next CI rejected test-only `--repo`|V149; move tests beside responsible script; existing portable CI discovery retains every regression and strict engine audit unchanged
B126|2026-10-05|explorer hashed checkout text bytes; CRLF-only conversion falsely reported code drift & changed generated input identities|V160; explicit UTF-8 CRLF→LF source/evidence hashes + equivalent-checkout regression
B127|2026-10-05|explorer skip link changed hash to `#detail`; route parser reset selected table/evidence to REFR|V159; prevent route mutation, focus/scroll existing detail; real browser route/focus check
B128|2026-10-06|P0 leaf stat/O_NOFOLLOW checks did not bind ancestor directories; concurrent parent symlink swap could attribute external bytes to recorded Data path|V143; document ancestry limit in reader/report/docs; opened-byte hash ≠ unchanged-path proof; no new invariant, immutable source retention stays P1
B129|2026-10-06|stacked CI included newer main converter bytes while local explorer checks used older branch; source-drift guard rejected stale pins|V134,V159,V160; normal main/parent integration + reviewed source-pin/anchor/projection refresh; retain historical MCP snapshot & fail-closed drift guard; no new invariant
