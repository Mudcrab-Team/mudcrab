# NIF to glTF 2.0 / GLB Transformation Specification

This document details the technical specification for converting Bethesda NetImmerse (`.nif`) 3D
mesh files into modern, GPU-ready **glTF 2.0 (`.glb`)** binary files.

---

## 1. Overview & Objectives

- **Input:** Skyrim `.nif` file (NiHeader, BSTriShape / BSDynamicTriShape / NiTriShape,
  BSLightingShaderProperty).
- **Output:** Standalone glTF 2.0 binary file (`.glb`).
- **Goal:** Convert legacy proprietary 3D geometry into standard PBR-compatible glTF primitives
  that render zero-copy inside Bevy.

---

## 2. Block Mapping Reference Table

| Skyrim NIF Block Type                            | glTF 2.0 Equivalent     | Conversion Logic                                                                  |
| :----------------------------------------------- | :---------------------- | :-------------------------------------------------------------------------------- |
| **`NiHeader`**                                   | `asset` metadata        | Copy generator & version tags                                                     |
| **`NiNode` / `BSFadeNode`**                      | `nodes`                 | Convert local transform matrix (`translation`, `rotation` quaternion, `scale`)    |
| **`BSTriShape` / `BSDynamicTriShape` / `NiTriShape`** | `meshes` + `primitives` | Extract vertex positions, normals, UVs, tangents, and index buffers; `NiTriShape` is static geometry only (§7.2) |
| **`NiBillboardNode`**                            | `nodes`                 | As `NiNode`, plus node extras `{"openSkyrim": {"billboard": <mode>}}`: nif.xml's `BillboardMode` name in camelCase (`rotateAboutUp`, ...), or its number |
| **`BSLightingShaderProperty`**                   | `materials`             | Map Bethesda shader flags to glTF PBR Metallic Roughness properties               |
| **`BSShaderTextureSet`**                         | `textures` + `images`   | Map Skyrim texture slots (`_d.dds`, `_n.dds`, `_s.dds`) to glTF URIs/KTX2 handles |
| **`NiSkinInstance` / `BSDismemberSkinInstance`** | `skins`                 | Map bone indices (`JOINTS_0`) and vertex weights (`WEIGHTS_0`); see §7             |

