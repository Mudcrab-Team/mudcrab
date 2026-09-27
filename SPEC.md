# SPEC

## §G GOAL

Riverwood interactive world → mouse-look NOCLIP/WALK, streamed terrain collision, spawned/grabbable tankards; fixed static collision follows. `V` switches modes; default NOCLIP.

## §C CONSTRAINTS

- Scope: playable first guess, not claimed vanilla parity; retain measurements as future tuning input.
- Riverwood primary manual physics test area; primitive fixture retained for automated regression only.
- Bevy `0.19.0`; Rapier 3D `0.36.x`; use Rapier collision/controller rather than parallel hand-written solver.
- Former P3 moves first, merged with thin WALK/tankard fixture from former P4; phases renumbered in execution order. Every collision gate uses same player capsule & dynamic tankard, not probes alone.
- Render terrain/collision terrain share validated 33×33 quadrant geometry, transforms, & streamed lifetime.
- Static collision source explicit per converted asset. Prefer original NIF/Havok collision; where unavailable, use declared render-triangle proxy only for verified fixed solids. Never use broad bounds boxes or treat all visuals as solid. Record proxy/skipped coverage.
- Database `statics` table also holds `MISC`/other movable model records; fixed collider eligibility requires base record type or equivalent authoritative metadata. Movable clutter cannot receive both fixed & dynamic colliders.
- Debug tankard uses visible cup/handle geometry & compound convex collider; converted tankard model absent in inspected asset set. Label it as physics fixture, not Skyrim asset parity. P1 auto-spawns in fixture; P2+ `T` spawns bounded debug tankards in loaded interactive world.
- Existing benchmark, visual fixtures, screenshot, headless, & `--auto-fly-speed` paths retain current camera behavior; interactive Riverwood owns controller.
- Physics uses Creation units throughout; WALK & dynamic tankards share gravity constant. Origin rebase moves Rapier body poses with Bevy transforms, preserving velocity; shifting visual transforms alone insufficient.
- Movement constants internal, provisional, centralized; no CLI tuning surface. NPCs, mounts, swimming, sneak, combat, animation, & general movable clutter outside scope.
- Phases remain planned until gate evidence passes. Later phase work waits for predecessor acceptance.

id|state|capability|depends_on|gate
P1|planned|NOCLIP + WALK + dynamic fixture|-|mouse camera, `V`, walking capsule, debug tankards pass primitive slope/wall tests
P2|planned|streamed terrain collision|P1 accepted|player walks/jumps across hill/seams; tankards roll/rest on hill; unload/rebase pass
P3|planned|fixed static collision|P2 accepted|player blocked by rock/wall, passes doorway; tankards hit statics without tunneling

## §I INTERFACES

- cmd: `--physics-fixture` → interactive primitive hill/wall arena, WALK/NOCLIP toggle, auto-spawned debug tankards; no Skyrim asset install required.
- runtime: P1+ interactive exterior → first-person NOCLIP at start-cell view; fixture supports both modes from P1. P2 enables WALK over streamed terrain.
- key: NOCLIP `W/A/S/D` fly relative to view; `Space` rise; `Ctrl` descend; `Shift` accelerate.
- input: mouse look; click viewport captures pointer; `Escape` or focus loss releases pointer; uncaptured pointer → no movement/look.
- debug: `NOCLIP: ON  [V]` or `NOCLIP: OFF  [V]`; `[V]` identifies toggle key, no player glyph; blocked WALK entry shows reason.
- key: `V` → toggle NOCLIP ↔ WALK once per press; NOCLIP default ON on interactive start.
- key: WALK `W/A/S/D` move relative to yaw; mouse look; `Space` jump; `Shift` walk slowly; `Alt` sprint.
- key: P2+ `T` → spawn one debug tankard ahead of camera only when local terrain collider ready; bounded live count; fixture auto-spawns several in P1.
- key: P2+ `E` → pick aimed nearby debug tankard; next `E` drops it. Held item follows view with collision/gravity suspended; release restores dynamic physics.
- mode: NOCLIP→WALK in free space keeps position; overlap → bounded upward search for free capsule placement; missing collision/no safe placement → remain NOCLIP & show reason.

## §R RESEARCH

