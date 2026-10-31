# Retail shader package map

The selected `shaders011.fxp` contains 135 groups and 16,044 program records: 3,390 vertex shaders, 12,635 pixel shaders and 19 compute shaders. They contain 8,057 distinct bytecodes. Every record is cataloged in [shader-inventory.json](shader-inventory.json), with a stable group ordinal, technique identifier, package offsets and bytecode SHA-256.

This is a complete static inventory of that package. Original executable constructors and ordered stream loading now prove all 135 group labels and ordinals. Lighting key masks and cache lookup are also traced; live program selection, pass order and resource bindings remain open. The catalog does not establish how a retail frame uses any program.

## Source identity

| Input | Identity |
| --- | --- |
| Archive | `Skyrim - Shaders.bsa` |
| Archive SHA-256 | `fd1ba630f443353ed21ed4ca0380968f9c7091c8434518ea3cc79bc6d38c18e7` |
| Embedded path | `shadersfx/shaders011.fxp` |
| Embedded offset | 102 bytes |
| Package length | 67,768,128 bytes |
| Package SHA-256 | `72cfc73eb0938d5949f0b3a8683722a33b671a34b41b750ba138de8d154c5fbc` |
| Catalog SHA-256 | `3679f4cab2d74ff41f444445beff5ccd73be3bbd8193399572647ceebbbfbcd3` |
| Historical extractor revision | `328916305165a46c4e4b527735bbcfd46b09a0ca` |

The archive payload was compared directly with the extracted package. Independent parsing consumed all 67,768,128 package bytes, without trailing data. Each of the 16,044 records matched the previous manifest and extracted bytecode. All records declare Shader Model 5.0 and their stage tokens match their enclosing group.

A fresh decoder run inspected all 8,057 distinct bytecodes in a new private directory. Its outputs matched the previous full disassemblies for all 16,044 records. An independent walk of every `SHEX` token stream consumed all instruction records and matched each decoded executable-instruction count. Immediate constant-buffer data and declarations are excluded from executable counts. Source archive, package and manifest size and modification times remained unchanged.

No shader bytes or assembly listings are copied into this document or the public catalog. Fresh disassembly, original bytecode, decoder provenance and extraction scripts remain in the private research directory.

## What the package retains

Every program contains `ISGN`, `OSGN` and `SHEX` chunks. None contains `RDEF`. Consequently, original constant-buffer variable names and texture names are unavailable from this package. Names assigned by earlier research must be traced to native producers and bindings before they become part of an implementation specification.

The catalog records the surviving interfaces: input and output semantics, component types, register masks, pixel-input interpolation qualifiers, global declaration flags, compute shared-memory layouts, constant-buffer slots and vector counts, static and dynamic constant accesses, texture kinds and slots, sampler slots, UAV declarations, thread-group dimensions and instruction footprints. A declared `texture2d` does not establish the bound view format, transfer function, filtering, addressing mode or semantic role. A `SV_Target` output does not establish its render-target format or blend state.

The instruction footprints give exact static counts and component accesses. They do not replace symbolic equations or native parameter traces. An operand occurrence counts a position in the shader instruction stream; loops and branches can change how often it executes. Component masks union referenced operands and can include components that do not affect the final output.

## Group labels and technique identifiers

The binary package stores group boundaries, stage counts, technique identifiers, metadata and bytecode. [native-shader-loader.json](native-shader-loader.json) now links every group to original executable constructors, loader strings, RTTI vtables and sequential stage readers. This proves names and ordinals for the recorded executable/package pair under successful construction and stream reads. Live resource resolution and pass activation remain unobserved. Stable IDs such as `group-123` remain unchanged.

The original labels were checked against native strings. Groups 81 and 82 are `ReflectionBlurHCS` and `ReflectionBlurVCS`; the historical list had prefixed both with `IS`. The two formerly unnamed tail groups are `ISVolumetricLightingGenerateCS` and `ISVolumetricLightingRaymarchCS`.

Technique identifiers are exact 32-bit package values. Their bit frequencies are included for every group and stage. A matching numeric VS and PS identifier is recorded as an inventory fact, not as a proven native pairing. Lighting VS/PS masks, serialized-key cache lookup and context binding have original-byte proofs described in [shader-selection.md](shader-selection.md). Other family selectors, feature conditions and state-dependent selection still need tracing.

## Complete group coverage

