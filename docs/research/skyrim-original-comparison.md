# Original Skyrim culling comparison

On 2026-11-15, four matched camera views in SkyrimSE 1.7.104.0 supported Mudcrab's broad
visibility decisions in three disputed shell views and preserved the joined-room cave corridor.
This is a limited visual comparison, not object-by-object or whole-engine parity.

The later [geometry-mismatch investigation](skyrim-culling-mismatches.md) identifies and fixes the
web and foliage overdraw recorded below. These observations preserve the earlier rendering revision.

The machine-readable [capture report](room-original-comparison-20261115.json) records the live
native cameras, executable hash, Mudcrab revision, render receipts, observations, and all 36 image
hashes. The local evidence bundle is named `skyrim-culling-20261115`.
It contains the original PNGs, Mudcrab render receipts, capture helpers, a contact sheet, and the
comparison viewer. Screenshots and proprietary game data are not included in the repository.

## Observations

| Camera pose | Original Skyrim and culled Mudcrab | Mudcrab full-render control |
| --- | --- | --- |
| `BleakFallsBarrow01-room1-c` | Both omit the distant timber hall behind the foreground cave rocks. Smaller geometry and effects differ. | Draws the distant hall. |
| `BleakFallsBarrow01-joined-slave-walk` | Both retain the cave corridor and slanted stone support. Water and lighting differ. | Matches the culled Mudcrab image at the persistent-difference threshold. |
| `GallowsRock01-outside-away` | Both omit the broad backside shell and retain the small right-edge arch. Mudcrab retains extra plants and clutter. | Draws the broad shell. |
| `GallowsRock01-inside-out` | Both retain the near overhead shell and right arch while omitting the farther shell. Smaller objects and effects differ. | Draws the farther shell. |

The three shell views were previously unresolved differences between Mudcrab's culled and full
rendering. The original game now supplies evidence for their broad geometry visibility. These
observations do not identify every submitted object or establish which native culling stage
removed each structure. In particular, this experiment has no native culling-disabled control.

## Capture method

- Native executable SHA-256:
  `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
- Mudcrab rendering revision: `7bd5d0ac4f5d0e38eb4a4752930cb507feb949dc`.
- Skyrim ran directly through Proton Experimental in an isolated X11 display. No SKSE module
  was loaded in that process. No save was loaded or written.
- FreeCamera Framework had failed to load in the earlier SKSE session with Windows error 998.
  The user selected the built-in free camera fallback. This observation does not establish the
  framework's compatibility on every installation of this game version.
- Built-in `coc`, `player.setpos`, `player.setangle`, `fov`, and `tfc 1` positioned and froze the
  camera. `TGM` and `TCL` allowed positioning, and `TM` hid the interface.
- A read-only `/proc/PID/mem` probe checked the live `NiCamera` RTTI, transform, frustum,
  viewport, parent cell, free-camera state, and enabled multibound setting. It did not modify
  camera memory, native functions, or culling state.
- Each native pose has three captures. Their camera positions and rotation matrices remained
  identical across repetitions. Four native poses were read back into `matched-shots.json`.
- Both renderers captured 1400 × 1030 images. Native frustum slopes establish horizontal FOV
  74.999997° and vertical FOV 58.892359°. The console argument was `fov 90.20674767`:
  `fov 75` did not produce 75° horizontal FOV at this window size. Mudcrab uses the measured FOV.

The local `control.py` contains the pinned live-camera reader and console helper. `collect.py`
records native poses. Its console timings were lengthened and key presses held for 200 ms after
missed key events during travel. Captures were accepted only after camera, cell, FOV, and state
assertions passed. The collector is a session-specific helper, not a general game-control API.

### MBC is not a native control in this executable

The tutorial recommends `MBC`, but the pinned runtime's `ToggleMultiboundCheck` / `mbc`
command entry at preferred VA `0x1420906f0` points to execute callback RVA `0x372eb0`.
Inspection of the installed executable shows that callback returns success immediately.
Invoking `mbc` twice left `bUseMultibounds:General` enabled. Those exploratory images are
excluded from the final capture set. This report does not label any Skyrim image "culling off."

## Mudcrab validation

The culled pack used the private source-sphere and authored-assignment fixture described in
[the room-culling research](skyrim-room-culling.md). The control database removes `room_bounds`,
`room_portals`, `room_links`, and `reference_rooms`, leaving the engine's missing-graph fallback.
Both packs resolve to the same mesh and texture trees. The installed asset pack was not changed.

Three independent runs per mode alternated full rendering and culling, producing 24 captures.
All captures settled without timeouts, pending room-graph reads, or failed room-graph reads.
The control drew every mesh listed after ordinary visibility filtering. Culled counts were
identical across all three runs:

| Pose | Listed meshes | Drawn after room culling | Persistent changed pixels versus full rendering |
| --- | ---: | ---: | ---: |
| `BleakFallsBarrow01-room1-c` | 2542 | 578 | 318412 |
| `BleakFallsBarrow01-joined-slave-walk` | 1945 | 571 | 0 |
| `GallowsRock01-outside-away` | 149 | 30 | 354936 |
| `GallowsRock01-inside-out` | 162 | 58 | 203863 |

Pixel counts compare Mudcrab against itself, across all nine cross-mode repetition pairs, with
an absolute per-channel threshold of 8. They are not comparisons against Skyrim or speedup
measurements. The verifier returns failure for the three nonidentical shell views by design;
the original-game visual inspection supplies the separate observations above.

The evidence directory's `verify.py` reproduces Mudcrab captures using:

```text
python verify.py ENGINE CULLED_ASSETS matched-shots.json OUTPUT --baseline-assets CONTROL_ASSETS --repeats 3
```

## Limits and cleanup

Lighting, fog, materials, water, particle effects, and clip planes differ between the renderers.
Some small geometry differs too. No native submitted-object list or object-identity correspondence
was recorded. These four static views in two interiors do not exercise continuous camera travel,
actors, shadow passes, lights, or all portal boundary cases. The remaining parity work listed in
[the native research](skyrim-native-culling.md) still applies.

Skyrim exited without saving. Temporary preferences were restored, the pose batch was removed,
and all existing `.ess` files were verified unchanged, with no new `.ess` files. The private game
display was stopped. The failed framework's INI had been restored before the vanilla launch;
its files were subsequently absent at cleanup and were not recreated.
