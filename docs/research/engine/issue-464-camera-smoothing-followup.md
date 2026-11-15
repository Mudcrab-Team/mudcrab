# Issue 464 first-person camera follow spring

**Base:** `main`. This follow-up implements the retail first-person camera follow spring in the WALK camera. The retail facts come from the retail-facts registry, entries RF-CAM-001 to RF-CAM-007. They were traced in `SkyrimSE.exe` 1.7.104.0 at `0x1408f47a0` (first person) and `0x1408fe3c0` (third person).

## Finding

Taylor's tower playtest showed that the view jumped at each stair rise. Skyrim does not hold the first-person camera rigidly on its node. It keeps a smoothed tracked point and offsets the camera by a lag. The lag comes from a second-order spring that runs once per rendered frame.

The earlier version of this change used a 55 ms first-order time constant, a 30 degree walkable-slope rule, a 1 unit slack, a 36 unit per-tick bound, a vertical-speed gate and a 256 unit discontinuity limit. None of these numbers exists in Skyrim's code. They are removed.

## The spring

For each axis, once per rendered frame, with `T` the camera clock in seconds and `P` the feet position:

```text
d = S_prev - P_now
e = W * (d * K^T + c * T * V_prev)
V = (e - d) / T
S = P_now + e
camera = camera_node + e
```

`S` is the smoothed tracked point, `V` its velocity and `W` the weight. The vertical axis is Y. The horizontal axes are X and Z.

| Value | Constant | Registry entry |
|---|---|---|
| Vertical spring constant K | 0.001 | RF-CAM-001 |
| Vertical velocity damping c | 0.6 | RF-CAM-001 |
| Horizontal spring constant K | 0.0001 | RF-CAM-001 |
| Horizontal velocity damping c | 0.4 | RF-CAM-001 |
| Snap distance (3D, `S_prev` to `P_now`) | 300 units | RF-CAM-002 |
| Weight ramp rate, inside [0, 1] | 1.5 per second | RF-CAM-003 |
| Weight fade-out speed (3D speed of `S`) | 500 units per second | RF-CAM-003 |
| Controls-driven gate | the spring fades out when not controls-driven | RF-CAM-003, RF-CAM-004 |
| Mounted gate | the spring fades out while the player is on a mount (same 1.5 per second ramp) | RF-CAMW-001 |
| Camera clock clamp | 0.1667 s (166.67 ms) | RF-CAM-007 |
| No slope threshold, no slack, no per-tick clamp, no vertical-only speed gate | absent | RF-CAM-005 |
| Third person: no vertical spring | absent | RF-CAM-006 |

The mounted gate is an input flag, `is_on_mount`. The caller sets it. Mudcrab has no mount state yet, so the WALK system passes false. No mount system is added here.

When `|S_prev - P_now|` is above 300 units, `e` and `V` are 0 for that frame. The weight ramps up by `1.5 * T` per frame. It ramps down by the same amount when the speed of `S` is above 500 units per second or when the player is not controls-driven.

## Where it lives

- `crates/engine/src/physics/camera_follow.rs` holds the pure function `follow_spring_step`. Its input is the feet position now, the previous smoothed point, the previous velocity, the previous weight, the frame delta and the view kind. It owns no state, so a later change that interpolates the view between physics ticks can feed it its own feet position.
- `walk_camera_follow_system` in `crates/engine/src/physics.rs` calls it from `Update`. That schedule runs once per rendered frame. The spring does not run per fixed physics tick, and it needs no record of physics steps.
- The engine has no third-person camera. The pure function returns no lag for `FollowView::ThirdPerson`, and the WALK system always passes `FollowView::FirstPerson`.

## Engine resets

A teleport and a `CameraSpace` change place the body and the view in a new coordinate frame. The old smoothed point has no meaning there, so the system reseeds the spring at the new feet position and keeps the weight. A door crossing and the safe-placement hold place the body without player control. They are plumbing for Mudcrab-only mechanisms that retail does not have. During them the view follows the body directly. None of this is a retail value.

A `RenderOrigin` rebase does not reset the spring. The streamer shifts the body and the view by whole cells, so the system subtracts the same displacement from the smoothed point. The lag stays the same in the world frame. While running, the steady lag is about 20 units, so a reset would make the view jump forward at every cell border. Entering WALK needs no reset: the spring starts at rest on the feet. For a teleport, retail reaches the same result through the 300 unit snap (RF-CAM-002). The authored startup view stays authoritative for the first WALK frame.

## Not implemented

- Open gates, not implemented: RF-CAMW-002 (actor handle at player `+0x1F0` that fades the weight out, medium confidence for its role) and RF-CAMW-003 (FirstPersonState byte `+0x88`, low confidence).
- The optional running average of the camera clock over N frames (RF-CAM-007) is left out. Its filter length is unverified.
- UNVERIFIED: the initial weight. The spring starts with a weight of 0 and ramps up at 1.5 per second. RF-CAMW-003 notes that the retail state reset sets W = 1 (low confidence, callers not traced). That is not enough to choose 1, so 0 stays until the registry resolves it.
- UNVERIFIED, ordering assumptions: the weight is updated before the lag is evaluated, and the 500 units per second gate reads the velocity of the previous frame. The registry states neither.
- Horizontal lag follows the registry formula. Its effect on running camera feel is not measured here.

## Tests

The pure unit tests in `camera_follow.rs` reproduce the numbers from the reverse-engineering notes for a 24 unit rise with `W = 1`:

| Frame rate | Camera rise in the first frame | First frame within 5 percent (index / fps) | Overshoot |
|---|---|---|---|
| 30 fps | 4.9 | 0.100 s | 4.1 units |
| 60 fps | 2.6 | 0.117 s | 1.3 units |
| 144 fps | 1.1 | 0.139 s | 0.0 |

The third column is the 0-based index of the first frame whose camera is within 5 percent of the target, divided by the frame rate. It is not a settled time. The elapsed time at the end of that frame is one frame later (0.133 s at 60 fps), and the camera can leave the band again after an overshoot.

More tests cover the snap above 300 units, the weight ramp up and down at 1.5 per second, the fade-out above 500 units per second, the absence of a slope threshold or slack, no vertical spring in third person, the horizontal constants and the 0.1667 s clock clamp. The engine test `camera_follows_the_retail_spring_on_a_production_stair_flight` runs a stair flight through the production WALK controller. It checks, on every rendered frame at 60 and at 20 frames per second, that the view equals the eye node plus the spring lag. It also checks that the physical stair endpoints do not change.

## Evidence limits

These are source and CPU test checks. They do not establish native gameplay feel, performance or Skyrim parity. Taylor's earlier in-game test of the 55 ms version does not apply to this spring. A new in-game test is required, and it must include running down stairs.
