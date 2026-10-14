# Skyrim lighting visual testing

This guide was imported from thread `68505759-f655-46eb-a471-4f079d8efff2` and updated for the combined worktree on 2026-10-09. The [lighting and shading hub](lighting-and-shading.md) owns the current integration plan and package identity. Source-thread captures below retain their original provenance; they are not verification of the combined build.

The preview uses real converted Skyrim meshes and textures with the production native material shader. A composed ruin scene makes stone normals, shaded arches, cutout foliage, and local illumination easy to inspect. Daylight and dusk presets are chosen test lighting. A separate clear-day input uses the recovered Skyrim weather arithmetic.

## Launch the prepared scene

From this worktree:

```sh
./scripts/skyrim-lighting-preview.sh
```

The launcher uses `/Users/taylor/Projects/mudcrab/modern_assets_lighting_test` and the combined private `bin/skyrim_lighting_visual` under `/Users/taylor/.local/share/mudcrab-research/lighting-consolidation-20261009/`. Its output defaults to that private directory's `captures/visual`. Missing executables report an isolated build instruction. `MUDCRAB_PREVIEW_ASSETS`, `MUDCRAB_PREVIEW_BINARY`, and `MUDCRAB_PREVIEW_OUTPUT` override those locations; `MUDCRAB_LIGHTING_ROOT` changes the private binary/output root.

| Control | Action |
| --- | --- |
| Right mouse drag | Orbit the scene |
| Mouse wheel | Move closer or farther away |
| WASD | Pan the view |
| 1 / 2 | Daylight / dusk test lighting |
| M | Switch native preview / approximate materials |
| H | Toggle directional shadows |
| F | Reset the camera |
| P | Save PNG and JSON report |
| Escape | Quit |

The title shows the lighting preset and primitive coverage. Unsupported shader families remain visible through reported approximate material fallbacks. Terrain, the sky, and ember accents are constructed test-scene elements. This scene does not use a retail placement layout or Skyrim image-space processing.

## Reproduce the comparisons

Captures use a fixed camera and wait for asset loading and material binding. Each command saves a 1920×1080 PNG plus the corresponding JSON report and then exits:

```sh
./scripts/skyrim-lighting-preview.sh --capture --variant daylight
./scripts/skyrim-lighting-preview.sh --capture --variant dusk
./scripts/skyrim-lighting-preview.sh --capture --variant daylight --mode approximate
./scripts/skyrim-lighting-preview.sh --capture --variant daylight --shadows off
```

Use different `MUDCRAB_PREVIEW_OUTPUT` directories to retain several runs with the same variant. `--width` and `--height` change the frame size. `--composition` accepts the actor and camera layout serialized in a capture report's `composition` field.

The material comparison holds placement, camera, exposure, and directional shadow settings fixed. StandardMaterial receives the same sun RGB and an isotropic projection of the ambient constants. It does not receive the injected native local lights. The constructed ground/path keeps its native test material in both modes. This compares renderer modes; it does not isolate the material equations under identical effective illumination.

The shadow toggle removes the directional visibility adapter. It should reveal direct illumination on occluded surfaces while retaining ambient and constant emission. The normal maps remain material inputs; normal alpha controls the ordinary direct specular amplitude.

Inspect stone from a grazing angle, the arch underside, tree and fern silhouettes, and shadows around the rocks. Check that changing material mode leaves geometry visible and that a missing texture never passes as a completed capture. Reports list native/fallback coverage, unsupported reasons, actual lighting inputs, camera position, display settings, and visibility producer.

## Use source-derived clear-day coefficients

```sh
MUDCRAB_PREVIEW_OUTPUT=/tmp/mudcrab-lighting-clear-day \
  ./scripts/skyrim-lighting-preview.sh --capture \
  --lighting-inputs crates/engine/tests/fixtures/skyrim-clear-day-inputs.json
```

This input is derived from winning `SkyrimClear` WTHR `0000081A` in the static `Skyrim.esm` + `Update.esm` view, at hour 12. It uses the observed byte/255 transfer and six-face affine arithmetic, with live additive ambient assumed zero. The sun points upward under the preview's circular trajectory approximation. The [environment evidence](../../research/skyrim-environment-preparation-20261009.md) records the source hashes, matrix signs, reference climate timing, and unresolved stages.

These coefficients produce much more ambient fill than the chosen daylight preset. Keep both views: one tests the recovered source preparation; the other makes response and shadow changes easier to see. The staged scene's sky and fog retain their test presentation even when its surface coefficients come from this input.

## Test the streamed world

The prepared Riverwood build has its own launcher:

```sh
./scripts/skyrim-world-lighting-preview.sh
```

It selects clear weather at hour 17, enables local lights and disables distance fog for lighting inspection. It uses the combined private `bin/engine` and metadata-26/world-9 assets, with reports in the private `captures/world` directory. Default shadows use an 8,000-unit range and 2,048 map; the launcher omits the older visual-test INI. `MUDCRAB_WORLD_ASSETS`, `MUDCRAB_WORLD_BINARY`, and `MUDCRAB_WORLD_OUTPUT` override these locations. Extra engine options follow the defaults, so `--game-hour 12` selects midday, `--fog` restores source-derived distance fog, and `--image-space` enables recovered stable-record HDR. Weather/hour aliases are synchronized with the fog controls. The report records `distance_fog_disabled_by_cli`; authored weather coefficients and the sky remain available when distance fog is off.

