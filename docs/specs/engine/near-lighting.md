# Lighting and output under the `skyrim` profile

Status: implemented, checked by unit and offscreen pixel tests. No retail frame capture confirms
any value, so parity with the game is unverified.

The `skyrim` visual profile shades a pixel with retail's unitless sum from record byte to screen
byte. `mudcrab` keeps Bevy's physical light model.

## Source

The facts come from two specifications of the skyrim-research lane, static readings of the pinned
1.7.104.0 executable and shader package: SPEC-1 (near lighting, `skyrim_decomp` pull request 1274,
with its amendment from SPEC-2) and SPEC-2 (from the lit scene to the displayed byte). Neither is
reviewed for transfer. They build on these static findings in `skyrim_decomp` pull requests, named
here by number: 812, 946, 963, 979, 1003, 1052, 1059, 1073, 1077, 1080, 1121, 1245, 1258, 1268,
1272 and 1273.

Section numbers in the code and below are SPEC-1's unless a row says SPEC-2. Where the two differ,
SPEC-2 wins for the frame after the first stage.

## Flicker guards

- Every pass that swaps the post-process texture has one order (`post_process_order.rs`). Two
  unordered passes showed the untoned scene in single frames.
- The chain shaders read a NaN as 0 and an infinity as the largest 16-bit float, so that one bad
  scene pixel cannot reach the adapted pair. A finite value does not change. This guard is
  Mudcrab's own because retail's behaviour on such a value is unverified.
- A `--shots` pose with a `sequence` fails on a solid or flashing frame
  ([reference-shots.md](reference-shots.md)).

## Clear colour

The frame clear colour is the fog far colour times the literal 0.83332998, not `k` (SPEC-4
section 10, which corrects SPEC-1 section 8.1; unverified in game).

## Units

