//! Deterministic NIF static-shape fixtures for Skyrim SE (`20.2.0.7`).
//!
//! The writer emits the minimal block set the converter renders:
//! `BSFadeNode` → `BSTriShape` → `BSLightingShaderProperty` → `BSShaderTextureSet`.
//! Geometry is validated before serialization and the output is byte-stable
//! for identical input.

use crate::bytes::{push_u16, push_u32, push_u64};
use color_eyre::{
    Result,
    eyre::{ensure, eyre},
};

const NIF_VERSION: u32 = 0x1402_0007;
const USER_VERSION: u32 = 12;
const BETHESDA_VERSION: u32 = 100;
const NULL_REF: u32 = u32::MAX;
const SHADER_TYPE_DEFAULT: u32 = 0;
/// `skin_tint`, whose property ends with the three-float skin tint colour.
const SHADER_TYPE_SKIN_TINT: u32 = 5;
const VERTEX_FLAGS: u16 = 0x0001 | 0x0002 | 0x0008;
const VERTEX_COLOR_FLAG: u16 = 0x0020;
const VERTEX_STRIDE: u8 = 6;
const VERTEX_FLAG_NORMAL: u16 = 0x0008;
const SKIN_VERTEX_FLAGS: u16 = 0x0001 | 0x0002 | VERTEX_FLAG_NORMAL | 0x0040;
const SLSF1_SKINNED: u32 = 1 << 1;
const SLSF1_MODEL_SPACE_NORMALS: u32 = 1 << 12;
const IDENTITY_ROTATION: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
/// Offsets in the vertex descriptor are counted in four-byte units.
const UV_OFFSET: u64 = 4;
const NORMAL_OFFSET: u64 = 5;
const COLOR_OFFSET: u64 = 6;
/// Bytes a shape block writes before its vertex and triangle data: `NiAVObject`,
/// the bounds, the raw property references, the vertex descriptor and the three
/// payload counts.
const SHAPE_BLOCK_HEADER: usize = 116;

/// Block indices of the static (`.nif`) layout.
const STATIC_SHADER_BLOCK: u32 = 2;
const STATIC_TEXTURE_SET_BLOCK: u32 = 3;

/// `KeyType::QuadraticKey`: keys carry a forward and a backward tangent.
const KEY_TYPE_QUADRATIC: u32 = 2;

/// A triangle mesh rendered as a single static shape.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticShape<'a> {
    /// Shape name stored in the NIF string table.
    pub name: &'a str,
    /// Vertex positions.
    pub positions: &'a [[f32; 3]],
    /// Per-vertex normals; must match `positions`.
    pub normals: &'a [[f32; 3]],
    /// Per-vertex UVs; must match `positions`.
    pub uvs: &'a [[f32; 2]],
    /// Triangle indices into `positions`.
    pub indices: &'a [[u16; 3]],
    /// Diffuse texture path, for example `textures/generated_color.dds`.
    pub diffuse: &'a str,
    /// Normal texture path.
    pub normal_texture: &'a str,
}

/// A Havok box rigid body attached to its own `NiNode` under the root, written as a
/// `bhkCollisionObject` -> `bhkRigidBody` (or `bhkRigidBodyT`) -> `bhkBoxShape` chain.
/// Every value is stored raw, in Havok units (a Creation unit is 1/70 of one).
#[derive(Debug, Clone, PartialEq)]
pub struct BoxBody<'a> {
    /// Name of the `NiNode` the collision object targets.
    pub node_name: &'a str,
    /// Box half extents (`bhkBoxShape` dimensions).
    pub half_extents: [f32; 3],
    /// `Some((translation, rotation xyzw))` writes a `bhkRigidBodyT`, else a `bhkRigidBody`.
    pub transform: Option<([f32; 3], [f32; 4])>,
    pub collision_layer: u8,
    /// `CollisionFilterFlags` and `hkResponseType` of the `bhkWorldObject`/`bhkEntityCInfo` copy.
    pub collision_flags: u8,
    pub collision_response: u8,
    /// The same two values in the `bhkRigidBodyCInfo2010` copy.
    pub inner_collision_flags: u8,
    pub inner_collision_response: u8,
    /// The body's shape: a box with `half_extents`, or a sphere or cylinder.
    pub shape: BodyShape,
    /// Raw `hkMotionType`, `hkDeactivatorType` and `hkQualityType` values (nif.xml).
    pub motion_system: u8,
    pub deactivator_type: u8,
    pub quality_type: u8,
    pub mass: f32,
    /// Row-major inertia tensor.
    pub inertia: [f32; 9],
    pub center_of_mass: [f32; 3],
    pub linear_damping: f32,
    pub angular_damping: f32,
    pub friction: f32,
    pub restitution: f32,
    pub max_linear_velocity: f32,
    pub max_angular_velocity: f32,
}

/// Shape block written for a [`BoxBody`] (Havok units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BodyShape {
    /// `bhkBoxShape` with the body's `half_extents`.
    Box,
    /// `bhkSphereShape`.
    Sphere { radius: f32 },
    /// `bhkMultiSphereShape`: the first `count` of `spheres` (centre, radius).
    MultiSphere {
        count: u32,
        spheres: [([f32; 3], f32); 8],
    },
    /// `bhkCylinderShape` from `a` to `b`.
    Cylinder {
        a: [f32; 3],
        b: [f32; 3],
        radius: f32,
    },
}

/// Which skin-instance block a [`LegacySkin`] writes. `Dismember` emits a
/// `BSDismemberSkinInstance` with its trailing body-part array; `Plain` emits a
/// bare `NiSkinInstance` with no trailing array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinInstanceKind {
    Dismember,
    Plain,
}

/// A deterministic legacy-skinned NIF: a `BSTriShape` bound to two bones through
/// a `NiSkinInstance` (or `BSDismemberSkinInstance`) → `NiSkinData` →
/// `NiSkinPartition` chain.
///
/// The block set is `BSFadeNode` → `NiNode` × 2 (bones) → `BSTriShape` → skin
/// instance → `NiSkinData` → `NiSkinPartition`, plus `BSLightingShaderProperty`
/// → `BSShaderTextureSet` so the material path stays exercised.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacySkin<'a> {
    /// Shape name stored in the NIF string table.
    pub name: &'a str,
    /// `BSDismemberSkinInstance` (with trailing array) or bare `NiSkinInstance`.
    pub instance_kind: SkinInstanceKind,
    /// Non-identity inverse binds for the two bones: `(translation, xyzw
    /// rotation, scale)`.
    pub bone_transforms: [([f32; 3], [f32; 4], f32); 2],
    /// Partition bone palette (indices into the instance's bone list), shuffled
    /// relative to identity so a missing palette remap is observable.
    pub palette: &'a [u16],
    /// Per-vertex influence weights; one row per vertex, four influences that
    /// deliberately do not sum to 1.0 before normalization.
    pub weights: &'a [[f32; 4]],
    /// When false, the skin blocks and bones are emitted without a `BSTriShape`.
    pub with_geometry: bool,
    /// Raw palette indices for vertex 0's influences, used to build the
    /// out-of-range-joint case. An index outside the palette is written into
    /// the vertex buffers unchanged.
    pub joint_override: Option<[u8; 4]>,
}

/// Generates a minimal static NIF containing one triangle mesh.
pub fn static_shape(shape: &StaticShape<'_>) -> Result<Vec<u8>> {
    static_shape_with_colors(shape, &[])
}

/// Generates a static shape under a `NiBillboardNode` with the raw nif.xml mode.
pub fn static_shape_with_billboard(shape: &StaticShape<'_>, mode: u16) -> Result<Vec<u8>> {
    validate(shape)?;
    let mut root = fade_node();
    push_u16(&mut root, mode);
    write_nif(
        &[
            "NiBillboardNode",
            "BSTriShape",
            "BSLightingShaderProperty",
            "BSShaderTextureSet",
        ],
        &[
            root,
            write_shape_block(&geometry(shape, &[]), STATIC_SHADER_BLOCK)?,
            lighting_shader_property(STATIC_TEXTURE_SET_BLOCK, false),
            texture_set(shape.diffuse, shape.normal_texture)?,
        ],
        &[shape.name],
    )
}

/// Generates a static NIF with authored RGBA vertex data and both color flags enabled.
pub fn static_shape_with_colors(shape: &StaticShape<'_>, colors: &[[u8; 4]]) -> Result<Vec<u8>> {
    build_static_shape(shape, colors, &[])
}

