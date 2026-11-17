# Explore the world and cross a load door

## Sub-features

- Start the engine with a converted world and reach usable movement.
- Move, change collision mode, release and recapture the cursor.
- Cross a load door and return after streaming the destination.
- Travel through `coc` and `coe` console commands.
- Jump and carry movable objects while in WALK.

## How to get to it (user POV)

- Run `engine --assets <pack> --worldspace 1 --grid-x 0 --grid-y 0 --stream-radius 0` for the one-cell generated world.
- Choose **PLAY MUDCRAB** in the launcher for a pack containing the normal default worldspace.
- Use W, A, S, D and mouse look. Use V for collision mode, F3 to release the cursor, and a left click to recapture it.
- In WALK, press Space to jump. Aim at a movable object and hold E to grab it, then press and release E again to drop it.
- Aim at a load door within 200 Creation units and press E. The generated door's auto-load flag does not enable proximity activation.
- Open the console and run `coe 0 0`, or `coc <exterior-editor-id>` for an exterior in the current worldspace.

## Driving it with the native desktop and stdin console

Preconditions:

- Prepare, doctor, and convert a disposable session using the skill.
- Use a working native display and Vulkan renderer. Record the adapter and any virtual display.
- For WALK, require actual collision readiness and a safe player placement. The synthetic fixture is not a complete playable level.
- For PLAY MUDCRAB, use a separate pack with the launcher's default worldspace. The worldspace 1 seed does not satisfy this prerequisite.

Run `control.py window --evidence <directory> --binary engine --timeout 180` inside devenv. Wait for a rendered scene and `SceneState` with `ready: true`. Capture the state and window. Send native movement, V, F3, and recapture input individually. Observe movement or mode changes and confirm controls remain usable.

The generated exterior door is near Creation coordinates `[2048,1024,0]`. Its arrival is in GeneratedInterior near `[128,256,0]`. The return door is near `[128,512,0]` and returns outside near `[2048,512,0]`. Approach each door, aim, and press E through native gameplay input. Capture the action, destination readiness, changed cell identity, and the return crossing. Record failure if the fixture cannot support a walkable approach. Do not teleport and call the door passed.

For the separate console travel journey, run `coe 0 0` while outside and capture exterior readiness. Use an exterior editor ID from the world database before trying `coc` with that name. Run `coc GeneratedInterior` as a rejection check. Require the interior-not-supported error and an unchanged location. Both commands reject travel from inside an interior. Mark the entry point used. Stdin travel does not prove native console focus or door activation.

For carrying, require a supported WALK placement and a movable object with collision and valid mass. The minimal seed does not supply that object. Hold E on it for longer than 0.2 seconds, release, move, and jump. Observe that the object stays held, then press and release E to drop it. Separately open the console or settings while holding it and require the hold to end. Record the fixture and the attempted route when these prerequisites are absent.

## Gotchas

- Interactive startup begins in WALK but can fall back to NOCLIP after collision recovery fails. A visible world is insufficient proof of WALK.
- Scripted shots can skip interior shots. An exterior PNG does not prove a door transition.
- Test-only physics fixtures do not prove conversion-to-world loading.
- Wait for pending streaming, assets, and GPU transfers to settle before declaring arrival.
- `SceneState` reports render readiness without active cell identity or WALK status. Retain crossing logs and native observations for those assertions.
