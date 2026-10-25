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

## Receipts

Every receipt names the profile it describes:

- `shots.log`: `visual_profile` on each capture line, and inside `captured_frame`.
- `SceneState` (live console) and `SceneShot` receipts: `visual_profile`.
- Bug-report snapshots: `configured_rendering.visual_profile`.

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
- **Stage 4** (`output`) is the mean RGB byte of the written PNG over the same patch.
- A probe that projects off screen keeps its row, with both stages under `unavailable`.
- The shot waits for the stage 3 copy to map, up to the capture timeout, before its `shots.log`
  line is written.

A probe hidden behind nearer geometry samples that geometry. Probe positions are chosen in the
open, and each committed probe is checked against its capture.

The probe list, with the world positions measured on the converted pack, and the V1 tolerance
derived from repeat-capture noise are recorded here and in #358 before any P1 result.