/// Like [`static_shape`], plus one collision body per entry. Each body hangs off its own
/// `NiNode` child of the root, so the converter's glTF node order is root, shape, then the
/// body nodes in order.
pub fn static_shape_with_bodies(
    shape: &StaticShape<'_>,
    bodies: &[BoxBody<'_>],
) -> Result<Vec<u8>> {
    build_static_shape(shape, &[], bodies)
}

/// Generates an ordinary static NIF whose shape carries an authored segment table.
/// This fixture checks that segment metadata does not multiply exported primitives.
pub fn static_shape_with_segment_table(
    shape: &StaticShape<'_>,
    segment_counts: &[u32],
) -> Result<Vec<u8>> {
    validate(shape)?;
    ensure!(
        segment_counts.len() <= 16,
        "NIF segment table exceeds 16 entries"
    );
    let total: u64 = segment_counts.iter().map(|&count| u64::from(count)).sum();
    ensure!(
        total == shape.indices.len() as u64,
        "NIF segment triangle count mismatch"
    );
    let mut block = write_shape_block(&geometry(shape, &[]), STATIC_SHADER_BLOCK)?;
    push_u32(&mut block, 0); // BSSubIndexTriShape's inherited trailing word
    push_u32(&mut block, u32::try_from(segment_counts.len())?);
    for &count in segment_counts {
        block.push(0);
        push_u32(&mut block, 0);
        push_u32(&mut block, count);
    }
    write_nif(
        &[
            "BSFadeNode",
            "BSSubIndexTriShape",
            "BSLightingShaderProperty",
            "BSShaderTextureSet",
        ],
        &[
            fade_node(),
            block,
            lighting_shader_property(STATIC_TEXTURE_SET_BLOCK, false),
            texture_set(shape.diffuse, shape.normal_texture)?,
        ],
        &[shape.name],
    )
}

fn build_static_shape(
    shape: &StaticShape<'_>,
    colors: &[[u8; 4]],
    bodies: &[BoxBody<'_>],
) -> Result<Vec<u8>> {
    validate(shape)?;
    ensure!(
        colors.is_empty() || colors.len() == shape.positions.len(),
        "vertex color count mismatch"
    );
    for body in bodies {
        validate_body(body)?;
    }
    let mut strings = vec![shape.name];
    strings.extend(bodies.iter().map(|body| body.node_name));
    let first_body_block = 4_u32;
    let mut children = vec![1_u32];
    children.extend((0..bodies.len()).map(|i| first_body_block + 4 * i as u32));
    let mut blocks = vec![
        fade_node_with_children(&children),
        write_shape_block(&geometry(shape, colors), STATIC_SHADER_BLOCK)?,
        lighting_shader_property(STATIC_TEXTURE_SET_BLOCK, !colors.is_empty()),
        texture_set(shape.diffuse, shape.normal_texture)?,
    ];
    let mut block_types = vec![
        "BSFadeNode",
        "BSTriShape",
        "BSLightingShaderProperty",
        "BSShaderTextureSet",
    ];
    for (index, body) in bodies.iter().enumerate() {
        let node_block = first_body_block + 4 * index as u32;
        let mut node = Vec::new();
        push_av_object_with(&mut node, 1 + index as u32, node_block + 1);
        push_u32(&mut node, 0); // children
        push_u32(&mut node, 0); // effects
        blocks.push(node);
        block_types.push("NiNode");
        let mut object = Vec::new();
        push_u32(&mut object, node_block); // target
        push_u16(&mut object, 1); // flags
        push_u32(&mut object, node_block + 2); // body
        blocks.push(object);
        block_types.push("bhkCollisionObject");
        blocks.push(rigid_body(body, node_block + 3));
        block_types.push(if body.transform.is_some() {
            "bhkRigidBodyT"
        } else {
            "bhkRigidBody"
        });
        let (shape, kind) = body_shape(body);
        blocks.push(shape);
        block_types.push(kind);
    }
    write_nif(&block_types, &blocks, &strings)
}

/// How a legacy skin fixture supplies its shading normals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkinNormals {
    /// Packed normals in every vertex buffer, tangent-space normal map.
    #[default]
    Authored,
    /// No vertex normals, and SLSF1 `Model_Space_Normals` set, as retail bodies, hands, feet
    /// and FaceGen heads ship.
    ModelSpaceMap,
    /// No vertex normals and a tangent-space normal map.
    Omitted,
}

impl SkinNormals {
    fn in_vertices(self) -> bool {
        self == Self::Authored
    }

    fn shader_flags_1(self) -> u32 {
        match self {
            Self::ModelSpaceMap => SLSF1_SKINNED | SLSF1_MODEL_SPACE_NORMALS,
            Self::Authored | Self::Omitted => 0,
        }
    }

    /// Bytes per vertex: position and bitangent X, UV, the optional normal word, then four
    /// half weights and four bone indices.
    fn vertex_size(self) -> u32 {
        if self.in_vertices() { 36 } else { 32 }
    }

    fn vertex_desc(self) -> u64 {
        let flags = if self.in_vertices() {
            SKIN_VERTEX_FLAGS
        } else {
            SKIN_VERTEX_FLAGS & !VERTEX_FLAG_NORMAL
        };
        let quads = u64::from(self.vertex_size() / 4);
        // Offsets in quads: UV after the 16-byte position, then the normal and skin data.
        let offsets = if self.in_vertices() {
            (4 << 8) | (5 << 16) | (6 << 28)
        } else {
            (4 << 8) | (5 << 28)
        };
        quads | offsets | (u64::from(flags) << 44)
    }
}

/// Generates a legacy-skinned NIF from `spec`, byte-stable for identical input.
pub fn legacy_skinned_shape(spec: &LegacySkin<'_>) -> Result<Vec<u8>> {
    legacy_skinned_shape_with(spec, LegacyShapeKind::Plain, false, SkinNormals::Authored)
}

/// Like [`legacy_skinned_shape`], with the shading normals supplied as `normals` says.
pub fn legacy_skinned_shape_with_normals(
    spec: &LegacySkin<'_>,
    normals: SkinNormals,
) -> Result<Vec<u8>> {
    legacy_skinned_shape_with(spec, LegacyShapeKind::Plain, false, normals)
}

/// Like [`legacy_dynamic_skinned_shape`], with the shading normals supplied as `normals` says.
pub fn legacy_dynamic_skinned_shape_with_normals(
    spec: &LegacySkin<'_>,
    normals: SkinNormals,
) -> Result<Vec<u8>> {
    legacy_skinned_shape_with(spec, LegacyShapeKind::Dynamic, false, normals)
}

/// Generates one legacy-skinned NIF with one `BSTriShape` per spec, all bound
/// to the same two bones. Every shape uses the plain skin kind.
pub fn legacy_skinned_shapes(specs: &[LegacySkin<'_>]) -> Result<Vec<u8>> {
    legacy_skinned_shapes_with_scale(specs, 1.0)
}

/// Like [`legacy_skinned_shapes`], with every vertex position multiplied by `scale`.
///
/// The topology is unchanged, so the result can stand in for a thin body part
/// against the unit-scale one.
pub fn legacy_skinned_shapes_scaled(specs: &[LegacySkin<'_>], scale: f32) -> Result<Vec<u8>> {
    legacy_skinned_shapes_with_scale(specs, scale)
}

fn legacy_skinned_shapes_with_scale(specs: &[LegacySkin<'_>], scale: f32) -> Result<Vec<u8>> {
    ensure!(!specs.is_empty(), "a skinned NIF needs at least one shape");
    for spec in specs {
        validate_skin(spec)?;
    }
    ensure!(
        specs
            .windows(2)
            .all(|pair| pair[0].bone_transforms == pair[1].bone_transforms),
        "shapes sharing a skeleton need the same bone transforms"
    );

    let mut strings = specs.iter().map(|spec| spec.name).collect::<Vec<_>>();
    let bone0 = u32::try_from(strings.len())?;
    strings.push("Bone0");
    let bone1 = u32::try_from(strings.len())?;
    strings.push("Bone1");

    let mut children = vec![1_u32, 2];
    children.extend((0..specs.len()).map(|index| 3 + 6 * index as u32));
    let mut blocks = vec![
        fade_node_with_children(&children),
        bone_node(bone0),
        bone_node(bone1),
    ];
    let mut block_types = vec!["BSFadeNode", "NiNode", "NiNode"];

    for (index, spec) in specs.iter().enumerate() {
        let shape_block = 3 + 6 * index as u32;
        let instance_block = shape_block + 1;
        let data_block = shape_block + 2;
        let partition_block = shape_block + 3;
        let shader_block = shape_block + 4;
        let texture_block = shape_block + 5;
        let vertices = skin_vertices_scaled(spec, scale);
        let triangles = fan_triangles(vertices.len());
        let positions = vertices
            .iter()
            .map(|vertex| vertex.position)
            .collect::<Vec<_>>();

        blocks.push(skinned_tri_shape(
            index as u32,
            &positions,
            instance_block,
            shader_block,
            &vertices,
            &triangles,
            SkinNormals::Authored,
        )?);
        block_types.push("BSTriShape");
        blocks.push(skin_instance(
            spec.instance_kind,
            data_block,
            partition_block,
            1,
            2,
        ));
        block_types.push(match spec.instance_kind {
            SkinInstanceKind::Dismember => "BSDismemberSkinInstance",
            SkinInstanceKind::Plain => "NiSkinInstance",
        });
        blocks.push(skin_data(&spec.bone_transforms, spec.weights)?);
        block_types.push("NiSkinData");
        blocks.push(skin_partition(
            &vertices,
            &triangles,
            spec.palette,
            spec.weights,
            SkinNormals::Authored,
        )?);
        block_types.push("NiSkinPartition");
        blocks.push(lighting_shader_property(texture_block, false));
        block_types.push("BSLightingShaderProperty");
        blocks.push(texture_set(
            "textures/generated_skin.dds",
            "textures/generated_skin_n.dds",
        )?);
        block_types.push("BSShaderTextureSet");
    }

    write_nif(&block_types, &blocks, &strings)
}

/// Generates a legacy-skinned NIF whose second bone hangs off an extra `NiNode`
/// under the first bone, so a skin joint can have a missing ancestor chain.
pub fn legacy_nested_skinned_shape(spec: &LegacySkin<'_>) -> Result<Vec<u8>> {
    legacy_skinned_shape_with(spec, LegacyShapeKind::Plain, true, SkinNormals::Authored)
}

/// Z offset applied to a dynamic fixture's live positions so they differ from
/// the partition copy of the same vertices.
pub const DYNAMIC_POSITION_OFFSET: f32 = 7.0;

/// Generates a legacy-skinned NIF whose shape is a `BSDynamicTriShape`.
///
/// The base `BSTriShape` carries no vertex or triangle payload, mirroring
/// retail facegen heads: the partition holds normals, UVs and triangles, while
/// positions come from the trailing dynamic array. Those positions are offset
/// by [`DYNAMIC_POSITION_OFFSET`] so a test can tell which source won.
pub fn legacy_dynamic_skinned_shape(spec: &LegacySkin<'_>) -> Result<Vec<u8>> {
    legacy_skinned_shape_with(spec, LegacyShapeKind::Dynamic, false, SkinNormals::Authored)
}

/// Generates a legacy-skinned NIF whose geometry block is a `BSLODTriShape`.
/// The legacy skin exporter does not emit LOD shapes, so this exercises the
/// diagnostic that names the shape type no exported mesh claimed.
pub fn legacy_lod_skinned_shape(spec: &LegacySkin<'_>) -> Result<Vec<u8>> {
    legacy_skinned_shape_with(spec, LegacyShapeKind::Lod, false, SkinNormals::Authored)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyShapeKind {
    Plain,
    Dynamic,
    Lod,
}

/// Like [`legacy_skinned_shape_with_normals`] with [`SkinNormals::ModelSpaceMap`], drawn by a
/// `skin_tint` lighting shader (type 5) with a white tint and the Specular flag, as retail bodies,
/// hands and feet are.
pub fn legacy_skin_tint_shape(spec: &LegacySkin<'_>) -> Result<Vec<u8>> {
    legacy_skinned_shape_with_shader(
        spec,
        LegacyShapeKind::Plain,
        false,
        SkinNormals::ModelSpaceMap,
        true,
    )
}

fn legacy_skinned_shape_with(
    spec: &LegacySkin<'_>,
    kind: LegacyShapeKind,
    nested: bool,
    normals: SkinNormals,
) -> Result<Vec<u8>> {
    legacy_skinned_shape_with_shader(spec, kind, nested, normals, false)
}

fn legacy_skinned_shape_with_shader(
    spec: &LegacySkin<'_>,
    kind: LegacyShapeKind,
    nested: bool,
    normals: SkinNormals,
    skin_tint: bool,
) -> Result<Vec<u8>> {
    validate_skin(spec)?;

    let (bone0, bone1, shape_block, instance_block) = if nested {
        (
            1_u32,
            3_u32,
            spec.with_geometry.then_some(4_u32),
            if spec.with_geometry { 5 } else { 4 },
        )
    } else {
        (
            1_u32,
            2_u32,
            spec.with_geometry.then_some(3_u32),
            if spec.with_geometry { 4 } else { 3 },
        )
    };
    let data_block = instance_block + 1;
    let partition_block = data_block + 1;
    let shader_block = partition_block + 1;
    let texture_block = shader_block + 1;

    let vertices = skin_vertices(spec);
    let triangles = fan_triangles(vertices.len());
    let positions = vertices
        .iter()
        .map(|vertex| vertex.position)
        .collect::<Vec<_>>();

    let mut strings = vec![spec.name, "Bone0", "Bone1"];
    let (mut blocks, mut block_types) = if nested {
        strings.push("NestedParent");
        let mut children = vec![bone0];
        if let Some(shape) = shape_block {
            children.push(shape);
        }
        (
            vec![
                fade_node_with_children(&children),
                bone_node_with_children(1, &[2]),
                bone_node_with_children(3, &[bone1]),
                bone_node(2),
            ],
            vec!["BSFadeNode", "NiNode", "NiNode", "NiNode"],
        )
    } else {
        let mut children = vec![bone0, bone1];
        if let Some(shape) = shape_block {
            children.push(shape);
        }
        (
            vec![
                fade_node_with_children(&children),
                bone_node(bone0),
                bone_node(bone1),
            ],
            vec!["BSFadeNode", "NiNode", "NiNode"],
        )
    };
    if shape_block.is_some() {
        match kind {
            LegacyShapeKind::Plain => {
                blocks.push(skinned_tri_shape(
                    0,
                    &positions,
                    instance_block,
                    shader_block,
                    &vertices,
                    &triangles,
                    normals,
                )?);
                block_types.push("BSTriShape");
            }
            LegacyShapeKind::Dynamic => {
                let dynamic_positions = positions
                    .iter()
                    .map(|position| {
                        [
                            position[0],
                            position[1],
                            position[2] + DYNAMIC_POSITION_OFFSET,
                        ]
                    })
                    .collect::<Vec<_>>();
                blocks.push(dynamic_tri_shape(
                    0,
                    &dynamic_positions,
                    instance_block,
                    shader_block,
                    normals,
                )?);
                block_types.push("BSDynamicTriShape");
            }
            LegacyShapeKind::Lod => {
                let mut block = skinned_tri_shape(
                    0,
                    &positions,
                    instance_block,
                    shader_block,
                    &vertices,
                    &triangles,
                    normals,
                )?;
                push_u32(&mut block, 0);
                push_u32(&mut block, 0);
                push_u32(&mut block, 0);
                push_u32(&mut block, 0);
                blocks.push(block);
                block_types.push("BSLODTriShape");
            }
        }
    }
    blocks.push(skin_instance(
        spec.instance_kind,
        data_block,
        partition_block,
        bone0,
        bone1,
    ));
    block_types.push(match spec.instance_kind {
        SkinInstanceKind::Dismember => "BSDismemberSkinInstance",
        SkinInstanceKind::Plain => "NiSkinInstance",
    });
    blocks.push(skin_data(&spec.bone_transforms, spec.weights)?);
    block_types.push("NiSkinData");
    blocks.push(skin_partition(
        &vertices,
        &triangles,
        spec.palette,
        spec.weights,
        normals,
    )?);
    block_types.push("NiSkinPartition");
    // Retail skin shaders set SLSF1 Specular (bit 0) on top of the skinning and normal flags.
    let flags_1 = normals.shader_flags_1() | u32::from(skin_tint);
    let mut shader = lighting_shader_property_with_flags(flags_1, 0, texture_block);
    if skin_tint {
        shader[0..4].copy_from_slice(&SHADER_TYPE_SKIN_TINT.to_le_bytes());
        for channel in [1.0_f32; 3] {
            push_f32(&mut shader, channel);
        }
    }
    blocks.push(shader);
    block_types.push("BSLightingShaderProperty");
    blocks.push(texture_set(
        "textures/generated_skin.dds",
        "textures/generated_skin_n.dds",
    )?);
    block_types.push("BSShaderTextureSet");

    write_nif(&block_types, &blocks, &strings)
}

/// Geometry shared by the authored shape writers.
struct Geometry<'a> {
    positions: &'a [[f32; 3]],
    normals: &'a [[f32; 3]],
    uvs: &'a [[f32; 2]],
    indices: &'a [[u16; 3]],
    /// Per-vertex RGBA colours, written only when present.
    colors: Option<&'a [[u8; 4]]>,
}

/// One shader float controller of a fixture effect shape.
///
/// One entry becomes a `NiTimeController` → `NiFloatInterpolator` →
/// `NiFloatData` triple, chained through `next_controller` in list order.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatController<'a> {
    /// `NiTimeController` flags; bits 1-2 are the cycle mode.
    pub flags: u16,
    pub frequency: f32,
    pub phase: f32,
    pub start_time: f32,
    pub stop_time: f32,
    /// Controlled variable index, as `EffectShaderControlledVariable` numbers it.
    pub variable: u32,
    /// Key type as `KeyType` numbers it: `1` linear, `2` quadratic, `5` constant.
    pub key_type: u32,
    /// `(time, value, forward tangent, backward tangent)` per key. The tangents
    /// are written only for the quadratic key type.
    pub keys: &'a [[f32; 4]],
}

/// Generates a minimal effect-shader NIF whose shader property is animated.
///
/// The block set is `BSFadeNode` → `BSTriShape` → `BSEffectShaderProperty`,
/// followed by one interpolator, one float data and one controller block per
/// entry in `controllers`, in that order. The shader property's controller
/// reference points at the first controller; each controller's `target` points
/// back at the shader property and its `next_controller` at the following one.
pub fn effect_shape_with_controllers(
    shape: &StaticShape<'_>,
    controllers: &[FloatController<'_>],
) -> Result<Vec<u8>> {
    validate(shape)?;
    let mut blocks = vec![
        fade_node(),
        triangle_shape_with_shader(shape, 2)?,
        effect_shader_property(shape, controller_block_index(0))?,
    ];
    for (index, controller) in controllers.iter().enumerate() {
        ensure!(
            !controller.keys.is_empty(),
            "NIF float controller {index} has no keys"
        );
        let interpolator = interpolator_block_index(index);
        let next_controller = if index + 1 < controllers.len() {
            controller_block_index(index + 1)
        } else {
            NULL_REF
        };
        blocks.push(float_interpolator(interpolator + 1));
        blocks.push(float_data(controller)?);
        blocks.push(float_controller(controller, interpolator, next_controller));
    }
    let mut block_types = vec![
        "BSFadeNode".to_owned(),
        "BSTriShape".to_owned(),
        "BSEffectShaderProperty".to_owned(),
    ];
    for _ in controllers {
        block_types.extend(
            [
                "NiFloatInterpolator",
                "NiFloatData",
                "BSEffectShaderPropertyFloatController",
            ]
            .map(str::to_owned),
        );
    }
    let block_types = block_types.iter().map(String::as_str).collect::<Vec<_>>();
    write_nif(&block_types, &blocks, &[shape.name])
}

/// One `BSEffectShaderPropertyColorController` on an effect shape's emissive colour, with its
/// `NiPoint3Interpolator` and `NiPosData`.
pub struct ColorController<'a> {
    /// `NiTimeController` flags; bits 1-2 are the cycle mode.
    pub flags: u16,
    pub frequency: f32,
    pub phase: f32,
    pub start_time: f32,
    pub stop_time: f32,
    /// Key type as `KeyType` numbers it: `1` linear, `2` quadratic, `5` constant.
    pub key_type: u32,
    /// `(time, r, g, b, forward rgb, backward rgb)` per key. The tangents are written only for
    /// the quadratic key type.
    pub keys: &'a [[f32; 10]],
}

/// An effect shape whose controller chain starts with an emissive-colour controller and
/// continues with `floats`, in that order.
pub fn effect_shape_with_color_controller(
    shape: &StaticShape<'_>,
    color: &ColorController<'_>,
    floats: &[FloatController<'_>],
) -> Result<Vec<u8>> {
    validate(shape)?;
    ensure!(!color.keys.is_empty(), "NIF colour controller has no keys");
    let mut blocks = vec![
        fade_node(),
        triangle_shape_with_shader(shape, 2)?,
        effect_shader_property(shape, controller_block_index(0))?,
    ];
    let next = if floats.is_empty() {
        NULL_REF
    } else {
        controller_block_index(1)
    };
    blocks.push(point3_interpolator(interpolator_block_index(0) + 1));
    blocks.push(pos_data(color)?);
    blocks.push(color_controller(color, interpolator_block_index(0), next));
    for (index, controller) in floats.iter().enumerate() {
        let index = index + 1;
        ensure!(
            !controller.keys.is_empty(),
            "NIF float controller {index} has no keys"
        );
        let interpolator = interpolator_block_index(index);
        let next_controller = if index < floats.len() {
            controller_block_index(index + 1)
        } else {
            NULL_REF
        };
        blocks.push(float_interpolator(interpolator + 1));
        blocks.push(float_data(controller)?);
        blocks.push(float_controller(controller, interpolator, next_controller));
    }
    let mut block_types = [
        "BSFadeNode",
        "BSTriShape",
        "BSEffectShaderProperty",
        "NiPoint3Interpolator",
        "NiPosData",
        "BSEffectShaderPropertyColorController",
    ]
    .map(str::to_owned)
    .to_vec();
    for _ in floats {
        block_types.extend(
            [
                "NiFloatInterpolator",
                "NiFloatData",
                "BSEffectShaderPropertyFloatController",
            ]
            .map(str::to_owned),
        );
    }
    let block_types = block_types.iter().map(String::as_str).collect::<Vec<_>>();
    write_nif(&block_types, &blocks, &[shape.name])
}

/// Block indices of one controller's triple: the interpolator at block 3, its
/// float data at 4 and the controller itself at 5, then three blocks per
/// further controller.
fn interpolator_block_index(index: usize) -> u32 {
    u32::try_from(3 + index * 3).unwrap_or(NULL_REF)
}

fn controller_block_index(index: usize) -> u32 {
    interpolator_block_index(index).wrapping_add(2)
}

/// Writes an effect shape's `BSTriShape` with an explicit shader-property
/// reference, on top of the shared [`write_shape_block`] writer rather than
/// duplicating it.
fn triangle_shape_with_shader(shape: &StaticShape<'_>, shader_property: u32) -> Result<Vec<u8>> {
    write_shape_block(&geometry(shape, &[]), shader_property)
}

/// A geometry-free NIF containing a manager and one readable transform sequence.
/// Its target deliberately has no scene node: public conversion must keep an empty static scene
/// and report optional animation loss instead of creating an animation buffer or failing.
pub fn manager_only_transform_sequence() -> Result<Vec<u8>> {
    let mut manager = Vec::new();
    push_u32(&mut manager, NULL_REF); // next controller
    push_u16(&mut manager, 0x48);
    for value in [1.0, 0.0, 0.0, 1.0] {
        push_f32(&mut manager, value); // frequency, phase, start, stop
    }
    push_u32(&mut manager, NULL_REF); // target
    manager.push(0); // cumulative
    push_u32(&mut manager, 1); // sequence count
    push_u32(&mut manager, 1); // sequence block
    push_u32(&mut manager, NULL_REF); // object palette

    let mut sequence = Vec::new();
    for value in [0, 1, 0, 2, NULL_REF] {
        push_u32(&mut sequence, value); // name, controlled count, grow, interpolator, controller
    }
    sequence.push(0); // priority
    push_u32(&mut sequence, 1); // target name
    for _ in 0..4 {
        push_u32(&mut sequence, NULL_REF); // property, controller type, controller id, interpolator id
    }
    push_f32(&mut sequence, 1.0); // weight
    push_u32(&mut sequence, NULL_REF); // text keys
    push_u32(&mut sequence, 0); // cycle type
    for value in [1.0, 0.0, 1.0] {
        push_f32(&mut sequence, value); // frequency, start, stop
    }
    push_u32(&mut sequence, 0); // manager
    push_u32(&mut sequence, NULL_REF); // accumulation root name
    push_u16(&mut sequence, 0); // notes

    let mut interpolator = Vec::new();
    for value in [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        push_f32(&mut interpolator, value); // translation, quaternion wxyz, scale
    }
    push_u32(&mut interpolator, 3); // transform data
    let mut data = Vec::new();
    for value in [0, 2, 1] {
        push_u32(&mut data, value); // no rotation keys, two linear translation keys
    }
    for value in [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0] {
        push_f32(&mut data, value); // time plus xyz for each key
    }
    push_u32(&mut data, 0); // no scale keys
    write_nif(
        &[
            "NiControllerManager",
            "NiControllerSequence",
            "NiTransformInterpolator",
            "NiTransformData",
        ],
        &[manager, sequence, interpolator, data],
        &["GeneratedClip", "MissingSceneNode"],
    )
}

/// Generates two exported shapes with the same name and distinct local translations.
/// Palette entries use that name and the supplied raw source-block pointers: 1 is the first
/// shape, 4 is the second. `None` leaves the manager's Object Palette null.
pub fn static_shape_with_duplicate_transform_targets(
    shape: &StaticShape<'_>,
    palette_targets: Option<&[u32]>,
) -> Result<Vec<u8>> {
    validate(shape)?;
    let mut root = fade_node_with_children(&[1, 4]);
    root[8..12].copy_from_slice(&6u32.to_le_bytes()); // root controller
    let mut first = write_shape_block(&geometry(shape, &[]), STATIC_SHADER_BLOCK)?;
    let mut second = first.clone();
    first[16..20].copy_from_slice(&10f32.to_le_bytes());
    second[16..20].copy_from_slice(&20f32.to_le_bytes());

    // nif.xml: NiDefaultAVObjectPalette's AVObject uses an inline SizedString and pointer.
    let mut palette = Vec::new();
    push_u32(&mut palette, 0); // scene
    let targets = palette_targets.unwrap_or_default();
    push_u32(&mut palette, u32::try_from(targets.len())?);
    for &target in targets {
        push_u32(&mut palette, u32::try_from(shape.name.len())?);
        palette.extend_from_slice(shape.name.as_bytes());
        push_u32(&mut palette, target);
    }
    let mut manager = Vec::new();
    push_u32(&mut manager, NULL_REF);
    push_u16(&mut manager, 0x48);
    for value in [1.0, 0.0, 0.0, 1.0] {
        push_f32(&mut manager, value);
    }
    push_u32(&mut manager, 0); // target root
    manager.push(0); // cumulative
    push_u32(&mut manager, 1);
    push_u32(&mut manager, 7); // sequence
    push_u32(
        &mut manager,
        if palette_targets.is_some() {
            5
        } else {
            NULL_REF
        },
    );
    let mut sequence = Vec::new();
    for value in [1, 1, 0, 8, NULL_REF] {
        push_u32(&mut sequence, value); // name, count, grow, interpolator, controller
    }
    sequence.push(0);
    push_u32(&mut sequence, 0); // duplicate target name
    for _ in 0..4 {
        push_u32(&mut sequence, NULL_REF);
    }
    push_f32(&mut sequence, 1.0);
    push_u32(&mut sequence, NULL_REF); // text keys
    push_u32(&mut sequence, 0); // cycle
    for value in [1.0, 0.0, 1.0] {
        push_f32(&mut sequence, value);
    }
    push_u32(&mut sequence, 6); // manager
    push_u32(&mut sequence, NULL_REF); // accumulation root name
    push_u16(&mut sequence, 0); // notes
    let mut interpolator = Vec::new();
    for value in [20.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        push_f32(&mut interpolator, value);
    }
    push_u32(&mut interpolator, 9);
    let mut data = Vec::new();
    for value in [0, 2, 1] {
        push_u32(&mut data, value);
    }
    for value in [0.0, 20.0, 0.0, 0.0, 1.0, 21.0, 0.0, 0.0] {
        push_f32(&mut data, value);
    }
    push_u32(&mut data, 0);
    write_nif(
        &[
            "BSFadeNode",
            "BSTriShape",
            "BSLightingShaderProperty",
            "BSShaderTextureSet",
            "BSTriShape",
            "NiDefaultAVObjectPalette",
            "NiControllerManager",
            "NiControllerSequence",
            "NiTransformInterpolator",
            "NiTransformData",
        ],
        &[
            root,
            first,
            lighting_shader_property(STATIC_TEXTURE_SET_BLOCK, false),
            texture_set(shape.diffuse, shape.normal_texture)?,
            second,
            palette,
            manager,
            sequence,
            interpolator,
            data,
        ],
        &[shape.name, "GeneratedClip"],
    )
}

/// Serializes a NIF container around already-encoded blocks.
fn write_nif(block_types: &[&str], blocks: &[Vec<u8>], strings: &[&str]) -> Result<Vec<u8>> {
    ensure!(
        block_types.len() == blocks.len(),
        "NIF has {} block types for {} blocks",
        block_types.len(),
        blocks.len()
    );
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"Gamebryo File Format, Version 20.2.0.7\n");
    push_u32(&mut bytes, NIF_VERSION);
    bytes.push(1);
    push_u32(&mut bytes, USER_VERSION);
    push_u32(
        &mut bytes,
        u32::try_from(blocks.len()).map_err(|_| eyre!("NIF block count overflow"))?,
    );
    push_u32(&mut bytes, BETHESDA_VERSION);
    push_string8(&mut bytes, "Mudcrab dummy-content");
    push_string8(&mut bytes, "");
    push_string8(&mut bytes, "");
    push_u16(
        &mut bytes,
        u16::try_from(block_types.len()).map_err(|_| eyre!("NIF block type overflow"))?,
    );
    for block_type in block_types {
        push_u32(
            &mut bytes,
            u32::try_from(block_type.len()).map_err(|_| eyre!("NIF block type overflow"))?,
        );
        bytes.extend_from_slice(block_type.as_bytes());
    }
    for index in 0..blocks.len() {
        push_u16(
            &mut bytes,
            u16::try_from(index).map_err(|_| eyre!("NIF block index overflow"))?,
        );
    }
    for block in blocks {
        push_u32(
            &mut bytes,
            u32::try_from(block.len()).map_err(|_| eyre!("NIF block size overflow"))?,
        );
    }
    push_u32(
        &mut bytes,
        u32::try_from(strings.len()).map_err(|_| eyre!("NIF string count overflow"))?,
    );
    push_u32(&mut bytes, max_string_length(strings));
    for value in strings {
        push_u32(
            &mut bytes,
            u32::try_from(value.len()).map_err(|_| eyre!("NIF string overflow"))?,
        );
        bytes.extend_from_slice(value.as_bytes());
    }
    push_u32(&mut bytes, 0);
    for block in blocks {
        bytes.extend_from_slice(block);
    }
    Ok(bytes)
}

fn validate_skin(spec: &LegacySkin<'_>) -> Result<()> {
    ensure!(!spec.name.is_empty(), "NIF shape name is empty");
    ensure!(
        spec.name.bytes().all(|byte| (0x20..0x7f).contains(&byte)),
        "NIF shape name is not printable ASCII: {:?}",
        spec.name
    );
    ensure!(
        spec.palette.len() == 2,
        "NIF skin needs a 2-entry bone palette, got {}",
        spec.palette.len()
    );
    ensure!(!spec.weights.is_empty(), "NIF skin has no vertex weights");
    ensure!(
        spec.weights.len() <= u16::MAX as usize,
        "NIF skin exceeds 65535 vertices"
    );
    for (translation, quaternion, scale) in spec.bone_transforms {
        ensure!(
            translation.iter().all(|value| value.is_finite())
                && quaternion.iter().all(|value| value.is_finite())
                && scale.is_finite(),
            "NIF skin contains a non-finite inverse bind"
        );
        ensure!(scale != 0.0, "NIF skin inverse bind scale is zero");
    }
    ensure!(
        spec.weights.iter().flatten().all(|value| value.is_finite()),
        "NIF skin contains a non-finite weight"
    );
    Ok(())
}

/// A single skinned vertex: position, normal, UV and its four influences.
struct SkinVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    weights: [f32; 4],
    /// Skin-instance bone indices, as the shared vertex buffers store them.
    joints: [u8; 4],
    /// Partition palette indices, as the per-partition index arrays store them.
    palette_joints: [u8; 4],
}

fn skin_vertices(spec: &LegacySkin<'_>) -> Vec<SkinVertex> {
    skin_vertices_scaled(spec, 1.0)
}

fn skin_vertices_scaled(spec: &LegacySkin<'_>, scale: f32) -> Vec<SkinVertex> {
    let count = spec.weights.len();
    (0..count)
        .map(|index| {
            let position = vertex_position(index, count).map(|value| value * scale);
            let palette_joints = if index == 0 {
                spec.joint_override.unwrap_or([0, 1, 0, 0])
            } else {
                [0, 1, 0, 0]
            };
            let joints = palette_joints.map(|local| {
                spec.palette
                    .get(usize::from(local))
                    .and_then(|bone| u8::try_from(*bone).ok())
                    .unwrap_or(local)
            });
            SkinVertex {
                position,
                normal: [0.0, 0.0, 1.0],
                uv: [0.5 + 0.5 * position[0], 0.5 + 0.5 * position[1]],
                weights: spec.weights[index],
                joints,
                palette_joints,
            }
        })
        .collect()
}

fn vertex_position(index: usize, count: usize) -> [f32; 3] {
    if count <= 1 || index == 0 {
        return [0.0, 0.0, 0.0];
    }
    let angle = std::f32::consts::TAU * (index - 1) as f32 / (count - 1) as f32;
    [angle.cos(), angle.sin(), 0.0]
}

fn fan_triangles(count: usize) -> Vec<[u16; 3]> {
    (1..count.saturating_sub(1))
        .map(|index| [0, index as u16, (index + 1) as u16])
        .collect()
}

/// Row-major rotation matrix from an xyzw quaternion (nif.xml convention).
fn quat_xyzw_to_matrix(q: [f32; 4]) -> [[f32; 3]; 3] {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - w * z),
            2.0 * (x * z + w * y),
        ],
        [
            2.0 * (x * y + w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - w * x),
        ],
        [
            2.0 * (x * z - w * y),
            2.0 * (y * z + w * x),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

fn push_ni_transform(
    out: &mut Vec<u8>,
    rotation: &[[f32; 3]; 3],
    translation: [f32; 3],
    scale: f32,
) {
    for row in rotation {
        for value in row {
            push_f32(out, *value);
        }
    }
    for value in translation {
        push_f32(out, value);
    }
    push_f32(out, scale);
}

fn push_skinned_vertices(out: &mut Vec<u8>, vertices: &[SkinVertex], normals: SkinNormals) {
    for vertex in vertices {
        for value in vertex.position {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&0.0f32.to_le_bytes());
        out.extend_from_slice(&encode_half(vertex.uv[0]).to_le_bytes());
        out.extend_from_slice(&encode_half(vertex.uv[1]).to_le_bytes());
        if normals.in_vertices() {
            out.push(pack_normal(vertex.normal[0]));
            out.push(pack_normal(vertex.normal[1]));
            out.push(pack_normal(vertex.normal[2]));
            out.push(0);
        }
        for weight in vertex.weights {
            out.extend_from_slice(&encode_half(weight).to_le_bytes());
        }
        out.extend_from_slice(&vertex.joints);
    }
}

fn skinned_tri_shape(
    name: u32,
    positions: &[[f32; 3]],
    skin: u32,
    shader_property: u32,
    vertices: &[SkinVertex],
    triangles: &[[u16; 3]],
    normals: SkinNormals,
) -> Result<Vec<u8>> {
    let vertex_bytes = vertices
        .len()
        .checked_mul(normals.vertex_size() as usize)
        .ok_or_else(|| eyre!("NIF vertex data overflow"))?;
    let triangle_bytes = triangles
        .len()
        .checked_mul(6)
        .ok_or_else(|| eyre!("NIF triangle data overflow"))?;
    let data_size = vertex_bytes
        .checked_add(triangle_bytes)
        .ok_or_else(|| eyre!("NIF geometry size overflow"))?;

    let (center, radius) = bounds(positions);
    let mut block = Vec::with_capacity(72 + 16 + 24 + data_size);
    push_av_object(&mut block, name);
    for value in center {
        block.extend_from_slice(&value.to_le_bytes());
    }
    block.extend_from_slice(&radius.to_le_bytes());
    push_u32(&mut block, skin);
    push_u32(&mut block, shader_property);
    push_u32(&mut block, NULL_REF);
    push_u64(&mut block, normals.vertex_desc());
    push_u16(
        &mut block,
        u16::try_from(triangles.len()).map_err(|_| eyre!("NIF triangle count overflow"))?,
    );
    push_u16(
        &mut block,
        u16::try_from(vertices.len()).map_err(|_| eyre!("NIF vertex count overflow"))?,
    );
    push_u32(
        &mut block,
        u32::try_from(data_size).map_err(|_| eyre!("NIF geometry size overflow"))?,
    );
    push_skinned_vertices(&mut block, vertices, normals);
    for triangle in triangles {
        for index in triangle {
            block.extend_from_slice(&index.to_le_bytes());
        }
    }
    Ok(block)
}

fn dynamic_tri_shape(
    name: u32,
    positions: &[[f32; 3]],
    skin: u32,
    shader_property: u32,
    normals: SkinNormals,
) -> Result<Vec<u8>> {
    let vertex_count =
        u16::try_from(positions.len()).map_err(|_| eyre!("NIF vertex count overflow"))?;
    let dynamic_data_size = u32::try_from(positions.len())
        .map_err(|_| eyre!("NIF dynamic vertex count overflow"))?
        .checked_mul(16)
        .ok_or_else(|| eyre!("NIF dynamic geometry size overflow"))?;

    let (center, radius) = bounds(positions);
    let mut block = Vec::with_capacity(72 + 16 + 24 + positions.len() * 16);
    push_av_object(&mut block, name);
    for value in center {
        block.extend_from_slice(&value.to_le_bytes());
    }
    block.extend_from_slice(&radius.to_le_bytes());
    push_u32(&mut block, skin);
    push_u32(&mut block, shader_property);
    push_u32(&mut block, NULL_REF);
    push_u64(&mut block, normals.vertex_desc());
    push_u16(&mut block, 0);
    push_u16(&mut block, vertex_count);
    push_u32(&mut block, 0);
    push_u32(&mut block, 0);
    push_u32(&mut block, dynamic_data_size);
    for position in positions {
        for value in position {
            push_f32(&mut block, *value);
        }
        push_f32(&mut block, 0.0);
    }
    Ok(block)
}

fn bone_node(name: u32) -> Vec<u8> {
    bone_node_with_children(name, &[])
}

fn bone_node_with_children(name: u32, children: &[u32]) -> Vec<u8> {
    let mut node = Vec::new();
    push_av_object(&mut node, name);
    push_u32(&mut node, children.len() as u32);
    for child in children {
        push_u32(&mut node, *child);
    }
    push_u32(&mut node, 0);
    node
}

fn skin_instance(
    kind: SkinInstanceKind,
    data: u32,
    skin_partition: u32,
    bone0: u32,
    bone1: u32,
) -> Vec<u8> {
    let mut block = Vec::with_capacity(32);
    push_u32(&mut block, data);
    push_u32(&mut block, skin_partition);
    push_u32(&mut block, 0);
    push_u32(&mut block, 2);
    push_u32(&mut block, bone0);
    push_u32(&mut block, bone1);
    if kind == SkinInstanceKind::Dismember {
        push_u32(&mut block, 1);
        push_u16(&mut block, 257);
        push_u16(&mut block, 32);
    }
    block
}

fn skin_data(
    bone_transforms: &[([f32; 3], [f32; 4], f32); 2],
    weights: &[[f32; 4]],
) -> Result<Vec<u8>> {
    let vertex_count = weights.len();
    let mut block = Vec::new();
    push_ni_transform(&mut block, &IDENTITY_ROTATION, [0.0; 3], 1.0);
    push_u32(&mut block, 2);
    block.push(1);
    for (bone_index, (translation, quaternion, scale)) in bone_transforms.iter().enumerate() {
        let rotation = quat_xyzw_to_matrix(*quaternion);
        push_ni_transform(&mut block, &rotation, *translation, *scale);
        for value in *translation {
            push_f32(&mut block, value);
        }
        push_f32(&mut block, 1.0);
        push_u16(
            &mut block,
            u16::try_from(vertex_count).map_err(|_| eyre!("NIF skin vertex count overflow"))?,
        );
        for (index, row) in weights.iter().enumerate() {
            push_u16(
                &mut block,
                u16::try_from(index).map_err(|_| eyre!("NIF skin vertex index overflow"))?,
            );
            push_f32(&mut block, row[bone_index]);
        }
    }
    Ok(block)
}

fn skin_partition(
    vertices: &[SkinVertex],
    triangles: &[[u16; 3]],
    palette: &[u16],
    weights: &[[f32; 4]],
    normals: SkinNormals,
) -> Result<Vec<u8>> {
    let data_size = vertices
        .len()
        .checked_mul(normals.vertex_size() as usize)
        .ok_or_else(|| eyre!("NIF partition vertex data overflow"))?;
    let mut block = Vec::new();
    push_u32(&mut block, 1);
    push_u32(
        &mut block,
        u32::try_from(data_size).map_err(|_| eyre!("NIF partition size overflow"))?,
    );
    push_u32(&mut block, normals.vertex_size());
    push_u64(&mut block, normals.vertex_desc());
    push_skinned_vertices(&mut block, vertices, normals);
    push_u16(
        &mut block,
        u16::try_from(vertices.len()).map_err(|_| eyre!("NIF partition vertex count overflow"))?,
    );
    push_u16(
        &mut block,
        u16::try_from(triangles.len())
            .map_err(|_| eyre!("NIF partition triangle count overflow"))?,
    );
    push_u16(
        &mut block,
        u16::try_from(palette.len()).map_err(|_| eyre!("NIF partition bone count overflow"))?,
    );
    push_u16(&mut block, 0);
    push_u16(&mut block, 4);
    for bone in palette {
        push_u16(&mut block, *bone);
    }
    block.push(0);
    block.push(1);
    for row in weights {
        for weight in row {
            push_f32(&mut block, *weight);
        }
    }
    block.push(0);
    block.push(1);
    for vertex in vertices {
        block.extend_from_slice(&vertex.palette_joints);
    }
    block.push(0);
    block.push(0);
    push_u64(&mut block, normals.vertex_desc());
    for triangle in triangles {
        for index in triangle {
            push_u16(&mut block, *index);
        }
    }
    Ok(block)
}

fn validate(shape: &StaticShape<'_>) -> Result<()> {
    ensure!(!shape.name.is_empty(), "NIF shape name is empty");
    ensure!(
        shape.name.bytes().all(|byte| (0x20..0x7f).contains(&byte)),
        "NIF shape name is not printable ASCII: {:?}",
        shape.name
    );
    validate_geometry(&geometry(shape, &[]))?;
    ensure!(
        !shape.diffuse.is_empty(),
        "NIF shape needs a diffuse texture"
    );
    for texture in [shape.diffuse, shape.normal_texture] {
        ensure!(
            texture.bytes().all(|byte| (0x20..0x7f).contains(&byte)),
            "NIF texture path is not printable ASCII: {texture:?}"
        );
    }
    Ok(())
}

fn geometry<'a>(shape: &StaticShape<'a>, colors: &'a [[u8; 4]]) -> Geometry<'a> {
    Geometry {
        positions: shape.positions,
        normals: shape.normals,
        uvs: shape.uvs,
        indices: shape.indices,
        // An empty slice means the caller authored no vertex colours.
        colors: (!colors.is_empty()).then_some(colors),
    }
}

fn validate_geometry(geometry: &Geometry<'_>) -> Result<()> {
    ensure!(!geometry.positions.is_empty(), "NIF shape has no positions");
    ensure!(
        geometry.positions.len() == geometry.normals.len(),
        "NIF shape has {} positions and {} normals",
        geometry.positions.len(),
        geometry.normals.len()
    );
    ensure!(
        geometry.positions.len() == geometry.uvs.len(),
        "NIF shape has {} positions and {} UVs",
        geometry.positions.len(),
        geometry.uvs.len()
    );
    ensure!(
        geometry.positions.len() <= u16::MAX as usize,
        "NIF shape exceeds 65535 vertices"
    );
    ensure!(
        geometry.indices.len() <= u16::MAX as usize,
        "NIF shape exceeds 65535 triangles"
    );
    ensure!(!geometry.indices.is_empty(), "NIF shape has no triangles");
    ensure!(
        geometry
            .positions
            .iter()
            .all(|position| position.iter().all(|value| value.is_finite())),
        "NIF shape contains a non-finite position"
    );
    ensure!(
        geometry
            .uvs
            .iter()
            .all(|uv| uv.iter().all(|value| value.is_finite())),
        "NIF shape contains a non-finite UV"
    );
    let vertex_count = geometry.positions.len();
    ensure!(
        geometry
            .indices
            .iter()
            .flatten()
            .all(|index| { usize::from(*index) < vertex_count }),
        "NIF shape contains an out-of-range triangle index"
    );
    if let Some(colors) = geometry.colors {
        ensure!(
            colors.len() == vertex_count,
            "NIF shape has {} positions and {} vertex colors",
            vertex_count,
            colors.len()
        );
    }
    Ok(())
}

fn fade_node() -> Vec<u8> {
    fade_node_with_children(&[1])
}

fn fade_node_with_children(children: &[u32]) -> Vec<u8> {
    let mut block = Vec::with_capacity(84);
    push_av_object(&mut block, NULL_REF);
    push_u32(&mut block, children.len() as u32);
    for child in children {
        push_u32(&mut block, *child);
    }
    push_u32(&mut block, 0);
    block
}

fn validate_body(body: &BoxBody<'_>) -> Result<()> {
    ensure!(
        !body.node_name.is_empty()
            && body
                .node_name
                .bytes()
                .all(|byte| (0x20..0x7f).contains(&byte)),
        "NIF body node name must be printable ASCII"
    );
    ensure!(
        body.shape != BodyShape::Box || body.half_extents.iter().all(|v| v.is_finite() && *v > 0.0),
        "NIF box body needs positive half extents"
    );
    let finite = body
        .inertia
        .iter()
        .chain(&body.center_of_mass)
        .all(|v| v.is_finite())
        && [
            body.mass,
            body.linear_damping,
            body.angular_damping,
            body.friction,
            body.restitution,
            body.max_linear_velocity,
            body.max_angular_velocity,
        ]
        .iter()
        .all(|v| v.is_finite());
    ensure!(finite, "NIF body contains a non-finite value");
    Ok(())
}

/// `bhkRigidBody` block: `bhkWorldObject`, `bhkEntityCInfo`, then `bhkRigidBodyCInfo2010`
/// (nif.xml, Skyrim), then constraints and body flags.
fn rigid_body(body: &BoxBody<'_>, shape: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(250);
    push_u32(&mut b, shape);
    b.extend_from_slice(&[body.collision_layer, body.collision_flags, 0, 0]); // havok filter
    b.extend_from_slice(&[0; 20]); // world object info
    b.extend_from_slice(&[body.collision_response, 0, 0xff, 0xff]); // entity info
    b.extend_from_slice(&[0; 4]); // unused
    b.extend_from_slice(&[body.collision_layer, body.inner_collision_flags, 0, 0]); // havok filter
    b.extend_from_slice(&[0; 4]); // unused
    push_u32(&mut b, 0); // unknown int
    b.extend_from_slice(&[body.inner_collision_response, 0, 0xff, 0xff]); // response, unused, callback delay
    let (translation, rotation) = body.transform.unwrap_or(([0.0; 3], [0.0, 0.0, 0.0, 1.0]));
    for value in translation.into_iter().chain([0.0]) {
        push_f32(&mut b, value);
    }
    for value in rotation {
        push_f32(&mut b, value);
    }
    b.extend_from_slice(&[0; 32]); // linear and angular velocity
    for row in body.inertia.chunks(3) {
        for value in row.iter().copied().chain([0.0]) {
            push_f32(&mut b, value);
        }
    }
    for value in body.center_of_mass.into_iter().chain([0.0]) {
        push_f32(&mut b, value);
    }
    for value in [
        body.mass,
        body.linear_damping,
        body.angular_damping,
        1.0, // time factor
        1.0, // gravity factor
        body.friction,
        0.0, // rolling friction multiplier
        body.restitution,
        body.max_linear_velocity,
        body.max_angular_velocity,
        0.15, // penetration depth
    ] {
        push_f32(&mut b, value);
    }
    b.extend_from_slice(&[
        body.motion_system,
        body.deactivator_type,
        1, // solver deactivation off
        body.quality_type,
        0, // auto remove level
        0, // response modifier flags
        3, // shape keys in contact point
        0, // force collided onto PPU
    ]);
    b.extend_from_slice(&[0; 12]);
    push_u32(&mut b, 0); // constraints
    push_u16(&mut b, 0); // body flags
    b
}

/// The body's shape block and its block type (nif.xml: material, convex radius, then fields).
fn body_shape(body: &BoxBody<'_>) -> (Vec<u8>, &'static str) {
    let mut b = Vec::with_capacity(64);
    push_u32(&mut b, 0); // material
    match body.shape {
        BodyShape::Box => {
            push_f32(&mut b, 0.0); // radius
            b.extend_from_slice(&[0; 8]);
            for value in body.half_extents {
                push_f32(&mut b, value);
            }
            push_f32(&mut b, 0.0);
            (b, "bhkBoxShape")
        }
        BodyShape::Sphere { radius } => {
            push_f32(&mut b, radius);
            (b, "bhkSphereShape")
        }
        BodyShape::MultiSphere { count, spheres } => {
            assert!(
                count as usize <= spheres.len(),
                "a fixture holds at most 8 spheres"
            );
            b.extend_from_slice(&[0; 12]); // shape property
            push_u32(&mut b, count);
            for (center, radius) in spheres.iter().take(count as usize) {
                for value in center.iter().copied().chain([*radius]) {
                    push_f32(&mut b, value);
                }
            }
            (b, "bhkMultiSphereShape")
        }
        BodyShape::Cylinder { a, b: end, radius } => {
            push_f32(&mut b, 0.0); // convex radius
            b.extend_from_slice(&[0; 8]);
            for point in [a, end] {
                for value in point.into_iter().chain([0.0]) {
                    push_f32(&mut b, value);
                }
            }
            push_f32(&mut b, radius);
            b.extend_from_slice(&[0; 12]);
            (b, "bhkCylinderShape")
        }
    }
}

fn write_shape_block(geometry: &Geometry<'_>, shader_property: u32) -> Result<Vec<u8>> {
    let vertex_stride = VERTEX_STRIDE + u8::from(geometry.colors.is_some());
    let vertex_bytes = geometry
        .positions
        .len()
        .checked_mul(usize::from(vertex_stride) * 4)
        .ok_or_else(|| eyre!("NIF vertex data overflow"))?;
    let triangle_bytes = geometry
        .indices
        .len()
        .checked_mul(6)
        .ok_or_else(|| eyre!("NIF triangle data overflow"))?;
    let data_size = vertex_bytes
        .checked_add(triangle_bytes)
        .ok_or_else(|| eyre!("NIF geometry size overflow"))?;

    let (center, radius) = bounds(geometry.positions);
    let mut block = Vec::with_capacity(SHAPE_BLOCK_HEADER + data_size);
    push_av_object(&mut block, 0);
    for value in center {
        block.extend_from_slice(&value.to_le_bytes());
    }
    block.extend_from_slice(&radius.to_le_bytes());
    push_u32(&mut block, NULL_REF);
    push_u32(&mut block, shader_property);
    push_u32(&mut block, NULL_REF);
    let color_offset = if geometry.colors.is_some() {
        COLOR_OFFSET
    } else {
        0
    };
    let vertex_flags = VERTEX_FLAGS
        | if geometry.colors.is_some() {
            VERTEX_COLOR_FLAG
        } else {
            0
        };
    let descriptor = u64::from(vertex_stride)
        | (UV_OFFSET << 8)
        | (NORMAL_OFFSET << 16)
        | (color_offset << 24)
        | (u64::from(vertex_flags) << 44);
    push_u64(&mut block, descriptor);
    push_u16(
        &mut block,
        u16::try_from(geometry.indices.len()).map_err(|_| eyre!("NIF triangle count overflow"))?,
    );
    push_u16(
        &mut block,
        u16::try_from(geometry.positions.len()).map_err(|_| eyre!("NIF vertex count overflow"))?,
    );
    push_u32(
        &mut block,
        u32::try_from(data_size).map_err(|_| eyre!("NIF geometry size overflow"))?,
    );
    let empty_colors = [[0u8; 4]; 0];
    let colors = geometry.colors.unwrap_or(&empty_colors);
    for (index, (position, (normal, uv))) in geometry
        .positions
        .iter()
        .zip(geometry.normals.iter().zip(geometry.uvs.iter()))
        .enumerate()
    {
        for value in position {
            block.extend_from_slice(&value.to_le_bytes());
        }
        block.extend_from_slice(&0.0f32.to_le_bytes());
        block.extend_from_slice(&encode_half(uv[0]).to_le_bytes());
        block.extend_from_slice(&encode_half(uv[1]).to_le_bytes());
        block.push(pack_normal(normal[0]));
        block.push(pack_normal(normal[1]));
        block.push(pack_normal(normal[2]));
        block.push(0);
        if let Some(color) = colors.get(index) {
            block.extend_from_slice(color);
        }
    }
    for triangle in geometry.indices {
        for index in triangle {
            block.extend_from_slice(&index.to_le_bytes());
        }
    }
    Ok(block)
}

/// Writes a `BSLightingShaderProperty` naming `texture_set` and, when the shape
/// carries authored vertex colours, the two shader flags that tell the runtime
/// to read them.
fn lighting_shader_property(texture_set: u32, colors: bool) -> Vec<u8> {
    if colors {
        lighting_shader_property_with_flags(1 << 3, 1 << 5, texture_set)
    } else {
        lighting_shader_property_with_flags(0, 0, texture_set)
    }
}

fn lighting_shader_property_with_flags(flags_1: u32, flags_2: u32, texture_set: u32) -> Vec<u8> {
    let mut block = Vec::with_capacity(100);
    push_u32(&mut block, SHADER_TYPE_DEFAULT);
    push_u32(&mut block, NULL_REF);
    push_u32(&mut block, 0);
    push_u32(&mut block, NULL_REF);
    push_u32(&mut block, 0);
    push_u32(&mut block, 0);
    push_f32(&mut block, 0.0);
    push_f32(&mut block, 0.0);
    push_f32(&mut block, 1.0);
    push_f32(&mut block, 1.0);
    push_u32(&mut block, texture_set);
    push_f32(&mut block, 0.0);
    push_f32(&mut block, 0.0);
    push_f32(&mut block, 0.0);
    push_f32(&mut block, 1.0);
    push_u32(&mut block, 0);
    push_f32(&mut block, 1.0);
    push_f32(&mut block, 0.0);
    push_f32(&mut block, 80.0);
    push_f32(&mut block, 1.0);
    push_f32(&mut block, 1.0);
    push_f32(&mut block, 1.0);
    push_f32(&mut block, 1.0);
    push_f32(&mut block, 0.3);
    push_f32(&mut block, 2.0);
    block[16..20].copy_from_slice(&flags_1.to_le_bytes());
    block[20..24].copy_from_slice(&flags_2.to_le_bytes());
    debug_assert_eq!(block.len(), 100);
    block
}

/// A `BSEffectShaderProperty` whose controller reference is `controller`.
///
/// Field order is the vendored parser's (`vendor/project-wormhole-nif`):
/// the inherited `NiObjectNET` header, the flags, the UV transform, the source
/// texture, four clamp/lighting bytes, four falloff floats, the base colour,
/// the base colour scale, the soft falloff depth and the greyscale texture.
fn effect_shader_property(shape: &StaticShape<'_>, controller: u32) -> Result<Vec<u8>> {
    let mut block = Vec::with_capacity(96);
    push_u32(&mut block, NULL_REF);
    push_u32(&mut block, 0);
    push_u32(&mut block, controller);
    push_u32(&mut block, 0);
    push_u32(&mut block, 0);
    for value in [0.0f32, 0.0, 1.0, 1.0] {
        push_f32(&mut block, value);
    }
    push_sized_string(&mut block, shape.diffuse)?;
    block.extend_from_slice(&[0, 0, 0, 0]);
    for value in [0.0f32, 0.0, 1.0, 0.0] {
        push_f32(&mut block, value);
    }
    for value in [1.0f32, 1.0, 1.0, 1.0, 1.0, 0.0] {
        push_f32(&mut block, value);
    }
    push_sized_string(&mut block, "")?;
    Ok(block)
}

/// An `NiTimeController` → `BSEffectShaderPropertyFloatController` block.
fn float_controller(
    controller: &FloatController<'_>,
    interpolator: u32,
    next_controller: u32,
) -> Vec<u8> {
    let mut block = Vec::with_capacity(40);
    push_u32(&mut block, next_controller);
    push_u16(&mut block, controller.flags);
    for value in [
        controller.frequency,
        controller.phase,
        controller.start_time,
        controller.stop_time,
    ] {
        push_f32(&mut block, value);
    }
    // The controller targets the shader property at block 2.
    push_u32(&mut block, 2);
    push_u32(&mut block, interpolator);
    push_u32(&mut block, controller.variable);
    block
}

fn color_controller(
    controller: &ColorController<'_>,
    interpolator: u32,
    next_controller: u32,
) -> Vec<u8> {
    let mut block = Vec::with_capacity(40);
    push_u32(&mut block, next_controller);
    push_u16(&mut block, controller.flags);
    for value in [
        controller.frequency,
        controller.phase,
        controller.start_time,
        controller.stop_time,
    ] {
        push_f32(&mut block, value);
    }
    // The controller targets the shader property at block 2.
    push_u32(&mut block, 2);
    push_u32(&mut block, interpolator);
    // `EffectShaderControlledColor`: 0, the emissive colour.
    push_u32(&mut block, 0);
    block
}

fn point3_interpolator(data: u32) -> Vec<u8> {
    let mut block = Vec::with_capacity(16);
    for _ in 0..3 {
        push_f32(&mut block, 0.0);
    }
    push_u32(&mut block, data);
    block
}

fn pos_data(controller: &ColorController<'_>) -> Result<Vec<u8>> {
    let stride = if controller.key_type == KEY_TYPE_QUADRATIC {
        10
    } else {
        4
    };
    let mut block = Vec::with_capacity(8 + stride * 4 * controller.keys.len());
    push_u32(
        &mut block,
        u32::try_from(controller.keys.len()).map_err(|_| eyre!("NIF key count overflow"))?,
    );
    push_u32(&mut block, controller.key_type);
    for key in controller.keys {
        for value in key.iter().take(stride) {
            push_f32(&mut block, *value);
        }
    }
    Ok(block)
}

fn float_interpolator(data: u32) -> Vec<u8> {
    let mut block = Vec::with_capacity(8);
    push_f32(&mut block, 0.0);
    push_u32(&mut block, data);
    block
}

fn float_data(controller: &FloatController<'_>) -> Result<Vec<u8>> {
    let stride = if controller.key_type == KEY_TYPE_QUADRATIC {
        4
    } else {
        2
    };
    let mut block = Vec::with_capacity(8 + stride * 4 * controller.keys.len());
    push_u32(
        &mut block,
        u32::try_from(controller.keys.len()).map_err(|_| eyre!("NIF key count overflow"))?,
    );
    push_u32(&mut block, controller.key_type);
    for key in controller.keys {
        for value in key.iter().take(stride) {
            push_f32(&mut block, *value);
        }
    }
    Ok(block)
}

fn push_sized_string(out: &mut Vec<u8>, value: &str) -> Result<()> {
    push_u32(
        out,
        u32::try_from(value.len()).map_err(|_| eyre!("NIF string overflow"))?,
    );
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn texture_set(diffuse: &str, normal_texture: &str) -> Result<Vec<u8>> {
    let slots = [diffuse, normal_texture, "", "", "", "", "", "", ""];
    let mut block = Vec::new();
    push_u32(
        &mut block,
        u32::try_from(slots.len()).map_err(|_| eyre!("NIF texture slot overflow"))?,
    );
    for slot in slots {
        push_u32(
            &mut block,
            u32::try_from(slot.len()).map_err(|_| eyre!("NIF texture path overflow"))?,
        );
        block.extend_from_slice(slot.as_bytes());
    }
    Ok(block)
}

fn push_av_object(out: &mut Vec<u8>, name: u32) {
    push_av_object_with(out, name, NULL_REF);
}

fn push_av_object_with(out: &mut Vec<u8>, name: u32, collision_object: u32) {
    push_u32(out, name);
    push_u32(out, NULL_REF);
    push_u32(out, NULL_REF);
    push_u32(out, 0);
    for value in [0.0f32, 0.0, 0.0] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    for value in [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&1.0f32.to_le_bytes());
    push_u32(out, collision_object);
}

fn push_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_string8(out: &mut Vec<u8>, value: &str) {
    out.push((value.len() + 1) as u8);
    out.extend_from_slice(value.as_bytes());
    out.push(0);
}

fn max_string_length(strings: &[&str]) -> u32 {
    strings
        .iter()
        .map(|value| u32::try_from(value.len()).unwrap_or(u32::MAX))
        .max()
        .unwrap_or(0)
}

fn pack_normal(value: f32) -> u8 {
    let scaled = ((value + 1.0) * 0.5 * 255.0).round();
    scaled.clamp(0.0, 255.0) as u8
}

/// Encodes an `f32` as an IEEE 754 half-precision value (round to nearest).
fn encode_half(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x007f_ffff;
    if exponent <= 0 {
        if exponent < -10 {
            return sign;
        }
        let mantissa = (mantissa | 0x0080_0000) >> (1 - exponent + 13);
        return sign | mantissa as u16;
    }
    if exponent >= 31 {
        return sign | 0x7c00 | u16::from(mantissa != 0) << 9;
    }
    sign | ((exponent as u16) << 10) | ((mantissa >> 13) as u16)
}

fn bounds(positions: &[[f32; 3]]) -> ([f32; 3], f32) {
    let count = positions.len() as f32;
    let mut center = [0.0f32; 3];
    for position in positions {
        for (axis, value) in position.iter().enumerate() {
            center[axis] += value / count;
        }
    }
    let radius = positions
        .iter()
        .map(|position| {
            let dx = position[0] - center[0];
            let dy = position[1] - center[1];
            let dz = position[2] - center[2];
            (dx * dx + dy * dy + dz * dz).sqrt()
        })
        .fold(0.0f32, f32::max);
    (center, radius)
}

#[cfg(test)]
const SKIN_WEIGHTS: [[f32; 4]; 4] = [
    [0.7, 0.3, 0.0, 0.0],
    [0.25, 0.75, 0.0, 0.0],
    [0.5, 0.2, 0.0, 0.0],
    [0.4, 0.6, 0.0, 0.0],
];

#[cfg(test)]
const SKIN_PALETTE: [u16; 2] = [1, 0];

#[cfg(test)]
fn skinned_spec() -> LegacySkin<'static> {
    LegacySkin {
        name: "GeneratedSkin",
        instance_kind: SkinInstanceKind::Dismember,
        bone_transforms: [
            (
                [0.0, 0.0, 0.0],
                [
                    0.0,
                    0.0,
                    std::f32::consts::FRAC_1_SQRT_2,
                    std::f32::consts::FRAC_1_SQRT_2,
                ],
                1.0,
            ),
            ([0.0, 0.0, 2.0], [0.0, 0.0, 0.0, 1.0], 1.0),
        ],
        palette: &SKIN_PALETTE,
        weights: &SKIN_WEIGHTS,
        with_geometry: true,
        joint_override: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> StaticShape<'static> {
        StaticShape {
            name: "GeneratedQuad",
            positions: &[
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
                [1.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
            ],
            normals: &[[0.0, 0.0, 1.0]; 4],
            uvs: &[[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
            indices: &[[0, 1, 2], [0, 2, 3]],
            diffuse: "textures/generated_color.dds",
            normal_texture: "textures/generated_normal.dds",
        }
    }

    /// The byte sequence every existing conversion depends on. It was captured
    /// before the authored fixture extensions landed, so this test fails if the static
    /// writer's output changes at all.
    const STATIC_SHAPE_BASELINE_HEX: &str = "47616d656272796f2046696c6520466f726d61742c2056657273696f6e2032302e322e302e370a07000214010c0000000400000064000000164d7564637261622064756d6d792d636f6e74656e74000100010004000a0000004253466164654e6f64650a000000425354726953686170651800000042534c69676874696e6753686164657250726f706572747912000000425353686164657254657874757265536574000001000200030054000000e00000006400000061000000010000000d0000000d00000047656e6572617465645175616400000000ffffffffffffffffffffffff000000000000000000000000000000000000803f0000000000000000000000000000803f0000000000000000000000000000803f0000803fffffffff01000000010000000000000000000000ffffffffffffffff000000000000000000000000000000000000803f0000000000000000000000000000803f0000000000000000000000000000803f0000803fffffffff000000000000000000000000f304b53fffffffff02000000ffffffff0604050000b00000020004006c000000000080bf000080bf00000000000000000000003c8080ff000000803f000080bf0000000000000000003c003c8080ff000000803f0000803f0000000000000000003c00008080ff00000080bf0000803f0000000000000000000000008080ff0000000100020000000200030000000000ffffffff00000000ffffffff000000000000000000000000000000000000803f0000803f030000000000000000000000000000000000803f000000000000803f000000000000a0420000803f0000803f0000803f0000803f9a99993e00000040090000001c00000074657874757265732f67656e6572617465645f636f6c6f722e6464731d00000074657874757265732f67656e6572617465645f6e6f726d616c2e64647300000000000000000000000000000000000000000000000000000000";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn static_shape_output_is_unchanged() {
        assert_eq!(
            hex(&static_shape(&quad()).unwrap()),
            STATIC_SHAPE_BASELINE_HEX
        );
    }

    #[test]
    fn writes_the_expected_header() {
        let bytes = static_shape(&quad()).unwrap();
        let line = b"Gamebryo File Format, Version 20.2.0.7\n";
        assert!(bytes.starts_with(line));
        let version = u32::from_le_bytes(bytes[line.len()..line.len() + 4].try_into().unwrap());
        assert_eq!(version, NIF_VERSION);
        assert_eq!(bytes[line.len() + 4], 1);
        let user = u32::from_le_bytes(bytes[line.len() + 5..line.len() + 9].try_into().unwrap());
        assert_eq!(user, USER_VERSION);
        let blocks = u32::from_le_bytes(bytes[line.len() + 9..line.len() + 13].try_into().unwrap());
        assert_eq!(blocks, 4);
    }

    #[test]
    fn output_is_deterministic() {
        assert_eq!(
            static_shape(&quad()).unwrap(),
            static_shape(&quad()).unwrap()
        );
    }

    #[test]
    fn rejects_invalid_geometry() {
        let mut shape = quad();
        shape.normals = &[];
        assert!(static_shape(&shape).is_err());
        let mut shape = quad();
        shape.indices = &[[0, 1, 9]];
        assert!(static_shape(&shape).is_err());
        let mut shape = quad();
        shape.diffuse = "";
        assert!(static_shape(&shape).is_err());
    }

    #[test]
    fn legacy_skin_writes_the_expected_header() {
        let bytes = legacy_skinned_shape(&skinned_spec()).unwrap();
        let line = b"Gamebryo File Format, Version 20.2.0.7\n";
        assert!(bytes.starts_with(line));
        let version = u32::from_le_bytes(bytes[line.len()..line.len() + 4].try_into().unwrap());
        assert_eq!(version, NIF_VERSION);
        assert_eq!(bytes[line.len() + 4], 1);
        let user = u32::from_le_bytes(bytes[line.len() + 5..line.len() + 9].try_into().unwrap());
        assert_eq!(user, USER_VERSION);
        let blocks = u32::from_le_bytes(bytes[line.len() + 9..line.len() + 13].try_into().unwrap());
        assert_eq!(blocks, 9);
    }

    #[test]
    fn legacy_skin_output_is_deterministic() {
        let spec = skinned_spec();
        assert_eq!(
            legacy_skinned_shape(&spec).unwrap(),
            legacy_skinned_shape(&spec).unwrap()
        );
    }

    #[test]
    fn legacy_skin_instance_kinds_differ_by_the_trailing_array() {
        let dismember = skin_instance(SkinInstanceKind::Dismember, 5, 6, 1, 2);
        let plain = skin_instance(SkinInstanceKind::Plain, 5, 6, 1, 2);
        // Trailing array = num_partitions u32 + one (i16 part_flag, i16 body_part).
        assert_eq!(dismember.len() - plain.len(), 8);
    }

    #[test]
    fn legacy_skin_without_geometry_omits_the_shape() {
        let mut spec = skinned_spec();
        spec.with_geometry = false;
        let bytes = legacy_skinned_shape(&spec).unwrap();
        let line = b"Gamebryo File Format, Version 20.2.0.7\n";
        let blocks = u32::from_le_bytes(bytes[line.len() + 9..line.len() + 13].try_into().unwrap());
        assert_eq!(blocks, 8);
    }

    #[test]
    fn legacy_skin_rejects_a_non_two_bone_palette() {
        let mut spec = skinned_spec();
        spec.palette = &[0, 1, 0];
        assert!(legacy_skinned_shape(&spec).is_err());
    }
}

/// Parses the emitted fixture with the real `project-wormhole-nif` parser to
/// prove the writer's byte layout is accepted, not merely self-consistent.
#[cfg(test)]
mod skin_parser_tests {
    use super::{SkinInstanceKind, legacy_skinned_shape, skinned_spec};
    use nom_derive::Parse;
    use project_wormhole_nif::bs::prelude::{BsDismemberSkinInstance, NiSkinData, NiSkinPartition};
    use project_wormhole_nif::nif_block::NifBlock;
    use project_wormhole_nif::nif_file::NifFile;

    fn parse(bytes: &[u8]) -> NifFile {
        let (rest, nif) = NifFile::parse(bytes).expect("fixture must parse");
        assert_eq!(nif.blocks.len(), nif.header.block_count as usize);
        // `NifFile::parse` returns the header's leftover (the block data), so
        // the block table covers it exactly when there is no trailing data.
        let declared: usize = nif
            .header
            .block_size_index
            .iter()
            .map(|size| *size as usize)
            .sum();
        assert_eq!(
            rest.len(),
            declared,
            "block table does not cover the block data exactly"
        );
        nif
    }

    fn skin_data<'a>(nif: &'a NifFile, instance: &BsDismemberSkinInstance) -> &'a NiSkinData {
        match nif.blocks.get(instance.skin_instance.data as usize) {
            Some(NifBlock::NiSkinData(data)) => data,
            other => panic!("skin data reference is not NiSkinData: {other:?}"),
        }
    }

    fn skin_partition<'a>(
        nif: &'a NifFile,
        instance: &BsDismemberSkinInstance,
    ) -> &'a NiSkinPartition {
        match nif
            .blocks
            .get(instance.skin_instance.skin_partition as usize)
        {
            Some(NifBlock::NiSkinPartition(partition)) => partition,
            other => panic!("skin partition reference is not NiSkinPartition: {other:?}"),
        }
    }

    #[test]
    fn legacy_dismember_skin_parses_with_the_real_parser() {
        let spec = skinned_spec();
        assert_eq!(spec.instance_kind, SkinInstanceKind::Dismember);
        let bytes = legacy_skinned_shape(&spec).unwrap();
        let nif = parse(&bytes);
        assert_eq!(nif.blocks.len(), nif.header.block_count as usize);

        let (instance_index, instance) = nif
            .blocks
            .iter()
            .enumerate()
            .find_map(|(index, block)| match block {
                NifBlock::BsDismemberSkinInstance(instance) => Some((index, instance)),
                _ => None,
            })
            .expect("no BSDismemberSkinInstance");
        assert_eq!(instance.skin_instance.bones.len(), 2);

        let data = skin_data(&nif, instance);
        assert_eq!(data.bone_count, 2);

        let partition = skin_partition(&nif, instance);
        assert_eq!(partition.bone_palette, spec.palette.to_vec());

        let shapes = nif.get_shapes();
        assert_eq!(shapes.len(), 1);
        assert_eq!(
            partition.weights.len(),
            shapes[0].get_vertex_positions().len()
        );

        println!(
            "dismember fixture={}B blocks={} instance_block={} bones={} palette={:?} vertices={} weights={}",
            bytes.len(),
            nif.blocks.len(),
            instance_index,
            data.bone_count,
            partition.bone_palette,
            partition.vertex_data.len(),
            partition.weights.len(),
        );
    }

    #[test]
    fn legacy_plain_skin_parses_with_the_real_parser() {
        let mut spec = skinned_spec();
        spec.instance_kind = SkinInstanceKind::Plain;
        let bytes = legacy_skinned_shape(&spec).unwrap();
        let nif = parse(&bytes);

        let instance = nif
            .blocks
            .iter()
            .find_map(|block| match block {
                NifBlock::NiSkinInstance(instance) => Some(instance),
                _ => None,
            })
            .expect("no NiSkinInstance");
        assert_eq!(instance.bones.len(), 2);

        let data = match nif.blocks.get(instance.data as usize) {
            Some(NifBlock::NiSkinData(data)) => data,
            other => panic!("skin data reference is not NiSkinData: {other:?}"),
        };
        assert_eq!(data.bone_count, 2);

        let partition = match nif.blocks.get(instance.skin_partition as usize) {
            Some(NifBlock::NiSkinPartition(partition)) => partition,
            other => panic!("skin partition reference is not NiSkinPartition: {other:?}"),
        };
        assert_eq!(partition.bone_palette, spec.palette.to_vec());
        assert_eq!(
            partition.weights.len(),
            nif.get_shapes()[0].get_vertex_positions().len()
        );

        println!(
            "plain fixture={}B blocks={} bones={} palette={:?} vertices={} weights={}",
            bytes.len(),
            nif.blocks.len(),
            data.bone_count,
            partition.bone_palette,
            partition.vertex_data.len(),
            partition.weights.len(),
        );
    }
}
