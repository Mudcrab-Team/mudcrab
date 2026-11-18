# Q11 Stealth detection formula (B7)

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Static trace (Ghidra 12.2 + decompiler). Values: Skyrim.esm where it overrides the executable (marked ESM), else the executable default (exe).

## Where
- `0x14041cd90` computes one detection value from prepared inputs (returns three components and the total).
- `0x1406a33b0` prepares the inputs for detector D and target T and calls it. (`0x14041d230` is a second, shorter variant with the same GMSTs; not traced.)

## Inputs prepared by `0x1406a33b0`
- Distance d = |D - T| (`0x1402f41e0`). Detection range R = max distance (`0x14041cd30`, below); x fDetectionCombatNonTargetDistanceMult (0.75 exe) when D is in combat and T is not its target.
- Angle a from D's facing to T (`0x1406a32c0`). If a > fDetectionLOSDistanceAngle (60 deg exe): R *= lerp(1, m, (a - 60 deg)/(180 deg - 60 deg)), m = fDetectionLOSDistanceMultInterior 0.75 / Exterior 0.5 (exe).
- If d < R: a line-of-sight test (`0x1406a2830`) gives LOS; "in view" = LOS and a < fDetectionViewCone/2 (190/2 = 95 deg, exe).
- Light level of T: T's computed light (`0x1406ff470`), x fDetectionNightEyeBonus (3.0 exe) if D has Night Eye (AV 0x37). Offset: exterior iLightLevelExteriorMod (0 exe); interior min(iLightLevelInteriorMod (25 ESM), 100 x max channel of the scene ambient colour (Sky +0xCC..+0xD4)). light = clamp(L - offset, 0, iLightLevelMax (300 ESM)), then perk entry point 0x2F, rounded.
- Sneak skills: D's Sneak (AV 0x0F) is the "perception" skill; T's Sneak. For the player, skill' = fPlayerDetectionSneakMult (0.4 ESM) x skill + fPlayerDetectionSneakBase (10 ESM); then perk entry point 0x39 on T's value.
- T state: equipped weight, action noise counter (T +0x11C), moving, running, sneaking, movement noise multiplier (AV 0x5C); a perk entry point (0x0F) clears running, and clears moving while sneaking. D state: alert, asleep, in combat, ambush mode (target 2.5 / non-target 0 ESM), blindness (AV 0x54), T invisible (AV 0x36 > 0).
- Size class of T's race (small / normal / large / very large).

## Formula (`0x14041cd90`)
```
Rmax   = fSneakMaxDistance (2500 ESM) x [exterior: fSneakExteriorDistanceMult 2.1 ESM] x [either flying: fSneakFlyingDistanceMult 3.0 ESM]
dist   = max(0, (Rmax - d) / Rmax) ^ fSneakDistanceAttenuationExponent (2.0 exe)
sDist  = dist x [no LOS and not ambush-target: fSneakSoundLosMult 0.3 ESM]

Sound  = max(0, ( fSneakActionMult(2.0 ESM) x actionNoise
                + [running ? fSneakRunningMult 2.0 : 1] x (fSneakEquippedWeightMult 0.5 exe x weight + fSneakEquippedWeightBase 12 exe)
                  x movementNoiseAV x moving )
              x fSneakSoundsMult (1.0 ESM) x sDist x (1 + [D asleep] x fSneakSleepBonus (0 ESM)) )

move   = 1 + moving x fSneakLightMoveMult (0.01 exe) + running x fSneakLightRunMult (0.2 exe), then perk entry point 0x30
Sight  = max(0, inView x !invisible x dist x move
              x (light x [exterior ? fSneakLightExteriorMult 0.5 ESM : fSneakLightMult 0.33 ESM] + fDetectionSneakLightMod 15 ESM)
              x [D asleep ? 0 : (100 - blindness)/100]
              x fDetectionLargeActorSizeMult (1.0 exe) x size(0.5 / 1.0 / 1.5 / 2.0 exe) )

Skill  = ( dist x (1 + alert x fSneakAlertMod(0 ESM) + asleep x fSneakSleepMod(0 ESM) + inCombat x fSneakCombatMod(-0.4 exe) + ambushMod)
              x lerp(fSneakPerceptionSkillMin 0, fSneakPerceptionSkillMax 100, perception/100)    (ESM 0 / 100)
         - [T sneaking] x T's sneak ) x fSneakSkillMult (0.5 ESM)

Detection = fSneakBaseValue (-15 ESM) + Sound + Sight + Skill;  result = int(Detection), but 0 < Detection < 1 gives 1
```
high (structure and constants); medium (meaning of some inputs taken from the call site: alert, asleep, ambush, movement-noise AV, action counter).

## Not traced
- How the integer is turned into detection states (unseen / noticed / seen / lost) and the per-actor update timing (fDetectionUpdateTimeMin/Max and their "Complex" variants are read at `0x140705279`–`0x140705293`; fDetectionStateExpireTime at `0x1406ff949`).
- The target light computation (`0x1406ff470`) and the max-distance helper `0x14041cd30`.
- fSneakSizeBase and fSneakStealthBoyMult: no code reference found in this build.

## Other engines
- spud / OpenSky: not checked for detection in this pass.