id|topic|finding|src
R1|dependency|`bevy_rapier3d 0.36.0` declares Bevy `0.19.0` dependency|https://docs.rs/crate/bevy_rapier3d/0.36.0/source/Cargo.toml
R2|controller|Rapier kinematic controller exposes translation, slope climb/slide angles, autostep, & ground snap|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/control/struct.KinematicCharacterController.html
R3|controller result|Rapier output exposes grounded, effective translation, collisions, & slope sliding|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/control/struct.KinematicCharacterControllerOutput.html
R4|terrain collider|Rapier `Collider::trimesh` consumes vertices + triangle indices & returns `Result`; handle failure during cell commit|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/geometry/struct.Collider.html#method.trimesh
R5|Skyrim controller|reverse-engineered `bhkCharacterController` exposes gravity, jumpHeight, supportNorm, collisionBound, & step/jump flags; no native update body|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/main/include/RE/B/bhkCharacterController.h
R6|Skyrim input|`hkpCharacterInput` includes movement, jump intent, gravity, velocity, & surface info; exact player values unmeasured|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/main/include/RE/H/hkpCharacterContext.h
R7|Skyrim slope|`hkpCharacterProxy` names `maxSlopeCosine` & friction fields; player use/value of each field unmeasured|https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/main/include/RE/H/hkpCharacterProxy.h
R8|dynamic shape|Rapier supports compound colliders for multi-part debug tankard shape|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/geometry/struct.Collider.html#method.compound
R9|dynamic body|Rapier supports `RigidBody::Dynamic` for gravity-driven tankards|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/dynamics/enum.RigidBody.html
R10|contact|Rapier exposes CCD for fast dynamic bodies; enable only if gate observes tunneling|https://docs.rs/bevy_rapier3d/0.36.0/bevy_rapier3d/dynamics/struct.Ccd.html

## §V INVARIANTS

V1: P2 ∀ valid visible terrain quadrants → matching fixed collider from same positions/indices & parent transform; invalid/hidden terrain → no collision; unload/reload → no stale/duplicate collider; player & tankard cross seams.
V2: P2 collider build error → reported asset/cell failure, no panic or invented floor; existing render/streaming acceptance behavior preserved.
V3: P3 ∀ eligible fixed placements → collider follows full instance/node transform & cell lifetime; source marked original or proxy; unsupported assets skipped & counted. Base record type distinguishes fixed statics from `MISC`/other movable clutter; no duplicate fixed/dynamic collider.
V4: P3 player blocks at rock/wall, passes doorway/opening & excluded visual; tankard hits same rock/wall without tunneling, falls through opening as geometry permits; unload/reload leaves no stale collider; P2 gate remains green.
V5: P1 interactive exterior → one streaming camera, NOCLIP ON, walking collider/gravity disabled; camera crosses fixture/world geometry; noninteractive paths unchanged.
V6: P1 mouse yaw/pitch controls view, pitch bounded; uncaptured/unfocused input cannot move or look; focus loss clears held keys; render-origin rebase preserves position & view.
V7: P1 debug overlay reports actual mode every frame with adjacent `[V]` key hint; overlay never obscures center view or screenshot path.
V8: P1 `V` press toggles once; NOCLIP→WALK only with nearby valid collision & non-overlapping capsule; failed entry leaves NOCLIP ON with visible reason; WALK→NOCLIP removes collision/gravity immediately.
V9: P1 WALK capsule upright independent of camera pitch; camera follows body at eye offset; yaw moves body, pitch moves view. P2+ render-origin rebase shifts Rapier body poses & Bevy transforms together; body/camera/tankards/world colliders stay aligned without added velocity.
V10: P1 WALK input normalized & camera-relative; acceleration/deceleration bounded; WALK < RUN < SPRINT; velocity frame-rate independent. Initial guesses: walk 160, run 300, sprint 420 Creation units/s; horizontal acceleration 1800 units/s².
V11: P1 gravity applies while airborne; jump requires grounded contact & new `Space` press; no air repeat. Initial guesses: gravity 900 units/s² downward, jump launch 340 units/s upward; fixture records actual apex/time.
V12: P1 capsule radius 28, standing height 126, eye height 112 Creation units; slope climb 50°, slide 55°, autostep height 24 units, ground snap 12 units; provisional values centralized. Fixture ground, wall, step, slope outcomes match configured behavior.
V13: P1+ missing/loading ground near WALK capsule → hold vertical position, suspend displacement, show status; resume when collider ready; no synthetic floor. NOCLIP remains available.
V14: P1 toggling clears stale flight/walk velocities & jump state; mode/overlay/collision response agree after switch in air, on ground, or near obstacle.
V15: P1 dynamic debug tankards use 60 Hz Rapier fixed step, same downward 900 Creation units/s² gravity as WALK, & compound convex colliders; fixture cups tumble/roll downhill, contact ground/wall, settle without persistent penetration; P2+ `T` spawns only near loaded collider, capped at 32 live bodies; switch to NOCLIP leaves tankard physics active.
V16: ∀ collision phase gate P1–P3 → exercise actual WALK capsule & dynamic tankard against phase geometry; probe-only success insufficient. Record position/contact outcomes, frame-step settings, asset/collision provenance, & manual playtest result.
V17: Player WALK, terrain, fixed statics, & tankards share one Rapier physics context with matching collision groups; player & tankards contact surfaces, tankards contact player, NOCLIP camera contacts none.
V19: P1 character controller keeps `apply_impulse_to_dynamic_bodies=false` until upstream Rapier manifold-transfer panic fixed; hill gate walks slope with tankards nearby & asserts zero Rapier panics.
V18: Input sampled once/frame, movement integrated once/60 Hz physics tick; streamed collider commits & origin rebase reach Rapier before next physics tick. Same fixture inputs at 30/60/120 render fps → positions/contact outcomes within declared tolerance.
V20: Interactive Riverwood uses one controller camera; WALK yaw & pitch consume current `LookIntent`; mouse up/down changes view while capsule stays upright. Benchmark/screenshot/headless/auto-fly camera behavior preserved.
V21: P2 `E` picks aimed `DebugTankard` within 240 units only; held body follows camera, collision & gravity suspended; second `E` releases dynamic body without stale velocity. Unload/rebase/mode toggle leaves no dangling hold. Overlay shows `T`/`E` controls.
V22: Fiji Riverwood launcher with unset display vars + live `/run/user/<uid>/wayland-N` socket → set `XDG_RUNTIME_DIR` + `WAYLAND_DISPLAY` before engine; preserve explicit display vars; no socket → clear launcher error.

