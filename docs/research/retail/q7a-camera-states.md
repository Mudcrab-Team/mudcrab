# Q7a Player camera states and settings (neighbourhood of 0x1408f47a0 / 0x1408fe3c0)

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Static trace (Ghidra 12.2 + decompiler, capstone). Values are executable defaults unless a game-data override is stated.

## Structure
- `PlayerCamera` (vtable `0x14196d980`, constructor `0x1408f79f0`, singleton `0x1431a5478`). State array at `+0xB8 + 8*i`, current state at `+0x28`. Index order (from the array use): 0 FirstPerson `+0xB8`, 1 AutoVanity `+0xC0`, 6 PlayerCameraTransition `+0xE8`, 9 ThirdPerson `+0x100`. Others (VATS, Free, IronSights, Furniture, TweenMenu, Animated, Horse, Bleedout, Dragon) exist as RTTI classes; indices beyond these four are from CommonLib order, unverified.
- Per frame: `PlayerCamera::Update` `0x1408f8680` -> auto-vanity logic -> `TESCamera::Update` `0x140558e70` -> current state's Update (vtable slot +0x18).
- State vtables (slot +0x08 Begin, +0x10 End, +0x18 Update, +0x20 GetRotation, +0x28 GetTranslation): FirstPerson `0x14196d8f0` (Update `0x1408f47a0`), ThirdPerson `0x1418ecc08` (Begin `0x1408fd390`, Update `0x1408fd590`), AutoVanity `0x14196d9a0` (Begin `0x1408f3770`, Update `0x1408f3810`), Transition `0x14196db20` (Update `0x1408fcd50`), Free `0x14196d9f0`, Furniture `0x14196dad0`, Bleedout `0x14196db70`, Horse `0x14196ce48`, Dragon `0x1418eccd0`, TweenMenu `0x14196e9e0`, IronSights `0x14196da80`, VATS `0x14196ea58`. Horse, Dragon and Bleedout reuse ThirdPerson's Update `0x1408fd590`.

## First person
- **Eye / head position:** the world translation of node `Camera1st [Cam1]` (first-person skeleton), read at node `+0xA0..+0xA8` (`0x1408f5310`), plus the lag spring e (RF-CAM-001). No eye-height constant in camera code. No procedural head bob in camera code; any bob comes from the animated node. high.
- Pitch offset feature (f1stPitchOffset* settings, `0x1408f5970`/`0x1408f5a40`) adjusts a view pitch offset on acceleration (target 0.75 x ...). Partly traced; medium.

## FOV
- `PlayerCamera +0x13C` = world FOV, `+0x140` = first-person FOV, both set from `fDefaultWorldFOV:Display` and `fDefault1stPersonFOV:Display` (exe 80 / 80) at construction `0x1408f79f0`.
- `0x1408fb1e0` returns the first-person FOV when the current state is FirstPerson, else the world FOV. high.
- The angle is **horizontal**: a retail capture at 16:9 records 80 degrees horizontal and 50.534 degrees vertical for FOV 80 (`research_notes/Skyrim native tone mapping/gpu_state.md`, "native camera export").
- **Window shape (Hor+)**: the angle is across a 16:9 window, and retail holds the vertical angle at every window shape. Measured 2026-11-24 on this build (SHA-256 846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f), Windows, `fov 80`, `coc riverwood`, noon, clear weather. (1) Scale: a 2560x1440 (16:9) and a 1920x1440 (4:3) capture of the same pose show the scene at the same size; normalised cross-correlation of the central region peaks at scale 1.00 (0.81) and is 0.31 at 1.33, the scale a held horizontal angle would give. (2) Focal length: at 2560x1440, `player.setangle z` steps of 5 degrees move distant scenery 133 px (267 px for 10), so f = 133 / tan 5 deg = about 1520 px: 80.2 degrees across 16:9 and 50.7 vertical. A wider window therefore shows more at the sides: 64.4 degrees across 4:3 and 95.5 across 21:9 at FOV 80. high.
- Other FOV changes found: iron sights / bow zoom (`fIronSightsFOVTimeChange` 0.25 s), dialogue zoom (`fDialogZoomInSeconds` 1.5, `fDialogZoomOutSeconds` 0.5), tween menu (`fTweenCamZoomFOVMod` 10). No sprint FOV change was found in the FOV consumers. Sprint FOV: not found (absence in the consumer list; medium).
- Look sensitivity scales with zoom: pitch input is multiplied by current FOV / fDefaultWorldFOV when zoomed (`0x1408fb2e0`).

