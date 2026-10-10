# Skyrim lighting and shadows implementation

Status: imported implementation contract, consolidated on 2026-10-09. The
uncommitted source snapshot came from thread
`68505759-f655-46eb-a471-4f079d8efff2` at HEAD `c39449b5`; the receiving worktree
was at HEAD `f40ca1df`. The [lighting and shading hub](lighting-and-shading.md)
owns the current plan and launch policy. The [runtime guide](skyrim-lighting-runtime.md)
records implemented coverage in metadata 26/world schema 9, retaining compatible
producer-25 native mesh outputs. This document preserves the original detailed
architecture and evidence gates; proposed names and staged requirements are not
an independent current work plan. Retail parity remains unverified.

Implement Skyrim's authored scene lighting and surface response so shadow depth
comes from the balance of direct light, directional ambient, material response,
and image-space processing. Replace the fixed scene lighting and radius-derived
lamp brightness with resolved game data. Preserve shadow visibility as a separate
input to each direct light.

The reference target is **unmodded Skyrim SE 1.7.104.0**, executable SHA-256
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
The original source-investigation baseline was
`c39449b5a8b6a62c7c3c43bb60164e8ba6911839`, Bevy 0.19, converter producer 24,
world database schema 7, and cell-cache version 3. Consolidation changes and the
current package contract are recorded in the hub.
Other executable versions and renderer mods require separate compatibility
profiles. The implementation may support a broader load order, but acceptance
must identify every plugin and override used.

This extends the [L1 material and color contract](color-pipeline.md). L1 owns
surface inputs and response; L2 owns environment lighting; L4 owns image-space
processing. L0 retail evidence gates parity acceptance, while synthetic tests and
implementation can proceed independently. The
[research report](../../research/skyrim-shadow-intensity-20261008.md),
[170-setting catalog](../../research/skyrim-shadow-settings-1.7.104.csv), and
[evidence index](../../research/skyrim-shadow-evidence-20261008.json) are the
behavioral references.

## 1. Evidence and support levels

Use these labels in code diagnostics, implementation reviews, and acceptance
reports. A loaded mesh or retained source flag does not establish shader support.

| Label | Meaning | Permitted claim |
| --- | --- | --- |
| Verified consumer | Selected retail shader or native instructions establish the operation | Implement that operation for the identified target and permutation |
| Source-assisted | Native evidence is combined with a pinned reconstruction or format schema | Implement behind a named compatibility rule; retain the evidence limitation |
| Proposed design | Mudcrab architecture, scheduling, storage, or test policy | Describe it as a project choice |
| Unresolved native behavior | The producer, upload, arithmetic, or runtime selection is untraced | Retain the inputs; mark the affected feature unsupported or approximate |

Verified consumers include ordinary direct-light visibility, the inspected
point-light attenuation, directional-ambient matrix evaluation, ordinary
normal-alpha specular masking, the inspected model-space normal path, and grass
visibility remapping. They do not establish all shader permutations, shadow-map
production or final display processing. [Subsequent environment tracing](../../research/skyrim-environment-preparation-20261009.md) establishes normalized byte transfer, the DALC affine stage and climate color-transition arithmetic. The combined world consumes active climate/GMST intervals when available. Live additive RGB, automatic weather selection, native light units and final sun scaling remain gated. The complete grass setting-to-buffer upload remains source-assisted.

Expose three rendering modes with separate support reports:

- `approximate`: the existing Bevy response, with its current compatibility
  corrections and explicit limitations.
- `authored-preview`: recovered ordinary response and weather preparation stages, with named assumptions, photographic display and reported material fallbacks.
- `native-diagnostic`: verified Skyrim response for supported permutations,
  with independently reported approximations for environment inputs, visibility
  production, and output processing.

Promote supported cases to `native-compatible` only after their retail gates
pass. Keep support separate for environment, material, shadow producer, AO, and
image-space processing. A mode switch must not turn an unsupported family into a
successful parity result. An ordinary interactive view may use an identified
fallback; strict acceptance must fail when that fallback is reachable in a
required case.

## 2. Consolidated implementation and remaining replacements

| Current owner | Current behavior | Remaining boundary |
| --- | --- | --- |
| `engine/src/app.rs`, `lighting_runtime.rs` | World sun/ambient initialize the approximate path; authored preview applies prepared weather coefficients and shared selection controls | Native unit mapping, trajectory, additive ambient and final dimmers |
| `engine/src/environment.rs`, `sky.rs`, `fog.rs` | Source-derived time/weather fog, sky and supported recovered fog paths; active climate/GMST intervals are available to world lighting | Automatic weather-transition selection, complete sky geometry and interior ownership |
| `engine/src/lights.rs`, `lighting_runtime.rs` | Existing Bevy light budget plus up to seven nearby source-derived local-light inputs for native surfaces | Per-draw/room selection, reference fades and retail local shadows |
| `engine/src/nif_material.rs`, `nif_material/native.rs` | Inspected ordinary native response, alpha prepass, support diagnostics, retained source dependencies and shared world fog finishing | Authored tangent-frame calibration and specialized shader families |
| `converter/src/material.rs`, `mesh.rs` | Lossless native surface metadata beside the glTF/PBR projection; retained source producer-25 native meshes survive metadata refresh | Complete live texture overrides and additional supported families |
| `engine/src/shaders/terrain.wgsl` | Shared repeating texture sampling followed by Bevy PBR with the recovered fog extension | Separately verified native terrain response |
| `engine/src/image_space.rs`, `environment_render.rs`, `color_pipeline.rs` | Optional recovered stable-record HDR/adaptation/bloom/grading; exclusive tone mapping and a separate photographic adapter | Animated IMGS/IMAD, native light calibration, sampler/target and final display transfer |
| `engine/src/skyrim_ini.rs`, `config.rs`, `lighting_settings.rs` | Typed settings provenance; world shadow defaults 8,000/2,048; selected requests map to the Bevy adapter | Retail producer semantics and remaining retained settings |
| `converter/src/esm/`, `engine/src/lighting_catalog.rs`, `world/database.rs` | Typed lighting snapshot, winning source provenance and combined environment catalogs | Remaining runtime activation/selection and unresolved record consumers |

The native response must bypass the existing roughness-to-GGX projection and
normal-alpha compensation. Those operations remain valid only for the Bevy
approximation. Apply the established normal convention exactly once in either
path; switching paths must not flip it twice.

## 3. Architecture and ownership

The following interfaces are proposed. Type and module names may change, but
their responsibilities and information boundaries must remain explicit.

```text
Winning ESM records + source NIF contracts + selected INI profile
    -> versioned converted records with source provenance
    -> environment/light/material resolution for a world generation
    -> immutable per-view lighting snapshot
    -> shadow depth maps and direct-light visibility
    -> native material response + directional ambient
    -> HDR scene composition, AO/fog and image-space processing
    -> one display encoding
```

| Proposed owner | Responsibility |
| --- | --- |
| `shared::lighting` | Versioned source/resolved contracts, provenance, coordinate and color-domain labels |
| `converter::esm` | Parse and publish winning records without substituting visual defaults |
| `converter::material` / `mesh` | Publish source shader parameters, semantic texture bindings, normals and tangent frames |
| `engine::environment` | Resolve interior inheritance or exterior weather/time into one environment snapshot |
| `engine::lights` | Resolve placed-light state and prepare native light parameters |
| `engine::nif_material` | Load native contracts, select supported permutations, preserve dependency lifetimes |
| `engine::native_lighting` | GPU bindings and common native response helpers |
| `engine::shadow` | Visibility producer backend, allocation, participation and diagnostics |
| `engine::color_pipeline` | Exposure/output-domain policy and native image-space integration |

Update a view's environment after camera-space selection and before render-world
extraction. Snapshot the resolved state, settings identity, world generation,
and participating lights together. A delayed database result from an old
generation must not change the current room's lighting. Streaming adjacent cells
must not make whichever cell loaded last own the global environment.

Environment selection is per view. The main camera and its reflection camera
share the same authored state and exposure identity; the reflection keeps its
own pose and shadow-view decisions. A menu or diagnostic camera must not overwrite
the world's environment. Unload light entities and associated shadow resources
with their owning cell/reference.