`BSMultiBoundNode`, `BSOrderedNode`, `NiBillboardNode` and `BSValueNode` are `NiNode`s with a few
trailing fields, and convert like `NiNode`: their transform and children join the scene, and the
extra fields other than the billboard mode are read but not exported. A billboard's authored
mode is exported for the runtime to evaluate. Before these node types were parsed, shapes under
them lost every parent transform and sat at the scene root (#288): a forge's glow, flames and
quench water floated about 170 units away from the forge.
The NIF asset-kind proof includes authored placement, billboard, material and transform-animation
contracts. Any change to this proof rebuilds **every NIF GLB** on the next normal conversion,
including static models and models without controllers. It is not a controller-only rebuild.
The global converter schema is unchanged. Metadata rebuild and asset repair cannot upgrade old
retained GLBs; both reject an old or missing NIF proof early and request normal conversion.

---

## 3. Detailed Data Extraction Steps

```
┌──────────────────────┐
│  Skyrim NIF File     │
└──────────┬───────────┘
           │
           ▼ (Binary Reader / nom)
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. Extract Geometry Buffers (BSTriShape)                                    │
│    - Positions:  Vec3<f32>  ➔ glTF Accessor "POSITION"                     │
│    - UV Map:     Vec2<f32>  ➔ glTF Accessor "TEXCOORD_0"                   │
│    - Normals:    Vec3<f32>  ➔ glTF Accessor "NORMAL"                       │
│    - Tangents:   Vec4<f32>  ➔ glTF Accessor "TANGENT"                      │
│    - Indices:    u16 / u32  ➔ glTF Accessor "ELEMENT_ARRAY_BUFFER"         │
└──────────────────────────┬──────────────────────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 2. Map Material & Textures (BSLightingShaderProperty)                       │
│    - Slot 0 (Diffuse)       ➔ baseColorTexture                             │
│    - Slot 1 (Normal Map)    ➔ normalTexture                                │
│    - Slot 2 (Subsurface/Env)➔ metallicRoughnessTexture                     │
│    - Alpha Flags            ➔ alphaMode ("OPAQUE" / "MASK" / "BLEND")      │
└──────────────────────────┬──────────────────────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 3. Build & Write glTF 2.0 Binary (.glb)                                     │
│    - Write JSON Chunk (Nodes, Meshes, Materials, Accessors, Views)          │
│    - Write BIN Chunk  (Interleaved Vertex & Index Buffers)                  │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 4. Material Parameter Conversion Matrix

Before glTF publication, Mudcrab builds a validated material contract for every reachable shape.
The contract follows the shape's explicit shader, texture-set and alpha-property block references;
block order and filename suffixes are not used to associate or classify materials. Unsupported
properties are recorded as explicit exclusions, while invalid references and non-finite values fail
conversion with the source file, shape block and shader block in the diagnostic.

| Skyrim Shader Feature    | Skyrim Flag / Value                           | glTF PBR Property                                                                                       |
| :----------------------- | :-------------------------------------------- | :------------------------------------------------------------------------------------------------------ |
| **Base Color**           | Diffuse texture (`Slot 0`) + material alpha   | `pbrMetallicRoughness.baseColorTexture` + `baseColorFactor`, interpreted by glTF as sRGB color + alpha |
| **Normal Map**           | Normal texture (`Slot 1`)                     | `normalTexture`, interpreted by glTF as linear data                                                     |
| **Roughness / Specular** | Glossiness value + specular texture (`Slot 7`)| `roughnessFactor = 1.0 - clamp(glossiness / 100.0)` + `KHR_materials_specular`                          |
| **Metallic Factor**      | No validated metalness input in the SSE IR    | Fixed to `0.0`; environment mapping is not misclassified as metalness                                   |
| **Emissive / Glow**      | Glow map (`Slot 2`) or emissive color/strength| `emissiveTexture`, `emissiveFactor` and `KHR_materials_emissive_strength`                               |
| **Two-Sided Rendering**  | `SLSF2_Double_Sided` flag                     | `doubleSided: true` only when the flag is set                                                            |
| **Alpha Transparency**   | `NiAlphaProperty` and alpha-related flags     | `alphaMode: "MASK"` with normalized threshold, or `"BLEND"`                                           |

Height/detail, environment, environment-mask, inner-layer and greyscale slots remain in the
`OPEN_SKYRIM_material` extension because core glTF has no equivalent Skyrim shader semantics.
The extension also records premultiplied-alpha and screen-door-alpha requirements. Texture URIs
always target the canonical KTX2 hierarchy; the semantic DDS-to-KTX2 encoding itself is closed by
the following conversion stage. Once that stage has published the hierarchy, the pipeline prunes
from every GLB the texture URIs whose source texture is absent from the installed game data -
including a base-color URI - together with every core or `OPEN_SKYRIM_material` slot that referenced
them, so a NIF naming a texture Bethesda never shipped still publishes and renders with its
remaining maps. The dropped paths are recorded per mesh under `pruned_texture_references` in
`conversion-manifest.json` and do not make the conversion incomplete.

### 4.1 Material animation (`OPEN_SKYRIM_material_animation`)

Skyrim animates hearth flames, lava, steam and glow cards by driving one shader variable from a
keyframe controller: a float controller on the shader property's `NiObjectNET` controller reference,
chained to further controllers through `next_controller`. Nothing in glTF animates a material
variable, so the channels are published on the shape's material as their own extension, beside
`OPEN_SKYRIM_material` (§4):

```json
"extensions": {
  "OPEN_SKYRIM_material_animation": {
    "channels": [{
      "variable": "vOffset",
      "interpolation": "LINEAR" | "QUADRATIC" | "STEP",
      "times": [0.0, 5.6667],
      "values": [0.0, 1.0],
      "tangents": [[1.0, 0.0], [0.0, 1.0]],
      "loop": "cycle" | "reverse" | "clamp",
      "frequency": 1.0, "phase": 0.0, "start": 0.0, "stop": 5.6667
    }]
  }
}
```

* One channel per float controller in the chain, in chain order.
* `variable` is the controlled shader variable in the camelCase spelling of the nif.xml enums.
  Effect shaders (`EffectShaderControlledVariable`): `emissiveMultiple`, `falloffStartAngle`,
  `falloffStopAngle`, `falloffStartOpacity`, `falloffStopOpacity`, `alpha`, `uOffset`, `uScale`,
  `vOffset`, `vScale`. Lighting shaders (`LightingShaderControlledVariable`) share those names for
  the variables the enums share and add `refractionStrength`, `environmentMapScale`, `glossiness`
  and `specularStrength`.
* `interpolation` is the key type: `LINEAR` (1) and `STEP` (5) keys store `(time, value)`;
  `QUADRATIC` (2) keys also store a forward and a backward tangent, published as `tangents`.
* A `uOffset`/`vOffset`/`uScale`/`vScale` channel replaces the matching component of
  the authored material's `uvOffset`/`uvScale` (§4) at playback; the other, unanimated component
  keeps its static value from there. Every other channel variable (`alpha`, `emissiveMultiple`,
  `glossiness`, ...) replaces its own static field the same way, wherever this document or the
  material contract publishes it.
* Units and timing, from the controller's `NiTimeController` fields:
  - `times`, `start` and `stop` are in seconds, as Skyrim's animation clock ticks them.
  - `phase` is also in seconds; it is added to the point in the cycle after scaling by
    `frequency`, i.e. the evaluated time is `now * frequency + phase`, wrapped into `[start, stop]`
    by `loop`.
  - `loop` is the cycle mode in flags bits 1-2 (0 cycle, 1 reverse, 2 clamp): `cycle` restarts at
    `start` once playback passes `stop`; `reverse` means ping-pong - playback runs forward to
    `stop`, then backward to `start`, and repeats; `clamp` holds the value at `stop` (or `start`,
    running backward) once playback reaches it, instead of restarting.
* `tangents` (`QUADRATIC` only) is one `[outgoing, incoming]` pair per key, in value units per
  key interval (the segment parameter `x` runs 0 to 1). The segment from key i to key i+1 is the
  Hermite curve `v = v_i (2x^3 - 3x^2 + 1) + v_{i+1} (-2x^3 + 3x^2) + t1 (x^3 - 2x^2 + x) +
  t2 (x^3 - x^2)` with `t1 = tangents[i][0]` and `t2 = tangents[i+1][1]`. The NIF names the two
  fields the other way round: `outgoing` is the key's `Backward` field and `incoming` its `Forward`
  field, as NifSkope's evaluator reads them (`src/gl/glcontroller.cpp`). The hearth flames publish
  `[[1, 0], [0, 1]]`: a steady scroll.
* Colour controllers (`BSEffectShaderPropertyColorController`,
  `BSLightingShaderPropertyColorController`,
  keyed by an `NiPoint3Interpolator` → `NiPosData`) publish `emissiveColor` (effect colour 0,
  lighting colour 1) or `specularColor` (lighting colour 0) with `"components": 3`. `values` then
  holds three floats per key, key-major (`[r0, g0, b0, r1, ...]`), in the space the static
  colour is published in; `tangents` holds one `[outgoing, incoming]` pair per component, in the
  same order. Each component evaluates like a float channel. `components` is omitted when it is 1.
* The extension is listed in `extensionsUsed`, never `extensionsRequired`: a consumer that does not
  play it renders the shape's still frame.
* A controller with no interpolator, no float data, an unknown variable, an unsupported key type,
  an unknown cycle mode or non-finite key data is dropped and counted in the mesh stage's
  `NifParseDiagnostics::animation_skipped_channels` (reason → count). The same counts appear in
  GLB `asset.extras.openSkyrim.animationSkippedChannels`,
  `PipelineReport.animation_skipped_channels`,
  and the per-file and aggregate `nif-audit` JSON. They are non-gating. Animation never fails a
  conversion. Controller traversal uses visited block indices, with no arbitrary chain-length cap.

---

### 4.2 Authored transforms and billboard metadata

Ordinary static references use the reference placement in place of block zero's local
transform, so render geometry and physics share one placement frame. The synthetic regression
`mesh::placement_tests::conversion_places_render_collision_com_and_inertia_in_one_reference_frame`
checks render vertices, collision, center of mass and inertia under nonidentity root, child
and reference transforms. It also checks that `.btr` and `.BTO` LOD roots remain authored.
Child transforms remain authored. Render export and collision extraction share this policy.
The synthetic regression
`mesh::placement_tests::parsed_skin_keeps_its_bone_tree_under_a_nonidentity_mesh_root`
checks that skeletal export retains its bone transforms. The scene root applies the
Creation-to-runtime rotation once. Billboard nodes retain their authored mode in
`extras.openSkyrim.billboard`.

Transform tracks resolve through their controller manager's `NiDefaultAVObjectPalette`, whose
`AVObject` entries identify source blocks. The exporter retains each source block's actual glTF
node index, including skinned bone bindings. A missing palette or entry retains unique-name
resolution; duplicate names and conflicting palette pointers remain non-gating notices.
Malformed palettes and references to unexported objects omit their affected animation.

The `Quaternion` structure in the pinned nif.xml stores `w, x, y, z`; the decoder
reorders it to glTF's `x, y, z, w`. Translation and scale channels retain the sequence
stop time with a held endpoint. A cubic trailing hold preserves the preceding segment's
incoming derivative and sets its outgoing derivative to zero.

Rotation decoding accepts at most 1,000,000 candidate sample slots per source NIF,
shared across its clips and tracks. Authored times, generated 30 Hz grid points and
endpoints consume this quota before allocation. This bounds sampled time/pose storage
to approximately 20 MB, excluding the source key data; it is decoder resource validation.
The retained vanilla audit's largest published file has 195,654 rotation samples.
The grid uses integer indices, avoiding accumulated floating-point steps that can stall
for very large finite durations. A rejected oversized track emits a non-gating notice;
it consumes no quota and does not discard healthy siblings. Sparse STEP rotations use
authored times and endpoints without allocating a dense duration-based grid.

NIF producer proof version 6 invalidates every retained NIF GLB on the next normal
conversion. Metadata rebuild and repair refuse old proofs rather than upgrading retained
bytes. Asset-only conversion preserves authored material-animation payloads in GLBs but
declares no dataset capabilities without a database core. An existing database still
requires a readable, nonempty build identity before capability publication.

Transform controllers publish ordinary glTF animation clips on their target nodes. Unsupported
or unreadable source tracks retain the static pose and record warnings in the GLB's
`asset.extras.openSkyrim.animationWarnings`. Normal conversion, cache reuse and metadata rebuild
replay those diagnostics into `PipelineReport.animation_notices`, separately from gating
`PipelineReport.warnings`. An unreadable optional track keeps a complete package usable by the
engine. Duplicate exported node names are ambiguous: their tracks are omitted with a notice,
while uniquely resolved sibling tracks still export. A manager-only GLB without a buffer keeps
its static bytes and emits a notice. Diagnostics use only asset filenames; the report supplies
the package-relative asset prefix. Fresh conversion carries diagnostics directly to the result;
manifest entries and staging-journal records retain them with source/configuration/output proofs.
Verified historical entries without diagnostics read their GLB JSON once on a blocking worker;
normal asynchronous receive performs no GLB metadata read. Invalid generated GLB data remains an
error. The decoder documents its nif.xml source and deliberately unsupported layouts in
`nif_animation.rs`.

Each exported shape retains exactly one primitive, including a `BSSubIndexTriShape` with an
authored segment table. All triangles and bounds remain intact. No per-cell segmentation or
`lodSegment` payload is produced, and this feature adds no native LOD discovery or inventory.

When terrain baking is disabled, a converted core with ordered plugin inputs still receives
its current build identity. Inputs are rehashed before stamping; a changed generation is
rejected. Missing plugins or a missing database do not create an artificial identity.

---

## 5. Rust Implementation (`mesh_tools` Builder Architecture)

We use the **`mesh_tools`** crate (`GltfBuilder`), which provides an incredibly clean, ergonomic
API for assembling vertices, normals, UVs, and PBR materials into binary `.glb` files.

```rust
use mesh_tools::GltfBuilder;

pub struct NifToGltfConverter;

impl NifToGltfConverter {
    /// Converts a parsed Skyrim NIF structure into a binary GLB file
    pub fn convert_and_export(nif: &SkyrimNif, output_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = GltfBuilder::new();

        // 1. Create PBR Material
        let material = builder.add_pbr_material(
            Some("SkyrimMaterial".to_string()),
            Some([1.0, 1.0, 1.0, 1.0]), // Base Color (RGBA)
            Some(nif.material.roughness),
            Some(nif.material.metallic),
        );

        // 2. Add Mesh Primitives (Positions, Normals, UVs, Indices)
        let mesh_index = builder.add_custom_mesh(
            Some("SkyrimMesh".to_string()),
            &nif.positions, // Vec<[f32; 3]>
            &nif.normals,   // Vec<[f32; 3]>
            &nif.uvs,       // Vec<[f32; 2]>
            &nif.indices,   // Vec<u32>
            Some(material),
        );

        // 3. Create Scene Node with Transform
        let node_index = builder.add_node(
            Some("RootNode".to_string()),
            Some(mesh_index),
            Some(nif.translation), // [x, y, z]
            Some(nif.rotation),    // Quaternion [x, y, z, w]
            Some(nif.scale),       // [sx, sy, sz]
        );

        builder.add_scene(
            Some("SkyrimScene".to_string()),
            Some(vec![node_index]),
        );

        // 4. Export binary GLB directly to disk
        builder.export_glb(output_path)?;

        Ok(())
    }
}
```

## 6. Collision extras and rigid-body dynamics

Scene extras carry `mudcrabCollision` (`shared::collision::CollisionAsset`): the authored
collision `shapes`, the `skipped` blocks, and, since version 2, a `bodies` array (#104 phase a).
A version 1 asset has no `bodies`; readers treat every shape in it as fixed and ignore body
fields they do not know.

Each `bhkCollisionObject` whose rigid body (`bhkRigidBody` or `bhkRigidBodyT`) yields shapes
produces one `CollisionBody`:

| Field | Source (byte offset in the Skyrim SE body block) | Conversion |
| --- | --- | --- |
| `node`, `target` | the collision object's target `NiNode` | glTF node index and NIF node name |
| `shapes` | the shapes extracted for this body | indices into `shapes` |
| `havok.collision_layer` | havok filter layer (4) | raw |
| `havok.motion_system`, `deactivator_type`, `quality_type` | 224, 225, 227 | raw (`hkMotionType`, `hkDeactivatorType`, `hkQualityType`) |
| `mass` | 180 | unchanged (kg) |
| `inertia` | `hkMatrix3`, three rows of four floats (116) | R I R^T for the shapes' transform, x 70^2, then the Creation-to-runtime basis |
| `center_of_mass` | `Vector4` (164) | x 70, same transform as the shapes, then the runtime basis |
| `linear_damping`, `angular_damping` | 184, 188 | unchanged |
| `friction`, `restitution` | 200, 208 | unchanged |
| `max_linear_velocity` | 212 | x 70 |
| `max_angular_velocity` | 216 | unchanged (rad/s) |

Shapes are read from the body's shape block, through `bhkMoppBvTreeShape`,
`bhkTransformShape`/`bhkConvexTransformShape` and `bhkListShape` wrappers, and map onto
`shared::collision::CollisionShape`:

| NIF block | Shape | Conversion |
| --- | --- | --- |
| `bhkCompressedMeshShape` | `Mesh` | decompressed vertices and triangles |
| `bhkNiTriStripsShape` | `Mesh` | strip data as triangles |
| `bhkBoxShape` | `Hull` | the eight transformed half-extent corners (16) |
| `bhkConvexVerticesShape` | `Hull` | vertices from 36 |
| `bhkCylinderShape` | `Hull` | two 16-point rings around the A-B axis (A @16, B @32), radius @48 |
| `bhkCapsuleShape` | `Capsule` | A @16 and B @32, radius max(Radius 1 @28, Radius 2 @44) |
| `bhkSphereShape` | `Capsule` | zero-length capsule (a = b = the shape origin), radius @4 |
| `bhkMultiSphereShape` | `Capsule` per sphere | count @16 (1..=8), `NiBound {centre, radius}` from @20 |

Points are Havok units scaled by 70 before the Creation-to-runtime basis; radii are scaled by
70 and the transform scale, like the capsule arm. A sphere or multi-sphere under a non-uniform
or sheared transform has no single radius and is skipped. A degenerate cylinder (A = B, radius
below or equal to zero, non-finite), an out-of-range multi-sphere count and any malformed or
unsupported shape land in `skipped` instead of becoming collision.

Only bodies on physical Skyrim layers are read (`SkyrimLayer` in nif.xml): 0 UNIDENTIFIED,
1 STATIC, 2 ANIMSTATIC, 3 TRANSPARENT, 4 CLUTTER, 5 WEAPON, 9 TREES, 10 PROPS, 13 TERRAIN,
17 GROUND, 26 TRANSPARENT_SMALL, 27 INVISIBLE_WALL, 28 TRANSPARENT_SMALL_ANIM, 31 STAIRHELPER
and 35 COLLISIONBOX. Layer 15 NONCOLLIDABLE (harvestable flora and other nonphysical objects)
is dropped silently; layer 12 TRIGGER is skipped with the reason "trigger volume (layer 12) is
not physical"; every other layer is reported as unsupported.

`kind` is `dynamic` when the motion system is a simulated one (dynamic, sphere or box inertia,
plain or stabilized, thin box: 1, 2, 3, 4, 5, 8), the quality type is a moving one (debris,
moving, critical, bullet: 3, 4, 5, 6) and the mass is finite and above zero; `keyframed` for
`MO_SYS_KEYFRAMED` (6) with mass 0; otherwise `fixed`. `convex` is true when every shape of the
body is a box, capsule or hull (a compressed or strip mesh makes it false).

The `node` index and `target` name are checked against the GLB that is actually written, not the
source NIF's static node list (skeletal and effect NIFs lay their nodes out differently): a body
is kept only when `target` is non-empty and the GLB's node at `node` has that name. A name
used by more than one node is trusted at its index only when the GLB's nodes start in the
same order as the NIF's static scene, which the index was predicted from; otherwise, and
always when the collision is added to a GLB built elsewhere (`annotate`), the name must be
unique.

A body on an unsupported layer, a trigger, a non-colliding body (a `HavokFilter` with the
"No Collision" flag, or a collision response of 2 RESPONSE_REPORTING or 3 RESPONSE_NONE, in
either of the two copies a body stores) or a body whose target node cannot be resolved is listed
in `skipped` and contributes no collision. A body whose shapes cannot all be read is listed in
`skipped`; the shapes it did read stay as fixed collision. A body whose dynamics cannot be read,
or whose glTF node fails the check above, is listed in `skipped` but its shapes stay as fixed
collision.

A cylinder's 16-point rings are inscribed in the true circle (about 2% inside at the chord
midpoints); like the box and hull arms, the Havok convex radius (a thin shell) is not added.

Collision coverage (sphere, multi-sphere and cylinder shapes) moved the converter to schema 27:
schema 24 and 26 outputs keep their verified scripts and archive ingestion and regenerate GLBs,
textures and world data, so no cached GLB keeps the older collision extras.

## 7. Skinning support boundary

### 7.1 Supported

- **Legacy skin instances:** `NiSkinInstance` and `BSDismemberSkinInstance`. Joint names come from
  `instance.bones` (block references resolved to `NiNode` names); inverse bind matrices come from
  the referenced `NiSkinData`, one per joint in the same order. Per-vertex joints and weights come
  from the shape's vertex buffer, or from the skin partition when the buffer carries none.
- **`BSSkin::Instance`** (Fallout 4) behaviour is unchanged. It does not occur in Skyrim meshes.
- **Geometry:** `BSTriShape` and `BSDynamicTriShape`. A dynamic shape keeps its live positions in
  the trailing dynamic `Vec4` array while `data_size` is zero; UVs, triangles and per-vertex
  weights come from the referenced `NiSkinPartition`. Missing vertex channels are filled from the
  partition channel by channel, not all-or-nothing.
- **Partition layouts:** both the SSE layout (inline vertex buffer plus per-partition records) and
  the legacy Bethesda layout. Multi-partition blocks are merged into one bone palette, and a
  partition whose palette differs is remapped into the merged one. The concatenated vertex map
  ties each weight row to its shape vertex, including when the row count equals the vertex count
  (the map can be a permutation) and when partitions repeat shared vertices. Rows are read in
  shape-vertex order only when the partition has no map.
- Bone indices in a shared vertex buffer (the shape's, or the partition's SSE copy) address the
  skin instance's bone list directly. Only the per-partition `bone_indices` rows are palette-local
  and are remapped through `bone_palette`. Only influences with a non-zero weight are validated,
  and weights are normalised to sum to 1.
- Inverse binds come from each `NiSkinData` bone transform as translation · rotation · scale.
- A skeleton that repeats a node name keeps the first occurrence; a skin that binds a repeated
  name is rejected, because either copy could be the bone it was authored against.
- Each skinned shape becomes one `SkeletalMesh` that records its source block, so material and
  vertex-colour contracts associate by block instead of by shape-type enumeration order.

### 7.2 Rejected

- `BSDismember` body-part flags are retained for diagnostics only; they do not split meshes.
- Legacy `NiTriShape` / `NiTriShapeData` geometry is not skinned (no such file exists in the
  Skyrim SE actor meshes).
- `BSSubIndexTriShape` skinning is not handled (same dataset: no such block).

### 7.3 Failure contract

A NIF that declares a skin and cannot produce one fails conversion. It does not publish a static
substitute and does not emit an empty scene. The diagnostic names the NIF path and the block
indices, lists every skin instance that no exported shape claimed together with the block type that
references it, and reports all unresolved joint names at once rather than the first one.

Classes that still fail:

- a rig that some skeleton covers at 50% or more, while no skeleton covers it fully and no path
  candidate covers it at 50%: the resolver does not guess between partial matches;
- a skin that binds a name its resolved skeleton repeats (§7.1).

Not failures, but approximations with their own follow-ups: a rig no skeleton covers at 50% converts
self-contained from its own hierarchy (FX meshes skinned to their own controller bones are the
retail case), and a skeleton only a RACE record identifies (for example Alduin) is chosen by joint
coverage instead, which can pick a near-identical rig.