Counts are records in the package, not draw calls. Instruction ranges describe executable instruction records in each stage. Texture counts describe declarations per program. A dash means that stage is absent from the group.

| Group | Native loader label | VS / PS / CS | Executable instructions, VS / PS / CS | Texture declarations per PS or CS |
| --- | --- | ---: | --- | --- |
| `group-000` | `BloodSplatter` | 2 / 2 / 0 | 12–13 / 5–10 / — | 2 (texture2d) |
| `group-001` | `DistantTree` | 4 / 4 / 0 | 17–22 / 15–19 / — | 1 (texture2d) |
| `group-002` | `RunGrass` | 18 / 18 / 0 | 70–71 / 8–46 / — | 1–2 (texture2d) |
| `group-003` | `Particle` | 6 / 6 / 0 | 31–70 / 7–18 / — | 1–3 (texture2d) |
| `group-004` | `Sky` | 9 / 9 / 0 | 15–47 / 13–19 / — | 0–2 (texture2d) |
| `group-005` | `Effect` | 606 / 3216 / 0 | 42–170 / 18–61 / — | 0–4 (texture2d) |
| `group-006` | `Lighting` | 127 / 6924 / 0 | 66–180 / 83–350 / — | 2–15 (texture2d, texturecube) |
| `group-007` | `Utility` | 339 / 177 / 0 | 3–104 / 2–144 / — | 0–4 (texture2d, texture2darray) |
| `group-008` | `Water` | 2172 / 2172 / 0 | 20–50 / 23–253 / — | 0–11 (texture2d, texturecube) |
| `group-009` | `ISFXAA` | 1 / 1 / 0 | 7 / 172 / — | 1 (texture2d) |
| `group-010` | `ISCopy` | 1 / 1 / 0 | 4 / 6 / — | 1 (texture2d) |
| `group-011` | `ISCopyDynamicFetchDisabled` | 1 / 1 / 0 | 4 / 2 / — | 1 (texture2d) |
| `group-012` | `ISCopyScaleBias` | 1 / 1 / 0 | 4 / 6 / — | 1 (texture2d) |
| `group-013` | `ISCopyCustomViewport` | 1 / 1 / 0 | 4 / 6 / — | 1 (texture2d) |
| `group-014` | `ISRefraction` | 1 / 1 / 0 | 4 / 44 / — | 2 (texture2d) |
| `group-015` | `ISDoubleVision` | 1 / 1 / 0 | 5 / 44 / — | 2 (texture2d) |
| `group-016` | `ISCopyTextureMask` | 1 / 1 / 0 | 7 / 7 / — | 1 (texture2d) |
| `group-017` | `ISDepthOfField` | 1 / 1 / 0 | 4 / 128 / — | 4 (texture2d) |
| `group-018` | `ISDistantBlur` | 1 / 1 / 0 | 4 / 120 / — | 4 (texture2d) |
| `group-019` | `ISMap` | 1 / 1 / 0 | 4 / 42 / — | 1 (texture2d) |
| `group-020` | `ISWorldMap` | 1 / 1 / 0 | 5 / 47 / — | 2 (texture2d) |
| `group-021` | `ISWorldMapNoSkyBlur` | 1 / 1 / 0 | 5 / 49 / — | 2 (texture2d) |
| `group-022` | `ISRadialBlur` | 1 / 1 / 0 | 4 / 46 / — | 1 (texture2d) |
| `group-023` | `ISRadialBlurMedium` | 1 / 1 / 0 | 4 / 46 / — | 1 (texture2d) |
| `group-024` | `ISRadialBlurHigh` | 1 / 1 / 0 | 4 / 46 / — | 1 (texture2d) |
| `group-025` | `ISCopyGreyScale` | 1 / 1 / 0 | 4 / 8 / — | 1 (texture2d) |
| `group-026` | `ISHDRTonemapBlendCinematic` | 1 / 1 / 0 | 4 / 49 / — | 3 (texture2d) |
| `group-027` | `ISHDRTonemapBlendCinematicFade` | 1 / 1 / 0 | 4 / 50 / — | 3 (texture2d) |
| `group-028` | `ISHDRDownSample16` | 1 / 1 / 0 | 4 / 22 / — | 1 (texture2d) |
| `group-029` | `ISHDRDownSample4` | 1 / 1 / 0 | 4 / 22 / — | 1 (texture2d) |
| `group-030` | `ISHDRDownSample16Lum` | 1 / 1 / 0 | 4 / 22 / — | 1 (texture2d) |
| `group-031` | `ISHDRDownSample4RGB2Lum` | 1 / 1 / 0 | 4 / 23 / — | 1 (texture2d) |
| `group-032` | `ISHDRDownSample4LightAdapt` | 1 / 1 / 0 | 4 / 36 / — | 2 (texture2d) |
| `group-033` | `ISHDRDownSample4LumClamp` | 1 / 1 / 0 | 4 / 22 / — | 1 (texture2d) |
| `group-034` | `ISHDRDownSample16LightAdapt` | 1 / 1 / 0 | 4 / 36 / — | 2 (texture2d) |
| `group-035` | `ISHDRDownSample16LumClamp` | 1 / 1 / 0 | 4 / 22 / — | 1 (texture2d) |
| `group-036` | `ISBlur3` | 1 / 1 / 0 | 4 / 29 / — | 1 (texture2d) |
| `group-037` | `ISBlur5` | 1 / 1 / 0 | 4 / 29 / — | 1 (texture2d) |
| `group-038` | `ISBlur7` | 1 / 1 / 0 | 4 / 29 / — | 1 (texture2d) |
| `group-039` | `ISBlur9` | 1 / 1 / 0 | 4 / 29 / — | 1 (texture2d) |
| `group-040` | `ISBlur11` | 1 / 1 / 0 | 4 / 29 / — | 1 (texture2d) |
| `group-041` | `ISBlur13` | 1 / 1 / 0 | 4 / 29 / — | 1 (texture2d) |
| `group-042` | `ISBlur15` | 1 / 1 / 0 | 4 / 29 / — | 1 (texture2d) |
| `group-043` | `ISNonHDRBlur3` | 1 / 1 / 0 | 4 / 21 / — | 1 (texture2d) |
| `group-044` | `ISNonHDRBlur5` | 1 / 1 / 0 | 4 / 21 / — | 1 (texture2d) |
| `group-045` | `ISNonHDRBlur7` | 1 / 1 / 0 | 4 / 21 / — | 1 (texture2d) |
| `group-046` | `ISNonHDRBlur9` | 1 / 1 / 0 | 4 / 21 / — | 1 (texture2d) |
| `group-047` | `ISNonHDRBlur11` | 1 / 1 / 0 | 4 / 21 / — | 1 (texture2d) |
| `group-048` | `ISNonHDRBlur13` | 1 / 1 / 0 | 4 / 21 / — | 1 (texture2d) |
| `group-049` | `ISNonHDRBlur15` | 1 / 1 / 0 | 4 / 21 / — | 1 (texture2d) |
| `group-050` | `ISBrightPassBlur3` | 1 / 1 / 0 | 4 / 31 / — | 2 (texture2d) |
| `group-051` | `ISBrightPassBlur5` | 1 / 1 / 0 | 4 / 31 / — | 2 (texture2d) |
| `group-052` | `ISBrightPassBlur7` | 1 / 1 / 0 | 4 / 31 / — | 2 (texture2d) |
| `group-053` | `ISBrightPassBlur9` | 1 / 1 / 0 | 4 / 31 / — | 2 (texture2d) |
| `group-054` | `ISBrightPassBlur11` | 1 / 1 / 0 | 4 / 31 / — | 2 (texture2d) |
| `group-055` | `ISBrightPassBlur13` | 1 / 1 / 0 | 4 / 31 / — | 2 (texture2d) |
| `group-056` | `ISBrightPassBlur15` | 1 / 1 / 0 | 4 / 31 / — | 2 (texture2d) |
| `group-057` | `ISWaterDisplacementClearSimulation` | 1 / 1 / 0 | 4 / 2 / — | 0 |
| `group-058` | `ISWaterDisplacementTexOffset` | 1 / 1 / 0 | 4 / 11 / — | 1 (texture2d) |
| `group-059` | `ISWaterDisplacementWadingRipple` | 1 / 1 / 0 | 8 / 2 / — | 0 |
| `group-060` | `ISWaterDisplacementRainRipple` | 1 / 1 / 0 | 4 / 2 / — | 0 |
| `group-061` | `ISWaterWadingHeightmap` | 1 / 1 / 0 | 6 / 22 / — | 1 (texture2d) |
| `group-062` | `ISWaterRainHeightmap` | 1 / 1 / 0 | 6 / 22 / — | 1 (texture2d) |
| `group-063` | `ISWaterBlendHeightmaps` | 1 / 1 / 0 | 4 / 8 / — | 2 (texture2d) |
| `group-064` | `ISWaterSmoothHeightmap` | 1 / 1 / 0 | 6 / 2 / — | 1 (texture2d) |
| `group-065` | `ISWaterDisplacementNormals` | 1 / 1 / 0 | 4 / 58 / — | 1 (texture2d) |
| `group-066` | `ISNoiseScrollAndBlend` | 1 / 1 / 0 | 4 / 14 / — | 1 (texture2d) |
| `group-067` | `ISNoiseNormalmap` | 1 / 1 / 0 | 4 / 35 / — | 1 (texture2d) |
| `group-068` | `ISLocalMap` | 1 / 1 / 0 | 5 / 51 / — | 1 (texture2d) |
| `group-069` | `ISAlphaBlend` | 1 / 1 / 0 | 4 / 82 / — | 16 (texture2d) |
| `group-070` | `ISDepthOfFieldFogged` | 1 / 1 / 0 | 4 / 137 / — | 4 (texture2d) |
| `group-071` | `ISDepthOfFieldMaskedFogged` | 1 / 1 / 0 | 4 / 132 / — | 4 (texture2d) |
| `group-072` | `ISDistantBlurFogged` | 1 / 1 / 0 | 4 / 129 / — | 4 (texture2d) |
| `group-073` | `ISDistantBlurMaskedFogged` | 1 / 1 / 0 | 4 / 122 / — | 4 (texture2d) |
| `group-074` | `ISVolumetricLighting` | 1 / 1 / 0 | 4 / 3 / — | 0 |
| `group-075` | `ApplyReflections` | 1 / 1 / 0 | 4 / 21 / — | 3 (texture2d) |
| `group-076` | `ISApplyVolumetricLighting` | 1 / 1 / 0 | 4 / 44 / — | 7 (texture1d, texture2d, texture3d) |
| `group-077` | `ISBasicCopy` | 1 / 1 / 0 | 4 / 2 / — | 1 (texture2d) |
| `group-078` | `ISBlur` | 1 / 1 / 0 | 4 / 21 / — | 1 (texture2d) |
| `group-079` | `ISVolumetricLightingBlurHCS` | 0 / 0 / 1 | — / — / 42 | 2 (texture2d) |
| `group-080` | `ISVolumetricLightingBlurVCS` | 0 / 0 / 1 | — / — / 42 | 2 (texture2d) |
| `group-081` | `ReflectionBlurHCS` | 0 / 0 / 1 | — / — / 37 | 1 (texture2d) |
| `group-082` | `ReflectionBlurVCS` | 0 / 0 / 1 | — / — / 37 | 1 (texture2d) |
| `group-083` | `ISParallaxMaskBlurHCS` | 0 / 0 / 1 | — / — / 42 | 1 (texture2d) |
| `group-084` | `ISParallaxMaskBlurVCS` | 0 / 0 / 1 | — / — / 42 | 1 (texture2d) |
| `group-085` | `ISDepthOfFieldBlurHCS` | 0 / 0 / 1 | — / — / 47 | 2 (texture2d) |
| `group-086` | `ISDepthOfFieldBlurVCS` | 0 / 0 / 1 | — / — / 47 | 2 (texture2d) |
| `group-087` | `ISCompositeVolumetricLighting` | 1 / 1 / 0 | 4 / 7 / — | 1 (texture2d) |
| `group-088` | `ISCompositeLensFlare` | 1 / 1 / 0 | 4 / 3 / — | 1 (texture2d) |
| `group-089` | `ISCompositeLensFlareVolumetricLighting` | 1 / 1 / 0 | 4 / 8 / — | 2 (texture2d) |
| `group-090` | `ISCopySubRegionCS` | 0 / 0 / 1 | — / — / 10 | 1 (texture2d) |
| `group-091` | `ISDebugSnow` | 1 / 1 / 0 | 4 / 5 / — | 2 (texture2d) |
| `group-092` | `ISDownsample` | 1 / 1 / 0 | 4 / 27 / — | 3 (texture2d) |
| `group-093` | `ISDownsampleIgnoreBrightest` | 1 / 1 / 0 | 4 / 17 / — | 1 (texture2d) |
| `group-094` | `ISDownsampleCS` | 0 / 0 / 1 | — / — / 46 | 3 (texture2d) |
| `group-095` | `ISDownsampleIgnoreBrightestCS` | 0 / 0 / 1 | — / — / 39 | 3 (texture2d) |
| `group-096` | `ISExp` | 1 / 1 / 0 | 4 / 5 / — | 1 (texture2d) |
| `group-097` | `ISIBLensFlares` | 1 / 1 / 0 | 4 / 40 / — | 1 (texture2d) |
| `group-098` | `ISLightingComposite` | 1 / 1 / 0 | 4 / 23 / — | 8 (texture2d) |
| `group-099` | `ISLightingCompositeNoDirectionalLight` | 1 / 1 / 0 | 4 / 18 / — | 5 (texture2d) |
| `group-100` | `ISLightingCompositeMenu` | 1 / 1 / 0 | 4 / 19 / — | 6 (texture2d) |
| `group-101` | `ISPerlinNoiseCS` | 0 / 0 / 1 | — / — / 105 | 0 |
| `group-102` | `ISPerlinNoise2DCS` | 0 / 0 / 1 | — / — / 105 | 0 |
| `group-103` | `ReflectionsRayTracing` | 1 / 1 / 0 | 4 / 313 / — | 4 (texture2d) |
| `group-104` | `ReflectionsDebugSpecMask` | 1 / 1 / 0 | 4 / 16 / — | 2 (texture2d) |
| `group-105` | `ISSAOBlurH` | 1 / 1 / 0 | 4 / 79 / — | 1 (texture2d) |
| `group-106` | `ISSAOBlurV` | 1 / 1 / 0 | 4 / 77 / — | 1 (texture2d) |
| `group-107` | `ISSAOBlurHCS` | 0 / 0 / 1 | — / — / 72 | 1 (texture2d) |
| `group-108` | `ISSAOBlurVCS` | 0 / 0 / 1 | — / — / 70 | 1 (texture2d) |
| `group-109` | `ISSAOCameraZ` | 1 / 1 / 0 | 4 / 10 / — | 1 (texture2d) |
| `group-110` | `ISSAOCameraZAndMipsCS` | 0 / 0 / 1 | — / — / 49 | 1 (texture2d) |
| `group-111` | `ISSAOCompositeSAO` | 1 / 1 / 0 | 4 / 184 / — | 7 (texture2d) |
| `group-112` | `ISSAOCompositeFog` | 1 / 1 / 0 | 4 / 196 / — | 6 (texture2d) |
| `group-113` | `ISSAOCompositeSAOFog` | 1 / 1 / 0 | 4 / 203 / — | 7 (texture2d) |
| `group-114` | `ISMinify` | 1 / 1 / 0 | 4 / 48 / — | 1 (texture2d) |
| `group-115` | `ISMinifyContrast` | 1 / 1 / 0 | 4 / 52 / — | 1 (texture2d) |
| `group-116` | `ISSAORawAO` | 1 / 1 / 0 | 4 / 145 / — | 4 (texture2d) |
| `group-117` | `ISSAORawAONoTemporal` | 1 / 1 / 0 | 4 / 112 / — | 2 (texture2d) |
| `group-118` | `ISSAORawAOCS` | 0 / 0 / 1 | — / — / 143 | 3 (texture2d) |
| `group-119` | `ISSILComposite` | 1 / 1 / 0 | 4 / 4 / — | 2 (texture2d) |
| `group-120` | `ISSILRawInd` | 1 / 1 / 0 | 4 / 131 / — | 5 (texture2d) |
| `group-121` | `ISSimpleColor` | 1 / 1 / 0 | 3 / 2 / — | 0 |
| `group-122` | `ISDisplayDepth` | 1 / 1 / 0 | 4 / 21 / — | 2 (texture2d) |
| `group-123` | `ISSnowSSS` | 1 / 1 / 0 | 4 / 45 / — | 3 (texture2d) |
| `group-124` | `ISTemporalAA` | 1 / 1 / 0 | 4 / 288 / — | 6 (texture2d) |
| `group-125` | `ISTemporalAA_UI` | 1 / 1 / 0 | 4 / 280 / — | 5 (texture2d) |
| `group-126` | `ISTemporalAA_Water` | 1 / 1 / 0 | 4 / 297 / — | 7 (texture2d) |
| `group-127` | `ISUpsampleDynamicResolution` | 1 / 1 / 0 | 4 / 4 / — | 1 (texture2d) |
| `group-128` | `ISWaterBlend` | 1 / 1 / 0 | 4 / 52 / — | 5 (texture2d) |
| `group-129` | `ISUnderwaterMask` | 1 / 1 / 0 | 8 / 2 / — | 0 |
| `group-130` | `LensFlare` | 1 / 1 / 0 | 13 / 16 / — | 2 (texture2d) |
| `group-131` | `LensFlareVisibility` | 1 / 1 / 0 | 3 / 60 / — | 1 (texture2d) |
| `group-132` | `ISWaterFlow` | 1 / 1 / 0 | 4 / 29 / — | 1 (texture2d) |
| `group-133` | `ISVolumetricLightingGenerateCS` | 0 / 0 / 1 | — / — / 79 | 4 (texture1d, texture2darray, texture3d) |
| `group-134` | `ISVolumetricLightingRaymarchCS` | 0 / 0 / 1 | — / — / 18 | 1 (texture3d) |

