# Skyrim rendering input contracts

This inventory maps 45 rendering-input domains to the native CPU and GPU
connections that have evidence. It records the remaining connections explicitly.
None of the 45 complete input-to-pixel traces is closed. The selected target is
`SkyrimSE.exe` 1.7.104.0, SHA-256
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
Addresses are preferred image addresses at base `0x140000000`.

The [machine-readable map](input-contracts.json) gives every connection a stable
ID, status and evidence reference. It pins the evidence-file hashes and separates
authored fields, recovered static CPU dataflow, selected shipped shader consumers,
project implementation and missing proof. It combines earlier audits with 11 new
bounded, read-only native queries. Their 19,563 unique instruction starts match
the original executable. Independent LLVM decoding checked 8,568 instruction
starts in 64 functions with zero mismatches. Requested-span checks account for
the remaining byte-check coverage. No GPU runs or renderer changes were made.

## Evidence rules

- **Native static** means that a target-pinned audit recovered the scoped CPU
  connection. Fresh and reused evidence are identified separately; effective
  runtime state was not captured.
- **Shader static** means that selected shipped shader arithmetic or input
  consumption was recovered. Material/permutation selection and CPU binding
  remain separate connections.
- **Native identity** establishes an original target metadata/object identity
  and address without inferring function behavior.
- **Serialized** means that source fields or links are decoded or retained.
  Their existence does not establish the native runtime combination.
- **Source assisted** means that a pinned declaration or reconstruction supplies
  a search seed. Historical addresses and descriptive names are not target proof.
- **Project** describes Mudcrab source or converted assets.
- **Open** means that no qualifying producer-to-consumer proof is attached.

The executable and shader evidence is private. This page publishes interpreted
behavior, addresses, hashes and explicit gaps, with no executable, shader, texture
or original asset payload bytes. None of the attached input evidence captures
effective retail state or a selected retail draw.

## Proven static segments and their limits

Weather color preparation blends normalized bytes without an additional sRGB
decode. DALC preparation preserves six-face order and builds the recovered affine
ambient rows. The extra RGB operand at `0x14331D6F0` has unresolved object meaning:
it could be mutable state or a shared zero/color sentinel. Partial reference
coverage cannot establish that it has no writers. The default Sun
writer copies prepared RGB directly and initializes dimmer to one. The ordinary
consumer multiplies diffuse RGB, dimmer and SunlightScale, but other writers and
the active light identity remain open. Full-mode local sun trajectory is recovered;
the effective parent world transform and overrides are unobserved.

Stable image-space fields, HDR constant packing and per-shader register remapping
are traced. Fresh queries also recover selected IMAD channel composition and an
ordered modifier frame route, described below. Active base records, concrete
curve evaluators and effective values remain open. The native
legacy DDS diffuse path creates and binds UNORM views; explicit typed DX10 formats
retain their source format. This source transfer proof supersedes the older
semantic sRGB rule for supported native legacy diffuse. UNORM describes sampled
code values, not a physical linear-light interpretation.

The ordinary shader consumes authored tangent, bitangent and normal independently.
Fresh target layout/signature evidence closes the selected component formats and
decode. Original source stream/stride binding remains open. Mudcrab currently
regenerates tangents and reconstructs bitangents. Three matching farmhouse NORMAL
streams do not establish a complete retail draw correspondence. Selected soft
directional arithmetic and object-field packing are recovered; original NIF flag
translation, specialized texture transfer and active values still need proof.

## Fresh native input segments

The material stream helper reads raw fields without normalization in the inspected
scalar/RGB helpers. Under native key mask `0x200`, material setup uploads RGB from
`+0x38/+0x3C/+0x40` multiplied by scalar `+0x8C`, followed by unchanged scalar
`+0x88`, at the offset given by the bound object's metadata entry 25. Lighting
`00006201` and `00006601` have entry 25 equal to 16 floats. The
[native shader-loader proof](native-shader-loader.json) connects the original
package's exact metadata and DXBC payloads to native keyed objects and the
lookup-to-bind path. When one of these programs is selected from the pinned
package, the vector occupies pixel `cb1[4]`. This static association requires
that the pinned package supplies the opened resource. The actual file and draw
are unobserved. External names identify specular RGB/scale and gloss exponent;
outer NIF/property selection and active overrides remain separate work.

