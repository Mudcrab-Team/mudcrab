# Skyrim shadow intensity and normal-map lighting

Investigated 2026-10-08 with the toolchain and saved analysis from the
`t3/explore-rea-skyrim` worktree. The target is **Skyrim SE 1.7.104.0**.
No game, user INI, or engine rendering behavior was changed.

## Findings

Ordinary lighting uses the shadow mask to attenuate direct light. Ambient light,
emission, and an image-based lighting term are added separately. A completely
occluded light therefore need not make the surface black. The ratio of direct
light to these other contributions determines much of its apparent shadow depth.
This was checked against the copied retail lighting shader, rather than inferred
only from Community Shaders.

Grass has an additional control: **`Skyrim.ini [Display] fShadowClampValue`**.
Its compiled initializer is `0.3`. The inspected retail grass shader remaps
visibility `S` to `S + (1 - S) * clamp`, leaving a 30% direct-light multiplier
when `S = 0`. This is a grass lighting rule; it is not a global shadow-opacity
setting for ordinary materials.

Normal-map RGB supplies the shading direction. In the inspected ordinary
lighting permutation, normal-map alpha scales specular highlights. It does not
contain cast-shadow opacity. Normal-based diffuse shading, cast-shadow
visibility, and screen-space ambient occlusion are separate mechanisms.

The native executable reads both `fShadowBiasScale` and
`fShadowDirectionalBiasScale`. An older guide's claim that the former is
universally unused in SE does not hold for this target. Bias changes depth
comparison behavior; it is not a general darkness multiplier.

The [settings catalog](skyrim-shadow-settings-1.7.104.csv) records names,
sections, owning INI collections, compiled initializers, preferred addresses,
and which consumers were checked. The [evidence index](skyrim-shadow-evidence-20261008.json)
contains hashes and verification metadata without proprietary bytes.

## Target and verification boundary

| Item | Observed value |
| --- | --- |
| Executable | `SkyrimSE.exe`, AMD64 PE32+ |
| File/product version | `1.7.104.0` |
| SHA-256 | `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f` |
| Preferred image base | `0x140000000` |
| Toolchain | REA 6.0.0, Node 24.21.0, Ghidra 12.1.4, Temurin JDK 21.0.12.1+1 |
| Native decoder cross-check | LLVM 21.1.8 |
| Shader decoder | d3dasm `a292206a15daf0723d945a475c6254413eda8551` |

The portable [d3dasm decoder][shader-decoder] was built privately with a small
read-only disassembly wrapper. The [FXP reader reference][fxp-reader] supplies
the package layout; the selected input bytes were checked against that layout.

The REA provider-scoped doctor passed its nine checks again. The earlier REA
worktree had already established that the default REA Skyrim cold import exceeds
its 330-second deadline. This investigation reused that setup's **saved Ghidra
project**, through read-only headless queries on an independent APFS clone.
It did not obtain Skyrim results through a new REA MCP import.

The original whole-program Ghidra analysis is partial. Missing recovered
references cannot establish that a setting or function has no consumer.
The three native queries supplied 1,394 unique decoded instructions whose
bytes match the PE; 1,379 were independently compared with LLVM from recovered
function entries, with no boundary/encoding mismatch. Fifteen surrounding
instructions fell before those entries and were not included in that comparison.

Eight selected retail DXBC shaders passed both instruction-chunk and complete
container decode-to-encode byte identity checks.
That validates lossless instruction decoding for the selected chunks; it does
not validate the decoder's interpretation with a second shader disassembler or
prove that a particular draw uses these permutations at runtime.

All addresses below are preferred image addresses. Runtime addresses require
the actual loaded module base and RVA. No retail frame capture, active profile
INI read, effective setting dump, or running-game measurement was performed.
These findings apply to this executable and selected shaders; LE, VR, older SE,
and modded rendering need separate checks.

## Ordinary surface lighting

Retail `Lighting` pixel permutation `0x6201` samples the deferred mask at texture
slot 14. Its red channel multiplies the directional RGB before both diffuse and
specular evaluation. Shadowed point lights select their own channels; their
visibility multiplies their RGB before the light's distance attenuation.

The mapped feature bits are vertex color, specular, directional shadows, and
deferred shadows, with base technique NONE. The compiled emission/IBL additions
do not establish that those constants are nonzero for a particular draw.

For the inspected diffuse/specular terms, a useful behavioral model is:

```text
N = normalized shading normal
Ssun = directional visibility from the deferred shadow mask
Sj = selected point-light visibility, or 1 for an unshadowed light
aj = 1 - saturate(distanceToLight / radius)^2
H = normalize(lightDirection + viewDirection)

directDiffuse = Ssun * sunRGB * saturate(dot(N, Lsun))
              + sum(Sj * pointRGBj * aj * saturate(dot(N, Lj)))
ambient = DirectionalAmbientMatrix * float4(N, 1)
diffuse = albedo * vertexRGB * (directDiffuse + ambient + emission + IBL)
directSpecular = maskedLightRGB * saturate(dot(N, H))^materialExponent
```

The shader adds ambient, emission, and IBL after the direct-light visibility
multiplications. The model includes diffuse and vertex RGB but omits later
material/specular masks, fog, and output controls; it is not the complete
final-pixel equation.

In the captured disassembly, directional visibility and the two direct responses
are at lines 76–88; point selection/attenuation at 99–128; ambient and emission
addition at 134–142; normal-alpha specular scaling at 153–154. Register-to-field
names are checked against the FXP constant offsets and the
[CPU lighting reconstruction][cpu-lighting].

Consequences:

- Increasing ambient fill can brighten an occluded surface without changing
  the shadow mask. Other unoccluded lights, emission, and reflections can do so too.
- A normal facing away from a light has a small diffuse term even when visibility
  is one. That dark side is a material lighting response, rather than an occluder
  detected by the shadow map.
- Diffuse texture and vertex RGB multiply the lighting. Authored dark detail can
  resemble occlusion and persist when cast shadows are disabled.
- Map resolution, cascade range, filtering, and bias can change where a pixel
  receives partial visibility. They do not substitute for ambient/direct balance.

## Grass shadow floor

Retail `RunGrass` pixel permutation `0x0` loads a shadow value from texture slot
1, then evaluates:

```text
Sgrass = S + (1 - S) * fShadowClampValue
grassDiffuse = outputScale * albedo * vertexRGB
             * (directGrassLighting * Sgrass + ambientGrassLighting)
```

With the stored initializer `0.3`, fully occluded grass retains a direct-light
multiplier of `0.3`; full visibility remains `1`. At visibility `0.5`, the
multiplier is `0.65`. These percentages describe that direct-light factor, not
the percentage brightness of the final displayed pixel.

The native read at `0x141522d44` copies the setting from `0x1420d6d48` into the
fourth component of state beginning at `0x1433d4da0`. The grass CPU
[reconstruction][cpu-grass] assigns `fShadowClampValue` to `ShadowClampValue`;
its [full constant struct][grass-constants] places that field at byte `0x15c`, corresponding to
retail `cb2[21].w`. The FXP offset entry alone does not name this component.
The complete 1.7.104 CPU upload from the copied state to this buffer was not
traced; the setting-to-buffer identification combines the native read with the
pinned source layout. The retail arithmetic itself is directly decoded.

This does not establish useful or supported behavior for values outside the
ordinary 0–1 interpolation range. No range-validation branch was traced.

## Normal maps and material response

| Input | Role | Evidence scope |
| --- | --- | --- |
| Tangent normal RGB | Decode the signed vector, transform through the tangent frame, normalize; steer diffuse, specular, and directional ambient | Selected retail Lighting shader; source reconstruction for field names |
| Ordinary normal alpha | Scale direct specular highlight amplitude | Selected retail Lighting shader |
| Model-space normal RGB | Different coordinate path with signed XZY decode | Retail Lighting permutation `0x6205` checked |
| Model-space specular map | Separate texture slot 2/red-channel mask | Retail `0x6205` checked; ordinary normal-alpha semantics do not apply |
| NIF specular RGB/strength and glossiness | Highlight color/amplitude and exponent | NIF schema and CPU reconstruction |
| Emissive color/multiple, glow map | Add a contribution that can remain visible in shadow | Material schema and inspected emission addition |
| Soft, rim, and backlighting | Specialized material terms that can fill weakly lit sides | Source reconstruction; specialized retail permutations unverified |
| NIF vertex RGB | Multiply material lighting; can contain authored darkening | Inspected retail shader |

NIF Receive Shadows/Cast Shadows flags determine requested shadow participation;
Model Space Normals chooses a different normal convention. Those flags are
distinct from the map's color channels. NIF texture array slots and GPU bindings
are separate mappings: NIF slot 2 can supply glow/rim, while the inspected
model-space shader reads specular from GPU texture slot 2. The latter does not
establish NIF array index 2. `TXST` names its slots differently from the NIF array.