The consolidated lighting runtime owns an active-view cell context and updates
`CameraSpace` on accepted exterior crossings or explicit interior selection.
Resident interior cells are not proof that the camera occupies them. Keep
camera-space selection, environment generation, sky visibility and fog updates
coupled to the same accepted transition. The current `--shots` path skips interior worlds; add interior
navigation/capture support before the required retail-interior campaign.

## 4. Converted data contract

### 4.1 Source records and provenance

Publish source values before resolving their runtime combination. Every
record carries stable FormID identity, winning plugin identity, record version,
and sufficient source/configuration hashes to reproduce the conversion.
Preserve absent fields as absent. Explicit black, zero fade, zero emission,
unset flags, and inherited values must remain distinguishable.

| Record | Required retained inputs | Resolution responsibility |
| --- | --- | --- |
| WTHR | All relevant NAM0 time/color rows, four six-face DALC cubes, IMSP image-space links, transition and fog data | Time-of-day interpolation, weather blending, direct/ambient state |
| CELL | XCLL values, lighting-template FormID, field inheritance mask, XCLL directional ambient payload, image-space links, interior/sky-related flags | Effective room lighting and fog, rather than raw XCLL alone |
| LGTM | Ambient/directional/fog values, directional fade, DALC and related lighting parameters | Per-field template inheritance |
| IMGS | HDR/adaptation, sunlight scale, brightness, contrast, tint and other serialized parameters | Native image-space state after its equations are traced |
| IMAD | Own animation/duration/counts, channel tracks, timing and blend inputs | Stateful modifier evaluation; static snapshots do not prove animated support |
| LIGH | RGB, radius, flags, fade/dimmer, falloff exponent, cone/FOV and other serialized lighting fields | Native attenuation, runtime dimmer, light type and shadow request |
| REFR | XRDS, XLIG fade/FOV/end-distance/depth bias, record participation flags and transforms | Placement overrides applied to the winning base light |
| WRLD / CLMT / REGN | Environment links and selection/time parameters required by the chosen exterior | Weather/climate selection and time anchors |
| VOLI and fog-related records | Referenced volumetric parameters and authored fog inputs | Separate fog/volumetric support, with no implied native formula |
| NIF / TXST | Source flags/type, material scalars, texture semantics/overrides and frame convention | Shader permutation and per-surface participation |

Do not bake a single resolved day color into the database. The runtime needs the
source rows to change time/weather without reconversion. Do not derive an ambient
cube from the scalar ambient color: they are distinct authored inputs.

Original parser prerequisites, retained as the data-integrity requirements for this contract. Current coverage is reported in the runtime guide:

- Preserve the header's form version in `RawRecord`; the original reader parsed
  it and then dropped it. The consolidated reader retains it. Use versioned layouts for weather and other
  affected records rather than inferring support from one payload length.
- Parse CELL `DATA` flags independently from record-header flags. The legacy
  `cells.flags` export contains the latter and cannot supply Interior/ShowSky/
  UseSkyLighting semantics; the typed lighting snapshot retains DATA flags separately.
- Iterate repeated WTHR `DALC` subrecords. `SubrecordView::find` returns the first
  match and cannot read all four ambient cubes.
- Extend FormID remapping for required weather image-space/volumetric arrays,
  including WTHR IMSP and HNAM where applicable. Apply version-aware decoding
  before remapping and test links across plugin masters.
- Expand runtime light queries beyond radius/RGB/flags. The database already
  stores fade/falloff fields that `LightRow` does not expose.

Publish a lossless `NativeSurfaceV1` beside the glTF projection. It must contain
raw exponent/glossiness, specular RGB/strength, emission RGB/multiplier, both
shader flag words, shader family/type, alpha-property bits/reference, enabled
vertex channels, UV transform/wrap, normal encoding/frame provenance and an
ordered semantic texture list. Each texture entry carries source slot, semantic,
channel use, transfer policy, runtime identity, and missing/pruned status.
Do not invert the GGX roughness approximation or recover native intensity from
clamped glTF factors.

### 4.2 Database and cache publication

Add structured tables or versioned payloads for weather, climates/selection,
lighting templates, cell lighting, image spaces/modifiers, and extended light
data. The implementation must document the final DDL in
[the database schema](../converters/db-schema.md). Numeric fields must retain
their original precision; opaque fields may be retained with a versioned layout
until decoded. A successful export must not silently drop a required unknown
subrecord.

Use nullable source columns and raw inheritance masks. Runtime resolution may
cache effective cell lighting, but that cache must include the winning-record
and resolver-version identities. Reference overrides belong to REFR rows, not
the shared LIGH row. Keep shader texture semantics separate from NIF array
indices, TXST indices, and GPU binding slots.

The consolidated publication uses metadata 26/world schema 9, retaining
compatible native mesh outputs with their producer-25 identities. The original
requirement was to allocate above producer 24/world 7. Future contract changes
need new version identities. Bump the cell cache only if its serialized contract changes.
Coordinate this with the launcher, runtime, `world-inspect`, manifest validation,
LOD producer checks, snapshots, and asset-closure tools. Do not relabel old output
with the new version. Rebuild affected GLBs/world databases and invalidate any
LOD payload that embeds changed material or frame data. Reuse unrelated outputs
only when their source/configuration/output checks still hold.

Older non-LOD packages may load in approximate mode under the existing
compatibility rules. Packages with `lod-manifest.json` currently require exact
latest producer/world versions in `shared::world_assets`; after the version bump,
older LOD packages need a verified rebuild. A mode switch cannot bypass that
startup check. Backward validation of the immutable producer-24/world-7 LOD
contract would be a separate implementation change. Native diagnostics must
identify the missing contract and reject strict tests that need it. Never fill a missing native column with the old fixed blue
ambient and report that as resolved Skyrim data.

The first typed schema should expose these proposed records rather than a
single opaque environment JSON with undocumented field semantics:

| Proposed table or payload | Required field groups |
| --- | --- |
| `weather_defs` | NAM0 color group/time slot, fog day/night near/far/power/max, transition/flags, four IMGS links and version-applicable VOLI links |
| `directional_ambient_sets` | Owner/role/time slot; source faces X+/X-/Y+/Y-/Z+/Z-; separate specular RGB and final raw scale/Fresnel float |
| `cell_lighting` | Header flags and DATA flags separately, LTMP/XCIM/XCCM/XCLR links, XCLL colors/fog/fade/rotation values and inheritance mask |
| `lighting_templates` | LGTM lighting/fog/fade fields and its separate DALC payload; retained unknown fields |
| `image_spaces` | Named HDR/adaptation floats, cinematic saturation/brightness/contrast, tint amount/RGB and optional depth-of-field data |
| `image_space_modifiers` and keys | Duration/counts and channels with separate multiply/add tracks, times and ordered key identities |
| Expanded `lights` | Existing color/radius/flags plus fade, falloff, FOV/near clip, flicker period/intensity/movement and retained source fields |
| `placed_light_overrides` | Presence/value for XRDS and XLIG components, enable-state metadata, header flags and base reference |

Decode known serialized layout variants by type/tag/form-version/length.
Malformed layouts and non-finite active values must fail with plugin, FormID,
record/tag, version and payload-length context. Do not
read a C++ in-memory struct as the plugin layout, interpret a padding/unused
ambient block as the active cube, or guess the final DALC float's native use.
Preserve LGTM's separate DALC rather than substituting its embedded unused
lighting-data block. Include remapping and target-type checks for consumed room
links `REFR.LNAM -> LGTM` and `REFR.INAM -> IMGS`, which were absent from the
original remapper and are included in the consolidated record-specific handling. Tag handling must be record-specific: CELL/WTHR opaque LNAM and IMGS
HNAM floats must not be treated as these links. CELL `XCCM` targets REGN and
`XCLR` lists regions; WRLD owns the direct climate link. Missing, null FormID, unresolved link and wrong target type
must produce distinguishable diagnostics.

Complete exports must rebuild their lighting tables from the winning snapshot
in the same transaction. Current insert-or-replace behavior alone can retain
rows deleted from a later load order. Subset/annotation tools must not clear
unrelated rows. Use fresh staged publication for the complete package and test
override/deletion cleanup explicitly. Load the environment catalog through the
existing database worker; no plugin parsing or SQLite queries on the render
thread.