Use a complete metadata-26/world-9 package with a complete lighting-source snapshot and compatible native mesh contracts. The refreshed test pack retains the source producer-25 native meshes. Metadata refresh cannot upgrade producer-24 meshes that lack those contracts.

```sh
./scripts/skyrim-world-lighting-preview.sh --game-hour 12 --fog --image-space
```

Here the runtime reads weather and lighting records rather than a frozen input file. The camera selects its active cell. The world uses shared weather/hour selection and active climate/GMST intervals; source sunlight, directional ambient, sky colors, and fog metadata update from those controls; up to seven nearby enabled local lights reach native surfaces. The report records effective weather, selected references, numeric rejections, settings mappings, and material coverage.

The retained `lighting-preview-prefs.ini` requests a 4096 map, four cascades, and 18000 Creation units of shadow range. Add `--ini crates/engine/tests/fixtures/lighting-preview-prefs.ini` explicitly to reproduce that source-thread test profile. These visual-test choices do not replace the combined world defaults and are not recovered Skyrim defaults. Other imported shadow settings remain recorded requests until their consumers are implemented.

For repeatable world captures, use [the shots format](reference-shots.md) with measured camera positions. A `shots.log` entry must say the view settled. A timed-out image is useful for diagnosis but does not pass visual readiness.

```sh
./scripts/skyrim-world-lighting-preview.sh \
  --shots crates/engine/tests/fixtures/lighting-riverwood-shots.json \
  --shots-out /tmp/mudcrab-lighting-riverwood
```

The fixture includes a village terrace view, a ground-level street view, and a farmhouse material closeup. It renders into an owned 1920×1080 image without opening a window, so desktop scaling and window visibility do not change or suppress the capture. Add `--game-hour 12` for midday; the launcher defaults to hour 17. Keep those runs in separate output directories when comparing the prepared weather states.

World captures wait for the current camera's GPU dependencies as well as streaming. Visible meshes must be uploaded, materials must have prepared bind groups, color and prepass pipelines must be compiled, and the camera must have a color output attachment. Its active shadow views must also have prepared caster meshes, materials, and pipelines. Unfinished work resets the ten-frame quiet period. Each screenshot logs its dependency snapshot; `MUDCRAB_RENDER_OUTPUT_TRACE=1` also logs counts during loading.

The renderer retains meshes that Bevy's specializers encountered before their GPU uploads finished. They are retried on subsequent frames instead of disappearing until a camera or material change. This applies to interactive views as well as captures.

## Source-thread capture history

The paths and counts in this section belong to the original source worktree
`/Users/taylor/.t3/worktrees/mudcrab/t3-c4129a3c`. They preserve prior implementation
checks; use the current combined binaries/package and new output directories for
new comparisons.

The source thread's complete converted pack at `target/lighting-visual/world-assets` used producer 25/world schema 8: 96,909 typed lighting records from 80 plugins, 76,213 verified primary outputs, and 4,673 verified LOD payloads. Riverwood's 25 requested cells resolved all 322 required models without missing texture dependencies. The source asset checks are recorded in `target/lighting-visual/world-final-asset-gates/summary.json`.

The fog-enabled reference captures are in `target/lighting-visual/captures/riverwood-final-12/` and `riverwood-final-17/`, with the composed dusk scene in `captures/final-stage/`. All six world images settled on Metal with no pending color, prepass, or shadow dependencies. Visual review confirmed continuous terrain, complete pine crowns, readable wood/stone detail, and connected building/foliage shadows at hour 17. `target/lighting-visual/final-capture-verification.json` records image hashes, dimensions, frame counts, dependency snapshots, and executable hashes; `readiness.json` records the overall gates.

The fog-free terrace capture is in `target/lighting-visual/captures/riverwood-no-fog/`, with a repeat in `riverwood-no-fog-repeat/` and a fog-enabled control in `riverwood-fog-control/`. All three settled. The control and fog-free repeat use identical reported lighting inputs; `target/lighting-visual/fog-off-verification.json` records the override and validation. An already running preview needs a relaunch to use the updated executable.

## Rendering boundary

The ordinary material response and weather affine preparation follow the inspected native arithmetic. The visibility producer is Bevy's directional shadow map; local visibility is unoccluded. Photographic output uses EV100 9.7, TonyMcMapface, and a `12000/pi` native radiance scale. The staged scene uses four-sample MSAA, a chosen fog treatment, and restrained bloom.

Current world lighting can consume active climate/GMST intervals. Automatic weather-transition selection, live additive ambient, native light units/final sunlight dimmer and trajectory, native room/light selection, retail shadow filtering, AO, animated IMGS/IMAD and final display transfer remain open evidence gates. The world can enable the [recovered stable-record image-space path](image-space.md); the composed fixture retains its photographic presentation. Specialized shader families and PBR terrain have reported approximate paths. The historical captures verify their source snapshot and visual behavior. Combined-build verification is recorded separately; retail screenshot parity requires matched Skyrim frames and runtime constants.