## Mouse look (PlayerControls `0x1431a5698`, look vector at `+0x2C/+0x30`)
- Input scaling `0x1407aeba0`: `lookX = counts_x * fMouseHeadingSensitivity * fMouseHeadingXScale / T_real`; `lookY = counts_y * fMouseHeadingSensitivity * fMouseHeadingYScale` (bInvertYValues flips Y). Defaults 0.0125, 0.02, 0.85. T_real = `0x14327560c`.
- **Yaw** (`0x1407a3fd0`): lookX becomes the actor's angular velocity Z, multiplied by a movement-type factor read from the movement controller (`[rbp-0x1c]`). spud identifies that factor as the MOVT "rotate in place / while moving" speed; NPC_Default_MT = pi rad/s (Skyrim.esm `0x3580D` SPED), NPC_Sprinting_MT = pi/2. Integrated over T, yaw per count = 0.0125 * 0.02 * pi = **0.000785 rad/count** walking/running, 0.000393 sprinting. The trace of the factor's source is medium; the formula with the factor is high. This confirms the controller-feel estimate (0.000785). Mudcrab's 0.0025 rad/count is about 3.2x too fast.
- **Pitch** (`0x1408fb2e0` -> `0x140693050`): pitch delta per frame = -lookY * fLookingSpeed:Camera (0.1) * zoomRatio. So **0.0010625 rad/count** (0.0125*0.85*0.1) at default FOV. Not time-scaled. high.
- **Pitch limits** (`0x140693050`): ±pi/2 (constants `0x141b5be30`, `0x14181ab90`). Positive pitch = looking down. Sitting: down limit fSittingMaxLookingDown 40 deg. Mounted: down fMountedMaxLookingDown 40, up fHorseMaxUpwardPitch 25. Flying mount: down 70, up fDragonMaxUpwardPitch 45. high.
- Gamepad only: look curve fLookGraphCoefficient1..4 (0.1151, -0.3827, 2.0287, -0.7545) as a cubic in stick magnitude, plus acceleration (fGamepadLookAccelSec 0.92 ...). Mouse has no acceleration or smoothing in this path. high (mouse), medium (gamepad details).

## Third person (ThirdPersonState)
- **Distance:** `D = fVanityModeMinDist + (fVanityModeMaxDist - fVanityModeMinDist) * zoom` = 155 + 445*zoom (`0x1408fe4bd`). Flying mount: min 1200, max x4.
- **Zoom:** target zoom in [fMinCurrentZoom -0.2, 1.0]. Mouse wheel step fMouseWheelZoomIncrement 0.075 (`0x1408fdd20`); thumbstick fThumbstickZoomSpeed 0.05 x stick. Current zoom moves toward the target at fMouseWheelZoomSpeed (0.8) zoom units per second, linear, no overshoot (`0x1408fe830`). Zooming in past -0.2 switches to first person. high.
- **Shoulder offset:** local offset (x, y, z) = (fOverShoulderPosX 30, fOverShoulderAddY... , fOverShoulderPosZ -10); combat: X 0, Z 20, AddY -100; horse: X 0, Z 0, AddY -300; dragon: AddY -600; ranged dragon: Z 90, AddY -150. Target set in `0x1408fd970`; current moves toward target by `fShoulderDollySpeed (3.0) * T * difference` per frame (first-order, rate 3/s) with a snap when the step would overshoot (`0x1408fe830`). Position = node + R*(x, -D + addY - collisionAdjust, z) (`0x1408fe3c0`). high (structure), medium (which field is AddY).
- **Chase yaw:** camera yaw follows the actor yaw at fChaseCameraSpeed (10, x T) and never lags more than fChaseCameraMaxAngle (30 deg) (`0x1408fe830`). high.
- **Target point:** node returned by actor vtable +0x378 (third-person camera node), else actor position. No vertical spring (RF-CAM-006).
- **Collision** (`0x1408fa5d0`): sphere cast with radius fCameraCasterSize 15 (bleedout fCameraCasterBleedOutSize 5); a second check with fCameraCasterTargetSize 20 at fCharControllerCheckHeightOffset 124 above the actor. Recovery blend fCollisionRecoverySpeed 3.0 x T after collision when the jump exceeds sqrt(4000) (RF-CAM-006). high (values), medium (exact geometry).
- **Damping:** fPlayerThirdPersonDampenTime:Controls 0.25 is read at `0x14075e478` (player movement, not camera). Not traced further.