### 4.3 Environment resolution

An interior resolver must apply the cell's inheritance mask **per field**.
Resolve the winning CELL, its LTMP template, and the fields selected for
inheritance; report the source of each effective value. Bannered Mare is a
required retail fixture because its stored cell ambient is inherited and cannot
be treated as effective ambient.

The serialized inheritance groups are Ambient Color, Directional Color, Fog
Color, Fog Near, Fog Far, Directional Rotation, Directional Fade, Clip Distance,
Fog Power, Fog Max and Light Fade Distances. Preserve paired values within a
group. Native coupling between ambient-color inheritance and the cell's ambient
cube/specular/final float remains part of G2; retain both candidate sets until
traced. Do not invent a separate cube-inheritance bit. Interior directional
rotation fields need their own sign/unit/order trace and must not automatically
use the REFR Euler convention.

Resolve image-space identity separately from its equations. A parsed IMAD is not
an active modifier: maintain explicit active instances with strength, elapsed
time, lifetime and order. Script/spell/trigger activation and native stacking
require their own evidence. Room-marker lighting overrides require containment
and room-link support; loading a marker must not change the whole view.

An exterior resolver must take explicit worldspace, climate/weather selection,
time, current and next weather, and transition state. Resolve sunlight,
directional ambient, sky, fog, and image-space inputs from a shared snapshot.
The correct time anchors, blend weights, sun direction, and encoded-versus-linear
interpolation must be traced before claiming native environment parity. Until
then, expose deterministic diagnostic time/weather inputs with a named resolver
approximation; a fixed clear-day palette is insufficient for native acceptance.

Interiors that request sky/exterior lighting need their own verified selection
rule. Do not equate every `Interior` cell with an unconditional zero sun and zero
fog. Preserve the flags and reject unsupported combinations in strict cases.

## 5. Units, coordinates and color

### 5.1 Coordinate contract

The world currently renders one Creation unit as one runtime unit. Radii,
shadow distances, fog distances, and biases with distance units must respect
that convention. Do not pass source distances through an API that assumes metres
without a documented conversion. Native RGB light coefficients are not Bevy
lumens or lux merely because both are floating-point values.

Use `shared::coordinates` consistently:

```text
Creation -> runtime: (x, y, z) -> (x, z, -y)
runtime -> Creation: (x, y, z) -> (x, -z, y)
```

Apply the same basis to sun/light directions, model-space normals, authored
tangent frames, and ambient orientation. Derive any matrix basis change from
its input/output spaces and row/column convention; do not swizzle its color
channels. Account for reference rotation, skin deformation, nonuniform scale,
and mirrored transforms in the relevant normal path. Origin rebasing changes
positions, not directions or the environment's ambient coefficients.

### 5.2 Color and energy contract

Preserve role-specific L1 texture transfer: diffuse/glow color textures decode
through an sRGB view once; normal and scalar data/mask channels use linear views;
texture alpha stays data. The existing colored specular projection uses an sRGB
color view and must remain distinct from a scalar mask. Recover the native
model-space red-mask resource format under G8 rather than assuming its view
matches the compatibility tint texture. Preserve signed raw material parameters even when an
approximate glTF representation clamps them.

For record colors, retain the raw bytes and label each conversion policy.
The inspected weather NAM0 and DALC preparation blends bytes and multiplies by
`f32(1/255)` without sRGB decoding. Apply that recovered stage to weather
lighting coefficients. It does not establish final sunlight scaling, interior
color transfer, LIGH dimmer preparation, or image-space tint arithmetic. The
current use of `Color::srgb_u8` elsewhere is not evidence for those stages.
Keep their preview policies explicit under G1/G4/G7. No per-scene exposure fit
may conceal a wrong transfer.

Separate native light coefficients from Bevy photometric intensity. The native
shader receives compatible RGB coefficients and dimmers directly. Bevy light
components may remain for culling or provisional visibility production, but
their inverse-square/intensity conversion must not also feed native lighting.

### 5.3 Directional ambient

The inspected retail shader evaluates:

```text
ambientRGB = DirectionalAmbientMatrix * float4(shadingNormal, 1)
```

Represent the matrix as three explicit RGB-output rows with four coefficients
per row, plus a declared normal coordinate space. Test the upload layout against
the CPU reference. Preserve precision/sign until the traced shader applies its
actual clamps.

The [recovered DALC preparation and matrix upload](../../research/skyrim-environment-preparation-20261009.md)
establish byte/255 transfer, stored-face order, signs, and affine rows. For each
RGB channel, Creation-space coefficients are
`daxis=(minusFace-plusFace)*0.5` and
`c=(sum(sixFaces)+G)*f32(1/6)`. Engine rows are `[dx,dz,-dy,c]`.
The serialized positive-axis face consequently corresponds to the opposite
cardinal input normal. Do not replace this stage with a matching-face blend,
spherical-harmonic fit, or extra NAM0 ambient multiplier.

The live additive RGB term `G` and interior CELL/LGTM cube selection remain
unresolved under G2. Authored preview may use the recovered affine stage with
`G=0` and a reported interior selection policy. Retain both the original six
faces and the resulting coefficients in diagnostics, including endpoint
residuals and the minimum over unit normals.
Capture equal-face cubes, six isolated face impulses, asymmetric mixtures,
scalar ambient changes with fixed cube, specular/final-float changes, inheritance,
time/weather transitions and rotated tangent/model-space objects. Match native
constant rows and cardinal/diagonal normal samples before accepting a builder.

## 6. Native material response

### 6.1 Shader integration

Implement a dedicated native material/fragment response for supported converted
NIF surfaces. Bevy's mesh/asset machinery may be reused. A `MaterialExtension`
is acceptable only if its fragment path replaces the incompatible lighting
evaluation; changing roughness or reflectance before calling Bevy PBR does not
implement Skyrim's diffuse/specular response.

Use named semantic bindings for albedo, tangent/model-space normal, ordinary
specular mask, separate model-space specular map, glow and environment inputs.
Use feature keys derived from retained source type/flags, not texture filenames.
Record the selected native shader implementation and support status per material.

Preserve `NifSourceMaterial`'s strong source dependency and the scene-only load
contract when replacing render bindings. Loading a native material must not make
textures appear ready only while an unrelated root glTF handle is retained.
Keep alpha/discard, UV transform, wrapping, double-sided behavior and dependency
validation from the existing material contract.

Consolidated streaming validation and collision construction preserve the
retained StandardMaterial source dependency while handling the recovered-fog
and native render bindings. `NativeSourceSurface` retains support, texture,
alpha and provenance inputs across mode changes and scene clones. Strict
injected mode validates native support and required textures; scene cloning
registers the added material/component types. Keep this material-independent
readiness contract when adding future bindings.

The dedicated material must implement `alpha_mode`, culling specialization and
its own `prepass_fragment_shader`. Add `prepass_vertex_shader` when authored
frames or deformation change its vertex contract. Stock PBR prepass code reads
`StandardMaterial` uniforms/textures and cannot use the native binding-0 layout.
Share native alpha, UV and deformation logic between visible, prepass/depth and
shadow variants; the existing vertex-alpha patch is not sufficient for a new
material layout.

### 6.2 Proposed GPU contract

Start with a dedicated forward native material. A fragment that writes Bevy's
standard deferred G-buffer still invokes its standard deferred response; native
deferred lighting needs a distinct G-buffer and lighting pass. Reject unsupported
deferred selection in the first native implementation.

Bevy 0.19 binds view group 0, view binding array group 1, mesh group 2 and
material group 3. Keep the first native resources in group 3 and reuse appropriate
view/mesh resources. An added group 4 needs a new pipeline layout, draw command
and compatible device limits; WGSL declarations alone do not bind it.

The following proposed binding layout applies to a standalone native `Material`
and is a starting point for a small renderer probe. It is not an already compiled
integration. An `ExtendedMaterial<StandardMaterial, ...>` prototype must choose
nonconflicting extension bindings, such as 100 and above, because the base
material already occupies the low bindings:

| Group 3 binding | Resource |
| --- | --- |
| 0 | Native surface uniform |
| 1 / 2 | Diffuse texture / sampler |
| 3 / 4 | Normal texture / sampler |
| 5 / 6 | Separate model-space specular texture / sampler |
| 7 | Read-only draw-lighting storage table |

