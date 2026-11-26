# Near lighting under the `skyrim` profile

Status: implemented, checked by unit and offscreen pixel tests. No retail frame capture confirms
any value, so parity with the game is unverified.

The `skyrim` visual profile shades a pixel with retail's unitless sum from record byte to screen
byte. `mudcrab` keeps Bevy's physical light model.

## Source

The facts come from SPEC-1, the near-lighting specification of the skyrim-research lane: a static
reading of the pinned 1.7.104.0 executable and shader package. It is not yet published. It builds
on these static findings in `skyrim_decomp` pull requests, named here by number: 812, 946, 963,
979, 1003, 1059, 1073, 1077, 1080, 1121, 1245, 1258, 1268, 1272 and 1273.

Section numbers in the code and below are SPEC-1's.

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
| 2.7 | emission, own-emit gate, external emittance | `nif_material/retail.rs`, `emittance.rs` |
| 2.8 | specular values, vertex colour | `nif_material/retail.rs`, `RetailSurface` |
| 3.2 | tangent frame and normals | Bevy's frame; no back-face flip; `terrain_normal.wgsl` (no Z rebuild) |
| 3.3 | vertex fog | `pbr_retail.wgsl`, evaluated per pixel |
| 6.1 | light sum, attenuation, no clamp | vendored `render/pbr_retail.wgsl` |
| 6.2 | specular | `render/pbr_retail.wgsl` |
| 6.3 | alpha test, hair decal | `nif_material/retail.rs`, `shaders/skyrim_skin.wgsl` |
| 7 | output tail, `k`, `B`, `B2` | `render/pbr_retail.wgsl`, object LOD `lod_object.wgsl` |
| 8 | fog property, composite records 53 and 49, clear colour | `sky.rs`, `retail_fog.rs`, `interior::FogFields` |
| 9.2 | model-space normals, face, face tint, hair, landscape | `shaders/skyrim_skin.wgsl`, terrain shaders |
| 10 | image space, adaptation, bloom, tonemap | `retail_hdr*.rs` and `shaders/retail_hdr*.wgsl` |
| 11 | worked examples A to D | `retail_lighting_examples.rs` (CPU), `retail_lighting_gpu_tests.rs` (GPU) |

`retail_lighting.rs` is the CPU twin of the pixel formulas. Grass takes the raw sun (no
`SunlightScale`) and the ambient cube's mean in the same units. Object LOD takes `SunlightScale`
and the same output tail.

SPEC-2 (HDR and output) is not written yet. Its inputs enter at `retail_hdr::drive_retail_hdr`
and `retail_hdr::RetailHdr`.

## Tests

- `retail_lighting_examples.rs`: examples A to D through the engine's weather mix, sun path, image
  space, composite and HDR functions.
- `retail_lighting_gpu_tests.rs` (ignored; offscreen, no window): examples A, B and D as pixels.
  Run with `MUDCRAB_RETAIL_PIXEL=1` and `--ignored retail_pixel --test-threads=1`.
- `retail_hdr_gpu_tests.rs` (ignored): the HDR chain against its CPU reference. This covers the
  adaptation and tonemap of example C. Example C's SAO factor has no engine input.
- `vendored_bevy_patches.rs`: every link of the vendored path.

## Stand-ins and gaps

Each row differs from retail, or rests on an inferred or open item of SPEC-1.

| Stand-in | Issue |
| --- | --- |
| Ambient occlusion factor `A` is 1 in the composite | STAND-IN(#995): no SAO pass |
| Every point light of a cluster adds; retail caps a geometry at 7 | STAND-IN(#999): light list order |
| Vertex fog is evaluated per pixel, with camera planes 15 and 10240 | STAND-IN(#891): vertex fog inputs |
| Composite uses the true axial depth, not `1.01 d - 0.01` | STAND-IN(#891): retail composite depth rescale |
| Shadow masks are Mudcrab's shadow maps; interiors have no sun shadow | STAND-IN(#362): shadow mask production |
| Blended decals draw after the composite with the second-stage tail | STAND-IN(#362): decal stage order |
| Additive term `cb1[8]` is 0; the specular distance fade is 1 | STAND-IN(#362): open run-time inputs |
| Ambient specular is not drawn | STAND-IN(#362): key bit 17 predicate |
| Specular strength and colour are clamped to `[0, 1]` by the converter, and a surface with only flag 41 loses its strength | STAND-IN(#362): converter publishes clamped values |
| Emittance uses the converter's day colour; only `XEMI` references take it; a surface without a source follows own-emit | STAND-IN(#362): emittance source and membership |
| Environment and eye cubemaps are not drawn; eye ambient uses the surface normal | STAND-IN(#490): authored environment and eye cubemaps |
| Parallax, projected UV, soft, rim, back, anisotropic, tree and multilayer variants shade as plain surfaces | STAND-IN(#362): the other Lighting shader variants |
| Landscape specular is not drawn; layer weights stay normalised by their sum | STAND-IN(#547): the LTEX specular exponent per layer |
| Snow rim and snow composite term are not drawn | STAND-IN(#435): retail snow shading terms |
| Terrain LOD takes the plain near sum | STAND-IN(#946): LOD techniques excluded |
| A short `XCLL` without fog power or max takes 1 | STAND-IN(#695): short cell data |
| One exterior weather; no weather transition, flash, IMAD modifier or room image space | STAND-IN(#364): weather and image-space state |
| The light distance dimmer and flicker gate measure from the camera | STAND-IN(#955): the player position as origin |

Water and effect shaders are outside SPEC-1. Water keeps its own program and its decoded record
colours, so its sky reflection does not match the retail sky exactly (unverified how far).
