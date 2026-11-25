# Visual profiles

Tracks [visual profiles #358](https://github.com/Mudcrab-Team/mudcrab/issues/358). This page covers
[P0 #359](https://github.com/Mudcrab-Team/mudcrab/issues/359), the profile setting, its receipts and
the stage receipt format, and [P1a #360](https://github.com/Mudcrab-Team/mudcrab/issues/360), raw
colour texels. Later packages extend it.

## The setting

A visual profile selects a whole color pipeline: inputs, shading, fog, post-processing and output.

| Profile | Pipeline |
|---|---|
| `mudcrab` (default) | The approved look: Bevy PBR, TonyMcMapface, EV100 9.7 and the snapshot grade. See [color-pipeline.md](color-pipeline.md). |
| `skyrim` | Retail Skyrim SE's color pipeline on raw authored values. Colour textures decode as raw UNORM texels (P1a); the sky and fog come from the active weather record (#891); near-object shading and output still follow the `mudcrab` pipeline until P1c. Distant object LOD lighting uses the retail terms (#946), and the opaque scene is fogged by the retail composite (#891). |

- Select it with `--visual-profile <mudcrab|skyrim>`, or with *Visual profile (restart)* in the
  interactive settings menu, which saves it. An explicit flag wins over the saved value.
- The profile is read once at startup. Changing it requires a restart, as Community Shaders requires
  for Linear Lighting; nothing in the engine rebuilds a running pipeline for another profile.
- Startup logs `visual profile active` with the profile name and the colour-texture decode
  (`sRGB` for `mudcrab`, `raw UNORM` for `skyrim`).

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

## Sky and fog source (#891)

The exterior sky colours, fog colours and fog distances come from one weather record in the world
database, `SkyrimClear` (`WTHR` 0x81A). The engine has no weather simulation, so that record is a
fixed Mudcrab choice; retail picks the weather from the climate and region lists
(`crates/engine/src/weather_sky.rs`).

| | `skyrim` | `mudcrab` + `vivid` | `mudcrab` + `cool-gray` (default) |
|---|---|---|---|
| Sky and fog colours | the record's `NAM0`, mixed by time of day | the same | Mudcrab-only: the approved grey replaces them |
| Fog near and far, power, max | the record's `FNAM`, mixed by time of day | the same | the same, with both distances divided by the density scale 0.25 (Mudcrab-only) |
| Sun glow in the fog | none | Mudcrab-only warm glow | Mudcrab-only warm glow |
| Where the opaque scene is fogged | one full-screen pass after the opaque stage (see below) | each material's shader | each material's shader |

- Retail facts and their tags come from
  [skyrim_decomp#812](https://github.com/Mudcrab-Team/skyrim_decomp/pull/812) (static reading of
  1.7.104.0, independently reviewed). Details and the unverified items are in [sky.md](sky.md#fog).
- **Colours (verified).** The four colour weights (sunrise, day, sunset, night) follow the climate's
  sunrise and sunset times and `fDaytimeColorExtension` (0.5 hours; RF-SUN-005). With `A`, `B`, `C`
  and `D` as in sky.md, the colour goes night to sunrise over the first half of `A` to `B`, sunrise to
  day over the second half, stays day from `B` to `C`, goes day to sunset over the first half of
  `C` to `D`, sunset to night over the second half, and is night elsewhere. Channels mix as 8-bit
  sRGB values and cap at 1.
- **Fog distances (verified).** The record holds one day set and one night set of near, far, power
  and max. One day weight mixes all four (sky.md#fog). The fog formula is a power ramp capped by max
  (verified); how Mudcrab's per-pixel world distance differs from retail's projected distance is
  unverified.
- **Opaque fog is a full-screen pass (verified, #891).** Retail does not fog opaque geometry in the
  Lighting pixel shader. An image-space composite (`ISSAOCompositeSAOFog` family) runs after the
  opaque colour stage and before the blended stage (skyrim_decomp PRs 1061 and 1077, static
  reading of 1.7.104.0, independently reviewed). Per channel, `out = saturate(k * ((1 - Fc) * A * L +
  Fc * Cfc))`, with `Fc = min(saturate(z * r - near * r) ^ p, max)` from the linear view depth,
  `Cfc` the fog colour at `Fc`, `A` the SAO factor and `k = 1 / FramebufferRange` (0.8333 from `output_scale.rs`; in an ordinary exterior frame the live retail value is the initial one, and only the console command `SetFramebufferRange` changes it, skyrim-research NEEDS 24, reviewed, PR pending). The
  sky (depth at the far plane) skips the fog and `k`. `skyrim` draws it in
  `crates/engine/src/retail_fog.rs`. At `Fc = 0` it still scales opaque pixels by `k`, so the
  whole opaque scene is 0.8333 as bright as before (brightness work: #796). The Fog setting
  (retail's `bSAOApplyFog`) turns the pass off, and `k` with it. A record whose fog ramp draws
  nothing keeps the pass with `Fc = 0`, as retail's fallback does. A record that omitted a fog
  field has no fog property: no pass.
  - **Stand-ins and omissions.** `A = 1`, STAND-IN(#995): factor one replaces the retail SAO texture, which Mudcrab has no pass for. The SSR term of `L` is
    omitted: Mudcrab's water reflection is a separate pass that draws after this one. The outer
    `saturate` is omitted: the frame is still HDR and uses the `mudcrab` tone curve until P1c.
    The retail depth rescale `d' = 1.01 * d - 0.01` is not copied: retail's `z` is about 1 percent
    below the true view depth near the camera, and Mudcrab uses the true depth (skyrim-research
    NEEDS 17b, unreviewed; open until a capture confirms it).
  - **Blended draws.** Water, effects and other blended draws fog themselves in retail. In Mudcrab
    they keep Bevy's shader fog (`FogFalloff::PowerRampBlended`, the same ramp). Retail's blended
    draws apply `min(k * M, B)` in the shader (`T = 1`) and Mudcrab's apply the plain mix: a
    follow-up. Grass skips Bevy's fog, but keeps its own `k` (`output_scale.rs`), so a grass
    pixel gets `k` squared at `Fc = 0`. Retail does the same (skyrim-research NEEDS 24, reviewed,
    PR pending). Whether grass is in retail's depth prepass is an open parity point (unverified).
  - **Unchanged.** Interiors keep the cell's own Bevy fog. `mudcrab` keeps Bevy's per-material fog.
- A record that lacks a `NAM0` colour cannot be used. `skyrim` then refuses to start and names the
  colour. `mudcrab` warns and keeps its built-in preset sky. A record that lacks an `FNAM` field
  keeps its colours, draws no exterior fog, and prints a warning that names the field. No
  distance is ever filled in.
- A run without a world database (a fixture) has no weather record and keeps the built-in
  `SkyrimClear` day palette in both profiles.

## Receipts

Every receipt names the profile it describes:

- `shots.log`: `visual_profile` on each capture line, and inside `captured_frame`.
- `SceneState` (live console) and `SceneShot` receipts: `visual_profile`.
- Bug-report snapshots: `configured_rendering.visual_profile`.

The benchmark run fingerprint (`fingerprint.rs`) carries the profile in its graphics section.
`scripts/analyze-streaming-comparison.py` does not check this field when it pairs runs. The operator
must check that both runs use the same profile, unless the comparison tool enforces it.

## Raw colour texels (P1a #360)

`mudcrab` samples every colour texture through an sRGB view: the converter publishes the texture's
bytes under a `.opensky-srgb` alias and Bevy decodes it. `skyrim` evaluates the authored bytes, so it
loads that same alias as the UNORM twin of the format `mudcrab` gets, chosen once at load.

- The choice is `VisualProfile::decodes_colour_srgb`, the single place that answers the question.
  Every `is_srgb` load and every colour-space validator asks it.
- A dedicated asset loader claims only the alias extensions, so `.ktx2` and `.png` files keep Bevy's
  own loader. Under `mudcrab` it registers nothing. Bytes on disk, packs and the converter do not
  change.
- The colour-space validators are the enforcement: under `skyrim` a colour slot that still arrives
  as sRGB (or as UNORM under `mudcrab`) fails its surface instead of drawing with the wrong values.
- Terrain diffuse images and merged terrain arrays follow the same helper. Their files still store
  the sRGB block format; only the load's decode changes.
- The shaders learn the choice too. The skin and effect-palette fragment shaders specialise on a
  `RAW_COLOUR_TEXELS` define: their sRGB re-encode/decode round-trips exist to undo Bevy's sRGB
  sampling, and with raw texels they are skipped. Without it, a raw palette index is encoded again
  and every effect picks an entry that is too bright, and skin, FaceGen and hair tints blend in the
  wrong space.
- Terrain LOD chunks embed their albedo in the GLB, so no asset loader reaches it. A glTF
  extension handler replaces that one image with its UNORM twin while the chunk's GLB is still
  loading (paths under `lod/` only), so it enters `Assets<Image>` already raw and is uploaded once;
  its bytes are colour-space-agnostic. Without it, distant terrain is a brightness step away from
  near terrain. A chunk whose albedo is still sRGB when it becomes ready is retagged then, with a
  warning, at the cost of a second upload.
- Effect palettes are the exception for their own colour, not their index. A palette is published
  at a plain linear path, not under an `.opensky-srgb` alias, and Bevy keys images by path, so both
  profiles sample the palette texture through an sRGB view. `skyrim` still picks the right entry:
  the source texture's grey level arrives raw and the shader no longer re-encodes it. A pack that
  publishes a palette alias would let the palette's own colours be read raw too.

One limit is known and accepted for this package. An image embedded in a mod glTF (`bufferView` or
a `data:` URI) is decoded by `bevy_gltf` itself, which no `AssetLoader` wraps and the engine does
not own, so under `skyrim` it arrives as sRGB and its surface fails validation loudly. NIF
conversion writes external KTX2 URIs, so this reaches mod glTFs only. A texture used through both
its colour alias and its linear twin uploads the same bytes twice; that costs no extra memory over
`mudcrab` and merging the two is a follow-up.

## Stage receipts

A stage receipt records the value at four stages of the pipeline, at a fixed set of probe points.
Retail and Mudcrab captures of the same pose write the same format, so they compare stage by stage.

| # | Stage | Retail source | Mudcrab source |
|---|---|---|---|
| 1 | raw sampled albedo | shader-replacement probe on the lighting draws | `material_texel_probe` (format, no scene); a scene probe lands with P1c (#362) |
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
      "unavailable": { "raw_albedo": "lands with P1c (#362); format proven by material_texel_probe", "lit_pre_fog": "lands with P1c" }
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
  --game-hour 12 --shots crates/engine/tests/fixtures/visual-profile-v1-shots.json --shots-out <dir>
scripts/compare_stage_receipts.py <reference>.stages.json <candidate>.stages.json
```

`--game-hour 12` pins the sun for the lighting stages: the default hour is the retail `GameHour`
record (GLOB 0x38, 8.0), and the native Riverwood reference captures use noon. Both receipts must use the same hour.

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
channel is within **2 % relative at stage 3** and within **2 bytes at stage 4**. Stage 3 (`stage3_ok`)
accepts a per-channel error of at most `max(0.02 * abs(reference), 1/255)`. This covers a 3 px
misregistration at every probe in the set. The tolerance is frozen in #358 before any P1 result.
