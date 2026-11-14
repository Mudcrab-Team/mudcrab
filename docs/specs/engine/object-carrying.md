# Object carrying

Tracking: [#104](https://github.com/Mudcrab-Team/mudcrab/issues/104), phase (b).

In WALK, the player picks up a movable object, holds it in front of the camera on a spring, and drops
or throws it. The code is `crates/engine/src/physics/carry.rs`.

## Controls

| Input | Effect |
|---|---|
| Hold `E` on a movable object for 0.2 s | Grab the object. A shorter tap grabs nothing. |
| Release `E` after that hold | Nothing. The object stays in hand. |
| Press and release `E` again | Drop the object. It keeps the velocity the spring gave it. |
| Left click while carrying | Throw the object along the view (`THROW_BUTTON`). |
| `T` | Spawn a debug tankard (unchanged). |

The player grabs the first thing on the line of sight, up to 180 units away. A wall, a fixed object or
an object heavier than 100 kg is refused, and a wall in front hides an object behind it. The object
stays at the distance where it was taken, never closer than the capsule radius plus 5 units.

The latch (the grab-ending release does nothing, the next release drops) is how spud documents the
game's rule. It is **not verified against the retail binary**. `controlmap.txt` in `Skyrim -
Interface.bsa` maps Activate to `E` and the attack button to the left mouse button, and says nothing
about holding. The throw is Mudcrab tuning. Whether retail has a throw or a shove on attack is not
verified.

## How the object is held

The spring acts on the point that was grabbed, not on the centre of mass. Each 60 Hz step:

1. Both body velocities are multiplied by 0.75.
2. The wanted velocity change at the point is `(target - point) * 12 / s - point velocity * 0.5`.
3. The impulse for that change goes through the point's effective mass, so an off-centre grab turns the
   object without shaking it.
4. The impulse is limited to 100 m/s^2 (7000 units/s^2) per kilogram of body mass.

The object keeps its collider and its collision group the whole time. It still hits the world,
clutter and the player capsule. Its sleeping is disabled while held and restored on release.

## When the object drops by itself

| Rule | Condition |
|---|---|
| Snag | The grabbed point is more than 96 units (1.37 m) from its target. |
| Stood on | The player's capsule rests on the held body. |
| Heavy contact | The point is more than 10 units from its target and a moving body more than 4 times heavier touches the object. Static geometry never counts. |
| Gone | The body despawned, for example when its cell unloaded. |
| Interrupted | WALK ended, a door crossing started, the cursor was released, or the console opened. |

All distances are in Creation units. The spud engine compares its lag distance in metres with a
constant in game units, so its snag rule never fires at play distances. A unit test pins this rule
at 96 units.

## Where the numbers come from

| Constant | Value | Source |
|---|---|---|
| Spring, damping and object damping | 12 / s, 0.5, 0.75 | Skyrim's `fZKeySpring*` and `fZKeyObjectDamping` defaults, as the MIT-licensed [spud](https://github.com/chasmlol/spud) engine documents them |
| Force limit | 100 m/s^2 per kg | `fZKeyMaxForce`, 100 in the pack |
| Weight limit, reach, key delay | 100 kg, 180 units, 0.2 s | `fGrabMaxWeightWalking`, `fActivatePickLength`, `fZKeyDelay` defaults (spud) |
| Snag, contact distance, contact ratio | 96, 10, 4 | Skyrim's rules as spud describes them |
| Throw momentum and top speed | 6000 kg units/s, 1000 units/s | Mudcrab tuning. Neither Skyrim nor spud has a throw. |

The converter publishes only `fMoveCharWalkBase` and `fJumpHeightMin` (#595). The pack's raw GMST
records hold `fZKeyMaxForce` but none of the other carry settings, so `CarryTuning::apply_gmst` is
ready for them and nothing calls it yet.

## Not done

- Heavy objects (above `fZKeyHeavyWeight`, 75 kg) do not block running yet.
- The player capsule cannot push loose objects, so a held object cannot push the player either.
- A held object is lost on save and load. Moved clutter is not saved (#90).
- `GetPlayerGrabbedRef` and the `OnGrab` and `OnRelease` events do not exist yet.
