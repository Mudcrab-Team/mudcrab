# Skyrim lighting runtime

This runtime guide was imported from lighting thread `68505759-f655-46eb-a471-4f079d8efff2` on 2026-10-09 and consolidated with the receiving fog/image-space work. The [lighting and shading hub](lighting-and-shading.md) owns the current integration plan, package identity and launch defaults. Source-thread captures remain historical evidence.

The combined implementation includes lossless authored inputs, field-level interior inheritance, settings provenance, inspected ordinary Lighting response, recovered weather byte transfer and ambient-matrix arithmetic, source-derived distance fog, and optional stable-record HDR/image-space processing. Native light units, final sun scaling, live additive ambient, retail shadow production and complete display parity remain unresolved.

Current converter metadata is **26**, world database schema **9**, and lighting source contract **1**; cell-cache version **3** is unchanged. The refreshed package retains compatible producer-25 native mesh outputs from the source snapshot. Metadata-only rebuilds retain mesh provenance and do not add missing native contracts. Older producer-24 meshes need regeneration for strict native material coverage.

## What runs

| Area | Implemented behavior | Remaining boundary |
| --- | --- | --- |
| Source data | Version-aware WTHR, CELL, LGTM, IMGS, LIGH and relevant REFR codecs; ordered DALC; raw subrecords; winning plugin, form version, flags and link diagnostics; active climate/GMST intervals supplied by the shared environment resolver | The lighting source catalog retains CLMT, REGN and IMAD as raw records; automatic transition/modifier activation remains incomplete |
| Database | Six typed tables and an atomic complete snapshot marker; plugin checksums are published in the same transaction | Sparse annotation invalidates the marker; it cannot produce a complete native snapshot |
| Interior inheritance | Eleven serialized groups, explicit zero/black values, per-field provenance and interior-flag validation | Legacy missing masks and ambient-cube coupling are unresolved |
| Settings | 170 typed keys with file hashes, lines, ordered candidates, requested values and status; three quality controls map to the optional Bevy shadow adapter | Other requests are retained; seven undecoded executable initializers remain absent |
| Ordinary response | Shadow visibility scales direct light; ambient matrix, constant emission and IBL join diffuse before albedo; specular remains separate | Diagnostic inputs are explicit; authored preview uses recovered stages with reported assumptions |
| Normal response | Full RGB tangent normal decode and normal-alpha specular mask; model-space XZY decode without final normalization and a separate red-channel specular mask | Generated Mikk tangents use an explicit Y correction; authored tangent-frame evaluation is pending |
| Local attenuation | `1 - saturate(distance / radius)^2`, for up to seven injected local lights | Enabled-light, room and retail shadow selection are pending |
| Shadow participation | Source-assisted cast/receive flags and native cutout prepass; recovered source cast flags remain effective across material mode changes | Retail binding and specialized-family participation remain gated |
| Output | Diagnostic uses unit exposure and no tone mapping; authored preview joins the shared HDR scene; recovered distance fog and optional stable-record HDR/adaptation/bloom/grading follow their specifications | Native light calibration, AO, animated IMGS/IMAD and retail output transfer remain gated; photographic display is still available |

Skinning, morphs, specialized shader families and unsupported flags are rejected by the native candidate path. Model-space materials also reject reflected, sheared or nonuniform transforms. Fixed resolved texture views accept Remappable_Textures; live texture-set swaps remain unsupported. EnvMap_Light_Fade is accepted only with inactive environment mapping, which remains rejected. Terrain, water, sky and generic glTF materials keep their existing approximate paths. Authored preview preserves unsupported geometry through explicit approximate materials built from StandardMaterial with the recovered fog extension and reports both native and fallback counts. Strict diagnostic rejects unsupported converted primitives. A mixed scene is not a retail acceptance result.

## Visual testing

The [visual testing guide](skyrim-lighting-visual-testing.md) gives the one-command scene launcher, captures and comparison controls.

Run the current combined world with source-prepared lighting:

```sh
./scripts/skyrim-world-lighting-preview.sh
./scripts/skyrim-world-lighting-preview.sh --game-hour 12 --fog --image-space
```

The first command isolates lighting with distance fog disabled. The second selects midday, restores source-derived distance fog and enables recovered stable-record image space. The launcher uses the combined metadata-26/world-9 package and isolated executable; binary/assets/output overrides are documented in the visual guide. Repeated `--ini` options remain available when an explicit profile is needed.

No input JSON is needed. The catalog supplies NAM0 sunlight and the six DALC faces. Without `--weather`, the preview selects `SkyrimClear` by editor ID and reports the effective selection. The recovered byte transfer and affine matrix run with additive ambient RGB assumed zero. World time blending uses active `ResolvedEnvironment` climate/GMST intervals when available. Standalone or unavailable-environment inputs retain the reported shipped-reference fallback. Automatic current/next weather selection remains incomplete. Sun trajectory is a declared circular approximation.

The photographic adapter maps native coefficient 1 to `12000/pi` scene radiance, applies Bevy camera exposure, then shares HDR composition and EV100 9.7 with terrain. It uses TonyMcMapface unless `--image-space` selects the recovered HDR path and disables the camera tone mapper. The approximate directional light uses 12000 lux and the same prepared RGB. PBR fallback ambient uses the cube's constant RGB terms. This is an explicit Bevy calibration; recovered IMGS arithmetic does not establish native light units. Stable IMGS sunlight scale is applied once to authored native sunlight. Main-world preview adds FXAA; the staged scene uses 4×MSAA.

With `--lights`, native surfaces receive at most seven nearby enabled streamed lights, using source RGB/255 times FNAM and the actual rebased positions/ranges. Their visibility is unoccluded; room selection, reference fade offsets and retail local shadows remain pending. Reports list chosen references and rejected numeric inputs. Add `--lighting-inputs` to authored preview to freeze explicit coefficients instead of reading weather.