Two small native functions recover the raw Lighting key translation:

```text
VSKey = (raw & 0x48007) | ((raw & 0x20A00) ? 0x200 : 0) | (raw & 0x3F000000)
PSKey = (raw & (0xFFFFFE05 | ((raw >> 1) & 2))) | 1
```

These use unsigned 32-bit operations. They do not identify which NIF flags, pass
or geometry state produced `raw`. Material mask `0x4` selects object `+0x68` for
GPU texture index 2. Masks `0x400/0x800` select object `+0x60` for cached `t12`
and write raw scalar fields `+0x90/+0x94` using metadata entry 28. Loader-associated
Lighting `00006601` has entry 28 equal to 28 floats and consumes `cb1[7].x`; the
producer pair occupies `cb1[7].xy` when that program is selected from the pinned
package. Mask `0x1000` selects object `+0x68` for cached
`t9`. Texture-set source indices, specialized SRV formats and fallback texels are
unresolved. Checked fallback pointer selection does not establish texture color.

Native layout creation binds `POSITION0` as `R32G32B32A32_FLOAT`, `TEXCOORD0` as
`R16G16_FLOAT`, and `NORMAL0/BINORMAL0` as `R8G8B8A8_UNORM` at descriptor-encoded
offsets. Original PE strings establish the `BINORMAL` name. The statically
loader-associated Lighting `00000000` VS consumes that register's XYZ as the
authored tangent. Its independent frame decode is:

```text
N = 2*v2.xyz - 1
T = 2*v3.xyz - 1
B = (v0.w, 2*v2.w - 1, 2*v3.w - 1)
```

The input-element formats, exact package program/metadata and native keyed loader
are statically connected under the pinned-package precondition. This does not
capture an active file or draw. Original NIF descriptor creation, stream strides,
legacy/dynamic meshes and every deformed frame remain open. The exact semantic
label must be preserved separately from the vector's use in arithmetic.

IMAD evaluation calls float/color interpolators, supplies exact null-pair values
`1` and `0`, and sends the result to manager `+0xC8`. For the first 12 numeric
channels, the checked scalar instruction sequence is:

```text
p = p - strength * (p - (p * mult + add))
```

Each modifier updates the current `p`; stacking therefore preserves order. Tint
and fade accumulate amount-weighted RGB, then divide by total amount and retain
the maximum individual amount. Other effect fields use maximum selection and
carry associated parameters for the winning radial/DOF input.

The checked frame routine prefers manager base pointer `+0xB0` when nonnull,
otherwise `+0xA8`, copies `0x50` bytes, optionally advances the clock operand,
walks modifier nodes in link order and finalizes accumulated colors. This is a
static route. Source base identities, concrete curve arithmetic, full Update and
crossfade lifecycle, active stack and effective time/delta values are unobserved.
The JSON's `I-N01` through `I-N09` entries retain exact functions, query artifacts
and limits. Exploratory query labels are explicitly rejected where native
behavior did not match the label.

## Complete domain inventory

The table summarizes the domain; the JSON splits each row into individual
connections. “Open” remains part of every domain's full trace even where a static
segment is recovered. Domain counts are inventory coverage, not rendering parity.