## Boundaries that need native tracing

The largest group, `Lighting`, has 127 VS and 6,924 PS records. All of its PS records declare 2D textures at `t0` and `t1`; other permutations declare additional resources up to `t15`, including cube textures. These declarations identify interfaces to trace. They do not establish which texture is diffuse, normal, environment, shadow or a landscape layer for a given technique.

The `Utility` group spans PS programs with two to 144 executable instructions. Some declare 2D-array resources. A single family name therefore cannot serve as its pass specification; native technique selection and resource binding must be mapped per program.

`group-123`, `ISSnowSSS`, contains one VS and one PS. The PS declares `t0`, `t1` and `t2` as 2D textures, `cb2` as two vectors and `cb12` as 45 vectors. Its instructions sample a scalar from `t2`, conditionally return a sample from `t1`, and otherwise execute a weighted loop with ten offset iterations and a center contribution. That confirms a compiled filtering path exists. Native resource identities, activation conditions and whether it runs in the target scene remain open. It does not establish a general skin or foliage scattering path.

`group-133`, `ISVolumetricLightingGenerateCS`, is a compute program with a `32 × 32 × 1` thread group. It declares two 2D-array inputs, one 1D input, one 3D input and two 3D UAV outputs. Its 79 executable instructions include conditional sampling and two store sites. `group-134`, `ISVolumetricLightingRaymarchCS`, has the same thread-group dimensions, one 3D input and one 3D UAV. Its 18 executable instructions include two sample sites and two store sites. Both names come from native loader strings and ordered compute loads. Their full equations, resources and execution conditions remain unproved.

