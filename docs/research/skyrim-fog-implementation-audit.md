# Skyrim fog implementation audit

This implementation follows the recovered record inputs and distance-fog arithmetic in the
[design spec](../specs/engine/distance-fog.md). Exact visual parity still requires retail frame
captures. This audit records the appearance differences that a curve test cannot settle.

## Authored inputs now available

The environment catalog preserves winning record IDs, plugin priorities, raw float bits,
weather color keys, climate timing, room inheritance, image-space links and volumetric links.
Database schema 8 and converter schema 25 refresh the converted environment projections.
Older databases remain readable and explicitly report missing environment projections.

`IMGS` now exports optional modern HDR, cinematic, tint and depth-of-field blocks. Legacy
`ENAM` stays separate: it contains no authored modern White or Eye Adapt Strength field.
The copied package contains 302 modern and two legacy image spaces. Of their depth blocks,
285 contain 16 bytes, two legacy blocks contain only 12 bytes, and 17 are absent. Missing
depth flags, sky/blur words and modern blocks remain absent in the typed projection.

The layouts agree with [pinned xEdit definitions](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas)
and [Mutagen's IMGS definition](https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/ImageSpace.xml).
`HNAM` holds nine floats; `CNAM` holds saturation, brightness and contrast; `TNAM` holds
tint amount and float RGB; `DNAM` holds three floats and, when present, two packed words.

All 42 copied `VOLI` records provide twelve separate float fields for intensity, custom color,
density, phase function and sampling. Their RGB fields are authored floats; the exporter
preserves them without normalization or clamps. [Mutagen's VOLI definition](https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/VolumetricLighting.xml)
corroborates the tags. `CELL.XCIM` resolves an interior image space. Weather render inputs
return the four authored sunrise/day/sunset/night targets without assuming that native
image-space blending uses the fog-color weights.

## Remaining appearance differences

| Area | Current limitation | Evidence needed |
|---|---|---|
| HDR and exposure | Bevy PBR output is exposure-scaled; authored fog byte RGB is not. | Retail RTV/SRV formats and transfer flags, framebuffer scale, exposure and active IMGS constants. |
| Source lighting | Bevy PBR supplies the lit surface color; native Lighting diffuse/specular clamps remain a source-shading approximation. | Lighting descriptor, intermediate diffuse/specular values and packed clamp controls. |
| Terrain LOD | Compiled terrain chunks use generic glTF materials and opaque depth fog, without the full-terrain/NIF clamp proxy. Native Lighting LOD depth bias is not assigned to these generated meshes. | Bound native landscape descriptor and corresponding LOD geometry/material inputs. |
| Effect routing | Raw NiAlpha factors survive conversion, but their mapping to shader descriptors is a semantic approximation. Framebuffer alpha uses Bevy's OVER behavior. | Native descriptor selection and independent RGB/alpha blend states. |
| Sky | The current analytic dome differs from retail Sky technique 8's authored vertex-color blend and screen noise. | Sky mesh channels/topology, native color uploads, framebuffer scale and noise bindings. |
| Multiple views | Shared geometry material uniforms use the main world's far plane. Reflection copies the world projection; other cameras need independent constants. | Per-view projection, fog uploads and view ownership. |
| Interior Fog Clip | Inherited CELL Fog Clip updates camera far metadata and fog reconstruction, but ordinary mesh rendering has no finite far clipping or far culling. | Native projection/clipping constants and geometry immediately inside, across and beyond the clip plane. |
| Pass ownership | Separate first-person rendering, sky-tagged Effect, grass/tree/particles and alternate Water paths still need parity captures. | Draw order, depth writes and fog ownership for each route. |
| Volumetrics | Outdoor scattering is separate from the distance-fog curve. | VOLI blending, solver constants, temporal state and volume pass order. |
| Environment selection | Region overlap/falloff and GLOB-conditioned weather choices are retained but not fully resolved. | Native region selection and weather scheduler state. |

The inherited interior clip distance currently caps `PerspectiveProjection.far` and supplies
the finite forward projection used by fog calculations. It does not establish a finite raster
clip plane: Bevy 0.19's perspective matrix remains infinite reverse-Z, and the common fog vertex
shader uses the reconstructed finite clip coordinates for its fog metric rather than raster
position. The recovered Water depth offset is separate from a far clip plane.

Bevy's `CameraProjection::compute_frustum` does construct a finite far plane from this metadata.
Its ordinary CPU visibility checks nevertheless call `intersects_sphere` and `intersects_obb`
with far-plane testing disabled. GPU `mesh_preprocess.wgsl` tests only the first five frustum
planes, also excluding far. Consequently, ordinary terrain and NIF geometry can remain visible
beyond CELL Fog Clip, even when its entire bounds lie beyond that distance. Streaming radius,
occlusion and render layers can hide such geometry, but do not enforce this clipping boundary.
The Water reflection activation gate separately tests the water bounds against camera far;
that decides whether to render a reflection, rather than clipping its scene draws. Finite
clipping and compatible depth reconstruction remain an implementation gap requiring explicit
boundary tests before claiming native Fog Clip parity.

Retail Sky technique 8 blends authored mesh vertex color channels into three uploaded RGB
colors, applies framebuffer scale, and adds screen noise with amplitude `0.03125` and offset
`-0.0078125`, plus a scalar. Its shader contains no adjacent gamma decode or clamp. Matching
the atmosphere colors alone cannot establish a matching horizon.

The four biased Lighting vertex programs have descriptor subtypes `9` and `18`, historically
identified as LOD landscape and LOD landscape noise. Object LOD subtypes `13` and `15` are
distinct. The generated terrain chunks preserve source LAND cells, but contain no native draw
descriptor; their LOD tier or material name cannot establish which native program would bind.
Ordinary Water receives its independently recovered depth bias. Lighting LOD needs routing
evidence before the same operation can be enabled.

Seven Effect vertex programs carry the SkyObject descriptor bit and retain vertex fog.
The inspected representative uses `length(clipX, clipY, clipW)` and outputs depth at the sky
plane. Another 264 Effect vertex programs carry MotionVectorsNormals and omit fog. Raw NIF
shader flags do not identify either bound descriptor. These special routes remain separate
from the current ordinary Effect proxies and the atmosphere dome's clear-depth bypass.

## Retail capture checklist

A read-only probe on 2026-10-08 found the installed Steam app 489830 on `fiji-desktop`, a valid
GE-Proton11-5 mapping, an active Hyprland/Xwayland Steam session, Radeon Vulkan drivers and
36 saved games. Skyrim was stopped. Startup and frame capture remain unverified. RenderDoc
commands, UI and Vulkan layer were absent from the checked paths.

The located user preferences specify 2560×1440, gamma `1`, SAO enabled, outdoor volumetric
lighting enabled at quality `1`, 64-bit HDR targets disabled, and PostLit/PostSpec clamps `1`.
The located Skyrim.ini disables volumetrics indoors. These are profile-file observations;
effective runtime values still need capture. Plugins.txt contains only comments, while
Skyrim.ccc supplies the implicit Creation Club list.

1. Verify a runnable session through the existing Steam/GE-Proton setup and pin the executable,
   effective load order, profile hashes and active Creation Club entries.
2. Prepare GPU capture without changing the measured graphics settings. Confirm that the
   capture contains Skyrim draws through DXVK and records render target formats and bindings.
3. Freeze game hour, current/previous weather, transition state, camera pose and temporal
   effects. Record every relevant INI/GMST, IMGS and VOLI value actually used.
4. Capture opaque fog inputs/output first, then material fog, sky, Water, first-person and
   reflection routes. Include center/off-axis geometry, near/far boundaries, interiors,
   alpha effects and distant LOD at the poses specified in the design spec.
5. Compare intermediate fog constants and linear/HDR outputs before final screenshots.
   Measure final pixels only after the transfer functions, exposure and postprocessing match.
6. Retain raw retail captures privately. Publish interpreted measurements, hashes and
   deviations alongside the implementation tests.

The static record and numerical fixtures verify the exported inputs and recovered equations.
They do not substitute for these GPU and visual comparisons.

## Blue cast in the clear-weather test

The user reported excessive blue haze on 2026-10-09 and confirmed vanilla Skyrim SE/AE as the
visual reference. The actual diagnostic report selected `SkyrimClear` (`0x81A`) at hour `12`,
with Near/Far `0 / 80000`, Power `0.4`, Max `0.85` and framebuffer scale `0.8333333134651184`.
Its normalized near/far colors match the authored byte RGB `(35,98,124) / (116,168,203)`.
`WeatherPineForest`, which covers the Riverwood starting cell, includes this clear preset.
The selected weather and exported fog fields therefore do not establish an incorrect weather
selection.

The linked daytime image space is `ISSkyrimClearDAY` (`0x12F88`). It authors tint amount `0.42`,
tint RGB approximately `(0.894118,0.839216,0.772549)`, saturation `1.5`, brightness `1.135` and
contrast `1.4`. Its HDR fields include sunlight scale `2.8`, sky scale `0.05`, white `1`, eye
adaptation speed `45` and strength `5`. The `--image-space` test path now applies the recovered HDR, bloom, saturation, tint,
brightness and contrast operations. The tint target favors red/green over blue, but its
luminance-based blend is not a warm RGB multiplier.
`ISSkyrimFogDAY` (`0xC820C`) instead authors tint amount `0.725` and RGB approximately
`(0.658824,0.756863,0.729412)`, whose green channel exceeds blue.

The shipped pixel programs identified as `ISHDRTonemapBlendCinematic` and
`ISHDRTonemapBlendCinematicFade` recover the grading arithmetic. After saturation, RGB blends
toward luminance multiplied by tint RGB: `H = D + tintWeight * (Y * tintRGB - D)`.
Brightness and contrast follow, with contrast centered on a sampled adaptation value; RGB
then clamps and receives the gamma exponent. The Fade program adds a final RGBA interpolation
after gamma. These are register-level findings. The family labels come from the shader
manifest's historical ordering. A warm RGB multiplier would not implement the recovered tint
equation.

A follow-up native trace confirms that the HDR updater packs live `ImageSpaceManager` data
into CPU `pixelConstantGroup[2]` as `(saturation + boost, 0, contrast + boost, brightness + boost)`
and `[3]` as `(tintRed, tintGreen, tintBlue, tintAmount)`. The observed tint copy performs no
normalization. The cinematic additions include `fGlobalSaturationBoost:Display`,
`fGlobalBrightnessBoost:Display` and `fGlobalContrastBoost:Display`; ordinary executable defaults
are zero, while map mode selects separate brightness/contrast boosts. Effective overrides and
the mode selector remain uncaptured. The setter writes at `index * 16` with no prefix. The shader loader reads a 64-byte metadata
table, and the draw setup uses its float offsets to map CPU entries `[2]/[3]` to shader
`cb2[3]/[4]`. The native graph identifies the bloom, scene and adaptation texture slots,
reduction dimensions, temporal update, target formats and bloom weight tables. The
[image-space spec](../specs/engine/image-space.md) records those findings and the implementation.
The producer of the manager's weather-blended/modifier-adjusted data remains unresolved;
only stable authored time keys and interior XCIM are enabled.

The opt-in path also resolves authored weather sunlight and ambient colors. It retains Bevy's
`12000`-lux sun, ambient brightness `160` and EV100 `9.7` as source-lighting proxies. The
selected HNAM Sunlight Scale multiplies directional RGB, following the native Lighting
consumer; this establishes the multiplier, not a native-to-lux conversion. Its
recovered pass replaces `TonyMcMapface` on the world camera and excludes the reflection view.
An explicit sRGB display bridge is used; the retail swapchain transfer remains uncaptured.
These source-lighting and transfer limits prevent an arithmetic match from establishing a
matching final image.

Inspection found no duplicate atmospheric blend in the ordinary opaque path: stock fog is
disabled, opaque NIF Lighting applies its clamp proxy before the single depth-fog pass, and
transparent routes use geometry fog. This does not settle every special draw route.
The next comparison must separate pre-fog RGB, post-fog HDR RGB and displayed RGB at a fixed
pose, paired with retail SE/AE outputs and active image-space constants. Compare the recovered
image-space path and weather colors before changing authored fog hue or power.

## Implementation validation

Original fixtures passed on Apple M1 Pro with Metal. The fog integration fixture checks nine
regions covering opaque fog, transparent phase order, alpha, clear-depth bypass, split viewports
and production terrain/water shaders; maximum sampled channel error is one RGBA8 code value.
The NIF material fixture checks ten material routes plus source and clear controls; all 108
sampled RGBA pixels pass, with maximum channel error of one RGBA8 code value. These are
tests of the documented engine proxies, not comparisons against captured retail draws.

Normal-map and specular converter/loader GPU regressions match their independent material
references, with zero sampled RGB error. The legacy specular negative control fails as expected.
All 357 engine library tests, 26 shared tests and 116 launcher tests pass. Converter material
tests cover 24 cases, and environment decoder/export tests cover ten cases. The analytical
design checker passes 66 cases. Workspace all-target compilation, strict Clippy, formatting
and whitespace checks pass.

The converter's 25 LOD, metadata-migration and snapshot integration tests pass. Its broader
library run has 384 passes, eleven existing ignores and one failure in the unchanged
case-collision filesystem fixture. That fixture requires two differently cased filenames on a
case-sensitive filesystem; this Mac resolves them to the same file. The failure is outside the
fog paths and remains recorded rather than counted as a passing suite.

The package audit decodes 74,057 CELL records and all relevant environment record families
from the copied 80-plugin candidate order. It verifies 1,019 selected serialized database
projections and passes 11,079 comparisons against the independent scan, public reference
inputs, identity remapping and IMGS/VOLI field offsets. This validates package inputs; the
effective retail load order remains part of the capture checklist.

## Exterior shadow correction

The world sun now uses the authored `fShadowDistance` and `iShadowMapResolution` settings,
with copied SE High defaults of `8000` Creation units and `2048` pixels. Its former range
grew with streaming radius and camera height, reaching about `23200` at the default grid.
NIF materials now preserve the shader flag controlling shadow casting through scene
instantiation. In the audited mountain set, 47 of 53 materials have that flag clear; the six
flagged casters retain their shadows. The [source audit](skyrim-exterior-shadow-audit.md)
records the scope and remaining LAND/LOD questions. Pale mountain faces still require a
separate material and source-lighting comparison.

## Mountain material selection

A separate source audit found ignored `STAT.MODS` alternate texture assignments on 21 of
63 nearby mountain references. Eight dirt/grass/rock trim instances consequently retain the
NIF's snow textures. Forty-four references also select directional snow material objects,
whose native projection path remains unsupported. Major cliff materials bind real slab
diffuse and normal textures, and decoded slab samples are dark gray rather than white.
Alternate texture selection is a demonstrated material gap; matching the screenshot's pale
faces still requires primitive ownership and source-lighting evidence.

## Image-space implementation validation

The recovered scalar pass passes 12 original Metal GPU cases with maximum float error
`2.3841858e-7`. Seven full-graph cases pass ten sampled regions in both RGBA16Float and native
packed HDR modes, with maximum RGBA8 error zero and one respectively. All 375 engine library
tests, 27 shared tests, 11 environment converter tests and 12 metadata rebuild integration
tests pass. Native-format availability is required by the packed probe.

The production Riverwood capture completes with 25 resident cells, 2029 ready assets and zero
asset, material or renderer validation failures. It confirms active Clear day IMGS, measured
adaptation, bloom and source light/sky inputs. Visual acceptance remains failed: highlights
are overbright and shadows remain too dark with the current Bevy source-light strengths and
exposure. The experimental pass is opt-in; no vanilla appearance result is claimed.