| ID | Input domain | Existing evidence | Missing connection |
| --- | --- | --- | --- |
| I-C01 | Plugin winners, resources and source identity | serialized, open | Active load order, loose-vs-archive precedence, overrides and source load cache identity. |
| I-C02 | WTHR NAM0 colors | native static, open | Live selected weather/weight state; every category consumer and exceptional lightning branch. |
| I-C03 | WTHR DALC directional ambient | native static | Extra RGB operand meaning/writers/effective value; ambient-specular/Fresnel runtime behavior; selected model-space branch. |
| I-C04 | CLMT/GMST color timing | native static, open | Active climate/GMST selection, runtime override and automatic current/next weather transition. |
| I-C05 | Directional sunlight RGB and dimmer | native static, open | Other dimmer/RGB writers, scene replacement and view-specific light selection. |
| I-C06 | Sun trajectory and parent transform | native static, open | Native parent world rotation, active setting overrides, exceptional modes and timing. |
| I-C07 | REGN/CLMT automatic weather selection | serialized, open | Overlap rules, chance/global evaluation, force weather and live progress owner. |
| I-C08 | WRLD/CELL sky and parent selection | native static, open | Active parent link selection, special interiors and reflection/map/menu ownership. |
| I-C09 | CELL/LGTM fog inheritance and load defaults | native static | Live room transitions/owner and finite clipping consumer; active per-view constants. |
| I-C10 | Interior lighting cube, directional angles and fade | serialized, open | Native cube precedence and inheritance coupling; angles/fade/transfer; room and view ownership. |
| I-C11 | Room markers, portals and local scene ownership | project, open | Native containment, portal visibility, override precedence and per-draw/local-light room selection. |
| I-C12 | IMGS stable fields and constant remapping | native static, open | Active record identity, transition/interior base state and modifier composition. |
| I-C13 | IMAD interpolation, stacking and crossfades | project, native static, native identity, open | Source loader and concrete curve evaluation; Update/crossfade strength and full list lifecycle; active base/list/time and effective channel receipts. |
| I-C14 | VOLI and volumetric/weather links | serialized, open | Selection/interpolation, transfer/units, ray/depth/noise/shadow bindings and compositing. |
| I-C15 | LIGH effective local-light inputs | serialized, shader static, open | Color/dimmer/light replacement, per-draw list/count/order, culling/room fade and local shadow-channel allocation. |
| I-C16 | REFR placement and light overrides | serialized, open | Override precedence, transform and scale units, enable parent, fades/bias and scene participation. |
| I-C17 | NIF material properties and effective constants | project, shader static, open, native static | CPU packing of specular/gloss/emission/UV/alpha/soft parameters, cache invalidation and controller overrides. |
| I-C18 | Material type/flags to compiled permutations | source assisted, open, native static | NIF/property/pass state to raw native key; cache/fallback handling and live selected shader/resource identities for every family. |
| I-C19 | External material files and BGSM/BGEM applicability | project, open | Original resource extension inventory, target loader recognition and actual references before defining a format contract. |
| I-C20 | Diffuse DDS resource/view transfer | native static, project | Live draw SRV identity; typeless/uncompressed/unsupported formats; render-target diffuse branches and durable converter publication. |
| I-C21 | Texture slot semantics and replacements | project, open, native static | Per-feature slot mapping, texture set/override precedence, missing/default resources and render-target bindings. |
| I-C22 | Normal texture data and specular masks | shader static, open, native static | Original DDS payload proof, model-space mask source slot and transfer, missing normal defaults and selected retail variant. |
| I-C23 | Authored tangent, bitangent and normal | shader static, native static, project | Active file/program/draw identity; original NIF descriptor/stream allocation and stride; rigid/mirrored/nonuniform, skin/morph and specialized signatures; live resources. |
| I-C24 | Vertex colors and UV channels | shader static, open, native static | Packed vertex/UV input layout, quantization, UV transform order, alpha and disabled vertex-color branches. |
| I-C25 | Mesh streams, indices, instances and culling | project, open | Descriptor/stream allocation, index format/topology, instance streams, bounds/culling and partition/LOD draw selection. |
| I-C26 | Transforms, spaces and origin ownership | native static, open, project | All transform branches, parent/live state, view origin, scale signs and deformation. |
| I-C27 | LAND/LTEX/TXST near terrain | project, open | Layer order/weights, normal and specular semantics, slot transfer, input layout, shadow/AO/fog/output and native selection. |
| I-C28 | Terrain LOD and distant material response | project, open | Original LOD atlas transfer/material/geometry and distance/morph/blend controls; coherent near/far response. |
| I-C29 | TREE, tree animation and distant trees | shader static, open | Tree constants/streams, wind/time, billboards/atlas, alpha fade and native cast/receive participation. |
| I-C30 | GRAS runtime grass | shader static, open | Population rules, layout/deformation, material transfer, all constant uploads and active variants. |
| I-C31 | Face, skin, hair and eye materials | source assisted, open | Exact specialized shader arithmetic, texture/tint transfer, bone/morph inputs and runtime actor/material overrides. |
| I-C32 | Snow and projected multi-index surfaces | serialized, open | Snow input selection, multi-index vertex layout, constants/SRV binding and settings-to-response equations. |
| I-C33 | Soft, rim and back lighting | shader static, native static, project, open | Original source fields/flags to object/key; specialized texture/fallback transfer, effective values and rim/back/local-light equations. |
| I-C34 | Parallax and multilayer materials | source assisted, open | Per-feature shader arithmetic, source parameters, sampler transfer and active branch selection. |
| I-C35 | Effect materials and particles | native static, open | Simulation/controller layout/time, material constants/SRVs, blends/depth/raster and complete pass-specific response. |
| I-C36 | Decals, projected textures and depth state | project, open | Raster/depth/blend constants, projection/normal handling and interaction with fog/AO/shadows. |
| I-C37 | WATR and water surface selection | native static, open | Water record selection/units/transfer, source textures, displacement/time, reflection/refraction/depth and underwater transitions. |
| I-C38 | Environment maps, IBL and scene reflections | shader static, open | Cubemap/target generation and SRV formats, source masks/Fresnel, ambient-specular transport and active reflection composition. |
| I-C39 | NIF controllers and material/vertex animation | open | Source parser retention, evaluator arithmetic, target/controller precedence, update ordering and shadow/depth consistency. |
| I-C40 | Skeletons, skin partitions and GPU skinning | project, open | Skin input layout, palette uploads, bone bind/inverse transform convention, partition selection and color/depth/shadow/motion deformation. |
| I-C41 | TRI/face morphs and dynamic vertex streams | open | Morph format/weight evaluation, source/target indexing, delta spaces, normal/frame updates and rendering-pass bindings. |
| I-C42 | INI, GMST and runtime rendering settings | native static, open | Complete rendering-setting discovery and all consumers; active paths/order/values and runtime override update timing. |
| I-C43 | Camera projection, fog clip and view state | native static, open | Finite clipping, exact projection conventions/jitter and all view-specific input snapshots. |
| I-C44 | Sky, clouds, sun/moons, precipitation and aurora | native static, open | All sky category/geometry/material bindings, cloud/wind/time and moon/aurora/precipitation selection. |
| I-C45 | Sampler, alpha and shared output controls | native static, open | Exact sampler/state object creation and selection, alpha order/defaults, effective clamp settings and final output chain. |

