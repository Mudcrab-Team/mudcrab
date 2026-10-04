# L1 material compatibility

Target: unmodded Skyrim SE. [Phase #131](https://github.com/Mudcrab-Team/mudcrab/issues/131) separates material correctness from L2 environment lighting and L4 image-space behavior. A passing synthetic comparison proves the stated conversion/runtime contract; it does not prove that Bevy reproduces Skyrim's shader.

| Source path | Implemented and tested | Remaining compatibility work |
|---|---|---|
| Ordinary `BSLightingShaderProperty` | Diffuse transfer space; linear factors; specular enable, normal-alpha mask and exponent-to-roughness approximation; authored vertex channels; UV scale/offset and S/T wrapping | Native BRDF and intensity, authored tangent frames, remaining shader flags |
| Tangent-space normals | Linear RGB/alpha preserved; DirectX Y applied once in native material construction; independent geometric-normal GPU comparison | Regenerated tangents can differ from authored NIF frames; mirrored and deformed retail surfaces need L0 coverage |
| Model-space normals | Source flag and normal convention retained; excluded from tangent specular-mask interpretation and Y correction | A model-space shader path; existing tangent interpretation is an approximation and blocks parity for these materials |
| Glow/static emission | Eligible glow slot, authored tint and multiplier, zero/dim/HDR/black cases | Animated controllers, signed shader arithmetic beyond the recorded nonnegative projection |
| Alpha test / ordinary alpha blend | NiAlphaProperty determines mode; source-enabled vertex alpha retained; visible/depth/shadow discard agrees | Simultaneous blend/test and nonstandard source/destination blend factors; full retail transparency ordering |
| Tree animation / object LOD | Static color projection; source tree/LOD exclusions prevent animation/fade alpha from entering ordinary alpha test | Animation, authored LOD/fade and specialized lighting |
| Environment map / eye environment map | Source family, flags and texture slots retained | Authored cube/mask reflection response and eye shading |
| Parallax / parallax occlusion / multilayer parallax | Base surface projection; source slots retained | Height sampling, displaced UVs, inner-layer compositing |
| Face / skin / hair tint | Base surface projection; source family retained | Specialized tint, subsurface and specular behavior |
| Snow / sparkle snow | Base surface projection; source family retained | Native snow coverage and sparkle response |
| NIF landscape / landscape LOD / cloud | Generic material projection with source classification | Specialized NIF shader paths; do not confuse these with the separate LAND renderer |
| Effect shader | Source values, UVs and alpha property preserved in generic projection | Particle/greyscale/falloff/refraction/animation semantics. Fire-refraction surfaces are explicitly excluded, not rendered as opaque cards |
| Declared editor marker | Exact `EditorMarker` shape plus BSX editor flag excluded with recorded reason | Not runtime geometry; ordinary shapes without both conditions remain visible |
| LAND terrain | Existing layer renderer participates in common output-domain probe; #102 sampler defaults integrated | Independent native terrain-normal direction proof, layer/material semantics and retail comparisons |
| Sky / fog / water / reflections | Common HDR composition, synchronized linear reflection exposure, one output transform | Authored state/units and full-scene comparisons belong to later phases |

Current producer 23 retains the source-surface metadata introduced by historical schema 19: it records `shaderFamily`, `lightingShaderType`, `shaderFlags1/2` and `normalConvention` on published materials. Excluded primitives retain `materialExclusion`. These fields support the retail inventory without implying support for a shader family merely because its mesh loads.

Evidence and source trace: [color-pipeline spec](specs/engine/color-pipeline.md), [GPU measurements](evidence/lighting-l1-20261003.json), and [superseded cutout workaround](adr/0009-cutout-vertex-alpha-normalization.md). Mods supply reverse-engineering evidence; enhanced lighting and an addon framework are outside this phase.

## Converted asset inventory

Historical schema 19 corpus (not reconverted or relabeled by this integration): 25,388 GLBs, 63,981 source-tagged materials. Counts describe
materials in converted files, not visible instances or accepted shader support.

| Source family/type | Materials |
|---|---:|
| `effect` | 5,848 |
| `lighting/default` | 45,163 |
| `lighting/environment_map` | 9,533 |
| `lighting/eye_environment_map` | 28 |
| `lighting/face_tint` | 10 |
| `lighting/glow` | 1,500 |
| `lighting/hair_tint` | 35 |
| `lighting/multi_layer_parallax` | 682 |
| `lighting/parallax` | 11 |
| `lighting/skin_tint` | 1,152 |
| `lighting/sparkle_snow` | 19 |

Source normal conventions: 56,976 tangent-space, 1,123 model-space, 5,882 without
a normal convention. Alpha modes: 45,075 opaque, 13,035 mask, 5,871 blend.
3,488 materials carry nonidentity UV transforms.

Excluded primitives: 1,151 declared editor markers, 267 fire-refraction surfaces,
1,951 shapes without shader properties, 36 native water shader shapes and 35
native sky shader shapes. The last three categories predate this slice. These
are recorded exclusions, not silent successful shader conversions.

The inventory reads published `extras.openSkyrim` on every material and primitive.
Tree/LOD behavior is also flag-driven; an absent specialized type in this table
does not prove that no tree/LOD flags occur. Model-space normals and specialized
shader responses remain named L1 parity blockers.