Retail has no light unit (unverified in game; see [Source](#source)). A colour byte of a light, of the ambient, of the fog or of a vertex is
`byte / 255`, with no sRGB decode. The engine writes those numbers into Bevy as follows, so that
Bevy's own shaders (water, effects) share one scale:

| Light | Bevy value | The retail shader reads |
| --- | --- | --- |
| sun, interior key | linear colour, illuminance `pi x SunlightScale` | `colour x illuminance / pi` |
| point | linear colour, intensity `4 pi^2 x fade x dimmer` | `colour x intensity / (4 pi^2)` |
| ambient | the raw `DALC` cube, brightness 1 | the cube |

The camera exposure is exactly 1. The HDR chain has no input scale.

## Where each section lives

| SPEC-1 | Rule | Code |
| --- | --- | --- |
| 2.1 | bytes / 255, no decode | `retail_lighting::raw_colour`, `sky.rs`, `interior.rs`, `lights.rs` |
| 2.2 | day-part weights | `weather_sky::slot_weights` (existing, #1002) |
| 2.3 | sun and interior key light | `sky::drive_exterior_lighting`, `interior::apply_interior_lighting`, `sun.rs` |
| 2.4 | `SunlightScale`, `SkyScale` | `retail_hdr::RetailImageSpace` (one block per scene), `shaders/sky.wgsl` |
| 2.5 | directional ambient | vendored `pbr_ambient.wgsl`, `DirectionalAmbientCube` (#1013) |
| 2.6 | point lights, flicker, pulse, emittance | `LightEvaluation::point_light_in`, `light_dynamics.rs` |
| 2.7 | emission, own-emit gate, external emittance by day part | `nif_material/retail.rs`, `emittance.rs` |
| 2.8 | specular values, vertex colour | `nif_material/retail.rs`, `RetailSurface` |
| 3.2 | tangent frame and normals | Bevy's frame; no back-face flip; `terrain_normal.wgsl` (no Z rebuild) |
| 3.3 | vertex fog | `pbr_retail.wgsl`, evaluated per pixel |
| 6.1 | light sum, attenuation, no clamp | vendored `render/pbr_retail.wgsl` |
| 6.2 | specular | `render/pbr_retail.wgsl` |
| 6.3 | alpha test, hair decal | `nif_material/retail.rs`, `shaders/skyrim_skin.wgsl` |
| 7 | output tail, `k`, `B`, `B2` | `render/pbr_retail.wgsl`, object LOD `lod_object.wgsl` |
| 8 | fog property, composite records 53 and 49, clear colour (corrected by SPEC-4, below) | `sky.rs`, `retail_fog.rs`, `interior::FogFields` |
| 9.2 | model-space normals, face, face tint, hair, landscape | `shaders/skyrim_skin.wgsl`, terrain shaders |
| 8.1, 9.2, 9.3 | the pass of a surface: unblended lit surfaces and decals before the composite, blended surfaces after it | `NifDepthState::retail_first_stage`, `nif_material/retail.rs`, vendored `extended_material.rs`; table `nif_material/pass_audit.rs` |
| 10 | image space, adaptation, bloom, tonemap | `retail_hdr*.rs` and `shaders/retail_hdr*.wgsl` |
| 11 | worked examples A to D | `retail_lighting_examples.rs` (CPU), `retail_lighting_gpu_tests.rs` (GPU) |

`retail_lighting.rs` is the CPU twin of the pixel formulas. Grass takes the raw sun (no
`SunlightScale`) and the ambient cube's mean in the same units. Object LOD takes `SunlightScale`
and the same output tail.

## SPEC-2: from the lit scene to the byte

| SPEC-2 | Rule | Code |
| --- | --- | --- |
| 1 | frame order: first stage, composite, saturate, blended draws, HDR, output | `retail_fog.rs`, `retail_hdr_render.rs` |
| 2.1, 2.2 | the image-space block, the base-block rules, a missing weather or form takes `0x167` | `retail_hdr::select_block`, `HdrBlock` |
| 2.3 | copy and normaliser | `retail_image_space.rs` applies sampled modifiers and accumulates tint and fade |
| 3 | composite, records 53, 51 and 49; its unsigned input | `retail_fog.rs`, `shaders/retail_fog.wgsl` |
| 4.1, 4.2 | blend modes of sorted draws | `nif_depth.rs` selects pipeline blend state and keeps mask discard independent of that state |
| 4.3 | draws after the composite add above 1 | blended draws run after the saturate |
| 5.1 | targets | `Rgba16Float` by default (Ultra.ini). The composite input copy uses `Rg11b10Ufloat` where supported |
| 5.2 to 5.4 | downsample, luminance, adaptation, bloom | `retail_hdr*.rs`, `shaders/retail_hdr*.wgsl` |
| 6 | tonemap, cinematic terms, fade | `RetailHdr`, `shaders/retail_hdr_tonemap.wgsl` |
| 7 | output and gamma | the tonemap writes an 8-bit display texture and the present pass copies it to the view. Temporal anti-aliasing is the identity step |
| 10 | settings | `skyrim_ini.rs` (`[Display]` and `[General]` HDR keys). Defaults follow Ultra.ini, Skyrim_Default.ini, then the compiled values |
| 11 | examples E0 to E4 | `retail_output_examples.rs`, `nif_depth.rs` tests |

## Tests

- `retail_lighting_examples.rs`: examples A to D through the engine's weather mix, sun path, image
  space, composite and HDR functions.
- `retail_lighting_gpu_tests.rs` (ignored; offscreen, no window): examples A, B and D as pixels.
  Run with `MUDCRAB_RETAIL_PIXEL=1` and `--ignored retail_pixel --test-threads=1`.
- `retail_hdr_gpu_tests.rs` (ignored): the HDR chain against its CPU reference. This covers the
  adaptation and tonemap of example C. Example C's SAO factor has no engine input.
- `retail_output_examples.rs`: SPEC-2 examples E0 to E3. Example E4 is a test in `nif_depth.rs`.
- `vendored_bevy_patches.rs`: every link of the vendored path.

## Stand-ins and gaps

Each row differs from retail, or rests on an inferred or open item of SPEC-1. How far each one
changes the picture is unverified.

| Stand-in | Issue |
| --- | --- |
| Ambient occlusion factor `A` is 1 in the composite | STAND-IN(#995): no SAO pass |
| Every point light of a cluster adds; retail caps a geometry at 7 | STAND-IN(#999): light list order |
| Vertex fog is evaluated per pixel, with camera planes 15 and 10240 | STAND-IN(#891): vertex fog inputs |
| The composite uses the nearer of copied prepass and colour-pass depth for surfaces such as merged terrain. The depth remap and sky cutoff use the active projection planes | STAND-IN(#362): prepass coverage and camera planes |
| Shadow masks are Mudcrab's shadow maps; interiors have no sun shadow | STAND-IN(#362): shadow mask production |
| Blended decals without alpha test draw after the composite with the second-stage tail ([Pass rules](#pass-rules)) | STAND-IN(#362): decal stage order |
| Additive term `cb1[8]` is 0; the specular distance fade is 1 | STAND-IN(#362): open run-time inputs |
| Ambient specular is not drawn | STAND-IN(#362): key bit 17 predicate |
| Specular strength and colour are clamped to `[0, 1]` by the converter, and a surface with only flag 41 loses its strength | STAND-IN(#362): converter publishes clamped values |
| Only `XEMI` references take an emittance source, mixed by day part from the converter's table; a surface without a source follows own-emit | STAND-IN(#362): emittance source and membership |
| Environment and eye cubemaps are not drawn; eye ambient uses the surface normal | STAND-IN(#490): authored environment and eye cubemaps |
| Parallax, projected UV, soft, rim, back, anisotropic, tree and multilayer variants shade as plain surfaces | STAND-IN(#362): the other Lighting shader variants |
| Landscape specular is not drawn; layer weights stay normalised by their sum | STAND-IN(#547): the LTEX specular exponent per layer |
| Snow rim and snow composite term are not drawn | STAND-IN(#435): retail snow shading terms |
| Terrain LOD takes the plain near sum | STAND-IN(#946): LOD techniques excluded |
| A short `XCLL` without fog power or max takes 1 | STAND-IN(#695): short cell data |
| The renderer accepts previous-weather and modifier inputs, but the game has no live producers for them. Room and cell blocks cross-fade. SPEC-2 leaves room-clock reset order open | STAND-IN(#364): weather and image-space state |
| Temporal anti-aliasing is not run: the tonemap writes the output | STAND-IN(#361): the identity step of SPEC-2 |
| Depth of field, radial and full-screen blur, double vision, refraction, volumetric lighting, lens flares and screen-space reflection are not drawn | STAND-IN(#364): the other post effects |
| `ClearAdaptedLight` clears the adapted pair and the pause timer uses the menu value. Separate retail sky and menu reset triggers have no producer | STAND-IN(#361): the adaptation reset triggers |
| Multisampled views and adapters without packed render targets clamp the composite input without storing it at 11/10-bit precision | STAND-IN(#361): the composite input format |
| The optional packed HDR chain can stall or bias upward eye adaptation on Metal. A positive step below the next representable value can truncate back to the previous value. Ultra uses the wide chain by default | SPEC-2 §5.3: retail rounding is unverified |
| HDR-off copies into Mudcrab’s display target. Retail copy encoding is open | SPEC-2 §13 P11 |
| A blend pair outside retail's table draws as a plain alpha blend | STAND-IN(#362): inherited blend modes |
| The light distance dimmer and flicker gate measure from the camera | STAND-IN(#955): the player position as origin |
| A palette effect surface does not take its reference's emittance colour; an additive effect decal takes it after the composite, unfogged | STAND-IN(#984): the effect shader's emittance, tint and fog |

## Pass rules

`nif_material/pass_audit.rs` holds the retail pass of a material as a table. A test compares it with
the loader over every flag combination, and an ignored test compares it over a converted pack. The
open rules of that table, then the rules it gives that the engine does not follow:

| Open rule |
| --- |
| STAND-IN(#362): water, sky and other shader families keep their own passes |
| STAND-IN(#946): LOD techniques are outside the near rules |
| STAND-IN(#362): a refraction surface has no Lighting pass and draws as a surface |
| STAND-IN(#362): the pass of a surface without a depth test is not specified |
| STAND-IN(#362): the effect shader's pass is specified only for a plain blended surface |
| STAND-IN(#362): depth write of an unblended surface follows its flag |
| STAND-IN(#362): a sorted blended draw writes no depth |
| STAND-IN(#362): a blend pair outside the table draws as a plain alpha blend |
| STAND-IN(#362): the two-sided flag does not change the pass |
| STAND-IN(#362): a decal with property alpha below one and no blend is not specified |
| STAND-IN(#362): the alpha cut of a flag-41 decal depends on the unknown prepass |
| STAND-IN(#362): a decal stays out of the depth prepass |
| STAND-IN(#362): a blended decal draws after the composite and applies it in its shader |
| STAND-IN(#362): a blended surface with an alpha test and depth write draws as a cutout |
| STAND-IN(#362): a blended surface with an alpha test and no depth write is not cut |

Water and effect shaders are outside SPEC-1. Water keeps its own program and its decoded record
colours, so its sky reflection does not match the retail sky exactly (unverified how far).
