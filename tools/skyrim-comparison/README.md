# Fiji comparison session

`session.json` describes the starter stack, exact Nexus file IDs and archive hashes, runtime identity, and capture inputs. It is our comparison-session manifest, not an Amethyst import format. Installed-game, deployment, plugin-startup, and matched-capture checks are separate gates.

The dedicated session lives at `/home/taylor/Games/MudcrabComparison` on Fiji. It has a separate game copy, Proton compatdata/prefix, `XDG_CONFIG_HOME`, Amethyst staging tree, profile INIs, and profile saves. Ordinary Steam Skyrim and the existing Amethyst profiles keep their paths. The copied prefix must retain only internal user-folder symlinks; no game Documents or save path may resolve into the normal prefix.

The initial stack uses SKSE64 2.3.1, Address Library 13 for Skyrim 1.7.104, Photo Mode 3.0.4, powerofthree's Tweaks 1.17.1, MCM Helper 1.6.3, SkyUI 6.11, ImGui Icons 1.2.0, KreatE 1.5.2, and Start On Save 2.8.0. Photo Mode, Tweaks, and Start On Save use their FOMOD `SSE/AE v1.7.99+` option. MCM Helper uses Skyrim Special Edition, ESL, and loose files. FreeCamera Framework 1.1.0 is staged disabled: timeline playback needs a framework client, whereas Photo Mode provides a camera interface directly.

## Read live tone-map state

The native packing and output investigations now have separate evidence notes:
[CPU parameters to shader constants](../../research_notes/Skyrim%20native%20tone%20mapping/gpu_binding_trace.md),
[reciprocal-gamma exponent](../../research_notes/Skyrim%20native%20tone%20mapping/output_transfer_trace.md),
and [runtime observations](../../research_notes/Skyrim%20native%20tone%20mapping/runtime_state.md).
The approved Mudcrab preset remains unchanged.

`scripts/inspect_skyrim_tonemap_memory.py` reads an already-running Linux/Wine
process. Deploy the script to this isolated session's `bin/` directory, identify
the game window's PID, and use a fresh output directory:

```sh
python3 /home/taylor/Games/MudcrabComparison/bin/inspect_skyrim_tonemap_memory.py \
  --pid PID \
  --exe /home/taylor/Games/MudcrabComparison/SkyrimSE/SkyrimSE.exe \
  --output /home/taylor/Games/MudcrabComparison/artifacts/FRESH-memory-run
```

The collector pins the executable hash, identifies the relocated image, checks
native code bytes and setting names, and reads bounded manager/parameter fields.
It retains two sequential samples, raw bytes, mapping and process-identity checks,
and failure receipts. It never attaches, pauses, writes target memory, launches
a game, or changes access policy. If process-memory access fails, retain the
failure; do not change system policy to make the collector succeed.

For independent console evidence, visually verify an open console with an empty
input line, then run:

```sh
python3 /home/taylor/Games/MudcrabComparison/bin/capture_skyrim_tonemap_state.py \
  --output /home/taylor/Games/MudcrabComparison/artifacts/FRESH-console-run \
  --console-open
```

This collector sends a fixed list of read-only queries. Each query gets a fresh,
CRC-checked Gamescope PNG, raw OCR, and hashes. It leaves values unparsed because
console history can contain older output. Review the echoed command and its
result together. Input-event completion alone does not establish execution.
Typing uses 60 ms between characters; Return is held for 200 ms and released
even if an error interrupts the hold. Use deliberate key holds for manual
navigation too; short control-key pulses were missed in the observed session.

Process samples and console frames do not synchronize GPU execution. Keep their
timestamps and claims separate from a screenshot's exact shader inputs. Neither
collector establishes the settings of an older reference image.


## Camera inputs

Save one native Photo Mode camera preset from the target scene. Its directory is beneath the dedicated prefix's Skyrim Documents directory at `Saves/PhotoMode/CameraPositions`. Keep its cell and player metadata. Record calibration in a separate JSON document:

```json
{
  "runtime_sha256": "<64-character hash of the calibrated SkyrimSE.exe>",
  "evidence": "<path or receipt identifying measured native camera checks>",
  "pitch_sign": 1,
  "yaw_sign": 1,
  "yaw_offset_degrees": 0,
  "fov_axis": "horizontal"
}
```

The signs, offset, and FOV axis above illustrate the fields; they are not measured defaults. Verify north/east headings, pitch sign, projection at 1920×1080, and position readback before filling them in.

Generate inputs from the same fixture Mudcrab uses:

```sh
python3 scripts/prepare_skyrim_comparison.py \
  --shots crates/engine/tests/fixtures/color-grade-riverwood-shots.json \
  --native-preset /path/to/native-camera-export.json \
  --calibration /path/to/measured-calibration.json \
  --output /path/to/new/preset-bundle
```

Only copy the generated `CameraPosition_*.json` files into Photo Mode's camera-preset directory. Keep `manifest.json` outside that directory, because Photo Mode attempts to parse every JSON file there as a camera preset. The exporter preserves native cell/player fields and unknown template fields, records input and output hashes, and refuses `[0, 0]` rotation because Photo Mode skips applying it. Generated inputs do not constitute native capture proof.

## Capture protocol and future tooling

