//! Retail object LOD block (`<world>.<level>.<x>.<y>.bto`).
//!
//! A `.bto` is a NIF 20.2.0.7 (user version 12, Bethesda version 100). Its geometry is one or
//! more `BSSubIndexTriShape` blocks: an ordinary `BSTriShape` payload followed by a segment table
//! that splits the triangles into runs. The base game's 2,392 object shapes all satisfy
//! "segment primitive counts sum to the triangle count", which the NIF parser enforces.
//!
//! The reader preserves stored segment ranges and the raw large-reference blocks. It reads the
//! Boolean from each shape's linked `BSDistantObjectLargeRefExtraData` block.
//! See https://github.com/Mudcrab-Team/skyrim_decomp/pull/821 G04.
//!
//! Vertex positions are block-local as stored. A retail block carries a shape transform
//! (a translation to the block origin, and a scale equal to the block level: 4 at level 4, 16 at
//! level 16) that the importer applies; it is returned here unapplied.

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
    /// Model-space axes for the atlas red and green channels (skyrim_decomp#948 G03).
    pub atlas_basis: Option<[[f32; 3]; 2]>,
    pub uv: Option<[f32; 2]>,
    /// Vertex colour as stored (`R, G, B, A`).
    pub color: Option<[u8; 4]>,
}

/// One entry of a segment table, verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectLodSegment {
    pub flag: u8,
    /// Start in the shape’s index buffer, in indices (skyrim_decomp#816, G02).
    pub start_index: u32,
    /// Triangles in this segment. All segments of a shape sum to its triangle count.
    pub num_primitives: u32,
}

/// One geometry block of a `.bto`.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectLodShape {
    /// Index of the block in the file.
    pub block_index: usize,
    /// True when the shape references a true BSDistantObjectLargeRefExtraData marker.
    pub large_reference: bool,
    /// Serialized `NiAVObject` flags. The runtime grouped-draw flags in skyrim_decomp#675 B3
    /// are a different field and are unavailable to this reader.
    pub flags: u32,
    /// Shape translation as stored (not applied to the vertices).
    pub translation: [f32; 3],
    /// Shape rotation as three columns (not applied).
    pub rotation_columns: [[f32; 3]; 3],
    /// Shape scale as stored (not applied): the block level, 4.0 at level 4 and 16.0 at level 16.
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

fn atlas_basis(x: f32, y: f32, z: f32, green: [u8; 3]) -> [[f32; 3]; 2] {
    let signed = |value: f32| value * (2.0 / 255.0) - 1.0;
    [
        [x, signed(y), signed(z)],
        green.map(|value| signed(f32::from(value))),
    ]
}

#[test]
fn packed_basis_preserves_float_x_and_decodes_the_signed_bytes() {
    let [red, green] = atlas_basis(-0.25, 0.0, 255.0, [255, 127, 0]);
    assert_eq!(
        red.map(f32::to_bits),
        [-0.25_f32, -1.0, 1.0].map(f32::to_bits)
    );
    assert_eq!(
        [green[0], green[2]].map(f32::to_bits),
        [1.0_f32, -1.0].map(f32::to_bits)
    );
    assert!((green[1] + 1.0 / 255.0).abs() < 1e-6);
}

