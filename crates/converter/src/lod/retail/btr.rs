//! Retail terrain LOD block (`<world>.<level>.<x>.<y>.btr`).
//!
//! A `.btr` is a NIF 20.2.0.7 (user version 12, Bethesda version 100). Its root is a
//! `BSMultiBoundNode` named `chunk` with two children: one `BSTriShape` named `land` (`Land` at
//! level 32) that is the terrain surface, textured by the block's colour and normal maps, and a
//! `BSMultiBoundNode` named `WATER` whose children are the unnamed water-plane shapes of the same
//! block. Water shapes are plain `BSTriShape` blocks at levels 8 to 32 and `BSSubIndexTriShape`
//! blocks (a `BSTriShape` payload followed by a segment table of 16 entries) at level 4.
//!
//! Source of every structure fact in this file: measured on all 9,584 base-game blocks by
//! `tests/retail_lod_readers.rs` (unverified against retail code).
//!
//! This reader returns the shapes as plain data. It decodes nothing it cannot read from the file:
//! a shape that is neither the `land` shape nor a child of the `WATER` node is kept and counted in
//! [`TerrainLodFile::unrecognised_shapes`], never guessed.
//!
//! Vertex positions are block-local and the shape's `scale` multiplies them (the base game
//! stores `scale = level`, so a level 4 block of 4 cells is positions of 0 to 4096 times 4). The
//! reader returns both unapplied; [`TerrainLodShape::world_positions`] applies them for a caller
//! that wants Creation units relative to the south-west corner of the block.

use crate::mesh::MeshConverter;
use color_eyre::{
    Result,
    eyre::{bail, ensure, eyre},
};
use project_wormhole_nif::{nif_block::NifBlock, nif_file::NifFile};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// The shape name the base-game files use for the terrain surface of a block (`Land` at level 32;
/// the match ignores case). Source: measured on all 9,584 base-game blocks by
/// `tests/retail_lod_readers.rs`; the meaning beyond the name is unverified.
pub const TERRAIN_SHAPE_NAME: &str = "land";
/// The node name that parents the water-plane shapes of a block.
pub const WATER_NODE_NAME: &str = "WATER";

/// What a shape of a `.btr` is, read from its stored name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TerrainLodShapeKind {
    /// Named [`TERRAIN_SHAPE_NAME`] (any case): the terrain surface, textured by the block's
    /// colour map.
    Terrain,
    /// A child of the node named [`WATER_NODE_NAME`]: a water plane.
    Water,
    /// Neither. Not decoded.
    Other,
}

/// One vertex of a terrain LOD shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerrainLodVertex {
    pub position: [f32; 3],
    pub uv: Option<[f32; 2]>,
    pub normal: Option<[f32; 3]>,
    /// Vertex colour as stored (`R, G, B, A`).
    pub color: Option<[u8; 4]>,
}

/// One entry of a `BSSubIndexTriShape` segment table, verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerrainLodSegment {
    pub flag: u8,
    /// Not decoded; kept as stored.
    pub value: u32,
    pub num_primitives: u32,
}

/// The shader inputs stored beside a shape, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainLodShader {
    pub shader_type: u32,
    pub flags_1: u32,
    pub flags_2: u32,
    /// Texture slots of the shader's texture set, in stored order. Empty slots are `None`.
    pub texture_slots: Vec<Option<String>>,
}

/// One geometry block of a `.btr`.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainLodShape {
    /// Index of the block in the file.
    pub block_index: usize,
    /// The name stored in the NIF string table, verbatim. `None` when the shape has no name.
    pub name: Option<String>,
    pub kind: TerrainLodShapeKind,
    /// Name of the parent node, `None` when no node lists the shape or the node has no name.
    pub parent_name: Option<String>,
    /// Index of the first non-identity node in the parent chain, nearest first.
    pub transformed_ancestor: Option<usize>,
    /// `BSTriShape` or `BSSubIndexTriShape`.
    pub block_type: &'static str,
    /// Shape translation as stored (not applied to the vertices).
    pub translation: [f32; 3],
    /// Shape rotation as three columns (not applied).
    pub rotation_columns: [[f32; 3]; 3],
    /// Shape scale as stored (not applied). The base game stores the block level.
    pub scale: f32,
    pub vertices: Vec<TerrainLodVertex>,
    pub triangles: Vec<[u16; 3]>,
    /// Empty for a `BSTriShape`.
    pub segments: Vec<TerrainLodSegment>,
    /// `None` when the shape names no shader property.
    pub shader: Option<TerrainLodShader>,
}

