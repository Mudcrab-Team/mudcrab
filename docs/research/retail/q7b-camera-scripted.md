# Q7b Scripted camera control

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Papyrus natives were located by their registration strings (the registration code takes the name and the native function). Static trace.

## Papyrus natives (Game script unless stated)
| native | function | what it does |
|---|---|---|
| Game.ForceFirstPerson | `0x140a2ad20` -> `0x1408f8de0` | Switches to FirstPersonState only if the camera target is the player (PlayerCamera `+0x3C` == player handle) and the state is not already first person. |
| Game.ForceThirdPerson | `0x140a2ad30` -> `0x1408f8f40` | If the current state id is 8 or less (not horse/bleedout/dragon), resets ThirdPersonState (`0x1408fd920`) and switches to it. |
| Game.SetCameraTarget(actor) | `0x140a2ade0` -> `0x1408fb250` | Sets PlayerCamera `+0x3C` (target handle). None/invalid -> player. The states then follow that actor's camera node. |
| Game.ShakeCamera(source, strength, duration) | `0x140a25500` -> `0x1405584a0` | See formula below. |
| Game.SetPlayerAIDriven(ai) | `0x140a2ae50` -> `0x1407559f0` | ai = true: clears the movement controller's controls-driven byte (slot +0x60); ai = false: sets it (slot +0x68) and, for one package type, calls a camera reset (`0x1408fa270`). Also mirrors the state in player `+0xBEA` bit 3. |
| Actor.SetPlayerControls / Debug ToggleControlsDriven | `0x140a02840` / `0x14037b3a0` | Same byte (Q6, RF-CAM-004). |
| Game.DisablePlayerControls / EnablePlayerControls | `0x140a23ee0` / `0x140a23fc0` | Builds a user-event flag mask and disables/enables it in the control map (`0x140cefaa0` on `0x1431a5690`). |

DisablePlayerControls argument -> effect (argument order per the Creation Kit signature: abMovement, abFighting, abCamSwitch, abLooking, abSneaking, abMenu, abActivate, abJournalTabs, aiDisablePOVType):
- abMovement -> flags 0x401 (movement + jumping, flag names per CommonLib `UserEvents`), and stops current movement (`0x1407ae890`).
- abFighting -> 0x40. abLooking -> 0x02. abSneaking -> 0x80. abMenu -> 0x100 and closes open menus (`0x14095f780`).
- abCamSwitch -> sets PlayerControls byte `+0x4C + aiDisablePOVType` (POV-switch block per type).
- abActivate -> PlayerControls activate gate (`0x1407ae150`, 0). abJournalTabs -> global `0x143258ec4`.
Enable reverses each item (no menu close, no movement stop).

**Link to the spring (Q6):** DisablePlayerControls does NOT change the controls-driven byte. Only SetPlayerAIDriven / SetPlayerControls / ToggleControlsDriven do. So the first-person lag spring keeps running under DisablePlayerControls, and fades out (W at 1.5/s) when a script makes the player AI-driven. high.

## Camera shake (`0x1405584a0`)
```
s = clamp(strength, 0, 1)
d = distance(camera, source)                         (source = the given ref, else the player)
fadeStart = max(fCameraShakeDistFadeStart * s, fCameraShakeDistMin) = max(2000 s, 300)
if d < fadeStart + fCameraShakeDistFadeDelta * s     (2500 s):
    if d > fadeStart: s *= 1 - (d - fadeStart) / (2500 s)
    shakeStrength = clamp(s, fCameraShakeMultMin 0, 1)
    shakeTime     = duration > 0 ? duration : fCameraShakeTime (1.25 s)
```
The shake offsets come from an animated NIF (`sCameraShakeFilename_G` = `%s\Actors\Character\CameraShake.nif`); fCameraShakeFadeTime 0.25 s and fCameraShakeExplosionDistMult 0.001 are used by the update/explosion paths (not traced). Animation events also start shakes (`CameraShakeHandler` RTTI). high (formula), medium (update path).

## Camera states a script or the engine enters
| state | entered by | values |
|---|---|---|
| FirstPerson / ThirdPerson | POV key, ForceFirstPerson / ForceThirdPerson, zoom | Q7a |
| AutoVanity | 120 s idle (Q7a); exits on any input | 0.01 rad/frame, 300 units |
| Free (tfc console) | console | fFreeCameraTranslationSpeed 20, RotationSpeed 3, RunSpeed 2 (consumers not traced) |
| Furniture | sitting / furniture with camera | fFurnitureCameraZoom 250, fFurnitureCameraAngle 0.3927 rad; workbench zoom 100, pitch 0.5, yaw -1, translate (0, 50, -50) (consumers not traced) |
| Horse / Dragon | mounting | ThirdPerson update; horse offset (0, -300, 0), dragon AddY -600, transition iHorseTransitionMillis 500 |
| Bleedout | bleedout | collision radius 5; iBleedoutTransitionMillis 500 |
| VATS (kill moves) | kill-move camera | fVATSCameraDollyTime 1.0, fVATSCameraDollyMin 256, fVATSCameraCutAwayDistance 25, fVATSCamZoomInTime 0.75, fVATSSmartCameraCheck* (Skyrim.esm overrides some: DollyTime 0.38, DollyMin 500000, CutAway 1.0) |
| Animated (camera driven by an animation) | AnimatedCameraStart/End/DeltaStart handlers, iAnimatedTransitionMillis 1000 | not traced |
| TweenMenu | tween menu | fTweenCamRotAngle 0.05, RotSpeed 4, ClosingSpeed 10, ZoomFOVMod 10, ZoomSpeed 25 |
| IronSights | bow/crossbow zoom | fIronSightsFOVTimeChange 0.25 |
| Transition | between states | t = elapsed/duration, optional smoothstep (Q7a) |
CameraOverrideStart/Stop handlers (animation events) also exist. Their behaviour was not traced.

