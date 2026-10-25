# Visual profiles

Tracks [visual profiles #358](https://github.com/Mudcrab-Team/mudcrab/issues/358). This page covers
[P0 #359](https://github.com/Mudcrab-Team/mudcrab/issues/359): the profile setting, its receipts and
the stage receipt format. Later packages extend it.

## The setting

A visual profile selects a whole color pipeline: inputs, shading, fog, post-processing and output.

| Profile | Pipeline |
|---|---|
| `mudcrab` (default) | The approved look: Bevy PBR, TonyMcMapface, EV100 9.7 and the snapshot grade. See [color-pipeline.md](color-pipeline.md). |
| `skyrim` | Retail Skyrim SE's color pipeline on raw authored values. Until P1 lands it renders the same as `mudcrab`; only its receipts and option checks differ. |

- Select it with `--visual-profile <mudcrab|skyrim>`, or with *Visual profile (restart)* in the
  interactive settings menu, which saves it. An explicit flag wins over the saved value.
- The profile is read once at startup. Changing it requires a restart, as Community Shaders requires
  for Linear Lighting; nothing in the engine rebuilds a running pipeline for another profile.
- Startup logs `visual profile active` with the profile name.

## Options that belong to one profile

Under `skyrim`, these launch options exit with a named reason before anything starts, instead of
being silently ignored:

| Option | Reason |
|---|---|
| `--tone-map`, `--tone-map-params` | `skyrim` uses the retail image-space tone curve |
| `--color-grade`, `--grade-preset` | `skyrim` grades with the authored image space |
| `--visual-preset` | `skyrim` takes sky and fog from weather records |

The check runs twice: once on the command line, and again after saved settings are applied, since a
saved `skyrim` profile can meet a mudcrab-only flag. `--image-space`, `--image-space-weather` and
`--image-space-slot` stay valid under both profiles; P1b reads them.

Only launch options are checked. Grade and exposure values saved from the settings menu are not
refused or hidden under `skyrim` until P1b.

## Receipts

Every receipt names the profile it describes:

- `shots.log`: `visual_profile` on each capture line, and inside `captured_frame`.
- `SceneState` (live console) and `SceneShot` receipts: `visual_profile`.
- Bug-report snapshots: `configured_rendering.visual_profile`.

The benchmark run fingerprint (`fingerprint.rs`) does not carry the profile yet. While both profiles
draw the same pixels it does not need to; once P1 makes `skyrim` render differently, the profile
belongs in the fingerprint's graphics section so two benchmark runs of different profiles are never
compared as one.

## Stage receipts

A stage receipt records the value at four stages of the pipeline, at a fixed set of probe points.
Retail and Mudcrab captures of the same pose write the same format, so they compare stage by stage.

| # | Stage | Retail source | Mudcrab source |
|---|---|---|---|
| 1 | raw sampled albedo | shader-replacement probe on the lighting draws | lands with P1a #360 |
| 2 | pre-fog lighting sum, albedo × (direct + directional ambient) | shader-replacement probe on the lighting draws | lands with P1c #362 |
| 3 | post-fog, pre-tonemap HDR color | tone-map group 26 scene input `t1` | stage-output hook before tone mapping |
| 4 | final output byte | tone-map group 26 output | the captured frame |

A probe is named by world position and surface, not screen coordinate. It averages a small flat
patch around the projected position, so a projection difference (HFOV 72.7 against native 80 in an
earlier capture) or a sub-pixel offset does not move the result.

```json
{
  "schema": 1,
  "side": "mudcrab",
  "visual_profile": "skyrim",
  "build": "<commit or SkyrimSE.exe SHA-256>",
  "build_dirty": false,
  "viewport": [1280, 720],
  "pose": { "worldspace_id": 60, "position": [21750, -44530, 140], "yaw": 90, "pitch": -3, "hfov": 80 },
  "probes": [
    {
      "name": "sunlit-thatch",
      "world_position": [0, 0, 0],
      "patch_px": 5,
      "stages": { "raw_albedo": null, "lit_pre_fog": null, "hdr_pre_tonemap": [0, 0, 0], "output": [0, 0, 0] },
      "unavailable": { "raw_albedo": "lands with P1a", "lit_pre_fog": "lands with P1c" }
    }
  ]
}
```

A stage is either a value or listed under `unavailable` with its reason; it is never left out.
`viewport` is the size of the pixel grid the patches index. `build_dirty` says whether the build's
worktree had uncommitted changes (`true`, `false`, or `"unknown"` when packaging supplied the commit
without it), so two builds of one commit are told apart.

### Writing stage receipts from a shots run

A shot in a `--shots` file may list `probes`. Each probe has a `name` that is unique within the
shot, a `position` in absolute Creation units, and an optional `patch_px`, the side of the averaged
square: odd, 1 to 31, default 5.

```json
{ "name": "sleeping-giant", "worldspace_id": 60, "position": [21750, -44530, 140],
  "yaw": 90, "pitch": -3, "hfov": 80,
  "probes": [ { "name": "sunlit-thatch", "position": [21900, -44300, 520] } ] }
```

The run writes `<out>/<name>.stages.json` next to the PNG:

- **Stage 3** (`hdr_pre_tonemap`) is the mean RGBA of the main view's `Rgba16Float` texture over the
  patch. It is copied in the same frame as the screenshot and the capture receipt, after bloom and
  before Bevy's `tonemapping` pass, so it includes exposure and fog and excludes the tone curve and
  the cinematic grade. A main texture in any other format fails the shot.
- **Stage 4** (`output`) is the mean RGB byte of the written PNG over the same patch. The patches
  index the main texture, so the PNG must be the size stage 3 sampled (`viewport`); any other size
  fails the shot rather than reading a different region. A camera with a sub-viewport fails too.
- A probe that projects off screen keeps its row, with both stages under `unavailable`. A patch
  holding an infinite or NaN HDR texel keeps its stage 4 value and lists stage 3 as unavailable.
- A stage that would have neither a value nor a reason, or a receipt with fewer samples than
  probes, fails the shot.
- A capture first removes the receipt an earlier run left at its path, even when the shot no
  longer lists probes, so a failed capture never leaves old measurements beside the new image. A
  transition capture's receipt is `<name>-transition.stages.json`, next to its image.
- The shot waits for the stage 3 copy to map, up to the capture timeout, before its `shots.log`
  line is written.

A probe hidden behind nearer geometry samples that geometry. Probe positions are chosen in the
open, and each committed probe is checked against its capture.

### The V1 probe set

One pose, the Sleeping Giant Inn in Riverwood: worldspace 60, position `[21750, -44530, 140]`, yaw
90, pitch -3, HFOV 80, 1280×720. The set is the shots file
`crates/engine/tests/fixtures/visual-profile-v1-shots.json`; a unit test projects each probe and
checks it lands on the pixel listed below. Capture it, then compare two receipts under the
tolerance:

```bash
engine --assets <pack> --worldspace 0x3c --visual-profile <profile> \
  --shots crates/engine/tests/fixtures/visual-profile-v1-shots.json --shots-out <dir>
scripts/compare_stage_receipts.py <reference>.stages.json <candidate>.stages.json
```

The script exits 1 when a stage measured on both sides is out of tolerance, when only one side
measured it, when the poses differ, when a probe's `world_position` or `patch_px` differs (a
different measurement under the same name), or when a stage value lacks its channels: stage 3 needs
RGB or RGBA, stage 4 RGB.

| Probe | Position | Patch | Pixel | Stage 3 (mudcrab) | Stage 4 (mudcrab) |
|---|---|---|---|---|---|
| `sunlit-thatch` | `[23089.6, -44485.0, 301.2]` | 15 | 614, 308 | 0.2586, 0.2123, 0.1375 | 220.8, 197.5, 154.2 |
| `upper-thatch` | `[23329.6, -44849.1, 494.1]` | 15 | 792, 230 | 0.0432, 0.0465, 0.0378 | 93.0, 91.5, 76.0 |
| `shaded-door-wood` | `[23188.0, -44354.3, 79.0]` | 15 | 546, 432 | 0.0215, 0.0351, 0.0390 | 62.1, 76.5, 75.7 |
| `distant-cliff` | `[27658.1, -44818.0, 2405.3]` | 15 | 676, 112 | 0.1226, 0.1459, 0.1399 | 165.7, 169.5, 156.1 |
| `sunlit-road` | `[23020.3, -43800.3, -120.5]` | 15 | 196, 558 | 0.0645, 0.0669, 0.0547 | 114.5, 111.0, 93.4 |
| `shaded-road` | `[22897.6, -45417.2, -153.9]` | 9 | 1238, 598 | 0.0082, 0.0136, 0.0147 | 26.6, 37.2, 36.2 |

How the set was chosen:

- Each patch is the one in its surface's region whose mean moves least when the patch shifts by
  up to 3 px in any direction, measured on the stage 4 image. The worst is `shaded-door-wood` at
  1.6 % (1.1 bytes); the largest absolute move is `sunlit-thatch` at 1.9 bytes. Patches that were
  merely flat did badly here: a 5 px door patch moved 31 %.
- No probe sits on foliage, water or anything animated. Foreground pine needles differ between
  runs of the same build, up to 88 bytes per pixel, though two shots in one run match.
- Each position lies on the camera ray through its pixel. Road and cliff depths are where that ray
  meets the terrain heightmap; inn depths are estimated from the terrain at the porch steps. Depth
  only matters if the other side's camera position differs, since any point on the ray projects to
  the same pixel from the same eye at any HFOV.
- Every patch was checked against the capture it describes.

Repeat-capture noise is zero: two shots of the pose in one run, in each of two runs, gave
bit-identical stage 3 and stage 4 values at every probe, and the V2 reference build at `88b25a2`
gives the same stage 4 values.

V1 tolerance, so the noise floor does not set it: registration does. A probe passes when each
channel is within **2 % relative at stage 3** and **2 bytes at stage 4**, which covers a 3 px
misregistration at every probe in the set. The tolerance is frozen in #358 before any P1 result.
