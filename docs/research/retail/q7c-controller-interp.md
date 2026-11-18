# Q7c Character controller and physics stepping (#619)

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Static trace.

## Per-frame order (main loop `0x140659d10`)
1. Frame timer update (`0x140ce36b0` on timer `0x1432755f0`) -> T (`0x143275608`, seconds, clamped at 0.1667 s; RF-CAM-007).
2. `0x14104ab70(T, complex, ...)` sets this frame's physics budget (below).
3. Havok world step (`0x14104ac30`), actor updates, then `PlayerCamera::Update` (`0x1408f8680`).

## Physics budget (`0x14104ab70`)
```
step     = fMaxTime:HAVOK (1/60) * timeMult          (complex mode: fMaxTimeComplex 1/30)
cap      = uMaxNumPhysicsStepsPerUpdate (3) * step   (complex: uMaxNumPhysicsStepsPerUpdateComplex 1)
sim      = min(debt + T, cap)
if sim < 0.001 s: no simulation this frame; the time is kept in debt
else: frameSim = sim; debt = debt + T - sim         (time beyond the cap carries over)
```
`timeMult` = global `0x1420ccac8` (slow-time / time-scale factor; name unverified). Globals: frameSim `0x1420d1748`, step `0x1420d1750`, debt `0x143336518`. high.

## Havok world step (`0x14104ac30`)
Rigid bodies step in fixed steps of `step` (1/60 s). The function sets a frame time marker and steps (`0x140b5b250`, step size `0x1420d1750`) until the simulation reaches the marker. This is Havok's asynchronous (frame-marker) stepping pattern; the API identities are inferred from call structure, not symbols. medium.
Solver sub-step count = max(4, round(timeMult * k)) (`0x14104ac30` lines with `+0x3D4`), at least 2. Exact meaning unverified. `fTimePerSubStep` 0.008 and `iMinNumSubSteps` 8 have no direct code reference in this build.

## Character controller (`0x141064d20`, called from the actor movement update `0x140689370`)
The controller integrates with this frame's physics time `frameSim` (`0x1420d1748`), or with the frame delta when no physics time is available. It runs once per rendered frame. So the player's position changes on every rendered frame that has `sim >= 1 ms`. It is not limited to the 1/60 s rigid-body steps and needs no interpolation. medium-high (per-frame call confirmed; the controller's internal integration was not fully traced).

## Answers for #619
| question | retail |
|---|---|
| interpolation of the rendered player position between physics steps | none needed: the player controller moves every rendered frame with that frame's time |
| step rate | rigid bodies: fixed 1/60 s steps, at most 3 per frame (time beyond carries over); character controller: per frame, variable dt |
| 120/144/240 Hz | each frame simulates its own dt (8.3 / 6.9 / 4.2 ms), so the player and camera move every frame. Frames shorter than 1 ms do not simulate. No repeated camera frames for the player. |
| camera | per frame on the current position (RF-CAM-018) |
Rigid bodies (clutter, carried objects) step only on 1/60 s boundaries. Whether their rendered transforms are interpolated was not traced (Havok can provide an interpolated transform; not confirmed here). This is unverified.

Implication: Mudcrab's 60 Hz fixed-step WALK controller with 50-75% repeated camera frames at high refresh is not retail behaviour. Retail moves the character every frame.

## Other engines
- spud runs its player on a fixed tick (`k_tick`) with a "waiting" accumulator (`src/engine/player.cpp:281,414`). That differs from retail per this trace.
- OpenSky: not checked.
