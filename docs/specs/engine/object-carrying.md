# Object carrying

Tracking: [#104](https://github.com/Mudcrab-Team/mudcrab/issues/104), phase (b). Follow-ups:
[#690](https://github.com/Mudcrab-Team/mudcrab/issues/690) (throw),
[#691](https://github.com/Mudcrab-Team/mudcrab/issues/691) (parity). The code is
`crates/engine/src/physics/carry.rs`.

Rule for this feature: every number comes from Skyrim's own data or code. Where no source has been
found, the behaviour is not implemented, not guessed. The source is the reverse-engineering
analysis of retail `SkyrimSE.exe` 1.7.104.0, cited as `RF-CARRY-001` to `RF-CARRY-012`, `RF-GMST-001`, `RF-SET-001`, `RF-SET-002` and
`RF-UNIT-001` (activate handler at `0x1407b2660`, grab start at `0x140751870`, hold checks at
`0x140753580`).

In WALK, the player picks up a movable object, holds it in front of the camera on a spring, and
drops it. Retail Skyrim SE is the source for every rule and constant.

## Controls

| Input | Effect | Source |
|---|---|---|
| Hold `E` on a movable object for longer than `fZKeyDelay` (0.2 s) | Grab the object. A shorter tap grabs nothing. | `RF-CARRY-001`: one grab attempt when the key has been down longer than `fZKeyDelay` |
| Release `E` after that hold | Nothing. The object stays in hand. | `RF-CARRY-001`: the press that started the grab is flagged, and its release does nothing |
| Press and release `E` again | Drop the object. It keeps the velocity the spring gave it. | `RF-CARRY-001`, `RF-CARRY-007`: a release with a grab active drops it; the drop applies no impulse and there is no throw |
| `T` | Spawn a debug tankard (unchanged). | n/a |

The player grabs the first thing on the line of sight, up to `fActivatePickLength` (180 units). A
wall, a fixed object or an object heavier than `fGrabMaxWeightWalking` (100 kg, inclusive;
`RF-CARRY-002`) is refused, and a wall in front hides an object behind it. The object stays at the distance where it
was taken, never closer than the player's capsule radius times 69.99125 plus 5 units. Retail's hold
distance is the eye-to-hit distance, shortened by any obstacle on the way, and at least that
minimum (`RF-CARRY-008`, medium-high; the 5.0 is at `0x141b5be6c`). The minimum uses Mudcrab's own capsule radius, which is provisional and not traced to retail, so it is only as retail as that radius. Mudcrab's
nearest-hit pick refuses a grab with an obstacle in front, so the first part is the same.

## How the object is held

The spring acts on the point that was grabbed, not on the centre of mass. It is the retail Havok
mouse-spring solver (`RF-CARRY-009`, `hkpMouseSpringAction::applyAction` at `0x140b99e20`, high
confidence), run once per 60 Hz step with `e` the point's error from the target:

1. Both body velocities are multiplied by `fZKeyObjectDamping` (0.75).
2. The point velocity is `v_p = v + w x r`, from the damped velocities.
3. The impulse at the point is `J = -K^-1 (0.5 v_p + 0.2 E e / dt)` (`fZKeySpringDamping` 0.5,
   `fZKeySpringElasticity` 0.2, E = 1 for a normal object), with the point's effective-mass matrix
   `K = I/m + [r]x W [r]x^T`, so an off-centre grab turns the object without shaking it. If K is
   singular nothing is applied in that step.
4. `|J|` is limited to `fZKeyMaxForce` x S x `m` x `dt` (`RF-CARRY-003`): an acceleration cap of
   100 S m/s^2 at the point. S is the mass scale: 1.0 up to `fZKeyMaxForceWeightLow` (0 kg), 0.2 from
   `fZKeyMaxForceWeightHigh` (150 kg), linear between. A 5 kg body gets 97 m/s^2, a 100 kg body
   46.7 m/s^2. Retail sums the masses of every rigid body in the object's node tree; Mudcrab bodies
   are one rigid body each (#691).

Without gravity or the clamp, the point loses 20 % of its error and 50 % of its velocity each step
(an error decay of 12 per second). The 0.75 damping is applied per physics step.

The code was first written as a step-by-step port of the algorithm in spud's `apply_grab` (see
Sources). The retail trace does the same steps, so the two agree.

The object keeps its collider and its collision group. It cannot sleep while held and sleeps
normally after.

## When the object drops by itself

| Rule | Condition | Source |
|---|---|---|
| Snag | The grabbed point is more than 96 units from its target. | `RF-CARRY-005`: hard-coded 96.0 at `0x14181ab88`, compared with the lag in units; `RF-UNIT-001` 69.99125 units per metre |
| Heavy contact | The point is more than `fZKeyMaxContactDistance` (10 units) from its target and a moving body more than `fZKeyMaxContactMassRatio` (4) times heavier touches the object. Static geometry never counts. | `RF-CARRY-005` |
| Gone | The body despawned, for example when its cell unloaded. | Engineering |
| Interrupted | WALK ended, a door crossing started, the cursor was released (window focus lost), or the console or a menu opened. | Engineering, no numbers. What retail does is unknown (#691) |

All distances are in Creation units.

## The steep-look clamp

Looking steeply down puts the plain target (`eye + forward * distance`) on the player's axis,
inside the capsule, where the held body and the capsule would fight. The target is kept at least
`radius + 5` units from the capsule axis, along the view's horizontal direction. This is a Mudcrab
stopgap: retail probably moves the held body to its `ZKeyObject` collision layer (#691).

The spring reads the camera `Transform`, the pose of the last rendered frame. #619 makes that an
interpolated view pose, so carry must read the tick pose once #619 lands (#691).

## Where each constant comes from

Primary source: the reverse-engineering analysis of retail `SkyrimSE.exe` (compiled defaults and
the code that reads them) and `Skyrim.esm`, which overrides two values. The engine reads both
overrides, `fZKeyMaxForce` and `fZKeyMaxForceWeightHigh`, from the pack's GMST records at startup
(`CarryTuning::from_world_database`); a test checks the real pack gives 100 and 150. The other
values are the compiled defaults, which no master overrides. `CarryTuning::apply_gmst` takes any
further setting that the converter publishes (#595).

| Constant | Value used | Executable default | `Skyrim.esm` | Notes |
|---|---|---|---|---|
| `fZKeyDelay:Controls` | 0.2 s | 0.2 (`RF-SET-002`, record `0x1420b8638`; reader function not traced) | none (not in `Skyrim.ini`) | |
| `fActivatePickLength:Interface` | 180 | 180 (`RF-SET-001`, record `0x14209e4c0`; reader function not traced) | none | **Not a GMST**: not in `Skyrim.esm`, the pack or `Skyrim.ini`, so it cannot be read from the pack. Source: `RF-SET-001`, from the executable's settings table dump. The executable also has `fActivatePickRadius` 16 (record `0x14209e4a8`; not used, #691) |
| `fZKeySpringElasticity` | 0.2 | 0.2 | none | |
| `fZKeySpringDamping` | 0.5 | 0.5 | none | |
| `fZKeyObjectDamping` | 0.75 | 0.75 | none | |
| `fZKeyMaxForce` | 100 | 250 | 100 (0x108B7B) | Read from the pack |
| `fZKeyMaxForceWeightLow` / `High` | 0 / 150 | 0 / 300 | `High` 150 (0x108B7C) | `High` read from the pack |
| `fZKeyMaxForceScaleLow` / `High` | 0.2 / 1.0 | 0.2 / 1.0 | none | |
| `fGrabMaxWeightWalking` | 100 kg | 100 | none | `fGrabMaxWeightRunning` (50) has no code reference |
| `fZKeyHeavyWeight` | 75 kg | 75 | none | Holding more makes run speed equal walk speed (`RF-CARRY-006`) |
| `fZKeyMaxContactDistance` | 10 | 10 | none | |
| `fZKeyMaxContactMassRatio` | 4 | 4 | none | |
| Snag distance | 96 units | hard-coded 96.0 (`RF-CARRY-005`) | n/a | |
| Units per metre | 69.99125 | constant (`RF-UNIT-001`) | n/a | |
| Minimum hold distance | radius + 5 | rule in the grab code | n/a | |
| Not retail, no constants | | | | The steep-look clamp (reuses radius + 5) and the interrupts |

## Not implemented

All tracked in [#691](https://github.com/Mudcrab-Team/mudcrab/issues/691).

- **Throw.** `RF-CARRY-007`: there is no throw or impulse on the Z-key path and no throw setting.
  The executable's throw settings belong to the telekinesis spell. Not implemented, and no toggle.
- **Stand-on drop.** spud drops the object when the player stands on it. The rule is not in the
  retail hold checks, so it is not implemented. **Known problem:** a held box that the spring pulls
  upward lifts a player standing on it by about 6 units (measured from 159.9 to 166.3) before it
  slides away. The cause is not verified. The strict test (under 1 unit) is `#[ignore]`d until the
  held body leaves the player's collision group, as retail's `ZKeyObject` layer probably does.
- **Spring special cases** (`RF-CARRY-004`, meanings `RF-CARRY-010`, medium confidence). A body
  on collision layer 14 (TRAP) with no held-body list gets S = 0.5 and E = 0.1. A multi-body object
  whose constraints are all contacts (type 11) or that has none gets S x 0.2 and E = 0.1. Mudcrab
  bodies carry neither their layer nor a constraint list yet.
- **Complex helper** (`RF-CARRY-011`, medium confidence). If the lag `L` is above
  `fZKeyComplexHelperMinDistance` (6 units) on a flagged object or a ragdoll, the body gets an extra
  velocity toward the target of `(1 - m/100) * fZKeyComplexHelperScale (2) * (L - 6) / L`, where `m`
  is the total mass clamped to 0 and 100 kg. The vector's units and the ragdoll factor are
  unverified. Mudcrab has no ragdolls or flagged nodes yet.

## Heavy carry

`RF-CARRY-012` (medium-high; supersedes `RF-CARRY-006`): holding a body above `fZKeyHeavyWeight` (75 kg)
makes the player's "may run" test fail, the same test that over-encumbrance uses. Every direction's
run speed is then replaced by its walk speed. Sprint was not traced and is left unchanged
(#691).

## Sources

- The reverse-engineering analysis of retail `SkyrimSE.exe` 1.7.104.0: registry entries
  `RF-CARRY-001` to `RF-CARRY-012`, `RF-GMST-001` and `RF-UNIT-001` (input, weight limit, spring,
  drops, hold distance, the mouse-spring solver, the special cases, the complex helper and the
  heavy-carry block). Ghidra decompiler and disassembly; a static trace, no game was launched. An earlier read
  of the same settings table and code, done while writing this PR, agrees with it.
- spud (<https://github.com/chasmlol/spud>, MIT, commit `e5f2de4b`): the spring step and the
  contact-mass scan were first written as a step-by-step port of its `apply_grab`, from the
  project's audit of that source (no code copied). The retail solver does the same steps. Its
  notice stays in `THIRD_PARTY.md`.
- `controlmap.txt` in `Skyrim - Interface.bsa`: Activate is `E` (0x12), attack is the left mouse
  button. It says nothing about holding.

## Acceptance numbers

These come from the audit of spud's carry mechanics (an internal evidence note, `carry-mechanics.md`,
phase 1; not in the repository, so the numbers are recorded here). Spud's measured values, not
Skyrim's, were the targets.

| Check | Limit | Measured here (5 kg 20-unit cube) |
|---|---|---|
| Grabbed point peak-to-peak motion once settled | 0.5 units | under 0.5 |
| Grabbed point under its target (gravity sag) | at most 2 units | 0.96 |
| One-step velocity change | the force limit, within 1 % | met for 1 to 100 kg |
| Step response (70 units: settle within 3.5 units in 20 ticks, overshoot 7 units) | | **not measured** |

The body centre creeps about 1 unit over the first second after a grab and under 0.1 unit over the
last 30 ticks of that window. The cause is not proven (see the test comment). Whether retail does
the same is unverified.

## Not done

See [#691](https://github.com/Mudcrab-Team/mudcrab/issues/691): the held-object collision layer, the
stand-on rule, the throw, the grab pick radius, the trap-layer scaling, the complex-helper
settings, publishing the settings (#595), menu behaviour, the view
pose (#619) and the script surface (`OnGrab`, `OnRelease`, `GetPlayerGrabbedRef`). A held object is
lost on save and load; moved clutter is not saved (#90).