impl TerrainLodShape {
    /// Vertex positions multiplied by the stored scale and moved by the stored translation. The
    /// stored rotation is not applied; [`TerrainLodShape::is_axis_aligned_identity_rotation`]
    /// says whether that is exact.
    pub fn world_positions(&self) -> Vec<[f64; 3]> {
        self.vertices
            .iter()
            .map(|vertex| {
                std::array::from_fn(|axis| {
                    f64::from(vertex.position[axis]) * f64::from(self.scale)
                        + f64::from(self.translation[axis])
                })
            })
            .collect()
    }

    /// True when the stored rotation is exactly the identity.
    pub fn is_axis_aligned_identity_rotation(&self) -> bool {
        self.rotation_columns == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    }

    /// Smallest and largest stored position on each axis, or `None` for an empty shape.
    pub fn local_bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let first = self.vertices.first()?;
        let (mut min, mut max) = (first.position, first.position);
        for vertex in &self.vertices {
            for axis in 0..3 {
                min[axis] = min[axis].min(vertex.position[axis]);
                max[axis] = max[axis].max(vertex.position[axis]);
            }
        }
        Some((min, max))
    }
}

/// The transform of a node block, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainLodNodeTransform {
    pub block_index: usize,
    pub name: Option<String>,
    pub translation: [f32; 3],
    pub rotation_columns: [[f32; 3]; 3],
    pub scale: f32,
}

/// A parsed `.btr`.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainLodFile {
    pub user_version: u32,
    pub bethesda_version: u32,
    /// Every block type in the file with its count.
    pub block_types: BTreeMap<String, usize>,
    /// Block types the NIF reader does not decode, with counts.
    pub unhandled_blocks: BTreeMap<String, usize>,
    /// Node blocks whose transform is not the identity, as stored.
    pub non_identity_nodes: Vec<TerrainLodNodeTransform>,
    pub shapes: Vec<TerrainLodShape>,
}

impl TerrainLodFile {
    /// The shapes of one kind, in file order.
    pub fn shapes_of(&self, kind: TerrainLodShapeKind) -> impl Iterator<Item = &TerrainLodShape> {
        self.shapes.iter().filter(move |shape| shape.kind == kind)
    }

    /// Shapes that are neither the terrain surface nor a water plane.
    pub fn unrecognised_shapes(&self) -> usize {
        self.shapes_of(TerrainLodShapeKind::Other).count()
    }

    /// Triangles of the shapes of `kind`.
    pub fn triangle_count(&self, kind: TerrainLodShapeKind) -> usize {
        self.shapes_of(kind)
            .map(|shape| shape.triangles.len())
            .sum()
    }

    /// Vertices of the shapes of `kind`.
    pub fn vertex_count(&self, kind: TerrainLodShapeKind) -> usize {
        self.shapes_of(kind).map(|shape| shape.vertices.len()).sum()
    }
}

fn classify(name: Option<&str>, parent: Option<&str>) -> TerrainLodShapeKind {
    if parent == Some(WATER_NODE_NAME) {
        TerrainLodShapeKind::Water
    } else if name.is_some_and(|name| name.eq_ignore_ascii_case(TERRAIN_SHAPE_NAME)) {
        TerrainLodShapeKind::Terrain
    } else {
        TerrainLodShapeKind::Other
    }
}

struct Parent {
    index: usize,
    name: Option<String>,
}

/// The node that lists each block as a child, by block index.
fn parents(nif: &NifFile) -> Result<BTreeMap<usize, Parent>> {
    let mut parents = BTreeMap::new();
    for (index, block) in nif.blocks.iter().enumerate() {
        if let Ok(node) = block.as_node() {
            let name = match node.av.object.name {
                u32::MAX => None,
                name => Some(
                    nif.header
                        .get_string(name as usize)
                        .map_err(|error| eyre!("{error}"))?
                        .to_owned(),
                ),
            };
            for &child in &node.children {
                if child == u32::MAX {
                    continue;
                }
                ensure!(
                    parents
                        .insert(
                            child as usize,
                            Parent {
                                index,
                                name: name.clone()
                            }
                        )
                        .is_none(),
                    "block {child} has more than one parent reference"
                );
            }
        }
    }
    Ok(parents)
}