The [older shader reconstruction][old-lighting] provides the specialized
normal/material paths; [Niftools' schema][nif-schema] defines serialized fields.
Tooling schema defaults are not measured native fallbacks. No exact
`fNormalMapStrength` setting name was found in this target's ASCII or UTF-16LE
string scan; that does not rule out material-specific controls.

## INI controls

The tables use **stored executable initializers**, rounded for readability.
They are not active user values or recommended tweaks. Presets and user
configuration can override them.

Setting objects use data at `+0x08` and a name pointer at `+0x10`, matching
[CommonLib's Setting layout][setting-layout]. Native RTTI distinguishes
`SettingT<INISettingCollection>` from `SettingT<INIPrefSettingCollection>`.
This establishes the owning collections corresponding to `Skyrim.ini` and
`SkyrimPrefs.ini`; actual loaded paths and override precedence were not captured.

### Darkness, AO, and appearance

| Key | File / section | Initializer | Meaning and verification |
| --- | --- | ---: | --- |
| `fShadowClampValue` | Skyrim.ini / Display | 0.3 | Grass direct-light shadow floor; native read and retail shader checked |
| `bSAOEnable` | SkyrimPrefs.ini / Display | 1 | Ordinary SAO enable flag; native initialization read checked |
| `fSAOIntensity` | Skyrim.ini / Display | 15 | AO intensity; native initialization and later setup reads checked |
| `fSAORadius` | Skyrim.ini / Display | 250 | AO sample scale/radius; native initialization and later setup reads checked |
| `fSAOBias` | Skyrim.ini / Display | 2.5 | AO bias, separate from shadow-map bias; native reads checked |
| `bSAONormalMap` | Skyrim.ini / Display | 1 | Normal-map versus reconstructed-normal option; initialization and console toggle checked, final shader selection untraced |
| `bSAODownscaled` | Skyrim.ini / Display | 0 | Native setup halves dimensions when enabled |
| `fSAOExpFactor`, `fSAOValueDiffFactor` | Skyrim.ini / Display | 0.11, 0.3 | Additional SAO parameters; initialization reads checked, final formulas untraced |
| `bSAOApplyFog` | Skyrim.ini / Display | 1 | AO/fog composition option; setting object verified |
| `bSAO_CS_Enable` | SkyrimPrefs.ini / Display | 0 | Alternate compute AO flag; setting object verified |
| `fSAO_CS_Intensity`, `fSAO_CS_Radius`, `fSAO_CS_Bias` | Skyrim.ini / Display | 10, 250, 2.5 | Alternate AO-family parameters; active path unverified |
| `bCharacterLighting` and `fCharacterLight*` | Skyrim.ini / Display | 1; see catalog | Character fill-light family; not general shadow opacity |
| `fGamma` | SkyrimPrefs.ini / Display | 1 | Display appearance candidate; pixel equation not traced |
| `fGlobalBrightnessBoost`, `fGlobalContrastBoost` | Skyrim.ini / Display | 0, 0 | Global appearance candidates; setting objects verified |
| `fLightingOutputColourClampPostLit/PostEnv/PostSpec` | SkyrimPrefs.ini / General | 1 each | Lighting/output controls, not cast-shadow coverage |

The native SAO constructor at `0x141541e20` reads the ordinary parameters.
Setup at `0x141541f40` later reads intensity, bias, and radius; the downscaled
flag conditionally shifts both dimensions right by one. Console function
`0x14037ebc0` accepts SAO configuration and distinguishes normal maps from
reconstructed normals. Finding the normal flag here does not prove its final
rendering effect in this version.

Snow has additional AO exemptions, rim lighting, specular response, SSS, and
sparkle controls. The catalog includes `bDeactivateAOOnSnow`,
`fShadowSparkleIntensity`, `fSnowRimLightIntensity`, `fSnowNormalSpecPower`,
`fSnowGeometrySpecPower`, and the `fSnowSSS*` family. These are specialized
appearance controls, not one common shadow-intensity setting.

### Map quality, coverage, filtering, and bias

All keys in this table use `[Display]`.

| Key | File | Initializer | Role and verification |
| --- | --- | ---: | --- |
| `iShadowMapResolution` | SkyrimPrefs.ini | 2048 | Map resolution; native object and shipped presets checked |
| `fShadowDistance` | SkyrimPrefs.ini | 8000 | Exterior range; native object and presets checked |
| `fInteriorShadowDistance` | SkyrimPrefs.ini | 3000 | Interior range parameter; consumer untraced |
| `fFirstSliceDistance` | Skyrim.ini | 1250 | Near directional slice/cascade parameter; consumer untraced |
| `iNumSplits` | SkyrimPrefs.ini | 2 | Directional split-count parameter; consumer untraced |
| `fSplitOverlap` | Skyrim.ini | 100 | Split-transition parameter; consumer untraced |
| `iNumFocusShadow` | SkyrimPrefs.ini | 4 | Focus-shadow count parameter; consumer untraced |
| `ffocusShadowMapDoubleEveryXUnit` | SkyrimPrefs.ini | 450 | Focus-map parameter; preserve spelling; consumer untraced |
| `fMaxFocusShadowMapDistance` | Skyrim.ini | 450 | Focus-shadow distance parameter; consumer untraced |
| `uShadowFilterType` | Skyrim.ini | 3 | Native initialization and technique-selection read checked; algorithms not fully traced |
| `fPoissonRadiusScale` | Skyrim.ini | 4 | Sampling-footprint parameter in source references; retail producer untraced |
| `iShadowMaskQuarter` | SkyrimPrefs.ini | 4 | Allocation dimension = `(screenDimension * value) >> 2`; native instructions checked |
| `fShadowBiasScale` | Skyrim.ini | 1 | Initial per-shadow-light bias at offset `+0x540`; native read checked |
| `fShadowDirectionalBiasScale` | Skyrim.ini | 0.3 | Conditional multiplier of per-light bias; native arithmetic checked |
| `fExponentialShadowMapScale` | Skyrim.ini | 10 | Registered shadow-map parameter; formula untraced |

`iShadowMaskQuarter=4` means full width and height in the verified allocation
path; `2` halves each, yielding one quarter of the pixel count; `1` quarters
each, yielding one sixteenth of the pixel count. This is an allocation rule,
not evidence that every alternative value renders correctly.

The bias setup at `0x1415688f4` selects the directional multiplier or `1`, then
multiplies the light's `+0x540` bias by approximately `0.00025` and that selected
factor. Another component uses approximately `0.00008333334` and a separate
scale. Both are subsequently scaled again. The captured branch does not
justify replacing this with a single global opacity equation.

### Participation, updates, and specialized controls

The catalog also covers tree/land/grass participation (`bTreesReceiveShadows`,
`bDrawLandShadows`, `bShadowsOnGrass`), high-tree limits
(`bDisableHighTreeShadow`, `fMaxHeightShadowCastingTrees`), sun-update controls
(`bDisableShadowJumps`, `fSunShadowUpdateTime`, `fSunUpdateThreshold`,
`fSunStaticTimeUpdateScale`),
viewport/target flags, actor self-shadowing, light LOD/fade, loading-menu shadows,
projected normals, map-menu appearance, indirect lighting, and volumetric
lighting. Most of these have verified objects/ownership but untraced consumers.
Their names alone do not establish supported values or exact behavior.

### Shipped quality presets

These values were read from the copied installation's `[Display]` presets.

| Key | Low | Medium | High | Ultra |
| --- | ---: | ---: | ---: | ---: |
| `iShadowMapResolution` | 1024 | 2048 | 2048 | 4096 |
| `iNumFocusShadow` | 1 | 2 | 4 | 4 |
| `fShadowDistance` | 3000 | 3000 | 8000 | 10000 |
| `bSAOEnable` | 1 | 1 | 1 | 1 |

The copied `Skyrim/SkyrimPrefs.ini` is a shipped High-like template, not the
user's active profile. Its presence does not establish current settings.

### Legacy and unsupported-name cautions

The exact names `iBlurDeferredShadowMask`, `bDeferredShadows`,
`iShadowMapResolutionPrimary`, `iShadowMapResolutionSecondary`, `iShadowFilter`,
`iShadowMode`, `bDrawShadows`, `bShadowMaskZPrepass`, `fShadowLODStartFade`,
`fShadowLODMaxStartFade`, `fSpecularLODStartFade`, and `fShadowMinDistance`
were absent from this executable's ASCII and UTF-16LE scans. So were
`fShadowIntensity`, `fSAONormalBias`, and `bSAOUseNormalMap`.

Some absent names still appear in shipped default INIs or LE-era tweak lists.
That is not proof that this executable reads them. Conversely, the verified
`fShadowBiasScale` consumer contradicts a blanket SE-unused claim. This study
does not classify those keys for LE or VR.

[BethINI's pinned settings][bethini-settings] and
[UI descriptions][bethini-ui] support file placement and discovery. Native
objects and selected consumers are the stronger evidence used here.

## Authored game data

| Layer | Relevant fields | Effect to investigate |
| --- | --- | --- |
| WTHR | NAM0 sunlight/ambient by time; four DALC directional ambient cubes; IMSP imagespaces | Sun-to-fill balance and time/weather changes |
| CELL / LGTM | XCLL ambient/directional/fog, directional fade, inheritance; LTMP template and DALC | Interior fill, directional lighting, and inherited room settings |
| IMGS / IMAD | SunlightScale, White, eye adaptation, brightness/contrast, tint and animated modifiers | Lighting scaling and displayed contrast; exact order remains untraced |
| LIGH | Color, radius, fade, exponent, shadow spot/hemi/omni flags | Other direct lights and requested shadow type |
| REFR | XRDS radius, XLIG fade/FOV/end-distance/bias; reference cast-shadow flags | Per-placement overrides and participation |
| NIF / TXST | Normal convention, specular/emissive/material response, cast/receive flags, texture overrides | Surface shading and shadow participation |
| VOLI / fog | Scattering/color/density, fog color/range/power | Appearance of shafts and distant contrast |

The [Weather][weather-schema], [Cell][cell-schema],
[LightingTemplate][template-schema], [Light][light-schema],
[PlacedObject][reference-schema], and [ImageSpace][imagespace-schema] schemas
identify these fields. The corresponding local records were read independently.
Serialized fields do not by themselves prove their runtime combination.

The record scan covered **Skyrim.esm and Update.esm only**, applying Update
overrides. It found 84 WTHR, 97 LGTM, 275 IMGS, and 435 LIGH records in that
view. It did not establish a live load order or apply DLC/Creation/mod overrides.

`SkyrimClear` (`WTHR 0000081A`, winning in Update) illustrates the distinction:
its day NAM0 Ambient is RGB `(180,192,197)`, while the DALC `Z+` face is
`(77,129,136)` and `Z-` is `(191,214,221)`. A single ambient RGB is not the
same input as the six-face ambient cube. These are stored values, not measured
pixel colors or an established color-space conversion.

Inheritance also matters: Bannered Mare (`CELL 0001605E`) stores raw ambient
RGB `(14,13,12)` but inherits ambient through its lighting-template mask.
Those raw cell bytes do not establish its effective ambient fill.

`REFR.XLIG` contains `ShadowDepthBias`; no field named `ShadowIntensity` was
found in the inspected schemas. Do not interpret depth bias as darkness.

Engine Fixes' author documents an ambient-specular/Fresnel transport bug in
the lighting shader. This makes those authored fields worth tracing; the bug's
status in this 1.7.104 target was not checked. A third-party fix is not evidence
that every version either implements those fields correctly or ignores them.
[Engine Fixes configuration][engine-fixes].

## Shadow production, AO, parallax, and mods

The directional/point depth maps, deferred visibility mask, and surface-lighting
consumer are distinct stages. The inspected retail consumer proves how a
visibility value affects its lighting; it does not fully recover how every map
producer computes that value. Bias, cascade blending, distance fade, and filter
footprints need their matching Utility permutations and native constants.

Vanilla has registered SAO controls in this target. AO is separate from the
cast-shadow mask and can darken screen-space detail. The exact ordinary SAO
pixel equation and its normal-option selection were not disassembled here.
The current Community Shaders AO compositor changes color-space handling and
cannot substitute for that missing retail check.

Basic parallax shifts texture coordinates in the older shader reconstruction;
its parallax-occlusion block is empty and its status notes describe that path as
unimplemented in the base game. A NIF flag or registered
`bEnableParallaxOcclusion` setting is insufficient proof of height self-shadowing.

Community Shaders adds or changes screen-space contact shadows, skylighting,
terrain-height/cloud shadows, extended-material height shadows, skin/hair/PBR
lighting, and AO. Its screen-space shadow feature has its own `ShadowContrast`.
ENB and other replacements likewise need their own configuration and shader
audit. These features must not be folded into a claim about this vanilla binary.
[Community Shaders pinned source][cs-source].

## Implications for Mudcrab

At this worktree's starting commit `c39449b`, world setup in
`crates/engine/src/app.rs` creates a 12,000-illuminance sun and one global ambient
color `(0.48,0.55,0.7)` with brightness 160. `crates/engine/src/lights.rs` creates
converted point lights with shadow maps disabled and its own intensity rule.
These are project approximations, not the recovered Skyrim shader model.

Parity work should first preserve the authored direct-light and directional
ambient inputs, then reproduce their distinct shading contributions. Grass
needs its own verified shadow-floor rule. Tangent and model-space normals,
normal-alpha specular masks, local-light shadow flags/fades, AO, and imagespace
processing need separate treatment. Tuning one Bevy ambient brightness or
shadow bias cannot establish Skyrim's complete shadow response.

The existing normal-map Y handling and normal-alpha mask path are useful
material compatibility work; they do not prove the full native BRDF, ambient,
AO, or shadow-production equation. No implementation changes were made here.

## Evidence and follow-up boundaries

Raw bytecode, disassembly, original record extracts, and the cloned Ghidra
project remain under `/private/tmp/skyrim-shadow-research` and the original
private REA research root. Repository deliverables contain behavioral notes,
setting metadata, addresses, and hashes only.

The FXP parser verified the primary Lighting, RunGrass, Utility, and Water
families before encountering a newer image-space family-order difference.
It did not parse the entire package. The selected Lighting and RunGrass
chunks were taken from the verified earlier families.

Still unverified: active INI values and paths; the precise runtime draw selecting
each permutation; complete Utility shadow production; specialized retail
lighting beyond the inspected tangent/model-space paths; SAO's final equation
and normal selection; the full ambient
cube-to-matrix native path; current ambient-specular bug status; final HDR/fog/
tone-map/exposure order; and controlled retail appearance measurements.

A useful runtime comparison holds weather/time, camera, geometry, material,
exposure, and load order fixed, then varies direct light, ambient fill, AO, and
grass clamp separately. A normal-map substitution should be checked separately
from the cast-shadow silhouette. Those measurements were not performed.

[cpu-lighting]: https://github.com/Nukem9/skyrimse-test/blob/328916305165a46c4e4b527735bbcfd46b09a0ca/skyrim64_test/src/patches/TES/BSShader/Shaders/BSLightingShader.cpp
[cpu-grass]: https://github.com/Nukem9/skyrimse-test/blob/328916305165a46c4e4b527735bbcfd46b09a0ca/skyrim64_test/src/patches/TES/BSShader/Shaders/BSGrassShader.cpp
[grass-constants]: https://github.com/Nukem9/skyrimse-test/blob/328916305165a46c4e4b527735bbcfd46b09a0ca/skyrim64_test/src/patches/TES/BSShader/Shaders/BSGrassShader.h#L36-L54
[old-lighting]: https://github.com/aers/Skyrim-SE-Shader-Tools/blob/b54d184a5c3acc25c60e6666a46c68b09a3759ed/old/shaders/Lighting/BSLightingShader.ps.hlsl
[setting-layout]: https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/S/Setting.h
[nif-schema]: https://github.com/niftools/nifxml/blob/develop/nif.xml
[bethini-settings]: https://github.com/DoubleYouC/Bethini-Pie-Skyrim-Special-Edition-Plugin/blob/74bbebaa0ac8c974a2cb1db7b0f4b7892e47d956/settings.json
[bethini-ui]: https://github.com/DoubleYouC/Bethini-Pie-Skyrim-Special-Edition-Plugin/blob/74bbebaa0ac8c974a2cb1db7b0f4b7892e47d956/Bethini.json
[weather-schema]: https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/Weather.xml
[cell-schema]: https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/Cell.xml
[template-schema]: https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/LightingTemplate.xml
[light-schema]: https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/Light.xml
[reference-schema]: https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/PlacedObject.xml
[imagespace-schema]: https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/ImageSpace.xml
[engine-fixes]: https://github.com/aers/EngineFixesSkyrim64/blob/master/Skyrim/Data/SKSE/Plugins/EngineFixes.toml
[cs-source]: https://github.com/doodlum/skyrim-community-shaders/tree/2f2919a71bed6132b125e41781304c8f6f73d002
[shader-decoder]: https://github.com/coconutbird/d3dasm/tree/a292206a15daf0723d945a475c6254413eda8551
[fxp-reader]: https://github.com/Nukem9/skyrimse-test/blob/328916305165a46c4e4b527735bbcfd46b09a0ca/shader_analyzer/FXPPackageExtractor.cs