Pack Rust/WGSL fields with `ShaderType`, explicit vec4/uvec4 alignment and tested
stride. The surface uniform retains UV offset/scale, raw specular RGB/strength,
exponent, alpha cutoff, emission RGB/multiplier, feature bits and channel modes.
Optional neutral bindings need presence bits and a named fallback rule; their
existence does not prove the native missing-texture default.

Proposed draw-lighting rows contain:

| Field | Contract |
| --- | --- |
| Ambient rows | Three vec4 rows and the declared input normal space |
| Sun | Common-space direction, resolved RGB and per-view shadow selection |
| Local lights | Selected position/radius, RGB/dimmer, shadow index/channel and type |
| Effective material inputs | Specular LOD fade, constant emission/IBL, effective alpha |
| Space transforms | World/common-space positions and the distinct model-normal branch |
| Grass inputs | Clamp/output scale in the grass row contract, not ordinary opacity |
| Counts and identity | Checked light counts, source/snapshot generation and support flags |

Bevy 0.19's buffer types are `ShaderBuffer`/`GpuShaderBuffer`. A proposed bridge
uses a shared storage table indexed through `MeshTag` and
`mesh_functions::get_tag(instance_index)`, with a specialized vertex output that
carries the instance index. An indirect instance index is not a persistent
FormID or row identity. Allocate stable checked row IDs, retire them after
in-flight GPU use, and reject invalid/stale rows rather than reusing old lighting.

For the first bridge, restrict rows to view-independent world/model-common
inputs, one asserted native sun and a shared main/reflection environment. Use
stock per-view view/shadow bindings for camera position and cascades. Mark
simultaneous views with different environments unsupported until the selection
mechanism is implemented. `MeshTag` identifies a mesh row; stock `View` has no
arbitrary view/table selector. A view dimension alone cannot select another row.

To expand support, provide a shader-visible view selector with a compatible view
binding, or a custom per-view material/table binding and draw command. The GPU
bridge probe must prove how the active view selects its data. Main/reflection/
menu views must never overwrite one row with incompatible spaces or shadow
indices. Prepare local render-world light indices after Bevy's
`prepare_lights`, using `GlobalClusterableObjectMeta.entity_to_index` where the
provisional backend permits. Define one upload owner so a later asset upload
cannot overwrite the accepted snapshot. Directional indices/cascades are
view-local; assert the chosen native sun instead of assuming a global index.

Use Bevy 0.19's `Core3d` ECS render schedules and `Core3dSystems` for additional
passes, not the older `Node3d` render-graph API. Prove the proposed storage,
indexing and shadow-resource bridge with a small GPU fixture before expanding
the renderer. Add extra native bindings and passes deliberately as the supported
material/producer set grows.

### 6.3 Verified ordinary response

The initial reference is retail `Lighting` pixel permutation `0x6201`: base
technique NONE with vertex color, specular, directional shadows and deferred
shadows. Implement its verified operations first. The following equations
describe selected terms; they are **not a complete final-pixel equation**:

```text
N = normalized shading normal
L = unit vector from the shaded point toward the relevant light
V = unit vector toward the view
Hsun = normalize(Lsun + V)
Hj = normalize(Lj + V)

sunMaskedRGB = sunRGB * sunVisibility
pointMaskedRGB[j] = pointRGB[j] * pointVisibility[j]
pointAttenuation[j] = 1 - saturate(distance[j] / radius[j])^2

directDiffuse = sunMaskedRGB * saturate(dot(N, Lsun))
              + sum(pointMaskedRGB[j] * pointAttenuation[j]
                    * saturate(dot(N, Lj)))
ambient = ambientMatrix * float4(N, 1)
diffuseAccumulator = directDiffuse + ambient + emissionTerm + optionalIBLTerm
diffuseResponse = albedoRGB * vertexRGB * diffuseAccumulator

sunSpecular = sunMaskedRGB * saturate(dot(N, Hsun))^nativeExponent
pointSpecular = sum(pointMaskedRGB[j] * pointAttenuation[j]
                   * saturate(dot(N, Hj))^nativeExponent)
specularResponse = (sunSpecular + pointSpecular)
                 * amplitudeMask * specularLodFade * effectiveSpecularRGB
```

Visibility scales the corresponding light RGB before diffuse and specular.
An unshadowed light has visibility one. A receive-shadow exemption bypasses that
visibility for the appropriate surface; it does not turn off the light.
Keep point distance attenuation in the light's own direct response.

The inspected exponent is `cb1[4].w` in both sun and local terms; no extra
exponent multiplier appears. Effective specular RGB is `cb1[4].rgb`, and the
mapped LOD fade is `cb2[3].y`. Ordinary amplitude is normal alpha. The CPU mapping
of raw specular RGB/strength/glossiness into these effective constants remains
source-assisted until its upload is checked. Complete those producers, final
channel additions, fog and output clamps before accepting the full native
material. The half-vector lobe must not be substituted with GGX. Normal alpha
in this ordinary path scales direct specular amplitude; it must not change cast-shadow visibility or become
a diffuse/ambient opacity mask.

Trace per-draw local-light selection, ordering and count/shadow limits under G4.
Do not replace a selected native light list with every Bevy clustered light.
The selected pixel shader does not establish the CPU selection policy.

The inspected constant emission and IBL additions enter the diffuse accumulator.
That does not establish every glow shader's behavior. Preserve the authored glow
contract and validate each selected emission permutation independently before
changing its response. Do not apply the existing Bevy emission exposure policy
to a native term without tracing its output domain.

The native path must expose separate buffers/debug outputs for direct diffuse,
directional ambient, direct specular, emission/IBL, and visibility. The displayed
shadow contrast must emerge from their composition. Do not introduce a global
shadow-opacity multiplier to fit a screenshot.

### 6.4 Normal paths

For tangent normals, retain RGB/alpha as linear data. Decode the signed normal,
apply the appropriate tangent frame, and normalize. Select orientation from the
frame convention. The current sampled-Y flip compensates the regenerated Bevy
frame; an authored native T/B frame may require no sampled flip. The retail
ordinary decode itself does not unconditionally invert green. Apply each chosen
correction once. Publish authored NIF tangent/bitangent data when available;
regeneration must be a reported fallback. Test mirrored UVs, negative reference
scale, nonuniform scale, and deformed frames separately.

The inspected tangent path decodes all RGB as `2 * sampleRGB - 1`; do not
reconstruct a positive Z from XY. Preserve authored T/B/N independently when
the source provides them; `cross(N,T)` is not automatically the authored
bitangent. Add custom mesh attributes and matching vertex/prepass outputs if
the standard glTF tangent representation loses required frame data.

For the inspected model-space permutation `0x6205`, the signed normal decode
uses XZY and the specular mask is the red channel of a separate GPU texture
binding. This inspected branch uses the decoded normal without a final
normalization; always normalizing changes nonunit texels. Match its model/common-
space draw branch and the corresponding light, view and ambient transforms.
Do not apply a generic world-normal transformation on top of an already
model-relative constant set. Do not treat it as a
tangent normal, apply the tangent Y correction, or use normal alpha as the
ordinary specular mask. Recover the source texture-to-GPU binding mapping before
accepting retail assets; GPU slot 2 does not establish NIF texture-array slot 2.

Start strict reference probes with rigid transforms and positive uniform scale.
Nonuniform/mirrored transforms and skin deformation need independently evidenced
vertex-path fixtures before acceptance. A mathematically suitable inverse-
transpose transform is a design choice until the selected native branch is
checked. Test nonunit model-space texels to detect an accidental normalization.

Keep geometric and shading normals available separately. The shading normal
steers the material response and directional ambient. Cast-shadow geometry and
depth comparison must not be distorted by a normal texture merely to reproduce
material darkening. The exact use of normals for native shadow bias and AO
belongs to their own research gates.

### 6.5 Material support inventory