## View switching and transitions
- First person POV key (`0x1408f54b0`): release after a hold shorter than f1st3rdSwitchDelay (0.25 s) and at least fTogglePOVDelay (0.0) -> switch to third person (`0x1408f8ed0`). Hold for f1st3rdSwitchDelay or more -> `0x1408f8f40` (third person free-look / vanity orbit; medium). Zoom-out input also switches to third person.
- Third person POV key (`0x1408fdd20`): same delays; zoom in past -0.2 -> first person (`0x1408f8de0`).
- **PlayerCameraTransitionState** (`0x1408fcd50`): progress `t = clamp((now_ms - start_ms) / duration_ms, 0, 1)`, optionally smoothstep `t*t*(3 - 2t)` (mode 1). Durations: iHorseTransitionMillis 500, iBleedoutTransitionMillis 500, iAnimatedTransitionMillis 1000. Clock `0x143275610` (ms). A plain first/third toggle does not use this state on the traced path (it switches directly). medium.

## Auto vanity (idle orbit)
- Idle timer `PlayerCamera +0x150` += T each frame while not in AutoVanity, auto vanity is allowed (`+0x160` = !bDisableAutoVanityMode), controls allow it (`0x1407ae030`) and the player is not mounted. Any look/move input (PlayerControls `+0x4A`) resets it. When > fAutoVanityModeDelay (120 s) -> enter AutoVanity (`0x1408fc170`). bForceAutoVanityMode forces entry. In AutoVanity, any input returns to the previous state (`0x1408fc8a0`). high.
- AutoVanity Update `0x1408f3810`: yaw -= fAutoVanityIncrement (0.01 rad) **per frame** (not time-scaled; 0.6 rad/s at 60 fps), wrapped to [0, 2pi). Position = third-person node + actor height term + fDefaultAutoVanityZoom (300) back along the view axis (`0x1408f3a10`), then the normal collision. Begin raises the "AutoVanity" UI event. high.

## Interpolation between physics ticks and frames (#619)
- All camera states run once per rendered frame and read the current node/actor position. No camera-side interpolation, prediction or tick accumulator was found in PlayerCamera::Update or the state updates. The camera clock T is the smoothed frame delta (RF-CAM-007). Whether the actor/node position itself is interpolated between Havok steps lives in the character controller and was not traced. medium.

## Readers of the W weight and the controls-driven byte
- W (`FirstPersonState +0x80`) is read and written only by `0x1408f47a0`.
- Controls-driven byte (MovementController +0x1C5): read by the first-person spring gate and by `ToggleControlsDriven` (`0x14037b3a0`); set/cleared through vtable slots +0x68/+0x60 by several AI/package callers (`0x14075ddc0`, `0x14075e410`, `0x14073cd00`, `0x14073d930`, `0x140742410`, `0x140742680`, `0x1407559f0`, `0x140a02840` SetPlayerControls). See Q7b.

## Other engines
- spud: mouse yaw = counts * sensitivity * xScale * MOVT rotate speed; pitch = counts * sensitivity * yScale then `pitch_delta` (`src/engine/player.cpp:320-329`). Derived; matches the binary. spud clamps pitch to ±pi/2 (matches).
- OpenSky: not checked for camera in this pass.