## Settings catalog

The retained setting catalog has **170 rows**. It records setting object/name
pointers, collection ownership, compiled initializers and selected native reads.
**13 rows** have a native consumer seed; **157 rows** do not.
These are not active values, a complete downstream semantic map or an exhaustive
list of rendering settings. Each row in the JSON has an `I-SET-*` identity,
an explicit null active value and its evidence reference. `fDaytimeColorExtension`
is a GMST; its name is insufficient reason to search only INI collections.

## Serial native query queue

These 18 questions retain the remaining acceptance conditions. Five have selected
static segments closed by the fresh queries; none has complete runtime acceptance.
Exact target addresses are distinguished from source-assisted symbols and IDs.

Root's original-PE class catalog identifies the modifier-form vtable at
`0x14181E860`; slot `0x26` points to the inspected Apply method `0x1402A3670`.
Checked Address Library candidates identify Trigger `0x1402A3940`, Stop
`0x1402A3DD0` and StopCrossFade `0x1402A4400`. Their bodies were queried, while the
complete list/crossfade lifecycle remains open. Slot `0x25` identifies Update
`0x1402A3650`, a precise remaining behavior seed.

| ID | Priority | Query | Required result |
| --- | ---: | --- | --- |
| I-Q01 | 1 | Ambient-builder extra RGB operand | Verified object/alias meaning and relevant write paths plus an effective operand/matrix receipt; zero-filled storage and partial READ-only references are insufficient. |
| I-Q02 | 1 | Effective sun dimmer and light identity | Checked writes/selection and live light identity, diffuse, dimmer and SunlightScale receipts attached to a draw. |
| I-Q03 | 1 | Image-space state and modifier composition | Verified channel write arithmetic, ordering, time source and lifecycle; controlled active modifier sequence captures. |
| I-Q04 | 1 | Authored vertex input layout | Target-byte-checked descriptor table and CreateInputLayout/IA binding chain with original NIF vertex and shader signature correspondence. |
| I-Q05 | 1 | Native terrain input and response | Original terrain input stream plus checked CPU constants/SRV slots, selected shader arithmetic and matched near/far draw receipts. |
| I-Q06 | 2 | Sun parent, overrides and exceptional modes | Captured original CLMT times, effective settings, local matrix, parent/world matrix and consumer vector at fixed hours. |
| I-Q07 | 2 | Interior ambient/directional selection | Separate field-specific native chains and room/view ownership receipts; scalar fog getter proof must not stand in for cube precedence. |
| I-Q08 | 2 | Local light coefficients, selection and overrides | Checked field preparation and room/culling/fade rules, plus constant and selected-light list receipts for an interior/exterior draw. |
| I-Q09 | 2 | Material fields, permutations and binding semantics | Checked flag translation, constant remapping, material-instance overrides, SRV/sampler binding and per-draw selected variant identities. |
| I-Q10 | 2 | Specialized texture transfer | Source slot/override to native texture object/SRV format to GPU slot chain for each selected specialized variant; no semantic-role guess. |
| I-Q11 | 3 | Effective settings and override precedence | Loaded paths/order, effective typed values, native consumer reads and refresh/reset timing; compiled initializers remain separate. |
| I-Q12 | 3 | Weather/region/sky selection | Checked candidate/priority/state transition rules and live selected records/weights; authored probabilities are not simultaneous color weights. |
| I-Q13 | 3 | Tree and grass producers | Input layouts/constants, population and LOD selection, material permutations and active draw receipts; existing static meshes are insufficient. |
| I-Q14 | 3 | Water and volumetric inputs | Record-to-material/constant/texture chains and selected pass resources; recovered atmospheric fog subset must remain separate. |
| I-Q15 | 3 | Animation, skin, morph and particles | Checked source decoding, time ownership, CPU/GPU deformation layouts and color/depth/shadow draw correspondence for each family. |
| I-Q16 | 3 | Overrides, decals and projected surfaces | Checked override winner and source slot remapping; exact active raster/depth/blend state attached to representative draw variants. |
| I-Q17 | 3 | Reflection/environment/skin/hair/snow/parallax | Selected target shader arithmetic plus checked CPU parameters/SRV transfer and native draw evidence for each branch independently. |
| I-Q18 | 3 | Camera and scene ownership | Checked per-view lifecycle/order and concrete constant/resource identity snapshots; no global-state alias between incompatible views. |

## Proof needed before implementation

Every implementation input needs its original source identity, selected active
state, native field preparation, constant/resource upload, selected shader
consumer and output owner. Arithmetic recovered in one segment must retain that
scope. A source codec, enum label, registered setting or shader program inventory
cannot fill an absent connection. Effective runtime defaults, unavailable texture
fallbacks, extra multipliers and correction effects remain unspecified until their
own chains are verified.

Terrain, unsupported foliage, specialized skin/hair/snow/water, deformations and
material overrides therefore remain explicit native-input work. No brightness,
gamma, saturation, sun direction or ambient coefficient is inferred from the
failed screenshots or from an incomplete chain.
