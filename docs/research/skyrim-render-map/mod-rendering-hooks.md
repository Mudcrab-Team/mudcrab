# Rendering mod source hooks

This audit maps source-available rendering interventions to the existing native frame and input map. It records what the pinned mod source changes and which original game paths still need tracing. It proves no new vanilla rendering behavior and contains no retail capture.

Six repositories were checked out privately under `~/.local/share/mudcrab-research/render-map-20261010/mod-hooks/`. No mod was installed or executed. Complete tracked-file SHA256 manifests and browser-derived primary-document records are stored there; the public JSON carries exact source anchors, file/line hashes, references, grades and query links.

## Source pins

| Repository | Commit | Commit date |
|---|---|---|
| [community-shaders](https://github.com/community-shaders/skyrim-community-shaders/tree/d001d4de3cde4feeec539dbaca5481cdda2b1b15) | `d001d4de3cde4feeec539dbaca5481cdda2b1b15` | 2026-10-10 |
| [vanilla-hdr](https://github.com/doodlum/skyrim-vanilla-hdr/tree/aa580b6f9f958e3460a83c118cee09e11f86f785) | `aa580b6f9f958e3460a83c118cee09e11f86f785` | 2024-05-25 |
| [light-placer](https://github.com/powerof3/LightPlacer/tree/6b7c05894de17cb3e7f0b5169e82833c9244e1ed) | `6b7c05894de17cb3e7f0b5169e82833c9244e1ed` | 2026-10-03 |
| [auto-parallax](https://github.com/doodlum/skyrim-auto-parallax/tree/99f5c9e20f6ff327757fe793d1d66dca6b593a43) | `99f5c9e20f6ff327757fe793d1d66dca6b593a43` | 2026-08-22 |
| [shadow-boost](https://github.com/doodlum/skyrim-shadow-boost/tree/84531158c663e27fb7b57d985496f624163c1045) | `84531158c663e27fb7b57d985496f624163c1045` | 2026-08-22 |
| [enb-light-patcher](https://github.com/tr4wzified/enblightpatcher/tree/56eac02f359fd7b097cf8d0d79909855ad76a27a) | `56eac02f359fd7b097cf8d0d79909855ad76a27a` | 2025-03-19 |

The CS pin is the fetched development branch, not a claim that a particular release is installed. Its `VersionedRelocation::Select` chooses SE, older AE or AE ≥1.7.99 offsets. The native map targets SkyrimSE1.7.104.0. Address Library IDs, source labels and patch signatures remain query seeds until the original target bytes and owning native dataflow corroborate them.

## What changes the visual interpretation

The source provides concrete reasons to trace interactions before adjusting brightness or saturation. Optional CS LinearLighting changes diffuse/light/ambient colors, fog colors and fog alpha independently. Skylighting attenuates directional ambient illumination. SSS blurs irradiance. The HDR replacement changes shadow contrast and bloom. Added particle lights can require reduced original-light fade. These are documented mod behaviors; none establishes the cause of the current Mudcrab screenshot.

A texture view being UNORM does not settle the color space of every lighting calculation. Likewise, a function named `SkyrimGammaToLinear` or a repository named Vanilla HDR does not close retail parity. Selected native shader instructions, their native CB producers and the original output operations must supply that evidence.

## Inspected interventions

### M-C01 — Shader interception and replacement

CS detours LoadShaders and BeginTechnique, modifies lookup descriptors, retains an original-function path and requests compiled replacement shaders. The shader source is a mod-maintained implementation; names and familiar constant layouts do not establish byte equality with the pinned retail package.

**Inputs/resources.** BeginTechnique records original VS/PS descriptors into permutation data; State::ModifyShaderLookup rewrites keys for feature flags; ShaderCache creates and selects replacement objects. Shader-stage setters are intercepted inside BeginTechnique; cached state application runs State::Draw after the original SetDirtyStates.

**Schedule.** Core hooks run per technique and per dirty-state apply; cache enablement and available shaders control replacement/fallback.

**Map/query.** Inputs `I-C17`, `I-C18`, `I-C21`, `I-C23`, `I-C45`; frame `apply`, `shader-prepare`; next `M-Q01`, `M-Q04`.

**Evidence.** [M-A001 src/Hooks.cpp:159–205](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Hooks.cpp#L159-L205); [M-A002 src/Hooks.cpp:1002–1031](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Hooks.cpp#L1002-L1031); [M-A003 src/State.cpp:925–1027](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/State.cpp#L925-L1027); [M-A004 src/ShaderCache.cpp:1820–1840](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/ShaderCache.cpp#L1820-L1840); [M-A005 src/ShaderCache.cpp:1880–1902](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/ShaderCache.cpp#L1880-L1902); [M-A006 src/Utils/VersionedRelocation.h:5–37](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Utils/VersionedRelocation.h#L5-L37)

**Limits.** Source Address Library IDs and offsets remain source references until resolved against target 1.7.104.0 and original bytes. No replacement compiler build or hooked retail draw was run.

### M-C02 — Deferred scheduling and feature CB production

CS inserts a deferred interval around world opaque batches and blended decals, changes eight MRT selections and independent blending, publishes an opaque-depth copy, then performs SSGI, SSS, cubemap update and its own compute composite. This is CS pass order under its predicates.

**Inputs/resources.** State::UpdateSharedData reads native camera/sky/DALC inputs and derives SH/HDR fields. GetFeatureBufferData packs feature settings in a fixed CPU order matching FeatureData HLSL. PS b4 permutation,b5 shared,b6 feature; CS b5/b6 plus native b12; deferred composite SRVs t0..t15, UAVs u0 MAIN/u1 normals/u2 motion vectors. Opaque color/motion targets plus NORMALROUGHNESS/ALBEDO/SPECULAR/REFLECTANCE/MASKS/MASKS2 MRTs; each extra target is mod-controlled.

**Schedule.** Main_RenderShadowMaps calls original then EarlyPrepasses. StartDeferred runs Prepass on loaded features before opaque batches. BlendedDecals calls original then EndDeferred. DeferredPasses dispatches SSGI→SSS→UpdateCubemap→composite→cubemap PostDeferred→Effects11 rays; calls may be skipped. ForEachLoadedFeature iterates GetFeatureList order, not alphabetical order.

**Map/query.** Inputs `I-C03`, `I-C12`, `I-C18`, `I-C25`, `I-C38`, `I-C43`, `I-C45`; frame `scene`, `shadow-work`, `accumulator`, `composite-stage`, `apply`; next `M-Q02`, `M-Q03`, `M-Q06`.

**Evidence.** [M-A007 src/Deferred.h:155–169](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Deferred.h#L155-L169); [M-A008 src/Deferred.cpp:211–299](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Deferred.cpp#L211-L299); [M-A009 src/Deferred.cpp:301–449](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Deferred.cpp#L301-L449); [M-A010 src/Deferred.cpp:652–714](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Deferred.cpp#L652-L714); [M-A011 src/Deferred.cpp:733–740](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Deferred.cpp#L733-L740); [M-A012 src/FeatureBuffer.cpp:25–68](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/FeatureBuffer.cpp#L25-L68); [M-A013 package/Shaders/Common/SharedData.hlsli:451–475](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/Common/SharedData.hlsli#L451-L475); [M-A014 src/State.cpp:1174–1201](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/State.cpp#L1174-L1201); [M-A015 src/Feature.cpp:223–265](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Feature.cpp#L223-L265); [M-A016 src/Feature.h:345–366](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Feature.h#L345-L366)

**Limits.** Opaque-batch/blended-decals labels come from CS source; native call-site semantics require target-byte tracing. A feature present in the checkout is not a loaded or enabled retail feature.

### M-C03 — Render target and output intervention

CS edits target properties before native target creation, restores them afterward, promotes snow targets to FP16, and enables views/resources used by its own compute writes. Its custom output mode must be separated from the original native target descriptors.

**Inputs/resources.** State::ModifyRenderTarget and HDRDisplay setup control target formats and bind flags. Snow/SnowSwap R16G16B16A16_FLOAT. Normal/TAA/SSR-mask targets are modified for CS write access. Main target hook and refraction/underwater/reflection target hooks are installed at explicit call offsets.

**Schedule.** Initialization and target recreation; independent feature state determines further modification.

**Map/query.** Inputs `I-C32`, `I-C37`, `I-C45`; frame `target-init`, `target-create`, `target-create-body`, `backbuffer`, `swapchain`; next `M-Q02`.

**Evidence.** [M-A017 src/Hooks.cpp:647–716](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Hooks.cpp#L647-L716); [M-A018 src/Hooks.cpp:1030–1046](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Hooks.cpp#L1030-L1046)

**Limits.** The source comment describing a vanilla snow format is a search seed, not a target-native format proof. Native map already closes one original swapchain/RTV/SRV endpoint; mod output cannot supersede it.

### M-C04 — Light Limit Fix clustered lights

LLF builds/culls its own clustered light lists and replaces the local-light path in Lighting/Effect/Water. It carries per-geometry strict-light/room/shadow information separately. Optional inverse-square and linear flags are mod-defined extensions.

**Inputs/resources.** ProcessLight overlays RuntimeLightDataExt on NiLight runtime fields. CreatePointLight detour initializes custom flag/cutoff/formID/size data from TESObjectLIGH. UpdateLights builds GPU structures from selected scene lights. PS b3 StrictLightData (15 strict light records plus count/room/shadow-mask), PS t35 lights/t36 index list/t37 grid. Build/cull compute uses own b0 and SRV/UAVs.

**Schedule.** Prepass updates lights then binds t35..t37; SetupGeometry before/after wrappers stage b3; world/room/non-world predicates select clustered versus strict behavior.

**Map/query.** Inputs `I-C11`, `I-C15`, `I-C16`, `I-C18`, `I-C35`, `I-C37`; frame `accumulator`, `apply`, `scene`; next `M-Q04`, `M-Q10`.

**Evidence.** [M-A019 src/Features/LightLimitFix.cpp:218–315](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LightLimitFix.cpp#L218-L315); [M-A020 src/Features/LightLimitFix.cpp:333–435](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LightLimitFix.cpp#L333-L435); [M-A021 src/Features/LightLimitFix.cpp:593–663](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LightLimitFix.cpp#L593-L663); [M-A022 src/Features/LightLimitFix.cpp:1029–1042](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LightLimitFix.cpp#L1029-L1042); [M-A023 src/Features/LightLimitFix/Common.h:1–34](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LightLimitFix/Common.h#L1-L34); [M-A024 features/Light Limit Fix/Shaders/LightLimitFix/LightLimitFix.hlsli:1–23](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/features/Light%20Limit%20Fix/Shaders/LightLimitFix/LightLimitFix.hlsli#L1-L23); [M-A025 features/Light Limit Fix/Shaders/LightLimitFix/Attenuation.hlsli:9–30](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/features/Light%20Limit%20Fix/Shaders/LightLimitFix/Attenuation.hlsli#L9-L30)

**Limits.** Regular attenuation is implemented as 1-saturate(distance/radius)^2; inverse-square mixes a size-biased reciprocal-square path with smooth fade and SCALE=0.8. These coefficients are mod source facts. An unlimited-lights description does not remove this implementation's finite buffers/MAX_LIGHTS predicates.

### M-C05 — ENB Light-style particles inside current LLF

The pinned current CS source recognizes SoftEffect+ZBufferTest geometry with NiAlphaProperty alphaFlags 4109, resolves particle config/material color, queues light records and optionally culls the emitter draw. Those lights join its clustered buffer.

**Inputs/resources.** QueueParticleLight multiplies effect base color/scale and optional emittance; geometry worldBound supplies radius. AddParticleLightsToBuffer multiplies RGB by alpha/pi, halves bound radius and applies source-referenced distance fade globals. Same clustered t35..t37 path as M-C04; batch render wrappers can suppress the original emitter draw.

**Schedule.** Batch render hooks queue visible candidates; a subsequent UpdateLights consumes the queue, subject to EnableParticleLights and MAX_LIGHTS.

**Map/query.** Inputs `I-C15`, `I-C16`, `I-C24`, `I-C35`; frame `accumulator`, `draw`; next `M-Q10`.

**Evidence.** [M-A026 src/Features/LightLimitFix.cpp:899–1042](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LightLimitFix.cpp#L899-L1042)

**Limits.** Recognition of ENB Light-style asset conventions does not demonstrate use of ENB binary code. Do not generalize old FAQ/release statements about particle support to this commit.

### M-C06 — Screen Space Shadows

CS computes directional visibility from scene depth using the included Bend dispatch/raymarch implementation, clears its texture to lit and exposes the result to replacement pixel shaders.

**Inputs/resources.** Camera/light projection, dynamic resolution and configurable Bend parameters populate RaymarchCB. CS t0 scene depth,u0 output,b1 raymarch data,point-border sampler; own R8G8_UNORM visibility texture bound PS t45.

**Schedule.** Prepass; enabled and Full-sky predicates; dispatch list determines wave groups. This is a new pass, not the native cascade generator.

**Map/query.** Inputs `I-C05`, `I-C06`, `I-C33`, `I-C43`, `I-C45`; frame `shadow-work`, `composite-stage`; next `M-Q03`, `M-Q09`.

**Evidence.** [M-A027 src/Features/ScreenSpaceShadows.cpp:133–245](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/ScreenSpaceShadows.cpp#L133-L245); [M-A028 src/Features/ScreenSpaceShadows.cpp:279–309](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/ScreenSpaceShadows.cpp#L279-L309); [M-A029 features/Screen-Space Shadows/Shaders/ScreenSpaceShadows/RaymarchCS.hlsl:1–30](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/features/Screen-Space%20Shadows/Shaders/ScreenSpaceShadows/RaymarchCS.hlsl#L1-L30)

**Limits.** Source abbreviation SSS in comments here means Screen Space Shadows; it is separate from SubsurfaceScattering.

### M-C07 — Skylighting

CS repurposes precipitation occlusion rendering, updates a 3D spherical-harmonic visibility grid, and attenuates the directional ambient component in its replacement shader. It adds both ambient visibility and shadow-history resources.

**Inputs/resources.** Camera-relative grid, native shadow cascade SRV and directional-shadow-light data, precipitation-style occlusion pass and settings. Own R16G16B16A16_FLOAT probe volume; CS t0..t3 inputs/u0..u3 outputs; PS t50 probes/t53 shadow visibility.

**Schedule.** Prepass skipped for map/interior; precipitation hook and own probe update; vfunc0x2D shader-property pass producer, MainDraw AE36559 version-selected+0x3BF for AE>=1.7.99, frustum AE26185+0x59D, accumulator vfunc0x28.

**Map/query.** Inputs `I-C03`, `I-C06`, `I-C18`, `I-C25`, `I-C38`, `I-C43`, `I-C44`; frame `scene`, `shadow-camera`, `accumulator`; next `M-Q03`, `M-Q06`.

**Evidence.** [M-A030 src/Features/Skylighting.cpp:79–115](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/Skylighting.cpp#L79-L115); [M-A031 src/Features/Skylighting.cpp:199–324](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/Skylighting.cpp#L199-L324); [M-A032 features/Skylighting/Shaders/Skylighting/Skylighting.hlsli:1–93](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/features/Skylighting/Shaders/Skylighting/Skylighting.hlsli#L1-L93)

**Limits.** This visibility grid is a CS addition; the native DALC producer remains a separate evidence domain.

### M-C08 — Subsurface Scattering versus native soft/back/rim lighting

CS implements screen-space diffusion with separable kernels or Burley sampling. It extracts diffuse irradiance, then blurs/recombines albedo or chooses pre-scatter behavior. This is distinct from the existing native soft/back/rim material path.

**Inputs/resources.** Per-profile radius/thickness/strength/falloff or mean free path, camera FOV and mod material-validity masks. Reset independently controls the native character-light state/strength. CS b1 BlurCB; t0 main/diffuse,t1 depth,t2 masks,t3 albedo,t4 normal; temporary SRV/UAV textures and writes back to MAIN.

**Schedule.** DeferredPasses calls DrawSSS after SSGI and before final deferred composite; validMaterials gates execution. Prepass extraction followed by horizontal+vertical or Burley compute.

**Map/query.** Inputs `I-C17`, `I-C22`, `I-C31`, `I-C33`, `I-C43`; frame `composite-stage`, `shader-effect`; next `M-Q09`.

**Evidence.** [M-A033 src/Features/SubsurfaceScattering.cpp:230–400](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/SubsurfaceScattering.cpp#L230-L400); [M-A034 src/Features/SubsurfaceScattering.cpp:408–425](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/SubsurfaceScattering.cpp#L408-L425); [M-A035 src/Deferred.cpp:338–350](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Deferred.cpp#L338-L350)

**Limits.** An SSS feature name does not prove retail screen-space diffusion exists. Query native soft/rim/back shader variants and the snow-specific image-space path independently.

### M-C09 — Complex materials and terrain extensions

ExtendedMaterials adds parallax marching, height blending and approximate parallax shadows. TerrainHelper adds six texture resources keyed by native material hash and requires the mod LandscapeDefault record. TruePBR hooks texture-set/property/material/technique producers and creates extra material slots; it cannot serve as a vanilla material specification.

**Inputs/resources.** ExtendedMaterials settings; TerrainHelper texture-set/default/seasonal resolution; TruePBR extra texture/material metadata. TerrainHelper PS t92..t97 extra parallax slots; TruePBR/Lighting shader declares terrain displacement t80..t85 and RMAOS-style t86..t91; feature settings supplied in mod b6.

**Schedule.** LAND/material/property lifecycle hooks and per-draw resource setup; mod flags decide replacement permutations.

**Map/query.** Inputs `I-C17`, `I-C18`, `I-C20`, `I-C21`, `I-C22`, `I-C23`, `I-C27`, `I-C32`, `I-C34`, `I-C36`; frame `shader-prepare`, `apply`; next `M-Q04`, `M-Q11`.

**Evidence.** [M-A036 src/Features/ExtendedMaterials.cpp:6–25](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/ExtendedMaterials.cpp#L6-L25); [M-A037 features/Extended Materials/Shaders/ExtendedMaterials/ExtendedMaterials.hlsli:1–114](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/features/Extended%20Materials/Shaders/ExtendedMaterials/ExtendedMaterials.hlsli#L1-L114); [M-A038 src/Features/TerrainHelper.cpp:20–102](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/TerrainHelper.cpp#L20-L102); [M-A039 src/Features/TerrainHelper.cpp:103–154](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/TerrainHelper.cpp#L103-L154); [M-A040 src/TruePBR.cpp:1683–1730](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/TruePBR.cpp#L1683-L1730); [M-A041 package/Shaders/Lighting.hlsl:400–438](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/Lighting.hlsl#L400-L438)

**Limits.** Existing native mesh TBN/storage decoding must be resolved before borrowing parallax/normal behavior. The source tests the address of EnableTerrain in DataLoaded, making that particular bLandSpecular change unconditional when called (M-X01).

### M-C10 — Distant terrain shadows

CS loads a world heightmap and computes sun-dependent shadow heights, then samples/interpolates them with an explicit self-shadow bias. This separate feature can produce distant mountain occlusion beyond native nearby cascades.

**Inputs/resources.** Heightmap path/world bounds, sun direction, time-jump synchronization and configurable enabled state. Own R16G16_UNORM shadow-height texture PS/CS t60; compute b0,SRVs/UAV; shader SelfShadowBias=256.

**Schedule.** EarlyPrepass updates shadows; ReflectionsPrepass rebinds them. Console/Papyrus/wait/sleep/travel events trigger synchronization.

**Map/query.** Inputs `I-C04`, `I-C06`, `I-C27`, `I-C28`, `I-C42`; frame `shadow-work`, `shadow-camera`, `reflections`; next `M-Q11`.

**Evidence.** [M-A042 src/Features/TerrainShadows.cpp:430–497](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/TerrainShadows.cpp#L430-L497); [M-A043 src/Features/TerrainShadows.cpp:568–633](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/TerrainShadows.cpp#L568-L633); [M-A044 features/Terrain Shadows/Shaders/TerrainShadows/TerrainShadows.hlsli:1–38](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/features/Terrain%20Shadows/Shaders/TerrainShadows/TerrainShadows.hlsli#L1-L38)

**Limits.** Its existence does not prove vanilla terrain casts equally distant or equally strong shadows. Native terrain caster filters, cascade distances and receiver rules remain the comparison target.

### M-C11 — Linear Lighting and color attribution

CS optional LinearLighting applies independently configurable power curves to diffuse/light/emissive/ambient/fog/alpha/sky/water/VL and adds brightness compensations. Common/Color names a power-1.6 conversion SkyrimGammaToLinear; that name alone is not evidence for native physical color space.

**Inputs/resources.** Settings default disabled; most gammas1.8, fog1.97, effect1.4, effect-alpha1.55; glowmap multiplier0.66 and effect-lighting multiplier0.32. Prepass reads native imagespace sunlightScale; geometry hook uploads emissive multiplier. Feature b6; Lighting PS b8 emissive geometry data. Color::DirectionalLight/PointLight multiply pi under optional linear mode; Fog and FogAlpha independently use configured powers.

**Schedule.** Prepass/SetupGeometry/per-shader operations; Effects11 active state disables this conversion and neutralizes its controls.

**Map/query.** Inputs `I-C02`, `I-C03`, `I-C05`, `I-C09`, `I-C12`, `I-C20`, `I-C35`, `I-C37`, `I-C45`; frame `hdr`, `apply`, `sao-fog`; next `M-Q06`, `M-Q07`, `M-Q08`.

**Evidence.** [M-A045 src/Features/LinearLighting.h:28–74](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LinearLighting.h#L28-L74); [M-A046 src/Features/LinearLighting.cpp:128–215](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LinearLighting.cpp#L128-L215); [M-A047 package/Shaders/Common/Color.hlsli:92–119](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/Common/Color.hlsli#L92-L119); [M-A048 package/Shaders/Common/Color.hlsli:180–267](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/Common/Color.hlsli#L180-L267); [M-A049 src/Features/LinearLighting.cpp:242–265](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LinearLighting.cpp#L242-L265)

**Limits.** Do not combine native UNORM evidence with these mod gamma names to declare a native linear/sRGB policy. No gamma/default/multiplier from this mod is approved as a retail parity fix.

### M-C12 — CS HDR tonemap and HDR display

The CS ISHDR replacement modifies the dark-range contrast curve, bloom/tonemap handling and HDR highlight mapping. HDRDisplay then independently composites UI and converts scene output to BT.2020/PQ with paper-white controls.

**Inputs/resources.** Native-like b2 IMGS inputs, mod b5/b6 HDR/linear settings and HDRDisplay output CB including isSceneLinear/applyAutoHDR. ISHDR b2 Flags/TimingData/Param/Cinematic/Tint/Fade and t0/t1/t2 inputs. HDRDisplay own FP16 intermediate/R8G8B8A8 UI, HDR R10G10B10A2 swapchain path; HDROutputCS t0 scene/t1 UI/u0 output/b0 settings. SetColorSpace1 chooses PQ/BT.2020 or G22/P709; no static HDR metadata is set.

**Schedule.** Tonemap replacement in image-space chain; HDRDisplay UI hooks and Present hooks run separately. HDR output can expand Effects11 SDR output through AutoHDR only when indicated.

**Map/query.** Inputs `I-C12`, `I-C13`, `I-C20`, `I-C45`; frame `hdr`, `effect-route`, `swapchain`, `backbuffer`, `end`; next `M-Q02`, `M-Q07`.

**Evidence.** [M-A050 package/Shaders/ISHDR.hlsl:23–40](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/ISHDR.hlsl#L23-L40); [M-A051 package/Shaders/ISHDR.hlsl:170–220](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/ISHDR.hlsl#L170-L220); [M-A052 src/Features/HDRDisplay.cpp:614–615](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/HDRDisplay.cpp#L614-L615); [M-A053 src/Features/HDRDisplay.cpp:649–710](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/HDRDisplay.cpp#L649-L710); [M-A054 src/Features/HDRDisplay.cpp:1650–1690](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/HDRDisplay.cpp#L1650-L1690); [M-A055 features/HDR Display/Shaders/HDRDisplay/HDROutputCS.hlsl:1–111](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/features/HDR%20Display/Shaders/HDRDisplay/HDROutputCS.hlsl#L1-L111)

**Limits.** The code comments explicitly identify contrast modifications. Using this shader as retail-reference math would erase that distinction.

### M-C13 — Effects11 postprocess intervention

Current CS contains an Effects11 preset execution layer. HandleTonemapRender bypasses the original tonemap call only when enabled, UseOriginalPostProcessing is false and ExecuteEffects actually writes output. It also has a separate mod volumetric-ray chain.

**Inputs/resources.** Preset/setting/weather managers, effect execution results, mod texture manager and ray parameters. Preset scratch targets include FP16/UNORM/R32 adaptation textures; raymarch uses half-resolution R16_FLOAT targets, separable compute blur and additive RGB ONE+ONE blend back to MAIN.

**Schedule.** Core HDRRender wrapper tests successful replacement; failure/menu routes retain original call. Volumetric rays run after CS deferred composite, gated separately.

**Map/query.** Inputs `I-C02`, `I-C12`, `I-C13`, `I-C14`, `I-C42`, `I-C45`; frame `hdr`, `effect-route`, `vl`, `composite-stage`; next `M-Q07`, `M-Q12`.

**Evidence.** [M-A056 src/Hooks.cpp:355–367](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Hooks.cpp#L355-L367); [M-A057 src/Features/Effects11.cpp:680–696](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/Effects11.cpp#L680-L696); [M-A058 src/Features/Effects11.cpp:800–842](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/Effects11.cpp#L800-L842); [M-A059 src/Features/Effects11.cpp:872–919](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/Effects11.cpp#L872-L919); [M-A060 src/Features/Effects11/TextureManager.cpp:15–40](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/Effects11/TextureManager.cpp#L15-L40)

**Limits.** Effects11 is CS source, not the ENB author's native binary implementation.

### M-C14 — Exponential height fog and near/far volumetrics

CS adds configurable exponential height fog and an optional volumetric density/light-scattering/integration pipeline. It can disable native fog or respect its fade; near/far accumulated fog is serially composed.

**Inputs/resources.** Mod fog density/height/falloff/color parameters, camera/temporal history, scene depth, copied directional shadows, LLF clustered lights, IBL and skylighting. Own near/far 3D density/scattering/history/integrated resources; PS t19 near/t22 far integrated fog; compute uses own b0 plus shared b5/b6 and native b12, local-light t35..37 and directional data t98.

**Schedule.** Prepass conditional on density/enabled/volumetric/extinction/suppression; depth reduction→material setup→light scattering→integration per volume, then PS binding. Shader returns near.rgb + near.a*far.rgb and near.a*far.a.

**Map/query.** Inputs `I-C02`, `I-C09`, `I-C14`, `I-C15`, `I-C38`, `I-C43`, `I-C45`; frame `sao-fog`, `vl`, `composite-stage`; next `M-Q08`, `M-Q12`.

**Evidence.** [M-A061 src/Features/ExponentialHeightFog.cpp:475–481](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/ExponentialHeightFog.cpp#L475-L481); [M-A062 src/Features/ExponentialHeightFog.cpp:563–610](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/ExponentialHeightFog.cpp#L563-L610); [M-A063 src/Features/ExponentialHeightFog.cpp:701–723](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/ExponentialHeightFog.cpp#L701-L723); [M-A064 src/Features/ExponentialHeightFog.cpp:725–816](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/ExponentialHeightFog.cpp#L725-L816); [M-A065 features/Exponential Height Fog/Shaders/ExponentialHeightFog/ExponentialHeightFog.hlsli:16–113](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/features/Exponential%20Height%20Fog/Shaders/ExponentialHeightFog/ExponentialHeightFog.hlsli#L16-L113)

**Limits.** A height-fog option is not evidence native Skyrim uses that height-fog law. Current source history weights were changed by the pinned commit; claims about older release behavior need a separate pin.

### M-C15 — Native volumetrics interception and shared volumetric shadows

CS changes the native volumetric descriptor quality/dimensions/enable flag and patches a native loop so its replacement raymarch iterates depth internally. VolumetricShadows adds a downsampled/blurred directional shadow SRV for transparent/effect receivers.

**Inputs/resources.** Native volume-size globals and descriptor accessors; own VL dimensions CB. Directional cascade matrices/end splits are copied from the native shadow light into a mod structured buffer. Volume-size source IDs AE414916/414919/414922; descriptor accessor107014/setter107016; CS b1 dimensions. Shared shadow map PS t18, directional shadow data t98.

**Schedule.** EarlyPrepass changes interior/exterior settings; AE107023 NOP patches at+0x406 and+0x4A9 alter dispatch loop. Shadow copying is triggered while preparing replacement Utility shadowmask technique.

**Map/query.** Inputs `I-C05`, `I-C14`, `I-C33`, `I-C35`, `I-C42`; frame `vl`, `shadow-work`, `apply`; next `M-Q12`.

**Evidence.** [M-A066 src/Features/VolumetricLighting.cpp:156–168](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/VolumetricLighting.cpp#L156-L168); [M-A067 src/Features/VolumetricLighting.cpp:176–237](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/VolumetricLighting.cpp#L176-L237); [M-A068 src/Features/VolumetricLighting.cpp:279–295](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/VolumetricLighting.cpp#L279-L295); [M-A069 src/Features/VolumetricShadows.h:18–20](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/VolumetricShadows.h#L18-L20); [M-A070 src/Features/VolumetricShadows.cpp:295–316](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/VolumetricShadows.cpp#L295-L316); [M-A071 src/Features/VolumetricShadows.cpp:365–377](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/VolumetricShadows.cpp#L365-L377); [M-A072 src/Deferred.cpp:541–582](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Deferred.cpp#L541-L582); [M-A073 src/State.cpp:170–182](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/State.cpp#L170-L182)

**Limits.** Loop patches and blur resources are mod replacements; descriptor names do not prove native loop/resource semantics on the target.

### M-C16 — Wetness and water additions

WetnessEffects derives rain/puddle state from current/last precipitation/weather transition data and binds precipitation occlusion. Its shader adds ripple/splash normals and a GGX wet-specular term. WaterEffects adds a caustics texture and a water parallax implementation; UnifiedWater replaces the LOD water/flow path when validated.

**Inputs/resources.** Weather/precipitation and feature settings; caustics DDS; UnifiedWater mesh/cache/flowmap/material producers. Wetness PS t70 precipitation occlusion; WaterEffects PS t65 caustics; own water cache/flowmap resources and shader material CRC override.

**Schedule.** Wetness frame data/Prepass, water shader sampling; UnifiedWater hooks world/cell/terrain attachment/displacement/material/setup, conditionally patches LOD branches. AE>=1.7.99 water-subvisibility ID523588 is selected explicitly.

**Map/query.** Inputs `I-C02`, `I-C07`, `I-C17`, `I-C27`, `I-C35`, `I-C37`, `I-C42`; frame `scene`, `reflections`, `apply`; next `M-Q11`, `M-Q12`.

**Evidence.** [M-A074 src/Features/WetnessEffects.cpp:851–957](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/WetnessEffects.cpp#L851-L957); [M-A075 features/Wetness Effects/Shaders/WetnessEffects/WetnessEffects.hlsli:16–128](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/features/Wetness%20Effects/Shaders/WetnessEffects/WetnessEffects.hlsli#L16-L128); [M-A076 src/Features/WaterEffects.cpp:1–23](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/WaterEffects.cpp#L1-L23); [M-A077 features/Water Effects/Shaders/WaterEffects/WaterParallax.hlsli:88–124](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/features/Water%20Effects/Shaders/WaterEffects/WaterParallax.hlsli#L88-L124); [M-A078 src/Features/UnifiedWater.cpp:76–102](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/UnifiedWater.cpp#L76-L102); [M-A079 src/Features/UnifiedWater.cpp:445–470](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/UnifiedWater.cpp#L445-L470)

**Limits.** Weather ownership/interpolation and original WATR CB producers remain native questions; none of these mod wetness constants are retail values.

### M-C17 — Dynamic cubemaps and image-based lighting

CS updates/infer cubemaps, computes specular irradiance and separately derives diffuse IBL. Those outputs feed its deferred composite and feature pixel shaders.

**Inputs/resources.** Scene capture/world position, current depth, native reflection target, optional sky/skylighting/IBL feature data. DynamicCubemaps PS t30/t31, own update/infer/specular/BC6H resources. IBL PS t76..t79 (conditional native/feature inputs), diffuse compute SRVs/UAVs and samplers.

**Schedule.** DynamicCubemaps update occurs before deferred composite; PostDeferred afterward. IBL prepass reads loaded feature resources and can be disabled by scene predicates.

**Map/query.** Inputs `I-C03`, `I-C10`, `I-C17`, `I-C21`, `I-C38`, `I-C45`; frame `reflections`, `composite-stage`; next `M-Q03`, `M-Q06`.

**Evidence.** [M-A080 src/Features/DynamicCubemaps.cpp:310–375](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/DynamicCubemaps.cpp#L310-L375); [M-A081 src/Features/DynamicCubemaps.cpp:391–422](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/DynamicCubemaps.cpp#L391-L422); [M-A082 src/Features/DynamicCubemaps.cpp:640–651](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/DynamicCubemaps.cpp#L640-L651); [M-A083 src/Features/IBL.cpp:270–342](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/IBL.cpp#L270-L342); [M-A084 src/Deferred.cpp:350–390](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Deferred.cpp#L350-L390)

**Limits.** Sampling original cubemaps does not prove the mod's diffuse/specular scale or normalization is native.

### M-C18 — Light Placer data/lifecycle interventions

Light Placer adds/reuses NiPointLight scene nodes from configuration, resolves base LIGH/override values and adds them to ShadowSceneNode. It also disables selected original lights through optional GenDynamic hooks. This is a scene-light producer, not a rendering shader.

**Inputs/resources.** Config-selected base light, RGB/radius/fade/size/cutoff/scale/flags, object/effect lifecycle and conditions. NiLight ambient.red stores bit-cast mod flags; green stores cutoff; blue bit-cast formID. radius.z carries scaled size. New diffuse/radius/fade and LIGHT_CREATE_PARAMS feed native AddLight. No direct D3D CB writer was found in these inspected producer functions.

**Schedule.** Load3D vfunc0x6A and reference-effect Init0x36; add/reattach/update/detach events, with background task queue. Prologue/call hooks use version-specific IDs. Defaults global radius/fade multipliers1.0.

**Map/query.** Inputs `I-C11`, `I-C15`, `I-C16`, `I-C35`, `I-C39`; frame `scene`, `accumulator`; next `M-Q10`.

**Evidence.** [M-A085 src/Hooks/Attach.h:9–83](https://github.com/powerof3/LightPlacer/blob/6b7c05894de17cb3e7f0b5169e82833c9244e1ed/src/Hooks/Attach.h#L9-L83); [M-A086 src/Hooks/Attach.cpp:42–79](https://github.com/powerof3/LightPlacer/blob/6b7c05894de17cb3e7f0b5169e82833c9244e1ed/src/Hooks/Attach.cpp#L42-L79); [M-A087 src/Hooks/Misc.cpp:1–24](https://github.com/powerof3/LightPlacer/blob/6b7c05894de17cb3e7f0b5169e82833c9244e1ed/src/Hooks/Misc.cpp#L1-L24); [M-A088 src/LightData.cpp:84–188](https://github.com/powerof3/LightPlacer/blob/6b7c05894de17cb3e7f0b5169e82833c9244e1ed/src/LightData.cpp#L84-L188); [M-A089 src/LightData.cpp:230–295](https://github.com/powerof3/LightPlacer/blob/6b7c05894de17cb3e7f0b5169e82833c9244e1ed/src/LightData.cpp#L230-L295); [M-A090 src/Settings.h:23–39](https://github.com/powerof3/LightPlacer/blob/6b7c05894de17cb3e7f0b5169e82833c9244e1ed/src/Settings.h#L23-L39)

**Limits.** The overlap between LP and LLF extension fields is a mod convention; do not reinterpret vanilla NiLight ambient fields as those flags without native evidence.

### M-C19 — Auto Parallax material mutation

Auto Parallax visits cloned geometry, optionally replaces default material with a parallax material when flags/textures allow it, loads a height texture and reinitializes geometry. Existing parallax/projection branches can clear flags.

**Inputs/resources.** Diffuse/normal path suffix discovery, texture resource presence, material feature/flags, clone object class. No direct D3D writer in inspected code; calls source AE105640 texture helper, changes material/heightTexture and kParallax/kBackLighting flags before native SetupGeometry/FinishSetupGeometry.

**Schedule.** Clone3D thunk calls original then UpdateMaterialParallax; this repo installs slot0x40 via its helper. STAT/movable static/container classes are selected.

**Map/query.** Inputs `I-C17`, `I-C18`, `I-C21`, `I-C25`, `I-C34`; frame `shader-prepare`, `apply`; next `M-Q04`, `M-Q11`.

**Evidence.** [M-A091 src/XSEPlugin.cpp:95–193](https://github.com/doodlum/skyrim-auto-parallax/blob/99f5c9e20f6ff327757fe793d1d66dca6b593a43/src/XSEPlugin.cpp#L95-L193); [M-A092 src/XSEPlugin.cpp:195–214](https://github.com/doodlum/skyrim-auto-parallax/blob/99f5c9e20f6ff327757fe793d1d66dca6b593a43/src/XSEPlugin.cpp#L195-L214); [M-A093 include/PCH.h:45–56](https://github.com/doodlum/skyrim-auto-parallax/blob/99f5c9e20f6ff327757fe793d1d66dca6b593a43/include/PCH.h#L45-L56)

**Limits.** The comment and existing-height-texture branch contradict each other (M-X02). CS TruePBR uses Clone3D slot0x4A in its pinned source; the class/layout/runtime reason for the discrepancy is unresolved (M-X03).

### M-C20 — Shadow Boost adaptive settings

Shadow Boost changes source-referenced fShadowDistance and object/item/actor LOD fade settings based on measured frame elapsed time and target FPS, then calls a shadow-distance update helper. It does not replace the shadow shader in the inspected code.

**Inputs/resources.** PerformanceCounter frame interval, target FPS/rate, min/max configured distances. AE391845 fShadowDistance; AE358960/358957/358954 object/item/actor LOD floats. Present vfunc8 calls Update; init hook AE77226+0x2BC installs the detour.

**Schedule.** Active Present wrapper drives Update; UpdateStart call hook stores frameStart. The second Main_Update_Render hook is commented out.

**Map/query.** Inputs `I-C25`, `I-C28`, `I-C29`, `I-C42`; frame `frame`, `end`, `shadow-work`; next `M-Q11`.

**Evidence.** [M-A094 src/D3D11.cpp:7–37](https://github.com/doodlum/skyrim-shadow-boost/blob/84531158c663e27fb7b57d985496f624163c1045/src/D3D11.cpp#L7-L37); [M-A095 src/ShadowBoost.cpp:99–137](https://github.com/doodlum/skyrim-shadow-boost/blob/84531158c663e27fb7b57d985496f624163c1045/src/ShadowBoost.cpp#L99-L137); [M-A096 src/ShadowBoost.h:147–170](https://github.com/doodlum/skyrim-shadow-boost/blob/84531158c663e27fb7b57d985496f624163c1045/src/ShadowBoost.h#L147-L170)

**Limits.** Elapsed interval is not a GPU shadow timing measurement. Native shadow distance gates/consumer addresses require independent tracing.

### M-C21 — Vanilla HDR is a replacement preset implementation

The Vanilla HDR repository includes an older game-like Vanilla function and a separate active shader with configurable tonemap/hue/adaptation/bloom/color grading. The repository name does not mean every output operation is retail vanilla.

**Inputs/resources.** b2 Params01 and b12 frame gamma/dynamic-resolution input, plus VanillaHDRSettings.fxh preset constants. TextureColor t1/Bloom t0/Adaptation t2; replacement color pipeline plus final log2→frame-gamma-multiply→exp2/dither output.

**Schedule.** ShaderTools wrapper includes the custom shader; source also supports ENB preset interfaces. No native C++ hook owner is present in the inspected repository files.

**Map/query.** Inputs `I-C12`, `I-C13`, `I-C20`, `I-C45`; frame `hdr`, `effect-route`, `end`; next `M-Q07`, `M-Q08`.

**Evidence.** [M-A097 Data/Shaders/ISHDRTonemapBlendCinematic/0.ps.hlsl:1–2](https://github.com/doodlum/skyrim-vanilla-hdr/blob/aa580b6f9f958e3460a83c118cee09e11f86f785/Data/Shaders/ISHDRTonemapBlendCinematic/0.ps.hlsl#L1-L2); [M-A098 Data/Shaders/ISHDR/ISHDRTonemapBlendCinematic.hlsl:142–193](https://github.com/doodlum/skyrim-vanilla-hdr/blob/aa580b6f9f958e3460a83c118cee09e11f86f785/Data/Shaders/ISHDR/ISHDRTonemapBlendCinematic.hlsl#L142-L193); [M-A099 Data/Shaders/ISHDR/ISHDRTonemapBlendCinematic.hlsl:455–485](https://github.com/doodlum/skyrim-vanilla-hdr/blob/aa580b6f9f958e3460a83c118cee09e11f86f785/Data/Shaders/ISHDR/ISHDRTonemapBlendCinematic.hlsl#L455-L485); [M-A100 Data/Shaders/ISHDR/VanillaHDRSettings.fxh:5–86](https://github.com/doodlum/skyrim-vanilla-hdr/blob/aa580b6f9f958e3460a83c118cee09e11f86f785/Data/Shaders/ISHDR/VanillaHDRSettings.fxh#L5-L86)

**Limits.** Older source/decompiler attribution is not equality with the target package; selected DXBC and native CB packing must corroborate it. Header credits/license are preserved privately; this audit provides source references and behavior, not republished shader code.

### M-C22 — ENB Light Patcher edits authored light records

The independently authored MIT-source Mutagen/Synthesis patcher halves positive placed-light FadeOffset for Candle/Torch/Camp names and halves matching LIGH FadeValue, while excluding ENB Light.esp. This is compensation for added mod lights in authored data.

**Inputs/resources.** Winning placed-object/light records and editor-ID name matching. Outputs a patch ESP; no runtime hook, shader or D3D producer.

**Schedule.** Offline patch generation only; it was inspected, not executed.

**Map/query.** Inputs `I-C15`, `I-C16`; frame `scene`; next `M-Q10`.

**Evidence.** [M-A101 ENBLightPatcher/Program.cs:20–68](https://github.com/tr4wzified/enblightpatcher/blob/56eac02f359fd7b097cf8d0d79909855ad76a27a/ENBLightPatcher/Program.cs#L20-L68)

**Limits.** This is not ENB Light author's private rendering implementation and cannot verify ENB binary behavior. Both a placed override and base light may be patched; effective retail/mod fade composition is a native question.

## Primary ENB and ENB Light documentation

The [SE author changelog](https://www.enbdev.com/mod_tesskyrimse_v0503.htm) describes SSR replacement, later water-only SSR, SSS eligibility changes, normal-mapping shadows, volumetric-ray desaturation and fog filters. Those are ENB changes. Its claims can orient native searches but cannot verify the original retail equations.

The [older Skyrim effect interface](https://enbdev.com/doc_skyrim_effect_en.htm) describes retaining original postprocessing through a mod control and separate bloom/adaptation/AO controls. The [AO documentation](https://enbdev.com/doc_skyrim_ssao_ssil_en.htm) describes fog-dependent AO fade. Their generic/older Skyrim scope is retained; this audit does not promote them to the pinned SE ABI.

The [ENB Light author description](https://www.nexusmods.com/skyrimspecialedition/mods/22574?tab=description) explains that added particle lights may require lower original-light intensity and describes screen-space/range limitations. M-C22 separately audits an independently authored source patcher implementing fade reduction. The asset description, that patcher and ENB rendering-core code are distinct evidence.

Direct HTML-body retrieval returned406/403. Primary browser-derived text was recorded privately and hashed as derived text. The [author normal-mapping shadows PDF](https://enbdev.com/NormalMappingShadows.pdf) was downloaded with SHA256 `8d700fecc49bfe1f837db67203838a0672d6ff5e7b38cb34a54416460140a512`; its algorithm is not promoted here because text extraction was unavailable. No ENB binary was downloaded or decompiled.

## Contradictions and unresolved source differences

- **M-X01 (confirmed_source_predicate_contradiction).** ExtendedMaterials settings/UI describe terrain parallax as optional; DataLoaded tests &settings.EnableTerrain. The address of an instance field is non-null, so this branch runs whenever DataLoaded is called, independently of its stored enabled value. bLandSpecular is set true when originally false. Treat bLandSpecular changes as mod side effects; do not infer native defaults or terrain behavior. Evidence: [M-A036 src/Features/ExtendedMaterials.cpp:6–25](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/ExtendedMaterials.cpp#L6-L25)
- **M-X02 (confirmed_source_comment_behavior_contradiction).** AutoParallax existing parallax material branch says only enable if the parallax file handle exists. Inside the non-null heightTexture/resourceStream branch, SetFlags(kParallax,false) clears the flag. No retail execution is claimed. A mod comment cannot establish original flag behavior or texture validity semantics. Evidence: [M-A091 src/XSEPlugin.cpp:95–193](https://github.com/doodlum/skyrim-auto-parallax/blob/99f5c9e20f6ff327757fe793d1d66dca6b593a43/src/XSEPlugin.cpp#L95-L193)
- **M-X03 (unresolved_cross_source_layout_difference).** AutoParallax hooks Clone3D at slot0x40; current CS TruePBR names Clone3D at slot0x4A for TESObjectSTAT. Both source helper/index paths were inspected. Different dependency/runtime/class layouts may explain it; the reason is not proved. Resolve each exact target vtable and ABI; do not choose a slot by name agreement. Evidence: [M-A092 src/XSEPlugin.cpp:195–214](https://github.com/doodlum/skyrim-auto-parallax/blob/99f5c9e20f6ff327757fe793d1d66dca6b593a43/src/XSEPlugin.cpp#L195-L214); [M-A093 include/PCH.h:45–56](https://github.com/doodlum/skyrim-auto-parallax/blob/99f5c9e20f6ff327757fe793d1d66dca6b593a43/include/PCH.h#L45-L56); [M-A040 src/TruePBR.cpp:1683–1730](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/TruePBR.cpp#L1683-L1730)
- **M-X04 (attribution_hazard_not_native_contradiction).** Names SkyrimGammaToLinear and Vanilla HDR suggest native transfer/retail behavior. CS hardcodes power1.6 helpers and optional separate gamma/compensation; ISHDR explicitly modifies shadow contrast; VanillaHDR preset chooses tonemap4/adaptive saturation+contrast and custom constants. Only pinned native shader operands plus native CB/resource producers can close original transfer and tone math. Evidence: [M-A045 src/Features/LinearLighting.h:28–74](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LinearLighting.h#L28-L74); [M-A046 src/Features/LinearLighting.cpp:128–215](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LinearLighting.cpp#L128-L215); [M-A047 package/Shaders/Common/Color.hlsli:92–119](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/Common/Color.hlsli#L92-L119); [M-A048 package/Shaders/Common/Color.hlsli:180–267](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/Common/Color.hlsli#L180-L267); [M-A049 src/Features/LinearLighting.cpp:242–265](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LinearLighting.cpp#L242-L265); [M-A050 package/Shaders/ISHDR.hlsl:23–40](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/ISHDR.hlsl#L23-L40); [M-A051 package/Shaders/ISHDR.hlsl:170–220](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/ISHDR.hlsl#L170-L220); [M-A097 Data/Shaders/ISHDRTonemapBlendCinematic/0.ps.hlsl:1–2](https://github.com/doodlum/skyrim-vanilla-hdr/blob/aa580b6f9f958e3460a83c118cee09e11f86f785/Data/Shaders/ISHDRTonemapBlendCinematic/0.ps.hlsl#L1-L2); [M-A098 Data/Shaders/ISHDR/ISHDRTonemapBlendCinematic.hlsl:142–193](https://github.com/doodlum/skyrim-vanilla-hdr/blob/aa580b6f9f958e3460a83c118cee09e11f86f785/Data/Shaders/ISHDR/ISHDRTonemapBlendCinematic.hlsl#L142-L193); [M-A099 Data/Shaders/ISHDR/ISHDRTonemapBlendCinematic.hlsl:455–485](https://github.com/doodlum/skyrim-vanilla-hdr/blob/aa580b6f9f958e3460a83c118cee09e11f86f785/Data/Shaders/ISHDR/ISHDRTonemapBlendCinematic.hlsl#L455-L485); [M-A100 Data/Shaders/ISHDR/VanillaHDRSettings.fxh:5–86](https://github.com/doodlum/skyrim-vanilla-hdr/blob/aa580b6f9f958e3460a83c118cee09e11f86f785/Data/Shaders/ISHDR/VanillaHDRSettings.fxh#L5-L86)
- **M-X05 (current_source_rejects_unversioned_absence_claim).** An unversioned assertion that current CS removed ENB Light-style particles is not supported by this checkout. M-C05 contains active detection, queue, conversion and culling code at the pinned commit. It does not prove installation, enablement or compatibility with every ENB Light asset. Keep feature-presence and active-runtime claims separate. Evidence: [M-A026 src/Features/LightLimitFix.cpp:899–1042](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LightLimitFix.cpp#L899-L1042)

## Native query queue

The following are proposed read-only traces, not completed queries. The JSON lists exact source IDs, existing native seeds and promotion gates.

- **M-Q01 / priority1.** Resolve CS LoadShaders/BeginTechnique IDs and actual stage setter call sites against target address library and original PE; trace descriptor lookup, original fallback, creation metadata and selected payload. Gates: Original call/jump bytes and vtable identity. Exact pinned DXBC keys and metadata offsets, without borrowing CS compile macros.
- **M-Q02 / priority1.** Trace target-init call arguments/descriptors/bind flags and actual RTV/SRV/UAV formats; compare original targets to each CS format change. Gates: Original bytes, resource descriptors and conditional initialization. Output view format and transfer operation separately.
- **M-Q03 / priority1.** Verify MainDraw/world start/blended-decals/first-person/precipitation call-site identities and native predicates before importing feature pass labels. Gates: Decode actual original transfer instruction and target. Trace native geometry groups and original pass order.
- **M-Q04 / priority1.** Trace Lighting SetupGeometry world/skin coordinate choice, point-light pack call, texture slots and original material flags/permutation producer; keep native TBN/streams connected. Gates: Original bytes around source patch sites, not already patched code. NIF/record producer→native material→selected package payload association.
- **M-Q06 / priority1.** Find original sunlight/DALC packing and all shared color/frame-gamma writers; compare resource numeric domain with shader operations. Gates: Original native producers and pinned selected shader instructions. Do not promote CS named gamma helpers or normalization scales.
- **M-Q07 / priority1.** Close native HDR preparation/tonemap shader selection, adaptation textures, bloom, IMGS/IMAD blend and final output math for the pinned package. Gates: PrepareRender native CB producer→exact keyed DXBC selected branch. Original saturation/contrast/tint/brightness/fade/adaptation operations; separate CS and VanillaHDR modified math.
- **M-Q08 / priority1.** Compare depth-fog/SAO composition, mesh fog and postprocess color transforms in selected pinned retail shaders and their native producer paths. Gates: Exact pinned key/instruction/CB connection. Trace native depth handling, sky exclusion and alpha separately from CS height/volume fog.
- **M-Q09 / priority2.** Separate native soft/rim/back/character lighting and snow-specific image-space behavior from mod screen-space diffusion and screen-space shadows. Gates: Native material flags/constants→exact selected PS operands and masks. Native snow/effect route and potential actual screen-space pass, if present.
- **M-Q10 / priority2.** Trace original TESObjectLIGH/REFR→NiPointLight radius/fade/attenuation/portal/shadow creation, plus effective placed/base-light fade composition; compare separately to mod overlays/particle extraction. Gates: Target-native constructor/call signatures and owning source fields. Do not assign mod bits14/15 or ambient extension meanings to vanilla structures.
- **M-Q11 / priority2.** Trace original terrain/LOD/water caster selection, cascade settings and fShadowDistance consumers; compare source parallax/height/LOD replacements without adopting them. Gates: Actual target setting address→writer/readers and original defaults. Original terrain/shadow/water route and finite cascade/filter distances.
- **M-Q12 / priority2.** Trace native VOLI descriptor/quality/enable/dimension producers and original generate/raymarch/blur/apply sequence; compare CS loop patch and shared-shadow inputs. Gates: Original bytes at AE+0x406/+0x4a9 and untouched dispatch-loop dataflow. Exact native CS/PS keys and volume density/color/history CB/resource producer chain.

## Coverage boundary

22 manually inspected intervention records carry101 bounded source anchors. The JSON also indexes561 literal hook/relocation references across the checked-out own source trees; those entries are navigation references, including explicitly marked commented lines. They do not assert that every reference is a rendering hook, every hook runs, or every mod path was semantically audited.

Additional CloudShadows, GrassLighting, Skin/Hair, TerrainBlending/Variation, SSGI, LOD and upscaling code remains available in the pinned source inventory. Only the anchored interactions are analyzed here. The next work is original-target producer/call/resource tracing followed by controlled retail captures. No native query, shader build or runtime measurement was performed for this audit.
