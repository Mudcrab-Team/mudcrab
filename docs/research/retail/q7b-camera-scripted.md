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
These are Papyrus scenes. Their camera behaviour comes from the calls above (DisablePlayerControls, SetPlayerAIDriven, ForceFirstPerson, fades through ImageSpaceModifiers) plus furniture and animated states. The addendum below summarises the call sequences from the retail Papyrus scripts; they have no registry rows.

## Other engines
- spud implements DisablePlayerControls flags and SetPlayerAIDriven in its script layer (not checked line by line). OpenSky: not checked.

## Addendum: Helgen and Thieves Guild sequences (retail Papyrus scripts)
Summary of the camera-related calls in the retail quest scripts (MQ101 quest and scene fragments; TG05 and TG05SP fragments), in our own words. Argument order for Disable/EnablePlayerControls: movement, fighting, camSwitch, looking, sneaking, menu, activate, journalTabs, POVType. FadeOutGame arguments: fadingOut, blackFade, secsBeforeFade, fadeDuration.

**Helgen intro (MQ101)**
- Cart start: SetInChargen(true, true, true); DisablePlayerControls blocks movement, fighting, POV switch, sneaking, menus, activation and journal tabs, but leaves looking enabled; FadeOutGame(false, true, 14.0, 15.0): the screen fades in from black after 14 s over 15 s.
- The player and the passengers are attached to the carts with SetVehicle; the camera follows the player, and the POV switch stays blocked.
- Arrival: SetInChargen(false, false, false); FadeOutGame(false, true, 1.0, 1.0).
- Scripted walks: EnablePlayerControls (movement, looking) followed by SetPlayerAIDriven(true): the first-person spring fades out (RF-CAM-021); later SetPlayerAIDriven(false) restores it.
- Bridge collapse: ShakeCamera from the bridge object with (strength, duration) = (0.5, 1.25), (1.0, 2.0), (0.6, 2.0) (RF-CAM-022).
- Dragon attack: DisablePlayerControls with looking disabled, later re-enabled. Image space modifiers used: a blackout, a fade up from black, a dragon-attack blur and an Alduin effect.

**Thieves Guild knock-out (TG05)**
- Karliah's arrow: the enemy stops combat; ForceFirstPerson(); the arrow-hit marker is enabled; a strike-and-fall image space modifier is applied; DisablePlayerControls blocks everything except the journal, including looking; a black-frame crossfade follows.
- Wake-up: the player is moved to the awaken marker; a wake-up image space crossfade; the get-up idle plays; EnablePlayerControls(all).
- Later TG quests use DisablePlayerControls with SetInChargen, image space crossfades, and ShakeCamera with script-set amounts.

The knock-out uses no special camera state: it forces first person, blocks input and fades with image space modifiers. These are summaries of the retail script content, not executable facts, so they have no registry rows.