## Inspect source data

Read the authored lighting snapshot without loading cell-cache or starting a renderer:

```sh
CARGO_TARGET_DIR=/Users/taylor/.local/share/mudcrab-research/lighting-consolidation-20261009/build \
  cargo run --offline --locked --profile quick -p engine --bin world-inspect -- \
  /path/to/assets --lighting --cell 0xCELL_ID --weather 0x81A \
  --output /path/to/lighting-source.json
```

Replace `0xCELL_ID` with a valid interior FormID. `--cell` and `--weather` are optional. Add `--reference 0xREFR_ID` to inspect a placed light's overrides, base LIGH and room links. The output includes table counts, selected records, referenced templates and image spaces, resolved interior groups, missing links and unresolved native preparation. RGB bytes stay raw.

For the existing full world report, append `--lighting` after its positional worldspace and grid coordinates. The source-only form starts with `--lighting` immediately after the assets path.

## Run the diagnostic

Use current metadata-26/world-9 assets with compatible native mesh contracts; the refreshed test pack retains producer-25 native mesh outputs. Then supply known linear coefficients:

```sh
/Users/taylor/.local/share/mudcrab-research/lighting-consolidation-20261009/bin/engine \
  --assets /path/to/assets \
  --lighting-mode native-diagnostic \
  --lighting-inputs crates/engine/tests/fixtures/native-lighting-inputs.json \
  --ini /path/to/Skyrim.ini --ini /path/to/SkyrimPrefs.ini \
  --weather 0x81A --game-hour 12 \
  --lighting-report /path/to/lighting-runtime.json
```

The checked-in input is synthetic. Its ambient rows have constant RGB terms and no directional coefficients. Set `sun_visibility` to zero to test that ambient survives while direct sunlight disappears. Normal alpha changes the direct specular amplitude; it does not multiply ambient or diffuse shading.

Input directions and positions use the engine's global world coordinate frame, in Creation units. The runtime projects local positions into the initial render frame and adjusts them when the render origin moves. The report records that origin and the current effective positions. Ambient rows act on `(world_normal.x, world_normal.y, world_normal.z, 1)`. Colors are already linear effective coefficients, not record RGB bytes or Bevy lux/lumen values. `emission_override: null` uses the provisional authored color-times-multiple projection; an explicit RGB value bypasses it.

Set `use_bevy_directional_shadow` to `true` to sample Bevy's directional shadow map and multiply it by the injected sun visibility. The adapter requires exactly one directional light, aligns it with the injected sun direction and uses its map at index zero. It remains an approximate visibility producer. Local visibility stays injected.

With that adapter enabled, requested `[Display]` values for `iShadowMapResolution`, `fShadowDistance` and `iNumSplits` control the Bevy map size, maximum range and cascade count. The report marks these mappings `approximate` and records their effective values. Adapter limits are power-of-two map sizes from 128 to 4096, distances from 1 to 1,000,000 Creation units, and one to four cascades. These limits describe this implementation, not Skyrim's validation rules. The combined world starts from the copied SE High-profile choice of a 2,048 map and 8,000 Creation units of range. These values are independent of streaming radius and do not establish native cascade/filter behavior. The older 4,096/18,000 fixture is opt-in.

`--weather`/`--fog-weather` and `--game-hour`/`--fog-hour` are synchronized aliases, with the last supplied value winning. Native diagnostic or frozen-input mode keeps its explicit lighting coefficients while weather/hour still select world fog, sky and inspection metadata. In authored preview without an input file, they prepare the selected weather coefficients using the reported world or fallback timing policy. The active exterior cell follows the camera and increments its own epoch on crossings. Interior selection belongs to the view owner, never to the most recently completed streaming request.

Repeated INI files apply in order. The report preserves all candidates and their locations. Native diagnostic and authored preview reject malformed winning values. Approximate mode may retain the last valid request and reports that fallback. Every imported setting has a separate effective value and support status; retained requests do not imply rendering support.

A requested report is written before renderer startup with an `initializing` status, then updated during the run. Report write failures fail the run. A source snapshot error is reported separately so approximate reference streaming can continue.

## Check the production shader

```sh
CARGO_TARGET_DIR=/Users/taylor/.local/share/mudcrab-research/lighting-consolidation-20261009/build \
  cargo run --offline --locked --profile quick -p engine --example native_lighting_probe -- \
  --output /path/to/native-lighting-probe
```

The probe renders the production material and native prepass to a floating-point target, reads it back and compares it with independent scalar expectations. Its 26 cases check visibility versus ambient, actual Bevy shadow occlusion, normal-alpha masking, gloss exponent, albedo/emission ordering, local attenuation, rotated model-space normals, alpha comparison and back-face behavior. Its JSON report identifies each visibility producer and states that retail execution and screenshot parity have not been tested.

The three added world-fog cases, `world_fog_separate_diffuse_clamp`,
`world_fog_separate_diffuse_and_specular_clamps`, and
`world_fog_blend_finishing_once`, passed the production native shader on Metal.
They check independent diffuse/specular clamping and one finishing operation
using a saturated constant varying. They do not verify the perspective fog
distance metric or retail pixel parity.

The [environment research](../../research/skyrim-environment-preparation-20261009.md) records the recovered byte transfer, six-face matrix signs and reference climate transitions. Remaining retail gates include live additive ambient, automatic weather-transition selection, native light units and final sun scaling/trajectory, interior cube precedence, native light selection, shadow production, AO, animated IMGS/IMAD and final display transfer. Recovered stable-record HDR is described in the [image-space specification](image-space.md). The preview does not claim native compatibility.