The native `ReflectionsRayTracing` label belongs to one VS and one PS in this Shader Model 5.0 package. Its PS declares four 2D textures and contains 313 executable instructions. The label alone does not establish the reflection algorithm or an active retail reflection pass.

For each stage and technique, the remaining work is to identify the native selector, bind each constant/resource slot, trace producer calculations and branch predicates, recover render state and target formats, and validate the resulting path in a retail frame. Sampler state, texture-view transfer functions, render-target transfer functions, blend equations, temporal history and pass scheduling require evidence outside the shader declaration itself.

## Reading the catalog

Each record in `programs` references `profiles_by_bytecode_sha256`. Profiles reference shared interface registries through fields ending in `_id`. This preserves every program's binding and instruction data without duplicating identical interfaces thousands of times.

```python
program = catalog["programs"][0]
profile = catalog["profiles_by_bytecode_sha256"][program["bytecode_sha256"]]
buffers = catalog["registries"]["constant_buffers"][profile["constant_buffers_id"]]
accesses = catalog["registries"]["constant_accesses"][profile["constant_accesses_id"]]
```

Constant-access rows have four columns: constant-buffer slot, vector index or dynamic expression, component mask, and static operand occurrences. All registry hashes were checked for collisions, and reconstructing the normalized profiles reproduced every field in the original parsed profiles.

The package scope is closed by byte-level validation and native loader tracing. Formula recovery, complete native program selection and retail execution remain open. [embedded-shaders.md](embedded-shaders.md) separately covers 57 structurally bounded executable occurrences; other generated, loose, fallback or version-specific code remains outside this package inventory.