Load the matching worldspace/cell first. Restore a preset and match the source weather and time of day. Wait for geometry and LOD to finish loading before entering `tfc 1` to freeze simulation. Hide the HUD with `tm` and leave Photo Mode image filters disabled. Both `tfc 1` and `tm` toggle state; inspect their current state before issuing either command. `tm` also hides the console. Player coordinates are not camera coordinates: retain native camera readback or explicitly label the eye offset unmeasured. Pair each screenshot with a receipt containing the executable and archive hashes, plugin/mod load order, INIs, source pose, native camera readback, weather, time, dimensions, and image hash. Mudcrab captures use `scripts/capture_color_grade.py`.

Keep PNGs, game saves, native presets, logs, and Nexus credentials outside Git. Add test suites and timeline clients to this profile as separate manifest entries; do not mix rendering-overhaul mods into a vanilla-reference run. Use fresh output directories for each comparison campaign and retain failed receipts.

## Launch and autoload

Fiji runs the isolated game in a GPU-backed Gamescope headless display. A fresh unattended launch has passed: SKSE and all five native plugins loaded, the pinned save reached the post-load event, startup notices were handled, and a 1920×1080 PNG was captured. Matched camera projection and angles remain uncalibrated.

Run the complete startup/capture check through SSH with a fresh output directory:

```sh
python3 /home/taylor/Games/MudcrabComparison/bin/run_skyrim_comparison.py \
  --output /home/taylor/Games/MudcrabComparison/artifacts/new-run
```

The runner starts task-owned display, Steam, and game services as needed. It reuses a running comparison game. The display service atomically publishes its environment with its systemd invocation ID; the runner waits for a matching ID and X socket before launch. It recognizes the achievements notice and declines the survival-mode prompt; it never accepts a missing-content warning. It verifies the newest completed SKSE save event before and after settling and capture. Every operational failure writes `failure.json` with its phase, subprocess diagnostics, available screenshots, and SKSE log. Successful runs record screenshot and log hashes. No desktop login is needed. Steam currently signs in using its existing local session; a future Steam authentication failure is a separate gate.

Service definitions are under `systemd/`, launch helpers under `bin/`, and tools are pinned locally by `artifacts/automation-tools.paths` and `artifacts/ocr-tool.path`. Services are started on demand rather than at boot. The game wrapper deploys only this Amethyst profile and invokes its SKSE loader through GE-Proton11-5 with the dedicated compatdata directory. Open the isolated manager with `bin/amethyst-comparison`; do not launch through the ordinary Steam Skyrim entry for this session.

Start On Save is pinned to the downloaded **Mr. Clean** level-1 outdoor save from `/home/taylor/Downloads/Stormcloack Save-1966-.7z`. Its embedded plugin list contains only Skyrim, Update, Dawnguard, HearthFires, and Dragonborn; all five are present. The save and archive hashes are recorded in `session.json`. New-game fallback is disabled, missing-content warnings remain enabled, and holding Shift during menu startup skips autoload. The original archive and normal save directories remain unchanged.

The newer Adventurer quicksave is a Skyrim VR/FUS save with 329 embedded plugins; 322 are absent from this SE session. Its copied save and SKSE sidecar were moved out of the active profile into `artifacts/rejected-fus-save`. It is not the autoload target.

Open KreatE with **End** to inspect or edit weather, image spaces, volumetric lighting, cell lighting, and lighting templates. No preset edits or optional DALC fix were configured. Keep an unedited baseline for comparisons; record each edited preset and whether DALC correction is enabled alongside its screenshots. Native plugin startup is verified. Matched poses and controlled visual baselines still need calibration.

Photo Mode is bound to **F10** in the dedicated game's `Data/MCM/Settings/PhotoMode.ini`, with `bNavigateWithMouse=false`. The game wrapper caps DXVK at 60 FPS. Capture uses `GAMESCOPECTRL_REQUEST_SCREENSHOT=1` on the task-owned Gamescope display, then copies its fresh complete PNG from `/tmp/gamescope.png`. X11 window imports were observed returning stale frames, including stale console output; do not use them as camera or state evidence. Serialize capture requests on this display because Gamescope uses a shared output path.

The Riverwood overlook was captured automatically at native camera position
`[26000, -50000, 6000]`, native rotation `[0.61086524, 5.7595863]` radians
(pitch 35 degrees, yaw 330 degrees), and native world FOV 75 degrees.
`riverwood-overlook-input.json` is the declarative input;
`riverwood-overlook-readback.json` is the camera export written by the running
Photo Mode plugin after loading it. Position and FOV match exactly; rotation
matches within 0.000001 radians. The player anchor in that export is separate
from the camera position. Mudcrab projection matching remains uncalibrated.

Evidence on Fiji is under `artifacts/riverwood-overlook-20261012`: clean PNG,
repeat PNG, camera input, native readback, `tfc 1` console evidence, and a receipt
with hashes. The recipe loaded `cow Tamriel 6 -13`, allowed terrain to load,
issued `set gamehour to 12`, `set timescale to 0`, `fw 81a`, and `tfc 1`, then
loaded the exact free-camera preset in Photo Mode and hid its UI with **T**.
Weather was commanded but has no native weather readback yet. Photo Mode filters
were not edited. A repeat seven seconds later was not byte-identical: normalized
mean absolute image difference was 0.00367146. `tfc 1` freezes simulation, but
these captures do not establish pixel-identical rendering across frames.

For Photo Mode keyboard navigation, use **Tab** to focus the camera-position
combo and **Space** to open it. **Down**, then **Enter**, selects the first
entry. **Tab**, then **Space**, activates **Load**. Arrow navigation skips the
combo. **Save Camera Position** writes the native readback without needing a
text-entry dialog. Keep only camera preset JSON files in its preset directory.