/// Builds a [`TerrainLodShape`] from a `BSTriShape` payload. A macro because the NIF crate does
/// not export the payload type, so a function cannot name it.
macro_rules! read_shape {
    ($nif:expr, $parents:expr, $block_index:expr, $block_type:expr, $base:expr, $segments:expr, $label:expr) => {{
        let (nif, parents, block_index, block_type, base, segments, label): (
            &NifFile,
            &BTreeMap<usize, Parent>,
            usize,
            &'static str,
            _,
            &[TerrainLodSegment],
            &Path,
        ) = (
            $nif,
            $parents,
            $block_index,
            $block_type,
            $base,
            $segments,
            $label,
        );
        // An index of `u32::MAX` is the NIF "no name"; anything else must be in the string table.
        let name: Option<String> = match base.av.object.name {
            u32::MAX => None,
            index => Some(
                nif.header
                    .get_string(index as usize)
                    .map_err(|error| {
                        eyre!(
                            "block {block_index} of {} has no usable name: {error}",
                            label.display()
                        )
                    })?
                    .to_owned(),
            ),
        };
        let label_name = name.as_deref().unwrap_or("(unnamed)").to_owned();
        let vertices = base
            .vertex_data
            .iter()
            .map(|vertex| {
                let position = vertex.position.ok_or_else(|| {
                    eyre!(
                        "shape `{label_name}` in {} has a vertex without a position",
                        label.display()
                    )
                })?;
                Ok(TerrainLodVertex {
                    position: position.to_array(),
                    uv: vertex.uv.map(|uv| uv.to_array()),
                    normal: vertex.normal.map(|normal| normal.to_array()),
                    color: vertex.vertex_colors.map(|color| color.to_array()),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let triangles: Vec<[u16; 3]> = base.triangles.iter().map(|t| t.to_array()).collect();
        ensure!(
            triangles
                .iter()
                .flatten()
                .all(|index| usize::from(*index) < vertices.len()),
            "shape `{label_name}` in {} has a triangle index past its {} vertices",
            label.display(),
            vertices.len()
        );
        if block_type == "BSSubIndexTriShape" {
            let sum: u64 = segments.iter().map(|s| u64::from(s.num_primitives)).sum();
            ensure!(
                sum == triangles.len() as u64,
                "shape `{label_name}` in {} has segments for {sum} triangles but {} triangles",
                label.display(),
                triangles.len()
            );
        }
        let shader = match base.shader_property {
            u32::MAX => None,
            reference => Some(read_shader(nif, reference, label)?),
        };
        let parent_name = parents
            .get(&block_index)
            .and_then(|parent| parent.name.clone());
        TerrainLodShape {
            block_index,
            kind: classify(name.as_deref(), parent_name.as_deref()),
            name,
            parent_name,
            transformed_ancestor: None,
            block_type,
            translation: base.av.translation.0.0.to_array(),
            rotation_columns: base.av.rotation.0.0.to_cols_array_2d(),
            scale: base.av.scale.0,
            vertices,
            triangles,
            segments: segments.to_vec(),
            shader,
        }
    }};
}

/// Parses the bytes of one `.btr`. `label` only names the file in errors.
///
/// Fails if the NIF is malformed, a shape has no vertex positions or no name, a triangle indexes
/// past its vertices, or a segment table does not add up to its shape's triangle count.
#[allow(
    clippy::float_cmp,
    reason = "a stored identity transform is compared exactly: any other value is reported, not rounded"
)]
pub fn read_terrain_lod(bytes: &[u8], label: &Path) -> Result<TerrainLodFile> {
    let (nif, diagnostics) = MeshConverter::parse_nif_bytes(bytes, label)?;
    let parents = parents(&nif)?;
    let mut shapes = Vec::new();
    let mut non_identity_nodes = Vec::new();
    for (block_index, block) in nif.blocks.iter().enumerate() {
        match block {
            NifBlock::BSTriShape(shape) => {
                shapes.push(read_shape!(
                    &nif,
                    &parents,
                    block_index,
                    "BSTriShape",
                    shape,
                    &[],
                    label
                ));
            }
            NifBlock::BSSubIndexTriShape(shape) => {
                let segments: Vec<TerrainLodSegment> = shape
                    .segments
                    .iter()
                    .map(|segment| TerrainLodSegment {
                        flag: segment.flag,
                        value: segment.value,
                        num_primitives: segment.num_primitives,
                    })
                    .collect();
                shapes.push(read_shape!(
                    &nif,
                    &parents,
                    block_index,
                    "BSSubIndexTriShape",
                    &shape.bs_tri_shape,
                    &segments,
                    label
                ));
            }
            _ => {
                let Ok(node) = block.as_node() else { continue };
                let av = &node.av;
                let identity_rotation = av.rotation.0.0.to_cols_array_2d()
                    == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
                if av.translation.0.0.to_array() != [0.0; 3]
                    || av.scale.0 != 1.0
                    || !identity_rotation
                {
                    non_identity_nodes.push(TerrainLodNodeTransform {
                        block_index,
                        name: match av.object.name {
                            u32::MAX => None,
                            index => nif
                                .header
                                .get_string(index as usize)
                                // silent-ok: an unreadable node name is reported as no name; the transform is still listed
                                .ok()
                                .map(str::to_owned),
                        },
                        translation: av.translation.0.0.to_array(),
                        rotation_columns: av.rotation.0.0.to_cols_array_2d(),
                        scale: av.scale.0,
                    });
                }
            }
        }
    }
    let transformed: BTreeSet<_> = non_identity_nodes
        .iter()
        .map(|node| node.block_index)
        .collect();
    for shape in &mut shapes {
        let mut ancestors = BTreeSet::new();
        let mut index = shape.block_index;
        while let Some(parent) = parents.get(&index) {
            ensure!(
                ancestors.insert(parent.index),
                "cycle in parent chain of block {}",
                shape.block_index
            );
            if transformed.contains(&parent.index) {
                shape.transformed_ancestor.get_or_insert(parent.index);
            }
            index = parent.index;
        }
    }
    Ok(TerrainLodFile {
        user_version: nif.header.user_version,
        bethesda_version: nif.header.bethesda_version,
        block_types: diagnostics.block_types,
        unhandled_blocks: diagnostics.fallback_blocks,
        non_identity_nodes,
        shapes,
    })
}

fn read_shader(nif: &NifFile, reference: u32, label: &Path) -> Result<TerrainLodShader> {
    let Some(NifBlock::BSLightingShaderProperty(property)) = nif.blocks.get(reference as usize)
    else {
        bail!(
            "shader property reference {reference} in {} is not a BSLightingShaderProperty",
            label.display()
        );
    };
    let texture_slots = match nif.blocks.get(property.texture_set as usize) {
        Some(NifBlock::BSShaderTextureSet(set)) => set.textures.clone(),
        _ => Vec::new(),
    };
    Ok(TerrainLodShader {
        shader_type: property.shader_type,
        flags_1: property.shader_flags_1.raw_flags,
        flags_2: property.shader_flags_2.raw_flags,
        texture_slots,
    })
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    reason = "test builders write small counts and compare exact stored values"
)]
mod tests {
    use super::*;

    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend(value.to_le_bytes());
    }

    fn push_f32(bytes: &mut Vec<u8>, value: f32) {
        bytes.extend(value.to_le_bytes());
    }

    /// The shared start of an `NiAVObject`: name, no extra data, no controller, flags, transform.
    fn av_object(name: u32, translation: [f32; 3], scale: f32) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, name);
        push_u32(&mut bytes, 0); // extra data count
        push_u32(&mut bytes, u32::MAX); // controller
        push_u32(&mut bytes, 14); // flags
        for value in translation {
            push_f32(&mut bytes, value);
        }
        for value in [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
            push_f32(&mut bytes, value);
        }
        push_f32(&mut bytes, scale);
        push_u32(&mut bytes, u32::MAX); // collision object
        bytes
    }

    /// A `BSMultiBoundNode` with the given children.
    fn node_block(name: u32, translation: [f32; 3], children: &[u32]) -> Vec<u8> {
        let mut bytes = av_object(name, translation, 1.0);
        push_u32(&mut bytes, children.len() as u32);
        for child in children {
            push_u32(&mut bytes, *child);
        }
        push_u32(&mut bytes, 0); // effect count
        push_u32(&mut bytes, u32::MAX); // multi bound
        push_u32(&mut bytes, 0); // culling mode
        bytes
    }

    /// A `BSTriShape` (or `BSSubIndexTriShape` with `segments`) of vertices with a full-precision
    /// position and a half-float UV when `uvs` is given. `with_positions` false declares the
    /// vertices without geometry data, which the NIF reader reads as vertices without positions.
    fn shape_block(
        name: u32,
        scale: f32,
        vertices: &[[f32; 3]],
        uvs: Option<&[[u16; 2]]>,
        triangles: &[[u16; 3]],
        segments: Option<&[(u8, u32, u32)]>,
        with_positions: bool,
    ) -> Vec<u8> {
        let mut bytes = av_object(name, [0.0; 3], scale);
        for _ in 0..4 {
            push_f32(&mut bytes, 0.0); // bounding sphere
        }
        push_u32(&mut bytes, u32::MAX); // skin
        push_u32(&mut bytes, u32::MAX); // shader property: none
        push_u32(&mut bytes, u32::MAX); // alpha property
        let (flags, dwords): (u64, u64) = if uvs.is_some() {
            (0x0403, 5)
        } else {
            (0x0401, 4)
        };
        bytes.extend((dwords | (flags << 44)).to_le_bytes());
        bytes.extend((triangles.len() as u16).to_le_bytes());
        bytes.extend((vertices.len() as u16).to_le_bytes());
        if with_positions {
            let vertex_bytes = vertices.len() * dwords as usize * 4;
            push_u32(&mut bytes, (vertex_bytes + triangles.len() * 6) as u32);
            for (index, vertex) in vertices.iter().enumerate() {
                for value in vertex {
                    push_f32(&mut bytes, *value);
                }
                push_f32(&mut bytes, 0.0);
                if let Some(uvs) = uvs {
                    for half in uvs[index] {
                        bytes.extend(half.to_le_bytes());
                    }
                }
            }
            for triangle in triangles {
                for index in triangle {
                    bytes.extend(index.to_le_bytes());
                }
            }
        } else {
            push_u32(&mut bytes, 0);
        }
        push_u32(&mut bytes, 0); // particle data size
        if let Some(segments) = segments {
            push_u32(&mut bytes, segments.len() as u32);
            for (flag, value, primitives) in segments {
                bytes.push(*flag);
                push_u32(&mut bytes, *value);
                push_u32(&mut bytes, *primitives);
            }
        }
        bytes
    }

    /// A NIF 20.2.0.7 (user version 12, Bethesda version 100) holding `blocks` in order, with the
    /// given string table.
    fn nif_bytes(strings: &[&str], blocks: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut types: Vec<&str> = Vec::new();
        for (name, _) in blocks {
            if !types.contains(name) {
                types.push(name);
            }
        }
        let mut bytes = b"Gamebryo File Format, Version 20.2.0.7\n".to_vec();
        push_u32(&mut bytes, 0x1402_0007);
        bytes.push(1); // little endian
        push_u32(&mut bytes, 12); // user version
        push_u32(&mut bytes, blocks.len() as u32);
        push_u32(&mut bytes, 100); // Bethesda version
        bytes.extend([0, 0, 0]); // author, process script, export script: empty
        bytes.extend((types.len() as u16).to_le_bytes());
        for name in &types {
            push_u32(&mut bytes, name.len() as u32);
            bytes.extend(name.as_bytes());
        }
        for (name, _) in blocks {
            let index = types
                .iter()
                .position(|candidate| candidate == name)
                .unwrap();
            bytes.extend((index as u16).to_le_bytes());
        }
        for (_, block) in blocks {
            push_u32(&mut bytes, block.len() as u32);
        }
        push_u32(&mut bytes, strings.len() as u32);
        push_u32(
            &mut bytes,
            strings.iter().map(|text| text.len()).max().unwrap_or(0) as u32,
        );
        for text in strings {
            push_u32(&mut bytes, text.len() as u32);
            bytes.extend(text.as_bytes());
        }
        push_u32(&mut bytes, 0); // groups
        for (_, block) in blocks {
            bytes.extend(block);
        }
        bytes
    }

    fn label() -> &'static Path {
        Path::new("synthetic.btr")
    }

    const TRIANGLE: [[f32; 3]; 3] = [
        [0.0, 0.0, -10.0],
        [4096.0, 0.0, -20.0],
        [0.0, 4096.0, -30.0],
    ];
    // Half floats: 0.0 is 0x0000, 1.0 is 0x3C00.
    const UVS: [[u16; 2]; 3] = [[0x0000, 0x3C00], [0x3C00, 0x3C00], [0x0000, 0x0000]];

    #[test]
    fn a_land_shape_and_its_water_child_are_told_apart_by_name_and_parent() {
        // Block 0 is the root, block 1 the land shape, block 2 the WATER node, block 3 its child.
        let root = node_block(0, [0.0; 3], &[1, 2]);
        let land = shape_block(1, 4.0, &TRIANGLE, Some(&UVS), &[[0, 1, 2]], None, true);
        let water_node = node_block(2, [0.0; 3], &[3]);
        let water = shape_block(u32::MAX, 4.0, &TRIANGLE, None, &[[0, 1, 2]], None, true);
        let bytes = nif_bytes(
            &["chunk", "Land", "WATER"],
            &[
                ("BSMultiBoundNode", root),
                ("BSTriShape", land),
                ("BSMultiBoundNode", water_node),
                ("BSTriShape", water),
            ],
        );
        let parsed = read_terrain_lod(&bytes, label()).unwrap();
        assert_eq!((parsed.bethesda_version, parsed.user_version), (100, 12));
        assert!(parsed.non_identity_nodes.is_empty());
        assert_eq!(parsed.shapes.len(), 2);

        let land = &parsed.shapes[0];
        assert_eq!(land.block_index, 1);
        assert_eq!(land.name.as_deref(), Some("Land"));
        assert_eq!(land.kind, TerrainLodShapeKind::Terrain);
        assert_eq!(land.parent_name.as_deref(), Some("chunk"));
        assert_eq!(land.block_type, "BSTriShape");
        assert_eq!(land.scale, 4.0);
        assert_eq!(land.vertices[1].position, [4096.0, 0.0, -20.0]);
        assert_eq!(land.vertices[0].uv, Some([0.0, 1.0]));
        assert_eq!(land.vertices[2].uv, Some([0.0, 0.0]));
        assert_eq!(land.triangles, vec![[0, 1, 2]]);
        assert!(land.is_axis_aligned_identity_rotation());
        assert_eq!(
            land.local_bounds(),
            Some(([0.0, 0.0, -30.0], [4096.0, 4096.0, -10.0]))
        );
        // Positions times the stored scale: a level 4 block is 16384 units wide.
        assert_eq!(land.world_positions()[1], [16384.0, 0.0, -80.0]);

        let water = &parsed.shapes[1];
        assert_eq!(water.block_index, 3);
        assert_eq!(water.name, None);
        assert_eq!(water.parent_name.as_deref(), Some("WATER"));
        assert_eq!(water.kind, TerrainLodShapeKind::Water);
        assert_eq!(water.vertices[0].uv, None);
        assert_eq!(parsed.unrecognised_shapes(), 0);
        assert_eq!(parsed.triangle_count(TerrainLodShapeKind::Terrain), 1);
        assert_eq!(parsed.vertex_count(TerrainLodShapeKind::Water), 3);
    }

    #[test]
    fn a_shape_that_is_neither_land_nor_water_is_counted_not_guessed() {
        let other = shape_block(0, 4.0, &TRIANGLE, None, &[[0, 1, 2]], None, true);
        let bytes = nif_bytes(&["mystery"], &[("BSTriShape", other)]);
        let parsed = read_terrain_lod(&bytes, label()).unwrap();
        assert_eq!(parsed.shapes[0].kind, TerrainLodShapeKind::Other);
        assert_eq!(parsed.unrecognised_shapes(), 1);
        // A land shape's name is not a prefix match.
        let landing = shape_block(0, 4.0, &TRIANGLE, None, &[[0, 1, 2]], None, true);
        let bytes = nif_bytes(&["landing"], &[("BSTriShape", landing)]);
        assert_eq!(
            read_terrain_lod(&bytes, label()).unwrap().shapes[0].kind,
            TerrainLodShapeKind::Other
        );
    }

    #[test]
    fn a_land_shape_with_a_transformed_grandparent_is_rejected_by_the_compiler() {
        let root = node_block(0, [10.0, 0.0, 0.0], &[1]);
        let parent = node_block(1, [0.0; 3], &[2]);
        let land = shape_block(2, 4.0, &TRIANGLE, Some(&UVS), &[[0, 1, 2]], None, true);
        let bytes = nif_bytes(
            &["root", "chunk", "Land"],
            &[
                ("BSMultiBoundNode", root),
                ("BSMultiBoundNode", parent),
                ("BSTriShape", land),
            ],
        );
        let parsed = read_terrain_lod(&bytes, label()).unwrap();
        let land = &parsed.shapes[0];
        assert_eq!(land.kind, TerrainLodShapeKind::Terrain);
        assert_eq!(land.parent_name.as_deref(), Some("chunk"));
        assert_eq!(land.transformed_ancestor, Some(0));
        let error = crate::lod::retail_terrain::cut_land(land, 4).unwrap_err();
        assert!(error.to_string().contains("ancestor block 0"));
    }

    #[test]
    fn a_node_with_a_transform_is_reported() {
        let root = node_block(0, [10.0, 0.0, 0.0], &[]);
        let bytes = nif_bytes(&["chunk"], &[("BSMultiBoundNode", root)]);
        let parsed = read_terrain_lod(&bytes, label()).unwrap();
        assert_eq!(parsed.non_identity_nodes.len(), 1);
        assert_eq!(parsed.non_identity_nodes[0].translation, [10.0, 0.0, 0.0]);
        assert_eq!(parsed.non_identity_nodes[0].name.as_deref(), Some("chunk"));
    }

    #[test]
    fn a_sub_index_shape_keeps_its_segment_table() {
        let shape = shape_block(
            u32::MAX,
            4.0,
            &[[0.0; 3], [1.0; 3], [2.0; 3], [3.0; 3]],
            None,
            &[[0, 1, 2], [1, 2, 3]],
            Some(&[(0, 7, 2), (0, 0, 0)]),
            true,
        );
        let bytes = nif_bytes(&[], &[("BSSubIndexTriShape", shape)]);
        let parsed = read_terrain_lod(&bytes, label()).unwrap();
        assert_eq!(parsed.shapes[0].block_type, "BSSubIndexTriShape");
        assert_eq!(
            parsed.shapes[0].segments,
            vec![
                TerrainLodSegment {
                    flag: 0,
                    value: 7,
                    num_primitives: 2
                },
                TerrainLodSegment {
                    flag: 0,
                    value: 0,
                    num_primitives: 0
                },
            ]
        );
    }

    #[test]
    fn a_name_outside_the_string_table_is_an_error() {
        let shape = shape_block(5, 1.0, &TRIANGLE, None, &[[0, 1, 2]], None, true);
        let error =
            read_terrain_lod(&nif_bytes(&["a"], &[("BSTriShape", shape)]), label()).unwrap_err();
        assert!(format!("{error:#}").contains("no usable name"), "{error:#}");
    }

    #[test]
    fn a_vertex_without_a_position_is_an_error() {
        let shape = shape_block(u32::MAX, 1.0, &TRIANGLE, None, &[], None, false);
        let error =
            read_terrain_lod(&nif_bytes(&[], &[("BSTriShape", shape)]), label()).unwrap_err();
        assert!(
            format!("{error:#}").contains("without a position"),
            "{error:#}"
        );
    }

    #[test]
    fn a_triangle_index_past_the_vertices_is_an_error() {
        let shape = shape_block(u32::MAX, 1.0, &TRIANGLE, None, &[[0, 1, 3]], None, true);
        let error =
            read_terrain_lod(&nif_bytes(&[], &[("BSTriShape", shape)]), label()).unwrap_err();
        assert!(
            format!("{error:#}").contains("triangle index past"),
            "{error:#}"
        );
    }

    #[test]
    fn a_file_that_is_not_a_nif_is_an_error() {
        assert!(read_terrain_lod(b"not a nif", label()).is_err());
    }
}
