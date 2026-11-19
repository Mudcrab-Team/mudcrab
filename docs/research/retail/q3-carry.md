# Q3 Carry: grab (Activate hold), spring, drop rules

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Static trace (Ghidra 12.2 decompiler + capstone). Effective GMST values = Skyrim.esm where it overrides the executable default.

## Effective values
| GMST | exe default | Skyrim.esm | effective |
|---|---|---|---|
| fZKeySpringDamping | 0.5 | — | 0.5 |
| fZKeySpringElasticity | 0.2 | — | 0.2 |
| fZKeyObjectDamping | 0.75 | — | 0.75 |
| fZKeyMaxForce | 250 | 100 (0x108B7B) | 100 |
| fZKeyMaxForceScaleHigh / Low | 1.0 / 0.2 | — | 1.0 / 0.2 |
| fZKeyMaxForceWeightHigh / Low | 300 / 0 | 150 (0x108B7C) / — | 150 / 0 |
| fZKeyMaxContactMassRatio | 4.0 | — | 4.0 |
| fZKeyMaxContactDistance | 10 | — | 10 |
| fZKeyComplexHelperMinDistance / Scale / WeightMax / WeightMin | 6 / 2 / 100 / 0 | — | same |
| fZKeyHeavyWeight | 75 | — | 75 |
| fZKeyDelay:Controls (INI) | 0.2 s | — | 0.2 s |
| fGrabMaxWeightWalking / Running | 100 / 50 | — | 100 / 50 (Running has no code reference: unused) |
(Update.esm and DLC masters do not override these; checked by GMST EDID scan.)

## Input (`0x1407b2660`, Activate button handler)
- Press and hold longer than fZKeyDelay (0.2 s, strict `>`), with no grab active: start a grab (`0x140751870`). A flag marks that this press started the grab.
- Release: if a grab is active and this press did not start it, drop (`0x140751a30`). If no grab happened, the release activates normally (`0x140751580`).
- So: tap = activate; hold 0.2 s = grab; the release that ends that hold does nothing; the next press-release drops. High.

> Update (2026-11-18): later research refined the hold distance and the fZKeyHeavyWeight rule: the hold distance is the eye-to-crosshair-hit distance shortened by an obstacle ray, minimum capsule radius x 69.99 + 5 units; holding more than 75 kg replaces run speeds with walk speeds.

## Grab start (`0x140751870` -> `0x140752630`)
- Refused if the crosshair object's rigid-body mass > fGrabMaxWeightWalking (100, Havok kg) (`0x140751974`).
- Hold distance: player state `+0x11C` = max(current, body radius term * 70 + 5 units) (`0x140752e63`..). Exact radius term unverified; spud reads it as 0.5*(half.x+half.y) + 5 units.
- Spring: a Havok `bhkMouseSpringAction` is created per held body (vtable names in the decompile) with
  - springDamping = fZKeySpringDamping
  - springElasticity = fZKeySpringElasticity * E
  - objectDamping = fZKeyObjectDamping
  - maxRelativeForce = fZKeyMaxForce * S
  where, for a normal object, E = 1 and S = mass scale:
  `m = clamp(total mass, WeightLow, WeightHigh)`; `S = ((m - WeightHigh)/(WeightLow - WeightHigh)) * (ScaleHigh - ScaleLow) + ScaleLow` = 1.0 at 0 kg falling linearly to 0.2 at >= 150 kg (`0x140752ffc`–`0x140753185`). Total mass = sum of all rigid-body masses in the object's node tree (`0x1404058f0`).
  - Special cases: a body on collision layer 14 with no held-body list: S = 0.5, E = 0.1. If every constraint on the body is of type 0xB: S *= 0.2, E = 0.1. Layer/type meanings: unverified.
  - Telekinesis (grab type 2) uses fMagicTelekinesis[Complex]SpringDamping 0.5/0.9, Elasticity 0.075/0.1, ObjectDamping 0.95/0.7, MaxForce 1000.
- The parameter semantics (damping, elasticity per step, max force per unit mass) are Havok `hkpMouseSpringAction` semantics. The binary passes the values through; the Havok solver internals were not traced.

## Per-frame hold checks (`0x140753580`, called from the player update `0x140745200` while grab type 1 is active)
Lag = distance between the grabbed point and its target, in game units (Havok metres * 69.99, `0x1417f423c`).
1. Lag > 96 units -> drop (`0x14181ab88` = 96.0, hard-coded). Not applied to telekinesis.
2. Contact rule: let R = (heaviest body touching the held body) / held mass. If lag > fZKeyMaxContactDistance (10) AND R > fZKeyMaxContactMassRatio (4) -> drop.
3. Complex helper: if lag > fZKeyComplexHelperMinDistance (6) and the object qualifies (a node flag, or a biped/layer 0x20-0x21 case), an extra velocity is added toward the target, scaled by (lag - 6) * fZKeyComplexHelperScale and by a mass term between WeightMin 0 and WeightMax 100. Exact formula partly traced; medium.
- fZKeyHeavyWeight 75: while holding an object of mass > 75 (player `+0x8DC`), a movement-permission check returns false (`0x140674390`, `0x140686089`). spud reads this as "no running". Which movement it blocks: see the update above (run speeds are replaced by walk speeds).

## Throw / shove on attack
No throw or impulse was found on the Z-key path. Drop (`0x140751a30`) removes the spring actions and re-enables the bodies; it applies no impulse. No GMST for a Z-key throw exists (only fMagicTelekinesisThrow* for telekinesis, fMagicGrabActorThrowForce for Grab Actor). Whether an attack input while holding drops the object or lets the swing hit it was not traced. Status: no retail source for a throw on left click; #689's `THROW_BUTTON` is Mudcrab tuning.

## #689 check
#689's rules (0.2 s hold, the release that ends the hold does nothing, 0.75 / 0.5 / 100, drop at 96 units, 4x heavier and > 10 units lag, radius + 5 hold distance, 100 kg refusal) all match retail. Missing from #689: the mass scale S (heavier objects get a weaker spring, down to 0.2 x at 150 kg), the layer-14 and type-0xB special cases, the complex helper, and the fZKeyHeavyWeight 75 movement block. #689's "error x 12/s" equals elasticity 0.2 at a 60 Hz step under Havok semantics (derived). The throw is invented.

## Other engines
- spud `src/engine/app_combat.cpp:1013-1080`: reads fGrabMaxWeightWalking, fZKeySpring*, fZKeyMaxForce (fallback 100 = the ESM value), fZKeyDelay, fZKeyHeavyWeight, the 96-unit drop and the contact rule. Derived (GMST names); matches the binary. Its special case (elasticity x 0.1, force x 0.5) matches the layer-14 case. It does not show the mass scale S.
- OpenSky: no grab implementation found (only a dialogue topic text mentions the Z-key object).