| Path | Initial implementation target | Remaining acceptance work |
| --- | --- | --- |
| Ordinary opaque/cutout | Verified response plus source alpha/UV/vertex channels | CPU constants, authored tangent frames and retail draws |
| Model-space normals | Distinct decode, transforms and specular binding | Source binding mapping and matched model-space assets |
| Glow/emission | Preserve raw source inputs; implement selected permutations | Native exposure/composition and animated controllers |
| Environment/eye environment | Retain cube/mask/Fresnel inputs | Native reflection/specular behavior and target-version bug status |
| Grass | Separate visibility floor and native grass response | CPU upload, grass permutation inputs and animation |
| LAND/terrain | Shared environment and visibility interfaces | Native layer normal, diffuse/specular and blending equations |
| Trees/LOD | Preserve specialized flags and participation | Native alpha fade, animation and lighting |
| Skin/hair/snow/rim/backlight | Explicit family dispatch and retained inputs | Selected retail shader/CPU recovery for each family |
| Effects/parallax/water | Preserve existing identified approximations | Separate native paths; no inferred height self-shadowing |

An unsupported specialized path must not pass because its base diffuse texture
renders. Keep the [compatibility inventory](../../lighting-l1-compatibility.md)
updated as each native path becomes supported.

## 7. Shadow production and visibility

### 7.1 Producer interface

Separate light visibility production from native lighting consumption. A
provisional backend may reuse Bevy directional/point depth maps and evaluate
visibility with Bevy filtering. Label it approximate, even when the downstream
material equation is verified. Add a native producer after matching Utility
shader permutations, CPU constants, filtering and update rules are recovered.

The consumer contract returns a linear scalar visibility in the ordinary 0–1
domain per direct light. Use a non-sRGB GPU representation. The inspected retail
consumer reads the directional mask's red channel and selects channels for
shadowed local lights. Recover the producer's channel assignment and lifetime
before implementing a literal retail mask layout; do not alias different lights
to one component without its allocation rule.

An internal per-light visibility abstraction is a proposed implementation choice.
Tests must exercise binding selection as well as arithmetic, so correct scalar
equations cannot hide a wrong local-light channel.

For the first forward backend, sample valid per-light depth resources directly;
a screen-sized mask is not required merely to imitate retail texture slot 14.
A later native deferred mask needs its own depth reconstruction and allocation
metadata. A frontmost opaque-depth mask cannot automatically serve a transparent
surface behind or in front of that depth; transparent visibility needs an
explicit per-surface design.

Schedule shadow/depth production before native receiver rendering. A native
visibility or AO pass must run after its required depth/shadow inputs and before
their consumers, using the actual Bevy systems' ordering. Preserve render layers
and their light/receiver intersections as part of participation.

### 7.2 Participation and alpha

Resolve cast and receive decisions separately from source NIF flags, light type,
reference flags, and supported INI participation settings. Preserve the raw flags
and report which rule produced the decision. Trace ambiguous flag combinations
before treating their names as sufficient behavior.

Honor the same alpha/discard calculation in color, depth/prepass and shadow
passes. Alpha-tested leaves, fences and holes must cast their cutout silhouette.
Preserve double-sided/culling semantics in all relevant passes. Alpha-blended,
additive and specialized surfaces need explicit native shadow rules; they must
not inherit opaque shadow participation by accident.

A material that does not cast may still receive. A material that does not
receive may still cast. A fully occluded direct light may leave ambient,
emission and other lights visible. A per-reference depth bias must not scale
material brightness.

### 7.3 Directional maps

Resolve directional range, near slice, split count/overlap, map resolution,
focus-shadow parameters, filter choice, bias and sun-update controls from the
selected compatibility profile. The earlier four Bevy cascades fitted to the
unload grid were an approximation. The combined world now keeps its 8,000-unit
default range independent of the streamed grid, with a 2,048 map; this still does
not establish the native split algorithm or filter.

Require enough streamed caster geometry for the chosen shadow view. Camera
visibility/HZB culling must not remove a caster whose shadow reaches a visible
receiver. Geometry outside the main camera frustum can cast a visible shadow.
Record any shadow range reduced because the streamed world cannot supply it;
strict parity must not silently accept that reduction.

Match native projection/depth conventions, range fade, cascade/focus blending,
filter taps/footprints, and update stabilization under G5. The native reads of
`fShadowBiasScale` and `fShadowDirectionalBiasScale` are verified, but the partial
bias chain is insufficient to copy their values into Bevy's normal/depth bias
fields. Add a documented translation only after the complete chain is traced.

### 7.4 Local maps and placed lights

Resolve LIGH and REFR values before choosing omni, spot or hemispherical shadow
behavior. Enable shadows for supported lights that request them; keep intentionally
unshadowed lights unshadowed. A hemisphere or spotlight must not silently become
an omnidirectional point light in an accepted case.

Replace radius-based brightness normalization with native RGB/dimmer and the
verified direct attenuation. Trace the combination of LIGH fade, reference fade,
runtime dimmer, exponent and distance/end-distance controls under G4. Radius is
reach; it must not be the only source of intensity. Existing safety caps and
invalid-override fallbacks are project policies until their native handling is
established. Report them as such.

Treat enabled-light and shadow-map budgets separately. A selected light without
an allocated shadow map must report its degraded visibility status. Do not
quietly keep its bright direct term and present the frame as native-compatible.
Any approximate ranking/hysteresis policy must be deterministic for diagnostics,
with FormID as a stable tie breaker, and recorded with the captured frame.

Recompute shadow transforms after origin rebasing. Despawn/unload must release
light and map ownership without leaking stale visibility into a reused slot.
Reflection rendering must use visibility valid for its view/receiver positions.

Retain negative-light and enable-state inputs. A negative native contribution
needs a signed supported path or an explicit unsupported status; permanently
skipping it is an approximation. Off-by-default state must ultimately follow
reference/runtime enable state. Preserve flicker and Doesn't Light Water/Landscape
flags without claiming their native effect until the relevant branches are
traced. Field names such as FOV/fade Offset do not establish whether native
combination is additive or multiplicative.

The inspected ordinary shader bounds its selected locals at seven and shadowed
locals at four. Trace CPU selection and any additional-pass scheduling before
interpreting those as global scene limits. Strict first-pass tests reject list
overflow; deterministic truncation is an identified approximation.

## 8. Grass and terrain

The inspected retail grass arithmetic is:

```text
Sgrass = S + (1 - S) * clampValue
grassDiffuse = outputScale * albedo * vertexRGB
             * (directGrassLighting * Sgrass + ambientGrassLighting)
```

`Skyrim.ini [Display] fShadowClampValue` has a stored initializer of 0.3. In the
ordinary interpolation domain, visibility 0, 0.5 and 1 produces factors 0.3,
0.65 and 1. These are direct-light factors, not final pixel brightness. Apply the
floor once in the grass shader, after obtaining visibility. Do not apply it to
ordinary meshes, their specular mask, terrain, or the visibility texture itself.
Retain the existing converted grass records, but the current renderer has no
RunGrass material or runtime population path. Add placement/instance ownership,
wind/deformation, fade, alpha and shadow/depth variants as a separate increment.
Their instance and deformation inputs must agree across passes. A generic
NIF/PBR material or a terrain texture named grass cannot count as RunGrass support.

Keep grass alpha, vertex inputs, output scale, native ambient, animation and
permutation selection separate from the verified floor. Confirm the setting's
complete CPU upload under G3. Values outside 0–1 have no established native
range-validation rule; preserve them in raw import diagnostics and reject them
in a strict compatibility profile until their behavior is traced.

Terrain and terrain LOD must consume the same resolved environment and visibility
identity. Preserve the existing LAND texture-frequency contract. Normal-frame
and layer-material acceptance is independent from NIF normal acceptance. Check
seams, terrain/object shadow alignment and full-detail/LOD transitions. Tree
billboards and object LOD likewise need correct cast/receive participation;
loading them is not proof of their specialized lighting.

## 9. AO, fog and image-space processing

### 9.1 Ambient occlusion

Vanilla SAO is separate from direct-light shadow visibility. Retain ordinary and
compute-family settings independently. The exact SAO equation, normal-map option,
snow exemptions and fog composition remain unresolved. Under G6, identify the
selected retail shaders and native constants before translating intensity,
radius, bias, exponent/value-difference parameters or normal flags.

Do not map `fSAOIntensity=15` directly to a similarly named Bevy parameter. A
provisional Bevy AO implementation must be separately labeled. It must not modify
the direct shadow mask or apply an assumed whole-pixel darkening formula as
native behavior. Acceptance must report which normal source is used and which
lighting terms are affected by AO.

