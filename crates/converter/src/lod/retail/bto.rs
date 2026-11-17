//! Retail object LOD block (`<world>.<level>.<x>.<y>.bto`).
//!
//! A `.bto` is a NIF 20.2.0.7 (user version 12, Bethesda version 100). Its geometry is one or
//! more `BSSubIndexTriShape` blocks: an ordinary `BSTriShape` payload followed by a segment table
//! that splits the triangles into runs. The base game's 2,392 object shapes all satisfy
//! "segment primitive counts sum to the triangle count", which the NIF parser enforces.
//!
//! This reader returns the shapes as plain data and keeps two things opaque on purpose:
//! - the segment table (`flag`, `value`, `num_primitives`) as stored, because the meaning of the
//!   middle value is not decoded;
//! - every `BSDistantObjectLargeRefExtraData` block as raw bytes, because large-reference
//!   semantics are not decoded (about 64% of base-game object shapes are large references).
//!
//! Vertex positions are block-local as stored. A retail block carries a shape transform
//! (translation and a scale of 4 in the base game) that the importer applies; it is returned
//! here unapplied.

use crate::mesh::{MeshConverter, parse_skyrim_header};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use std::{collections::BTreeMap, path::Path};

/// The block type whose payload is kept as opaque bytes.
pub const LARGE_REF_BLOCK_TYPE: &str = "BSDistantObjectLargeRefExtraData";

/// One vertex of an object LOD shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectLodVertex {
    pub position: [f32; 3],
    pub normal: Option<[f32; 3]>,
    pub uv: Option<[f32; 2]>,
    /// Vertex colour as stored (`R, G, B, A`).
    pub color: Option<[u8; 4]>,
}

/// One entry of a segment table, verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectLodSegment {
    pub flag: u8,
    /// Not decoded; kept as stored.
    pub value: u32,
    /// Triangles in this segment. All segments of a shape sum to its triangle count.
    pub num_primitives: u32,
}

/// One geometry block of a `.bto`.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectLodShape {
    /// Index of the block in the file.
    pub block_index: usize,
    /// Shape translation as stored (not applied to the vertices).
    pub translation: [f32; 3],
    /// Shape rotation as three columns (not applied).
    pub rotation_columns: [[f32; 3]; 3],
    /// Shape scale as stored (not applied); 4.0 in the base game.
    pub scale: f32,
    /// Raw block reference of the shader property, for the importer's material lookup.
    pub shader_property: u32,
    pub vertices: Vec<ObjectLodVertex>,
    pub triangles: Vec<[u16; 3]>,
    pub segments: Vec<ObjectLodSegment>,
}

/// A block the reader does not decode, kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueBlock {
    pub block_index: usize,
    pub block_type: String,
    pub bytes: Vec<u8>,
}

/// A parsed `.bto`.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectLodFile {
    pub user_version: u32,
    pub bethesda_version: u32,
    /// Every block type in the file with its count.
    pub block_types: BTreeMap<String, usize>,
    /// Block types the NIF reader does not decode, with counts.
    pub unhandled_blocks: BTreeMap<String, usize>,
    pub shapes: Vec<ObjectLodShape>,
    /// `BSDistantObjectLargeRefExtraData` blocks, bytes unparsed.
    pub large_ref_blocks: Vec<OpaqueBlock>,
}

impl ObjectLodFile {
    pub fn triangle_count(&self) -> usize {
        self.shapes.iter().map(|shape| shape.triangles.len()).sum()
    }

    pub fn vertex_count(&self) -> usize {
        self.shapes.iter().map(|shape| shape.vertices.len()).sum()
    }
}

/// Parses the bytes of one `.bto`. `label` only names the file in errors.
///
/// Fails if the NIF is malformed, a shape has no vertex positions, or a triangle indexes past
/// its vertices.
pub fn read_object_lod(bytes: &[u8], label: &Path) -> Result<ObjectLodFile> {
    let (nif, diagnostics) = MeshConverter::parse_nif_bytes(bytes, label)?;
    let mut shapes = Vec::new();
    for shape in nif.get_seg_shapes() {
        let base = &shape.bs_tri_shape;
        let vertices = base
            .vertex_data
            .iter()
            .map(|vertex| {
                let position = vertex.position.ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "a shape in {} has a vertex without a position",
                        label.display()
                    )
                })?;
                Ok(ObjectLodVertex {
                    position: position.to_array(),
                    normal: vertex.normal.map(|normal| normal.to_array()),
                    uv: vertex.uv.map(|uv| uv.to_array()),
                    color: vertex.vertex_colors.map(|color| color.to_array()),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let triangles: Vec<[u16; 3]> = base
            .triangles
            .iter()
            .map(|triangle| triangle.to_array())
            .collect();
        ensure!(
            triangles
                .iter()
                .flatten()
                .all(|index| usize::from(*index) < vertices.len()),
            "a shape in {} has a triangle index past its {} vertices",
            label.display(),
            vertices.len()
        );
        shapes.push(ObjectLodShape {
            block_index: 0,
            translation: base.av.translation.0.0.to_array(),
            rotation_columns: base.av.rotation.0.0.to_cols_array_2d(),
            scale: base.av.scale.0,
            shader_property: base.shader_property,
            vertices,
            triangles,
            segments: shape
                .segments
                .iter()
                .map(|segment| ObjectLodSegment {
                    flag: segment.flag,
                    value: segment.value,
                    num_primitives: segment.num_primitives,
                })
                .collect(),
        });
    }

    // Block indices and the opaque large-reference blocks come from the block table, in file
    // order, so they do not depend on how the NIF reader folds blocks.
    let (mut data, header) = parse_skyrim_header(bytes, label)?;
    let mut large_ref_blocks = Vec::new();
    let mut seg_shape_indices = Vec::new();
    for index in 0..header.block_count as usize {
        let size = usize::try_from(header.block_size_index[index])
            .wrap_err("a block size is out of range")?;
        ensure!(
            data.len() >= size,
            "block {index} of {} is truncated",
            label.display()
        );
        let (raw, rest) = data.split_at(size);
        data = rest;
        let block_type = header
            .get_block_type(index)
            .map_err(|_| color_eyre::eyre::eyre!("block {index} has an invalid type index"))?;
        match block_type {
            LARGE_REF_BLOCK_TYPE => large_ref_blocks.push(OpaqueBlock {
                block_index: index,
                block_type: block_type.to_string(),
                bytes: raw.to_vec(),
            }),
            "BSSubIndexTriShape" => seg_shape_indices.push(index),
            _ => {}
        }
    }
    ensure!(
        seg_shape_indices.len() == shapes.len(),
        "{} declares {} BSSubIndexTriShape blocks but {} parsed",
        label.display(),
        seg_shape_indices.len(),
        shapes.len()
    );
    for (shape, index) in shapes.iter_mut().zip(seg_shape_indices) {
        shape.block_index = index;
    }

    Ok(ObjectLodFile {
        user_version: header.user_version,
        bethesda_version: header.bethesda_version,
        block_types: diagnostics.block_types,
        unhandled_blocks: diagnostics.fallback_blocks,
        shapes,
        large_ref_blocks,
    })
}