/// Parses the bytes of one `.bto`. `label` only names the file in errors.
///
/// Fails if the NIF is malformed, a shape has no vertex positions, or a triangle indexes past
/// its vertices.
pub fn read_object_lod(bytes: &[u8], label: &Path) -> Result<ObjectLodFile> {
    let (nif, diagnostics) = MeshConverter::parse_nif_bytes(bytes, label)?;
    let mut shapes = Vec::new();
    let mut shape_extra_data = Vec::new();
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
                    atlas_basis: vertex
                        .bitangent_x
                        .zip(vertex.bitangent_y)
                        .zip(vertex.bitangent_z)
                        .zip(vertex.tangent)
                        .map(|(((x, y), z), green)| atlas_basis(x, y, z, green.to_array())),
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
        shape_extra_data.push(base.av.extra_data_refs.clone());
        shapes.push(ObjectLodShape {
            block_index: 0,
            large_reference: false,
            flags: base.av.flags.bits(),
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
                    start_index: segment.value,
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
    let marker_blocks: BTreeMap<_, _> = large_ref_blocks
        .iter()
        .map(|block| (block.block_index, &block.bytes))
        .collect();
    for ((shape, index), extra_data) in shapes
        .iter_mut()
        .zip(seg_shape_indices)
        .zip(shape_extra_data)
    {
        shape.block_index = index;
        for reference in extra_data {
            let block = usize::try_from(reference).wrap_err("extra data index is out of range")?;
            if let Some(bytes) = marker_blocks.get(&block) {
                let flag = bytes.get(4).ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "{} large-reference marker block {block} lacks a Boolean",
                        label.display()
                    )
                })?;
                shape.large_reference |= *flag != 0;
            }
        }
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend(value.to_le_bytes());
    }

    fn push_f32(bytes: &mut Vec<u8>, value: f32) {
        bytes.extend(value.to_le_bytes());
    }

    /// A `BSSubIndexTriShape` block. `with_positions` stores full-precision positions (a 16-byte
    /// vertex: three floats and a spare float); otherwise the block declares `vertices.len()`
    /// vertices without geometry data, which the NIF reader reads as vertices without positions.
    pub(crate) fn shape_block(
        translation: [f32; 3],
        scale: f32,
        vertices: &[[f32; 3]],
        triangles: &[[u16; 3]],
        segments: &[(u8, u32, u32)],
        with_positions: bool,
        with_uvs: bool,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, u32::MAX); // name
        push_u32(&mut bytes, u32::MAX); // extra data
        push_u32(&mut bytes, u32::MAX); // controller
        push_u32(&mut bytes, 0); // flags
        for value in translation {
            push_f32(&mut bytes, value);
        }
        for value in [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
            push_f32(&mut bytes, value); // identity rotation
        }
        push_f32(&mut bytes, scale);
        push_u32(&mut bytes, u32::MAX); // collision object
        for _ in 0..4 {
            push_f32(&mut bytes, 0.0); // bounding sphere
        }
        push_u32(&mut bytes, u32::MAX); // skin
        push_u32(&mut bytes, u32::MAX); // shader property: none
        push_u32(&mut bytes, u32::MAX); // alpha property
        // Full-precision positions take four words. The optional half-float UV pair adds one.
        let words = 4 + u64::from(with_uvs);
        let uv_descriptor = if with_uvs {
            (4 << 8) | (0x0002u64 << 44)
        } else {
            0
        };
        let desc = words | (0x0401u64 << 44) | uv_descriptor;
        bytes.extend(desc.to_le_bytes());
        bytes.extend((triangles.len() as u16).to_le_bytes());
        bytes.extend((vertices.len() as u16).to_le_bytes());
        if with_positions {
            let payload_bytes =
                vertices.len() * (16 + 4 * usize::from(with_uvs)) + triangles.len() * 6;
            push_u32(&mut bytes, u32::try_from(payload_bytes).unwrap());
            for vertex in vertices {
                for value in vertex {
                    push_f32(&mut bytes, *value);
                }
                push_f32(&mut bytes, 0.0);
                if with_uvs {
                    push_u32(&mut bytes, 0x3800_3800); // (0.5, 0.5) as two half floats
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
        push_u32(&mut bytes, segments.len() as u32);
        for (flag, value, primitives) in segments {
            bytes.push(*flag);
            push_u32(&mut bytes, *value);
            push_u32(&mut bytes, *primitives);
        }
        bytes
    }

    /// A NIF 20.2.0.7 (user version 12, Bethesda version 100) holding `blocks` in order.
    pub(crate) fn nif_bytes(blocks: &[(&str, Vec<u8>)]) -> Vec<u8> {
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
        push_u32(&mut bytes, 0); // strings
        push_u32(&mut bytes, 0); // longest string
        push_u32(&mut bytes, 0); // groups
        for (_, block) in blocks {
            bytes.extend(block);
        }
        bytes
    }

    pub(crate) const SHAPE: &str = "BSSubIndexTriShape";
    const LARGE_REF: &str = LARGE_REF_BLOCK_TYPE;

    fn label() -> &'static Path {
        Path::new("synthetic.bto")
    }

    #[test]
    fn reads_transforms_block_indices_and_exact_opaque_bytes() {
        let mut first = shape_block(
            [100.0, -200.0, 300.0],
            4.0,
            &[[0.0, 1.0, 2.0], [3.0, 4.0, 5.0], [6.0, 7.0, 8.0]],
            &[[0, 1, 2]],
            &[(0, 9, 1)],
            true,
            false,
        );
        first.splice(4..8, [1_u32.to_le_bytes(), 0_u32.to_le_bytes()].concat());
        let second = shape_block(
            [0.0; 3],
            1.0,
            &[[1.0, 1.0, 1.0], [2.0, 2.0, 2.0], [3.0, 3.0, 3.0]],
            &[[2, 1, 0], [0, 1, 2]],
            &[(0, 5, 1), (0, 6, 1)],
            true,
            false,
        );
        let opaque_a = vec![0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3];
        let opaque_b = vec![0x00, 0xFF, 0x7F];
        let bytes = nif_bytes(&[
            (LARGE_REF, opaque_a.clone()),
            (SHAPE, first),
            (LARGE_REF, opaque_b.clone()),
            (SHAPE, second),
        ]);
        let parsed = read_object_lod(&bytes, label()).unwrap();

        assert_eq!((parsed.bethesda_version, parsed.user_version), (100, 12));
        assert_eq!(parsed.shapes.len(), 2);
        let a = &parsed.shapes[0];
        assert_eq!(a.block_index, 1);
        assert_eq!(a.translation, [100.0, -200.0, 300.0]);
        assert_eq!(a.scale, 4.0);
        assert_eq!(
            a.rotation_columns,
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        );
        assert_eq!(a.shader_property, u32::MAX);
        assert_eq!(a.vertices[1].position, [3.0, 4.0, 5.0]);
        assert_eq!(a.triangles, vec![[0, 1, 2]]);
        assert_eq!(
            a.segments,
            vec![ObjectLodSegment {
                flag: 0,
                start_index: 9,
                num_primitives: 1
            }]
        );
        let b = &parsed.shapes[1];
        assert_eq!(b.block_index, 3);
        assert_eq!(b.scale, 1.0);
        assert_eq!(b.triangles, vec![[2, 1, 0], [0, 1, 2]]);
        assert_eq!(b.segments.len(), 2);
        assert_eq!((parsed.triangle_count(), parsed.vertex_count()), (3, 6));

        // The large-reference blocks come back untouched, with their block indices.
        assert!(parsed.shapes[0].large_reference);
        assert!(!parsed.shapes[1].large_reference);
        assert_eq!(parsed.large_ref_blocks.len(), 2);
        assert_eq!(parsed.large_ref_blocks[0].block_index, 0);
        assert_eq!(parsed.large_ref_blocks[0].block_type, LARGE_REF_BLOCK_TYPE);
        assert_eq!(parsed.large_ref_blocks[0].bytes, opaque_a);
        assert_eq!(parsed.large_ref_blocks[1].block_index, 2);
        assert_eq!(parsed.large_ref_blocks[1].bytes, opaque_b);
        // The NIF reader does not decode that block type, and the counts say so.
        assert_eq!(parsed.unhandled_blocks.get(LARGE_REF), Some(&2));
        assert_eq!(parsed.block_types.get(SHAPE), Some(&2));
    }

    #[test]
    fn a_vertex_without_a_position_is_an_error() {
        let shape = shape_block(
            [0.0; 3],
            1.0,
            &[[0.0; 3], [1.0; 3], [2.0; 3]],
            &[],
            &[],
            false,
            false,
        );
        let error = read_object_lod(&nif_bytes(&[(SHAPE, shape)]), label()).unwrap_err();
        assert!(
            format!("{error:#}").contains("without a position"),
            "{error:#}"
        );
    }

    #[test]
    fn a_triangle_index_past_the_vertices_is_an_error() {
        let shape = shape_block(
            [0.0; 3],
            1.0,
            &[[0.0; 3], [1.0; 3], [2.0; 3]],
            &[[0, 1, 3]],
            &[(0, 0, 1)],
            true,
            false,
        );
        let error = read_object_lod(&nif_bytes(&[(SHAPE, shape)]), label()).unwrap_err();
        assert!(
            format!("{error:#}").contains("triangle index past"),
            "{error:#}"
        );
    }

    #[test]
    fn a_file_that_is_not_a_nif_is_an_error() {
        assert!(read_object_lod(b"not a nif", label()).is_err());
    }

    #[test]
    fn the_shape_node_flags_are_read_from_the_node() {
        let verts = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let mut block = shape_block(
            [0.0; 3],
            16.0,
            &verts,
            &[[0, 1, 2]],
            &[(0, 0, 1)],
            true,
            false,
        );
        let file = |block: &[u8]| {
            read_object_lod(&nif_bytes(&[(SHAPE, block.to_vec())]), label()).unwrap()
        };
        assert_eq!(file(&block).shapes[0].flags, 0);
        // The flags word follows name, extra data and controller: offset 12.
        block[12..16].copy_from_slice(&0x108u32.to_le_bytes());
        assert_eq!(file(&block).shapes[0].flags, 0x108);
    }
}