### 9.2 HDR composition and image space

Retain the existing single-display-transform invariant. Reflection storage stays
linear HDR, shares the main view's exposure policy, and receives no display
transform before water samples it. Fog, sky, transparent surfaces and reflections
must occupy declared compatible domains. Diagnostic EV100 9.7/TonyMcMapface
remain explicit diagnostic settings.

Subsequent fog/image-space work recovered and implemented the supported fog
paths and ordinary stable-record HDR graph: reduction/history, adaptation,
bloom, white curve, saturation/brightness/contrast/tint, Cinematic and Fade.
The [image-space specification](image-space.md) owns those equations and CPU
packing evidence. G7 still requires animated IMGS/IMAD activation and ordering,
retail sampler/target state, native light calibration and final display transfer.
AO composition remains G6. Record adaptation history and modifier state so
interior/exterior transitions can be reproduced.

The proposed interfaces should allow a deterministic frozen-exposure mode and a
stateful native-adaptation mode. Tests of native surface radiance must disable
or account for adaptation explicitly. The native output path must bypass Bevy's
diagnostic exposure/tone transform where appropriate; applying both would change
the shadow contrast twice. Apply `SunlightScale` at its traced stage once.

Implement specialized character fill, snow, environment reflection, volumetric
lighting and other cataloged appearance controls only through their own verified
paths. The `IBLF` setting prefix includes lens-flare/bloom-related controls and
does not itself establish an ambient-light term.

## 10. INI import and effective settings

### 10.1 Import policy

Use the native catalog to identify exact names, owning collection, section,
stored initializer and evidence status for this target. Add a reviewed typed
registry for value kind, parse/domain validation and path relevance; the CSV
has no explicit type/domain columns. Seven catalog initializers are
`not_decoded` and cannot supply effective defaults. Only decoded initializers
for implemented, selected paths become profile defaults. Preserve spelling,
including `ffocusShadowMapDoubleEveryXUnit`. Stored initializers are compatibility
profile defaults, not measured active user values or recommended tweaks.

Keep the current explicit file-list policy: repeated INI files merge in supplied
order and CLI values override them. Extend case-insensitive parsing with typed
lighting settings. This is Mudcrab's import policy; the target's actual loaded
paths and complete precedence are not yet established. A retail capture must
record effective native settings independently.

Keep candidate values and source positions before resolving them. For new
lighting fields, a malformed highest-priority candidate produces an invalid
status and a failure in strict mode. An explicitly approximate mode may use the
last valid lower-priority candidate only if it reports that fallback. Preserve
existing terrain/streaming key behavior unless a separate change deliberately
updates it; today's parser keeps only the final raw string, so changing invalid
candidate handling would otherwise be a compatibility change.

Source collection is metadata. Explicit `--ini` filenames may be arbitrary;
do not reject a recognized renderer key because its file was renamed. If preset
import is added, make it explicit and lower priority than supplied files. Do not
automatically select a shipped template or infer the active mod-manager profile.

For every imported renderer key, emit its raw value, parsed value, source file,
override chain, effective value, and implementation status:

- `supported`: applied through an accepted mapping.
- `approximate`: applied through a named Mudcrab/Bevy translation.
- `retained`: recognized, stored, and awaiting implementation or research.
- `unknown-for-target`: no matching catalog/native-name evidence for 1.7.104.
  Recognized setting objects with untraced consumers remain `retained`, with
  unresolved evidence; they are not unknown names.
- `invalid`: type/domain/resource validation failed, with a contextual error.

Report unknown key names, counts and status; public diagnostics need not retain
arbitrary unrelated string values. Do not silently accept ignored lighting keys
as active controls. Validate allocation arithmetic, positive dimensions, finite
values and device limits before GPU allocation. Range limits inferred only from
a key's name are not native range validation.

The following native stored initializers are useful profile anchors. The full
catalog remains authoritative for collection/section and all other rows:

| File and section | Key | Stored initializer |
| --- | --- | ---: |
| Skyrim.ini Display | `fShadowClampValue` | 0.3 |
| SkyrimPrefs.ini Display | `iShadowMapResolution` | 2048 |
| SkyrimPrefs.ini Display | `fShadowDistance` | 8000 |
| SkyrimPrefs.ini Display | `fInteriorShadowDistance` | 3000 |
| SkyrimPrefs.ini Display | `iNumSplits` | 2 |
| SkyrimPrefs.ini Display | `iShadowMaskQuarter` | 4 |
| Skyrim.ini Display | `fShadowBiasScale` | 1 |
| Skyrim.ini Display | `fShadowDirectionalBiasScale` | 0.3 |
| SkyrimPrefs.ini Display | `bSAOEnable` | 1 |
| Skyrim.ini Display | `fSAOIntensity` | 15 |
| Skyrim.ini Display | `fSAORadius` | 250 |
| Skyrim.ini Display | `fSAOBias` | 2.5 |
| Skyrim.ini Display | `bSAONormalMap` | 1 |
| SkyrimPrefs.ini Display | `fGamma` | 1 |

### 10.2 Required setting groups

| Group | Required keys or families | Implementation contract |
| --- | --- | --- |
| Grass intensity | `fShadowClampValue` | Separate grass direct-light remap; default 0.3, complete upload gate G3 |
| Map extent | `iShadowMapResolution`, `fShadowDistance`, `fInteriorShadowDistance` | Map allocation/range; no brightness compensation |
| Slices/focus | `fFirstSliceDistance`, `iNumSplits`, `fSplitOverlap`, `iNumFocusShadow`, focus-distance family | Retain now; native calculation gate G5 |
| Mask scale | `iShadowMaskQuarter` | Verified allocation uses `(screenDimension * value) >> 2`; check both dimensions and resource bounds |
| Filtering/bias | `uShadowFilterType`, `fPoissonRadiusScale`, `fShadowBiasScale`, `fShadowDirectionalBiasScale`, `fExponentialShadowMapScale` | Separate filter/projection mappings; native producer gate G5 |
| Participation/update | Tree/land/grass receive/cast controls, actor/self-shadow, sun-update family | Per-path rules, not a common opacity multiplier |
| Ordinary AO | `bSAOEnable`, `fSAOIntensity`, `fSAORadius`, `fSAOBias`, `bSAONormalMap`, `bSAODownscaled`, `fSAOExpFactor`, `fSAOValueDiffFactor`, `bSAOApplyFog` | Retain exact native defaults/status; equation gate G6 |
| Compute AO | `bSAO_CS_Enable`, `fSAO_CS_*` | Separate selection and parameter family; active path gate G6 |
| Output | `fGamma`, global brightness/contrast boosts, post-light/environment/specular clamps | Retain; ordering/mapping gate G7 |
| Specialized appearance | Character light, snow, projected-normal, indirect/volumetric, light fade/LOD, menu families | Explicit support inventory and independent implementation |

For verified mask allocation, `iShadowMaskQuarter=4` is full width/height, 2 is
half each dimension, and 1 is quarter each dimension. The latter pixel counts
are one quarter and one sixteenth of full resolution. This does not prove every
alternative value is rendered correctly or allow unchecked signed overflow.

Keep all 170 catalog rows classified, even when only a subset is supported.
Names absent from the target scan, such as `fShadowIntensity` and several LE
keys, must not acquire native semantics from an old tweak guide. Absence from
the scan is not a claim about other executable versions.

## 11. Diagnostics and reproducibility

The runtime provides `world-inspect --lighting`,
`engine --lighting-mode native-diagnostic --lighting-report`, and
`native_lighting_probe`; the runtime guide gives complete invocations.
The remaining names below describe proposed tools rather than available CLI:

```text
lighting_response_probe --output <dir>
shadow_visibility_probe --output <dir>
environment_lighting_probe --output <dir>
```

Reports must include executable/profile identity, engine commit/worktree,
converter/database/cache versions, source/configuration hashes, ordered plugins,
camera pose, world generation, origin, worldspace/cell, weather/time/transition,
resolved inheritance, color conversion, ambient matrix, native light RGB/dimmers,
material permutation/normal convention, all effective settings, visibility
backend, budget exclusions, exposure/modifier state, GPU/driver and frame identity.