## Worked examples (Helgen cart ride, Thieves Guild knock-out)
These are Papyrus scenes. Their camera behaviour comes from the calls above (DisablePlayerControls, SetPlayerAIDriven, ForceFirstPerson, fades through ImageSpaceModifiers) plus furniture/animated states. The quest scripts (PEX) were not decompiled in this pass, so the exact call sequences are unverified. Next step: read the converted scripts in a registered pack (no reconversion needed).

## Other engines
- spud implements DisablePlayerControls flags and SetPlayerAIDriven in its script layer (not checked line by line). OpenSky: not checked.

## Addendum: Helgen and Thieves Guild sequences (converted scripts, read-only)
Source: the scripts converted from the retail PEX files in a maintainer pack (Luau, fragment names as in the PEX). Argument order for Disable/EnablePlayerControls: movement, fighting, camSwitch, looking, sneaking, menu, activate, journalTabs, POVType. FadeOutGame args: fadingOut, blackFade, secsBeforeFade, fadeDuration.

**Helgen intro, MQ101 (`qf_mq101_0003372b.luau`)**
| fragment (line) | call | camera effect |
|---|---|---|
| Fragment_204 (1261) | DisablePlayerControls(T,T,T,F,T,T,T,F,0) | no move/fight/POV switch/sneak/menu/activate; looking stays enabled |
| Fragment_4 (1306-1477) | SetInChargen(T,T,T); DisablePlayerControls(T,T,T,F,T,T,T,T,0); FadeOutGame(F,T,14.0,15.0) | cart start: fade in from black after 14 s over 15 s; free look only |
| Fragment_175 (5696-5723) | SetVehicle(cart) on the player and the passengers | the player rides the cart as a vehicle (camera follows the player; POV switch blocked) |
| Fragment_27 (1887-1896) | SetInChargen(F,F,F); FadeOutGame(F,T,1.0,1.0) | fade up after 1 s over 1 s |
| Fragment_227 (2164-2167) | EnablePlayerControls(T,F,F,T,...); SetPlayerAIDriven(T) | player walks under AI: first-person spring fades out (RF-CAM-021) |
| Fragment_191 / 197 / 57 | Disable/EnablePlayerControls variants; SetPlayerAIDriven(T) (3639) | scripted walks |
| Fragment_295 (5017) | SetPlayerAIDriven(F) | spring returns |
| Fragment_101 (5980-6019) | ShakeCamera(CollapsingBridgeAnim, 0.5, 1.25), (…, 1.0, 2.0), (…, 0.6, 2.0) | shakes from the bridge source (RF-CAM-022) |
| mq101questscript.luau 1058 / 1091 | DisablePlayerControls(T,T,F,T,F,T,T,F,0) / EnablePlayerControls(…looking…) | dragon attack: looking disabled then re-enabled |
Image space modifiers used: BlackoutImod, FadeUpFromBlack, CGDragonAttackBlurLong, PlayerAlduinIMOD.

**Thieves Guild knock-out, TG05 (Karliah's arrow)**
| script (line) | call |
|---|---|
| `qf_tg05sp_0002c367.luau` 245-260 | Mercer StopCombat; **ForceFirstPerson()**; enable arrow-hit ref; ImageSpaceModifier StrikeandFall.Apply(1.0); DisablePlayerControls(T,T,T,T,T,T,T,F,0) (everything but journal, including looking) |
| `sf_tg05spkillscene_00032e64.luau` 97 | TG05BlackFrame.ApplyCrossfade(1.0) (black frame) |
| `qf_tg05_00021551.luau` 289-332 | player MoveTo(TG05PlayerAwakenMarker); pimodWakeUp.ApplyCrossFade(1.0); PlayIdle(GetUp); EnablePlayerControls(all) |
Other TG camera calls: TG08 (`qf_tg08_00021554.luau` 244-861) DisablePlayerControls + SetInChargen; TG08A oath scene imod crossfade; TG08B tower fall / rising water ShakeCamera with script-set amounts; TG09 imod crossfade.
The knock-out uses no special camera state: it forces first person, blocks input, and uses image space modifiers for the fade. high (script calls as converted).