## §T TASKS

id|status|task|cites
T1|x|P1 add Rapier fixed-step setup, `--physics-fixture` primitive slope/wall arena, debug tankard mesh + compound dynamic collider; capture existing camera regression baseline|V5,V15,V17,R1,R8,R9
T2|x|P1 add upright WALK capsule, camera follow, movement/jump/slope settings, & collision-safe toggle against fixture geometry|V8,V9,V10,V11,V12,V13,V14,V18
T3|x|P1 add mouse-look NOCLIP, `V` toggle, `NOCLIP: ON/OFF  [V]` overlay, cursor lifecycle; preserve noninteractive camera paths|V5,V6,V7,V8
T4|x|P1 test controller + tankards on primitive hill/wall, toggle/focus/rebase, 30/60/120 render fps, & camera regressions; record gate evidence|V5,V6,V7,V8,V9,V10,V11,V12,V14,V15,V16,V18,V19
T5|~|P2 attach validated terrain trimesh to quadrant lifetime; handle failures & missing-ground transition|V1,V2,V13,R4
T6|~|P2 enable bounded `T` tankard spawn; rebase Rapier poses with world; test player + tankards on real hill, seams, stream unload/reload; record gate evidence|V1,V2,V9,V13,V15,V16,V18
T7|.|P3 inventory NIF/Havok collision support, base record types, & representative statics; record original/proxy/skip policy|V3
T8|.|P3 convert/load eligible fixed colliders with full transforms, streamed lifetime, & source/skip counts; exclude movable records|V3
T9|.|P3 test player + tankards at rocks, walls, openings, excluded visuals & unload/reload; adjust CCD/contact only from measured failures|V3,V4,V15,V16,V17,R10
T10|.|P3 rerun P1/P2 gates, interactive playtest, screenshot/benchmark regression; record evidence, controls, static proxy limits|V1,V2,V3,V4,V5,V7,V15,V16
T11|x|P2 mount controller in Riverwood, fix WALK pitch, add `E` tankard pickup/drop; preserve noninteractive camera paths|V5,V6,V9,V20,V21
T12|x|Package Fiji launcher with display discovery; verify from shell with display vars unset|V22

## §B BUGS

id|date|cause|fix
B1|2026-09-26|probe-only collision phases deferred WALK/dynamic validation until too late|V16
B2|2026-09-26|Rapier 0.35 controller panics slicing empty manifold vec when pushing dynamic bodies on slope|V19
B3|2026-09-26|WALK follow reused prior view pitch; interactive world omitted controller plugin|V20
B4|2026-09-27|Fiji launcher assumed graphical display variables inherited by terminal; winit panicked before startup|V22