Provide debug views for shading/geometric normals, albedo/vertex color, native
ambient, each direct-light contribution, direct specular, emission/IBL, AO,
sun/local visibility, cascade or local-map selection, and final HDR/display
output. A pick/inspection result must connect a visible primitive to
`REFR -> base record -> NIF shape -> material -> texture -> shader path`.

Each probe removes stale output before running, writes machine-readable expected
and measured values, and returns nonzero on failure. Debug contributions need
linear floating-point readback; a tone-mapped screenshot alone cannot verify
the light equation. Retain exact supported/approximate/unsupported reasons with
the capture. Publish synthetic fixtures and hashes/metadata in Git; keep retail
assets, bytecode and raw proprietary evidence outside the repository.

## 12. Native research gates

These gates name the work needed to finish the approach without inventing
missing behavior. Each closure requires target/version identity, inspected
inputs/constants, the recovered operation, and an independent check or matched
retail observation. A source reconstruction alone must retain its source-assisted
status.

| Gate | Required trace or capture | Unblocks |
| --- | --- | --- |
| G1 | Automatic current/next weather selection, native light units and final sun scaling/trajectory, and remaining record transfer; world lighting consumes active climate/GMST intervals when available, while static byte transfer/reference timing arithmetic are recovered | Complete effective environment and light coefficients |
| G2 | Writers/live value of additive ambient RGB and interior CELL/LGTM cube selection, checked against runtime matrix captures; static DALC affine arithmetic and upload are recovered | Effective directional ambient in changing exterior and interior views |
| G3 | Complete 1.7.104 grass state-to-buffer upload and selected grass draw inputs | Native grass profile mapping |
| G4 | LIGH/REFR fade/dimmer/exponent/cone/override combination, output units and per-draw light selection | Native placed-light strength and type |
| G5 | Utility/shadow producer permutations and native projection, cascade/focus, filtering, bias, channel assignment and update chains | Native visibility producer and shadow INI mappings |
| G6 | Ordinary/compute SAO selection, normal source, equations, composition and exemptions | Native AO |
| G7 | Animated IMGS/IMAD state, native light calibration, sampler/target selection and final display transfer; stable-record HDR graph and supported fog arithmetic are recovered and implemented | Native final appearance |
| G8 | Remaining material constants, source texture bindings and selected specialized permutations | Per-family native material acceptance |

Use the REA saved analysis and copied retail shader package described in the
research report. Extend analysis with narrowly scoped queries and selected
shader extraction. Verify instruction bytes and shader decoder round trips as
before. A partial Ghidra project with no recovered xref is not evidence that a
setting or path has no consumer. Capture actual selected draw permutations where
possible; static package presence alone does not prove runtime selection.

## 13. Original implementation sequence

These stages preserve the original specification sequence. The hub owns the
current work order; the runtime guide identifies completed scope.

Each stage produces a reviewable change with named support limits. Record
implementation, synthetic verification, retail evidence, and acceptance as
separate states. Stage completion must not imply the whole renderer is native.

### S0. Freeze references and add inspection

Extend L0 capture metadata with effective settings, authored environment,
selected materials/permutations and output state. Add lighting support/provenance
to inspection reports. Create an asset-independent fixture bundle for the verified
consumer terms. Preserve the existing approximate renderer as the comparison.

Exit: a capture can explain its direct/ambient/material/output inputs; fixtures
run without retail assets. Retail capture availability does not block writing
the data contract or synthetic consumer.

### S1. Preserve source inputs and resolve environments

Implement parsers/export/schema changes, native material metadata and extended
light/reference inputs. Add field-level interior inheritance and a versioned
environment resolver. Complete G1/G2 for authored native acceptance; support
injected matrices and frozen source state in diagnostics while those gates run.

Exit: deterministic database output and inheritance tests pass; missing/zero/
inherited values remain distinct; old packages cannot silently enter strict
native tests; every effective environment field has provenance.

### S2. Implement ordinary and model-space native materials

Add the material plugin/bindings and native response, preserve authored frames,
retain loader dependencies, and bypass PBR-specific compensation. Complete the
ordinary and model-space portions of G8. Start with injected light/visibility
inputs to isolate response from shadow-production errors.

Exit: linear GPU response agrees with an independent reference; normal-alpha
changes specular without changing diffuse or visibility; all existing UV/alpha/
dependency invariants still pass. Unsupported families remain explicit.

### S3. Integrate authored lighting and placed lights

Drive world, sky/fog and reflection state from the resolver. Complete G4 and
replace radius-derived intensity for supported native lights. Implement supported
local shadow types using the producer interface. A provisional Bevy producer may
remain identified as approximate while native response is tested.

Exit: time/weather and interior changes update the shared state; wall/occluder
fixtures block shadow-requesting lights; intentionally unshadowed controls stay
lit; rebasing, generation changes and unloading preserve ownership.

### S4. Implement native visibility and settings

Complete G5 and the relevant setting mappings. Match projection, filtering,
range/slice/focus, bias, local mask channels, participation and sun updates.
Add budget and caster-coverage diagnostics; extend acceptance to native producer
cases.

Exit: producer/consumer binding tests pass; native setting changes affect their
intended geometry/sampling behavior; no unsupported setting is reported applied;
shadowed direct terms do not multiply ambient or emission by mistake.

### S5. Add grass, terrain and specialized paths

Complete G3 and targeted G8 work. Implement the grass floor, native grass
response, LAND/full-detail/LOD normal/material response and reachable specialized
families. Track each family independently; broaden the strict case inventory
only when its required paths are supported.

Exit: grass visibility cases and cutouts pass; terrain/LOD and object receivers
agree at shared boundaries; specialized unsupported paths cannot pass ordinary
material acceptance.

### S6. Add native AO and image-space behavior

Complete G6/G7, implement native AO selection/composition, image-space state,
adaptation and output processing. Replace diagnostic transforms only in the
native path. Add transition history and frozen-state comparisons.

Exit: AO changes remain distinct from cast shadows; adaptation/IMAD sequences
are reproducible; sky/fog/water/reflections retain a single display transform;
the matched retail appearance gates pass for the declared case set.

### S7. Accept on target hardware and enable supported defaults

Run the retail matrix and the project's full acceptance/performance campaign.
Update the compatibility inventory and user-facing support status. Make a native
path the default only for cases whose data, shader, producer and output gates
have passed. Continue reporting approximations for the remaining case set.

S0/S1 and consumer fixtures can proceed in parallel with research. S2 needs the
native source contract, but can use injected environments. S3 depends on S1/S2;
S4 can develop behind the producer interface in parallel. S5/S6 expand support
after the common contracts exist. S7 requires all gates needed by its declared
retail cases.

## 14. Acceptance tests

### 14.1 Independent arithmetic and data tests

Use an independently written CPU evaluator or hand-calculated fixture values;
do not call the WGSL implementation from both sides of the comparison. The
following are proposed test cases and tolerances, not measured retail results.

