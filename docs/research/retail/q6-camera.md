# Q6 Camera: vertical follow on stairs and slopes (#520, #511, #619)

Binary: `SkyrimSE.exe` 1.7.104.0, SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`, image base `0x140000000`. Method: Ghidra 12.2 full auto-analysis (headless) + native decompiler, cross-checked with capstone disassembly. Static only. Raw decompiler output is kept private and is not published.

## Answer: first person (FirstPersonState update `0x1408f47a0`, vtable `0x14196d8f0` slot +0x18)

The camera keeps a smoothed "tracked point" S (state +0x30/+0x34/+0x38), its velocity V (+0x3C..+0x44) and a weight W (+0x80). P = player reference position (actor +0x54/+0x58/+0x5C, the feet). Every camera update, with T = frame delta in seconds:

```
d      = S_prev - P_now                         (per axis)
if |d| <= 300:                                  (3D length; constant 0x1420ba1e0)
    e_z  = W * ( d_z * powf(f1stVertDampeningSpringConstant, T) + f1stVertDampeningVelocityDampening * T * V_z )
    e_xy = W * ( d_xy * powf(f1stHorzDampeningSpringConstant, T) + f1stHorzDampeningVelocityDampening * T * V_xy )
else:
    e = 0                                       (snap: no lag)
camera_position = camera_node_position + e      (node "Camera1st [Cam1]", from vtable slot +0x28)
S = P_now + e
V = (e - d) / T   if |d| <= 300 and T > 0, else 0     (= velocity of S)
```

Defaults (executable): vertical spring 0.001, vertical damping 0.6 (`0x1420ba090`, `0x1420ba0a8`); horizontal spring 0.0001, damping 0.4 (`0x1420ba0c0`, `0x1420ba0d8`).

**Meaning and units.** "Spring constant" is the fraction of the lag that remains after one second: lag decays as `K^T`. Vertical: `0.001^t` = time constant `-1/ln(0.001)` = 0.145 s. Horizontal: 0.109 s. "Velocity dampening" is a momentum factor: the smoothed point keeps moving by 0.6 x its last per-frame displacement (scaled by T_now/T_prev). This makes a second-order response with slight overshoot that depends on frame rate. It is not a 55 ms first-order exponential.

**Step response (derived by simulating the formula above, W = 1, a 24-unit rise, tolerance 5% = 1.2 units).** Convention: the rise happens between two frames; the first rendered frame after it is at t = T (one frame time), the next at 2T, and so on.
| fps | camera rise in the first frame | first inside 5% | stays inside 5% from | overshoot |
|---|---|---|---|---|
| 30 | 4.9 | 0.133 s | 0.333 s | 4.1 units |
| 60 | 2.6 | 0.133 s | 0.217 s | 1.3 units |
| 144 | 1.1 | 0.146 s | 0.146 s | 0.0 |
At 30 and 60 fps the camera overshoots and leaves the 5% band again before it settles.

**Weight W (the gate).** Each update: W += 1.5*T (clamped to 1.0), or W -= 1.5*T (clamped to 0) when any of these is true (constant 1.5 at `0x141b5be2c`):
1. |V| > 500 units/s (3D speed of the smoothed point, constant `0x1420ba1e4`). This is the only speed gate. It is not vertical-specific.
2. The movement controller is NOT controls-driven: byte +0x1C5 of the player's movement controller (actor +0x150), read through vtable slot +0x78 `0x140796ac0`. Slot +0x68 `0x1407969f0` sets it, slot +0x60 `0x140796940` clears it. The console command handler `0x14037b3a0` names this state: "ToggleControlsDriven ... actor movement is now controls-driven / AI-driven". So the spring is ON in normal player-controlled walking and OFF (fades out) when the player is AI-driven.
3. A state byte at +0x88 is set (meaning unverified).
4. A reference handle at player +0x1F0 resolves (candidate: furniture or mount; unverified).
5. A check `0x1401573e0(player+0x70, 0xA9)` passes when player +0x204 bit 1 is clear. Later research identified it as "the actor is on a mount": the spring fades out while mounted.
W fades in or out over 1/1.5 = 0.67 s.

**Answers to the #520 / lead questions (first person):**
| question | retail answer | confidence |
|---|---|---|
| spring meaning, update rule | formula above, per camera update | high |
| units | positions in game units; T in seconds; K = lag fraction left after 1 s | high (T units medium-high) |
| slope threshold for smoothing | none. The spring acts on all motion, flat, slope or stair, on all three axes | high (no slope input exists in the function) |
| snap or slack allowance | none, except the 300-unit discontinuity snap | high |
| max per-tick vertical delta | none (no clamp on e or on camera motion) | high |
| vertical-speed gate | none vertical-only. 3D smoothed-point speed > 500 u/s fades the spring out at 1.5/s | high |
| discontinuity (teleport) limit | 300 units (3D distance between S and P): lag and velocity reset to 0 that frame | high |
| cadence | once per camera update (per rendered frame), not a fixed step | medium-high |

**Clock T** (`0x143275608`, writer `0x140ce36b0`, from #520): T = tick-count delta in ms x 0.001 (`0x141b5bdc4`). A delta over 166.67 ms is clamped to 0.1667 s (`0x141a220f8`, `0x141869dd8`). Later research found that the optional running average is inactive in this build (its filter length is 0) and that the published delta is multiplied by the global slow-time factor (1.0 normally).

## Third person (`0x1408fe3c0`, called from ThirdPersonState update `0x1408fd590`)

No vertical spring. The follow target is the world position of a node returned by actor vtable slot +0x378 (offsets +0xA0..+0xA8), or the actor position if none. In some states it adds the shoulder offset (fOverShoulderPosX 30, fOverShoulderPosZ -10, combat X 0 / Z 20 / addY -100) along the camera axes, then the camera collision call. The only smoothing is collision recovery: after a collision ends, if the squared distance to the saved position exceeds 4000 (fCollisionRecoveryMinDist, compared with squared distance), it blends from the saved position at fCollisionRecoverySpeed 3.0 x T (to 1.0). So on stairs the third-person camera moves exactly with the node. Any softness comes from the body (character controller and animation), not the camera. Confidence: high for "no spring in this function"; the node identity is medium.

## Verdict on #511's three temporary values
| #511 value | retail |
|---|---|
| 55 ms first-order time constant | Replace with the 2nd-order spring above (0.001 / 0.6 vertical, 0.0001 / 0.4 horizontal, applied per frame with powf). |
| 30 deg walkable-slope rule | No retail equivalent. Retail smooths all motion. |
| 1 unit per-tick slack | No retail equivalent. |
Teleport reset: retail uses a 300-unit distance test, plus the W gates. #511's event-based resets (door, mode change) are extra.

## Cross-checks
- spud: uses vertical 0.001/0.6 and horizontal 0.0001/0.4 (per the controller-feel report). Derived; matches the binary. Whether spud implements the 300/500/1.5 gates was not checked.
- OpenSky: not checked for this topic.
- CommonLibSSE-NG names the +0x1C5 flag area of MovementControllerNPC "controlsDriven" (not checked against the header in this pass).