| ID | Fixture | Required result |
| --- | --- | --- |
| D1 | Winning-record override/deletion and referenced template override | Resolver uses the final load-order view and reports the effective sources |
| D2 | CELL inheritance mask with different cell/template values per field | Inherited fields change; explicit zero/black fields remain explicit |
| D3 | Six uniquely colored source axes, normals and rotated references | Coordinate/frame conversion agrees with the shared basis; origin changes have no directional effect |
| D4 | Eligible legacy non-LOD package without native tables/material contract, and old LOD package | Existing non-LOD compatibility remains explicit; strict native test names the missing contract; old LOD startup requires a rebuild |
| D5 | Repeated DALC, supported versions, header/DATA flags and absent/zero fields | All four cubes and distinct flag/value states survive exporter -> SQLite -> worker round trip |
| D6 | Remapped links, deleted winners, malformed/truncated layout and non-finite active value | Type-correct winning links survive; stale rows disappear; invalid selected inputs fail contextually |
| M1 | One white light, zero ambient, visibility 0/0.5/1 | Corresponding direct diffuse and specular scale by 0/0.5/1 |
| M2 | Zero direct visibility with nonzero matrix ambient | Ambient remains unchanged by cast-shadow visibility |
| M3 | Different ambient coefficients and opposite normals | Output matches `M * float4(N,1)` rather than a uniform scalar fill |
| M4 | Point distances 0, radius/2, radius and beyond | Attenuation factor follows 1, 0.75, 0 and 0; avoid undefined direction at exactly zero distance in the actual lighting fixture |
| M5 | Normal alpha 0/0.5/1, fixed RGB normal and material | Only ordinary direct specular amplitude changes; diffuse/ambient/visibility stay fixed |
| M6 | Asymmetric tangent normal swatches with regenerated Bevy and authored-native frames | Each frame matches independent geometric normals; no-flip/double-flip controls fail for the Bevy-frame fixture, with authored-frame orientation tested separately |
| M7 | Model-space normal under reference rotation, including nonunit texels and separate specular red mask | Correct XZY/common-space branch without added normalization; ordinary alpha-mask control fails |
| M8 | `0x6201` vertex RGB, black/nonblack albedo, constant emission/IBL and isolated specular | Constant emission/IBL follows diffuse multiplication; specular remains outside albedo multiplication |
| M9 | Multiple exponent values and half-vector dot products with fixed amplitude | Native power-lobe width changes as independently calculated; a fixed/GGX lobe control fails |
| G1-test | Grass visibility 0/0.5/1 at clamp 0.3 | Direct factors 0.3/0.65/1; ordinary material control has no floor |
| G2-test | Grass clamp 0 and 1 | Direct factors become S and 1 respectively within the verified domain |
| C1 | Role-specific color/data textures with distinct alpha and HDR factors | Exactly one color decode; normals/scalar masks remain data; colored specular projection and native mask view are distinct; no alpha gamma conversion |

For isolated GPU response tests, use a linear floating-point render target with
explicit unit exposure, no AO/fog/adaptation and no tone mapping/display encoding.
Frozen EV100 9.7 still applies exposure and is not an unscaled radiance reference.
Record the selected format and all effective constants with the samples.

For CPU arithmetic/packing, use an absolute tolerance of `1e-5` where floating
evaluation is appropriate. For controlled linear GPU readback, start with
`max(1e-3, 0.005 * abs(expected))` per component to accommodate the chosen HDR
format. Document tighter/wider bounds with format evidence; do not relax them
to hide a systematic mismatch. Existing display-domain probes retain their
2/255 bounds and diagnostic output policy.

INI import must additionally cover BOM/case handling, repeated files/duplicate
keys, wrong sections, full candidate provenance, and CLI priority independent of
argument position. Test malformed winning candidates with strict nonzero failure
and an explicitly reported last-valid approximate fallback. Preserve the five
existing streaming/terrain keys' behavior. Include supported, retained, unknown
and out-of-scope keys together so ignored settings cannot appear applied. Test
odd-sized mask dimensions, checked multiplication/shifts, invalid domains,
texture/device limits and failed allocations independently of the shader.

After G6, add independently calculated depth/normal fixtures for SAO, both normal
sources and its traced composition/fog order. After G7, replay fixed IMGS/IMAD
inputs, time steps, active-modifier order and adaptation history against an
independent reference, plus frozen-output controls. Include entry/exit and
history reset cases. A pending native equation cannot produce a passing
native AO/adaptation test from a Bevy approximation.

### 14.2 GPU visibility and lifecycle tests

| ID | Fixture | Required result |
| --- | --- | --- |
| V1 | Opaque wall between a supported shadowing local light and receiver | Direct response is blocked; ambient survives; intentionally unshadowed control stays lit |
| V2 | Two local lights with different visibility/channel assignments | Each mask affects only its assigned light, including specular |
| V3 | Cast-only and receive-only surfaces | Independent participation, with no accidental light disable |
| V4 | Alpha-tested fence/foliage and shadow receiver | Color, depth and shadow silhouettes agree |
| V5 | Caster outside camera frustum casting onto an in-view receiver | Camera/HZB visibility does not remove the needed shadow caster |
| V6 | Cascade boundary, range fade and focus/local-map selection | Correct native transitions after G5, with identified approximate backend before then |
| V7 | Change native bias/filter/resolution independently | Sampling/coverage changes as traced; no material brightness multiplier appears |
| V8 | Rebase, unload/reload, old-generation DB response and reused shadow slot | No stale positions, light state or visibility; resources return to bounded ownership |
| V9 | Reflection and main camera under environment/exposure change | Shared source state, valid visibility, linear HDR reflection and one display transform |
| V10 | Exceed a chosen local shadow/light budget | Deterministic exclusions/degradation appear in the report; strict required-case acceptance fails |

Include negative controls for uniform ambient, PBR lighting, normal-alpha shadow
opacity, wrong local mask channel, opaque cutout shadows, stale generation state,
and duplicate display transformation. A probe that also passes its relevant
negative control cannot establish the intended invariant.

### 14.3 Matched retail cases

Capture retail and Mudcrab with matching worldspace/cell, camera, FOV, resolution,
plugin/asset view, time/weather state, effective settings, material/placement,
and adaptation/modifier history. Freeze or account for animated noise/dither,
wind, actors, particles and stochastic sampling. Prefer controlled swatches and
native constant captures before comparing a complete scene.

Required case coverage:

- Exterior clear day, overcast, dawn/dusk and night with sun-facing and shadowed
  ordinary surfaces.
- SkyrimClear directional ambient axes, including upward/downward surfaces.
- Bannered Mare template inheritance plus a room with explicit non-inherited
  lighting and a supported local shadowing light.
- Ordinary tangent and model-space assets with diffuse/specular isolation.
- Cutout foliage/fences, grass shadow floor, terrain/object contact and LOD
  boundaries.
- Native map-resolution/filter/bias changes, ordinary AO toggle/normal source,
  and frozen/adapting image-space transitions.
- Reachable environment/skin/hair/snow/other specialized materials when their
  paths are included in the declared acceptance scope.

Predeclare comparison regions and capture at least three unchanged retail
baselines plus before/after controls for each varied input. Derive case thresholds
from their measured repeatability before evaluating the change. Missing required
captures leave retail acceptance pending.

Compare linear terms where captures permit, then displayed images under matched
output state. Shadow-to-lit radiance ratios require pre-tone-map floating-point
samples. PNG ratios measure displayed contrast unless the inverse transform is
verified. Record the domain alongside each ratio, hue, normal/specular response,
silhouette, temporal-stability and local-leakage measure. This spec supplies no
invented whole-scene pixel-error limit. Whole-scene similarity without matching
inputs is not parity evidence.

### 14.4 Regression and performance

Preserve the existing material alpha, normal, specular, emission, color-output,
asset-lifetime and terrain-frequency probes in approximate mode. Add native-mode
cases rather than weakening the earlier contracts to accommodate new equations.
Use the supported tests/examples named in
[the color-pipeline specification](color-pipeline.md); the new probe names in
section 11 become runnable only when implemented.

Run the relevant crate tests and GPU probes per stage. For final acceptance,
run formatting, workspace tests/targets, Clippy, release build, asset closure,
robustness and the full
[Phase 2 acceptance campaign](../../roadmap/02-acceptance.md). Current campaign
thresholds include average FPS at least 60, frame P95 at most 16.67 ms, memory
growth at most 0.5 GiB and zero streaming failures, with its documented baseline
regression/noise policy. Report the rendering mode, supported case set and target
hardware alongside those results.

Measure environment-resolution CPU cost, GPU response cost, shadow draw count,
map/filter/AO GPU time, map/mask memory, enabled/excluded lights and temporal
resource growth. Estimate map memory from resolution, format bytes, layers/faces
and lifetime before allocating it. Device-limit failures need an explicit
fallback/status; never trade shadow participation away silently to pass a
performance gate. Software GPU probes verify function, not target performance.

## 15. Completion criteria

For each declared native case, authored state resolves with provenance; its
material/normal path is supported; direct visibility applies to the correct
light terms; native producer/settings, AO and image-space dependencies required
by that case are accepted; and matched retail plus target-hardware gates pass.
Retained metadata, synthetic consistency, and a plausible screenshot are
separate evidence states.

Update the research/evidence references and compatibility inventory as gates
close. Leave unsupported families and unverified translations visible. No
global ambient, exposure, emission or shadow-opacity adjustment may compensate
for a pending material, producer, or source-data error in an accepted case.
