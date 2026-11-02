use project_wormhole_ba2::dev::Bounds;
use project_wormhole_shared::glam::{self, U16Vec3, U8Vec4, Vec2, Vec3, Vec4};

use super::prelude::*;

#[derive(Debug, Clone)]
pub struct BSTriShape {
    pub av: NiAVObject,
    pub bounding_sphere: Bounds,
    pub skin: u32,
    pub shader_property: u32,
    pub alpha_property: u32,
    pub vertex_desc: BSVertexDesc,
    pub num_triangles: u32,
    pub num_vertices: u16,
    pub data_size: u32,
    pub vertex_data: Vec<BSVertexData>,
    pub triangles: Vec<U16Vec3>,
}

/// Skyrim Special Edition appends full-precision dynamic positions to the
/// regular BSTriShape payload.
#[derive(Debug, Clone)]
pub struct BSDynamicTriShape {
    pub bs_tri_shape: BSTriShape,
    pub dynamic_data_size: u32,
    pub dynamic_vertices: Vec<Vec4>,
}

/// Skyrim LOD geometry uses the ordinary triangle payload followed by the
/// triangle counts for its three distance levels.
#[derive(Debug, Clone)]
pub struct BSLODTriShape {
    pub bs_tri_shape: BSTriShape,
    pub lod_sizes: [u32; 3],
}

#[derive(Debug, Clone)]
pub struct NiSkinInstance {
    pub data: u32,
    pub skin_partition: u32,
    pub skeleton_root: u32,
    pub bones: Vec<u32>,
}

impl Parse<&[u8]> for NiSkinInstance {
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, data) = le_u32(i)?;
        let (i, skin_partition) = le_u32(i)?;
        let (i, skeleton_root) = le_u32(i)?;
        let (mut i, bone_count) = le_u32(i)?;
        if bone_count as usize > i.len() / 4 {
            return Err(nom::Err::Failure(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Count,
            )));
        }
        let mut bones = Vec::with_capacity(bone_count as usize);
        for _ in 0..bone_count {
            let (next, bone) = le_u32(i)?;
            i = next;
            bones.push(bone);
        }
        Ok((
            i,
            Self {
                data,
                skin_partition,
                skeleton_root,
                bones,
            },
        ))
    }
}

/// `BSDismemberSkinInstance` extends `NiSkinInstance` with one
/// `BodyPartList` entry per partition (a `BSPartFlag` followed by a
/// `BSDismemberBodyPartType`). The entries are stored in both halves so a
/// round-trip keeps the body part alongside its flag.
#[derive(Debug, Clone)]
pub struct BsDismemberSkinInstance {
    pub skin_instance: NiSkinInstance,
    /// `BSPartFlag` for each partition, in partition order.
    pub body_part_flags: Vec<i16>,
    /// `BSDismemberBodyPartType` for each partition, in partition order.
    pub body_parts: Vec<i16>,
}

impl Parse<&[u8]> for BsDismemberSkinInstance {
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, skin_instance) = NiSkinInstance::parse(i)?;
        let (i, partition_count) = le_u32(i)?;
        // A broken partition reference must not discard the rest of the block.
        // The count is self-declared, so validate it against what is actually
        // present and degrade to an empty list instead of failing the file.
        if partition_count as usize > i.len() / 4 {
            warn!(
                "BSDismemberSkinInstance declares {} partitions but only {} bytes remain",
                partition_count,
                i.len()
            );
            return Ok((
                i,
                Self {
                    skin_instance,
                    body_part_flags: Vec::new(),
                    body_parts: Vec::new(),
                },
            ));
        }
        let mut body_part_flags = Vec::with_capacity(partition_count as usize);
        let mut body_parts = Vec::with_capacity(partition_count as usize);
        let mut rest = i;
        for _ in 0..partition_count {
            let (next, flag) = le_u16(rest)?;
            let (next, body_part) = le_u16(next)?;
            rest = next;
            body_part_flags.push(flag as i16);
            body_parts.push(body_part as i16);
        }
        Ok((
            rest,
            Self {
                skin_instance,
                body_part_flags,
                body_parts,
            },
        ))
    }
}

/// A Gamebryo `NiTransform`: row-major rotation, translation, uniform scale.
///
/// This is the bind transform stored by `NiSkinData` and by every
/// `NiSkinDataBoneData` (as the bone's inverse bind).
#[derive(Debug, Clone, Copy)]
pub struct NiTransform {
    pub rotation: BSMatrix3,
    pub translation: BSVec3,
    pub scale: f32,
}

impl Parse<&[u8]> for NiTransform {
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, rotation) = BSMatrix3::parse(i)?;
        let (i, translation) = BSVec3::parse(i)?;
        let (i, scale) = le_f32(i)?;
        Ok((
            i,
            Self {
                rotation,
                translation,
                scale,
            },
        ))
    }
}

impl NiTransform {
    /// Same convention as `BSSkinBoneTrans::into_matrix4`, with the uniform
    /// scale applied to the rotation basis (translation · rotation · scale).
    pub fn into_matrix4(&self) -> BSMatrix4 {
        let mut m4 = glam::Mat4::from_mat3(self.rotation.0 * self.scale);
        m4.w_axis = glam::Vec4::new(
            self.translation.0.x,
            self.translation.0.y,
            self.translation.0.z,
            1.0,
        );
        BSMatrix4(m4)
    }
}

/// One bone entry of a `NiSkinData` block.
#[derive(Debug, Clone)]
pub struct NiSkinDataBoneData {
    /// Offset of the skin from this bone in bind position (the inverse bind).
    pub skin_transform: NiTransform,
    pub bounding_sphere: Bounds,
    pub vertex_count: u16,
    /// Mirrors the parent `NiSkinData` flag; controls `vertex_weights`.
    pub has_vertex_weights: bool,
    /// `(vertex index, weight)` pairs when `has_vertex_weights` is set.
    pub vertex_weights: Vec<(u16, f32)>,
}

impl NiSkinDataBoneData {
    pub fn parse_with_weights(
        i: &[u8],
        has_vertex_weights: bool,
    ) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, skin_transform) = NiTransform::parse(i)?;
        let (i, bounding_sphere) = Bounds::parse(i)?;
        let (i, vertex_count) = le_u16(i)?;
        let mut vertex_weights = Vec::new();
        let rest = if has_vertex_weights {
            let needed = usize::from(vertex_count).saturating_mul(6);
            if needed > i.len() {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    i,
                    nom::error::ErrorKind::Count,
                )));
            }
            vertex_weights.reserve(usize::from(vertex_count));
            let mut cursor = i;
            for _ in 0..vertex_count {
                let (next, index) = le_u16(cursor)?;
                let (next, weight) = le_f32(next)?;
                cursor = next;
                vertex_weights.push((index, weight));
            }
            cursor
        } else {
            i
        };
        Ok((
            rest,
            Self {
                skin_transform,
                bounding_sphere,
                vertex_count,
                has_vertex_weights,
                vertex_weights,
            },
        ))
    }
}

/// `NiSkinData`: the skin transform plus one inverse bind per bone.
#[derive(Debug, Clone)]
pub struct NiSkinData {
    pub skin_transform: NiTransform,
    pub has_vertex_weights: bool,
    pub bone_count: u32,
    pub bones: Vec<NiSkinDataBoneData>,
}

impl Parse<&[u8]> for NiSkinData {
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, skin_transform) = NiTransform::parse(i)?;
        let (i, bone_count) = le_u32(i)?;
        // Smallest possible BoneData is the 52-byte transform, 16-byte bound
        // and 2-byte vertex count. Keep a corrupt count from reserving memory.
        const MIN_BONE_BYTES: usize = 52 + 16 + 2;
        if bone_count as usize > i.len() / MIN_BONE_BYTES {
            return Err(nom::Err::Failure(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Count,
            )));
        }
        let (i, has_vertex_weights) = le_u8(i)?;
        let has_vertex_weights = has_vertex_weights != 0;
        let mut bones = Vec::with_capacity(bone_count as usize);
        let mut rest = i;
        for _ in 0..bone_count {
            let (next, bone) = NiSkinDataBoneData::parse_with_weights(rest, has_vertex_weights)?;
            rest = next;
            bones.push(bone);
        }
        Ok((
            rest,
            Self {
                skin_transform,
                has_vertex_weights,
                bone_count,
                bones,
            },
        ))
    }
}

/// The data one partition of a `NiSkinPartition` contributes.
#[derive(Debug, Clone, Default)]
struct ParsedPartition {
    bone_palette: Vec<u16>,
    weights_per_vertex: u16,
    weights: Vec<Vec<f32>>,
    bone_indices: Vec<Vec<u8>>,
    vertex_map: Option<Vec<u16>>,
    partition_flags: u16,
}

#[derive(Debug, Clone)]
pub struct NiSkinPartition {
    pub vertex_desc: BSVertexDesc,
    pub vertex_data: Vec<BSVertexData>,
    pub triangles: Vec<U16Vec3>,
    pub partition_count: u32,
    /// Bone indices for the merged partitions. Later partitions whose palette
    /// differs are remapped into this palette so every `bone_indices` entry
    /// addresses `bone_palette`.
    pub bone_palette: Vec<u16>,
    pub weights_per_vertex: u16,
    /// Per vertex (across all partitions, in file order), per influence.
    pub weights: Vec<Vec<f32>>,
    /// Per vertex (across all partitions, in file order), per influence.
    pub bone_indices: Vec<Vec<u8>>,
    /// Concatenated vertex maps; `None` if any partition omitted its map.
    pub vertex_map: Option<Vec<u16>>,
    /// Whether partitions disagreed on having a vertex map, so the maps that some
    /// of them carried were dropped from `vertex_map`. Code that needs to know
    /// which vertices any partition references cannot rely on `vertex_map` then.
    pub vertex_map_discarded: bool,
    pub partition_flags: u16,
    /// Whether a later partition's palette could not be folded into the merged palette with
    /// `u8` indices, so its bone indices still address its own palette.
    pub palette_overflow: bool,
}

impl NiSkinPartition {
    fn empty(
        vertex_desc: BSVertexDesc,
        vertex_data: Vec<BSVertexData>,
        triangles: Vec<U16Vec3>,
        partition_count: u32,
    ) -> Self {
        Self {
            vertex_desc,
            vertex_data,
            triangles,
            partition_count,
            bone_palette: Vec::new(),
            weights_per_vertex: 0,
            weights: Vec::new(),
            bone_indices: Vec::new(),
            vertex_map: None,
            vertex_map_discarded: false,
            partition_flags: 0,
            palette_overflow: false,
        }
    }

    /// Merges one partition into the block-level view. The retail actor meshes
    /// repeat the same palette in every partition; a differing palette is
    /// folded into the merged one so the flat bone indices stay meaningful.
    fn absorb(&mut self, mut part: ParsedPartition, first: bool) {
        if first {
            self.bone_palette = part.bone_palette.clone();
            self.weights_per_vertex = part.weights_per_vertex;
            self.partition_flags = part.partition_flags;
            self.vertex_map = part.vertex_map.take();
        } else {
            if part.bone_palette != self.bone_palette {
                let mut remap = Vec::with_capacity(part.bone_palette.len());
                let mut representable = true;
                for bone in &part.bone_palette {
                    let position = match self.bone_palette.iter().position(|value| value == bone) {
                        Some(position) => position,
                        None => {
                            self.bone_palette.push(*bone);
                            self.bone_palette.len() - 1
                        }
                    };
                    match u8::try_from(position) {
                        Ok(position) => remap.push(position),
                        Err(_) => {
                            representable = false;
                            remap.push(0);
                        }
                    }
                }
                if representable {
                    for indices in &mut part.bone_indices {
                        for index in indices.iter_mut() {
                            if let Some(mapped) = remap.get(usize::from(*index)) {
                                *index = *mapped;
                            }
                        }
                    }
                } else {
                    self.palette_overflow = true;
                    warn!(
                        "NiSkinPartition palette exceeds 255 bones; later partition bone indices left partition-local"
                    );
                }
            }
            match (part.vertex_map, self.vertex_map.as_mut()) {
                (Some(map), Some(existing)) => existing.extend(map),
                (None, None) => {}
                // Mixed presence: the merged map cannot describe every partition.
                _ => {
                    self.vertex_map = None;
                    self.vertex_map_discarded = true;
                }
            }
        }
        self.weights.append(&mut part.weights);
        self.bone_indices.append(&mut part.bone_indices);
    }

    /// Legacy Skyrim stores geometry in NiTriShapeData, not an SSE vertex buffer.
    pub fn parse_legacy(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (mut i, partition_count) = le_u32(i)?;
        if partition_count as usize > i.len() / 16 {
            return Err(nom::Err::Failure(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Count,
            )));
        }
        let (_, vertex_desc) = BSVertexDesc::parse(&[0; 8])?;
        let mut result = Self::empty(vertex_desc, Vec::new(), Vec::new(), partition_count);
        for partition_index in 0..partition_count {
            let (next, num_vertices) = le_u16(i)?;
            let (next, num_triangles) = le_u16(next)?;
            let (next, num_bones) = le_u16(next)?;
            let (next, num_strips) = le_u16(next)?;
            let (next, weights_per_vertex) = le_u16(next)?;
            let bone_bytes = usize::from(num_bones).saturating_mul(2);
            if bone_bytes > next.len() {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    next,
                    nom::error::ErrorKind::Count,
                )));
            }
            let (next, raw_palette) = take(bone_bytes)(next)?;
            let (mut next, has_vertex_map) = le_u8(next)?;
            let vertex_map = if has_vertex_map != 0 {
                let map_bytes = usize::from(num_vertices).saturating_mul(2);
                if map_bytes > next.len() {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        next,
                        nom::error::ErrorKind::Count,
                    )));
                }
                let (rest, raw) = take(map_bytes)(next)?;
                next = rest;
                Some(read_u16s(raw))
            } else {
                None
            };
            let (rest, has_weights) = le_u8(next)?;
            next = rest;
            let weights = if has_weights != 0 {
                let count =
                    usize::from(num_vertices).saturating_mul(usize::from(weights_per_vertex));
                let weight_bytes = count.saturating_mul(4);
                if weight_bytes > next.len() {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        next,
                        nom::error::ErrorKind::Count,
                    )));
                }
                let (rest, raw) = take(weight_bytes)(next)?;
                next = rest;
                read_weight_rows(raw, num_vertices, weights_per_vertex)
            } else {
                Vec::new()
            };
            let strip_bytes = usize::from(num_strips).saturating_mul(2);
            if strip_bytes > next.len() {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    next,
                    nom::error::ErrorKind::Count,
                )));
            }
            let (rest, strip_lengths) = take(strip_bytes)(next)?;
            next = rest;
            let (rest, has_faces) = le_u8(next)?;
            next = rest;
            if has_faces != 0 {
                if num_strips == 0 {
                    for _ in 0..num_triangles {
                        let (rest, mut triangle) = parse_u16_vec3(next)?;
                        next = rest;
                        for index in 0..3 {
                            if triangle[index] >= num_vertices {
                                return Err(nom::Err::Failure(nom::error::Error::new(
                                    next,
                                    nom::error::ErrorKind::Verify,
                                )));
                            }
                            if let Some(map) = vertex_map.as_deref() {
                                triangle[index] = map[usize::from(triangle[index])];
                            }
                        }
                        result.triangles.push(triangle);
                    }
                } else {
                    // Legacy render geometry already carries its triangles in NiTriShapeData.
                    for length in strip_lengths.chunks_exact(2) {
                        let length = u16::from_le_bytes([length[0], length[1]]);
                        let bytes = usize::from(length).saturating_mul(2);
                        if bytes > next.len() {
                            return Err(nom::Err::Failure(nom::error::Error::new(
                                next,
                                nom::error::ErrorKind::Count,
                            )));
                        }
                        (next, _) = take(bytes)(next)?;
                    }
                }
            }
            let (rest, has_bone_indices) = le_u8(next)?;
            next = rest;
            let bone_indices = if has_bone_indices != 0 {
                let count =
                    usize::from(num_vertices).saturating_mul(usize::from(weights_per_vertex));
                if count > next.len() {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        next,
                        nom::error::ErrorKind::Count,
                    )));
                }
                let (rest, raw) = take(count)(next)?;
                next = rest;
                read_index_rows(raw, num_vertices, weights_per_vertex)
            } else {
                Vec::new()
            };
            let (rest, partition_flags) = le_u16(next)?; // Bethesda partition flags
            next = rest;
            result.absorb(
                ParsedPartition {
                    bone_palette: read_u16s(raw_palette),
                    weights_per_vertex,
                    weights,
                    bone_indices,
                    vertex_map,
                    partition_flags,
                },
                partition_index == 0,
            );
            i = next;
        }
        Ok((i, result))
    }
}

impl Parse<&[u8]> for NiSkinPartition {
    /// Parses an SSE skin partition, validating vertex-buffer sizes before reading vertices and triangles.
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, partition_count) = le_u32(i)?;
        let (i, data_size) = le_u32(i)?;
        let (i, vertex_size) = le_u32(i)?;
        let (mut i, vertex_desc) = BSVertexDesc::parse(i)?;
        if vertex_size == 0
            || data_size % vertex_size != 0
            || data_size as usize > i.len()
            || vertex_size as usize != usize::from(vertex_desc.vertex_data_size) * 4
        {
            return Err(nom::Err::Failure(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Verify,
            )));
        }
        let vertex_count = (data_size / vertex_size) as usize;
        let mut vertex_data = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            let (next, raw) = take(vertex_size as usize)(i)?;
            let (_, vertex) = BSVertexData::parse_with_flags(raw, &vertex_desc)?;
            i = next;
            vertex_data.push(vertex);
        }

        let mut result = Self::empty(vertex_desc, vertex_data, Vec::new(), partition_count);
        for partition_index in 0..partition_count {
            let (next, num_vertices) = le_u16(i)?;
            let (next, num_triangles) = le_u16(next)?;
            let (next, num_bones) = le_u16(next)?;
            let (next, num_strips) = le_u16(next)?;
            let (next, weights_per_vertex) = le_u16(next)?;
            let bone_bytes = usize::from(num_bones).saturating_mul(2);
            if bone_bytes > next.len() {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    next,
                    nom::error::ErrorKind::Count,
                )));
            }
            let (next, raw_palette) = take(bone_bytes)(next)?;
            let (mut next, has_vertex_map) = le_u8(next)?;
            let vertex_map = if has_vertex_map != 0 {
                let map_bytes = usize::from(num_vertices).saturating_mul(2);
                if map_bytes > next.len() {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        next,
                        nom::error::ErrorKind::Count,
                    )));
                }
                let (rest, raw) = take(map_bytes)(next)?;
                next = rest;
                Some(read_u16s(raw))
            } else {
                None
            };
            let (rest, has_weights) = le_u8(next)?;
            next = rest;
            let weights = if has_weights != 0 {
                let count =
                    usize::from(num_vertices).saturating_mul(usize::from(weights_per_vertex));
                let weight_bytes = count.saturating_mul(4);
                if weight_bytes > next.len() {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        next,
                        nom::error::ErrorKind::Count,
                    )));
                }
                let (rest, raw) = take(weight_bytes)(next)?;
                next = rest;
                read_weight_rows(raw, num_vertices, weights_per_vertex)
            } else {
                Vec::new()
            };
            let strip_bytes = usize::from(num_strips).saturating_mul(2);
            if strip_bytes > next.len() {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    next,
                    nom::error::ErrorKind::Count,
                )));
            }
            let (rest, raw_strips) = take(strip_bytes)(next)?;
            next = rest;
            let strip_lengths = read_u16s(raw_strips);
            let (rest, has_faces) = le_u8(next)?;
            next = rest;
            if has_faces != 0 {
                if num_strips == 0 {
                    let bytes = usize::from(num_triangles).saturating_mul(6);
                    if bytes > next.len() {
                        return Err(nom::Err::Failure(nom::error::Error::new(
                            next,
                            nom::error::ErrorKind::Count,
                        )));
                    }
                    (next, _) = take(bytes)(next)?;
                } else {
                    for length in strip_lengths {
                        let bytes = usize::from(length).saturating_mul(2);
                        if bytes > next.len() {
                            return Err(nom::Err::Failure(nom::error::Error::new(
                                next,
                                nom::error::ErrorKind::Count,
                            )));
                        }
                        (next, _) = take(bytes)(next)?;
                    }
                }
            }
            let (rest, has_bone_indices) = le_u8(next)?;
            next = rest;
            let bone_indices = if has_bone_indices != 0 {
                let count =
                    usize::from(num_vertices).saturating_mul(usize::from(weights_per_vertex));
                if count > next.len() {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        next,
                        nom::error::ErrorKind::Count,
                    )));
                }
                let (rest, raw) = take(count)(next)?;
                next = rest;
                read_index_rows(raw, num_vertices, weights_per_vertex)
            } else {
                Vec::new()
            };
            (next, _) = le_u8(next)?; // LOD level
            (next, _) = le_u8(next)?; // global vertex buffer
            (next, _) = BSVertexDesc::parse(next)?;
            let triangle_bytes = usize::from(num_triangles).saturating_mul(6);
            if triangle_bytes > next.len() {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    next,
                    nom::error::ErrorKind::Count,
                )));
            }
            for _ in 0..num_triangles {
                let (rest, triangle) = parse_u16_vec3(next)?;
                next = rest;
                result.triangles.push(triangle);
            }
            result.absorb(
                ParsedPartition {
                    bone_palette: read_u16s(raw_palette),
                    weights_per_vertex,
                    weights,
                    bone_indices,
                    vertex_map,
                    partition_flags: 0,
                },
                partition_index == 0,
            );
            i = next;
        }
        Ok((i, result))
    }
}

fn read_u16s(raw: &[u8]) -> Vec<u16> {
    raw.chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect()
}

fn read_weight_rows(raw: &[u8], num_vertices: u16, weights_per_vertex: u16) -> Vec<Vec<f32>> {
    let width = usize::from(weights_per_vertex);
    if width == 0 {
        return Vec::new();
    }
    raw.chunks_exact(width * 4)
        .take(usize::from(num_vertices))
        .map(|row| {
            row.chunks_exact(4)
                .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                .collect()
        })
        .collect()
}

fn read_index_rows(raw: &[u8], num_vertices: u16, weights_per_vertex: u16) -> Vec<Vec<u8>> {
    let width = usize::from(weights_per_vertex);
    if width == 0 {
        return Vec::new();
    }
    raw.chunks_exact(width)
        .take(usize::from(num_vertices))
        .map(|row| row.to_vec())
        .collect()
}

impl Parse<&[u8]> for BSLODTriShape {
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, bs_tri_shape) = BSTriShape::parse(i)?;
        let (i, lod0) = le_u32(i)?;
        let (i, lod1) = le_u32(i)?;
        let (i, lod2) = le_u32(i)?;
        let total = lod0.saturating_add(lod1).saturating_add(lod2);
        if total > bs_tri_shape.num_triangles {
            return Err(nom::Err::Failure(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Verify,
            )));
        }
        Ok((
            i,
            Self {
                bs_tri_shape,
                lod_sizes: [lod0, lod1, lod2],
            },
        ))
    }
}

impl Parse<&[u8]> for BSDynamicTriShape {
    /// Parses an SSE dynamic triangle shape and applies its full-precision vertex positions.
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        // BSDynamicTriShape extends the ordinary SSE BSTriShape payload; its
        // dynamic vertex array follows the base payload directly.
        let (i, mut bs_tri_shape) = parse_tri_shape(i, 0)?;
        let (mut data, dynamic_data_size) = le_u32(i)?;
        let vertex_count = dynamic_data_size as usize / 16;
        if dynamic_data_size as usize % 16 != 0
            || dynamic_data_size as usize > data.len()
            || vertex_count != bs_tri_shape.num_vertices as usize
        {
            return Err(nom::Err::Failure(nom::error::Error::new(
                data,
                nom::error::ErrorKind::Verify,
            )));
        }
        let mut dynamic_vertices = Vec::with_capacity(vertex_count);
        for vertex in &mut bs_tri_shape.vertex_data {
            let (i, x) = le_f32(data)?;
            let (i, y) = le_f32(i)?;
            let (i, z) = le_f32(i)?;
            let (i, w) = le_f32(i)?;
            data = i;
            let position = Vec4::new(x, y, z, w);
            vertex.position = Some(position.truncate());
            dynamic_vertices.push(position);
        }
        Ok((
            data,
            Self {
                bs_tri_shape,
                dynamic_data_size,
                dynamic_vertices,
            },
        ))
    }
}

impl Parse<&[u8]> for BSTriShape {
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        // Most SSE files use NiBound (16 bytes), while a sizeable official
        // subset carries the additional 8-byte min/max extension. Select the
        // layout by validating the declared geometry payload size.
        parse_tri_shape(i, 0).or_else(|_| parse_tri_shape(i, 8))
    }
}

/// Parses a triangle shape with the given bounds extension and validates its declared geometry size.
fn parse_tri_shape(
    i: &[u8],
    bound_extension_size: usize,
) -> IResult<&[u8], BSTriShape, nom::error::Error<&[u8]>> {
    let (i, av) = NiAVObject::parse(i)?;
    let (i, bounding_sphere) = Bounds::parse(i)?;
    let (i, _) = take(bound_extension_size)(i)?;
    let (i, skin) = le_u32(i)?;
    let (i, shader_property) = le_u32(i)?;
    let (i, alpha_property) = le_u32(i)?;
    let (i, vertex_desc) = BSVertexDesc::parse(i)?;
    let (i, num_triangles) = le_u16(i)?;
    let num_triangles = u32::from(num_triangles);
    let (i, num_vertices) = le_u16(i)?;
    let (i, data_size) = le_u32(i)?;
    let expected_data_size = usize::from(vertex_desc.vertex_data_size)
        .saturating_mul(4)
        .saturating_mul(usize::from(num_vertices))
        .saturating_add((num_triangles as usize).saturating_mul(6));
    if data_size > 0 && data_size as usize != expected_data_size {
        return Err(nom::Err::Failure(nom::error::Error::new(
            i,
            nom::error::ErrorKind::Verify,
        )));
    }
    let mut vertex_data = Vec::new();
    let mut data = i;

    let triangles = if data_size == 0 {
        vertex_data.resize_with(num_vertices as usize, BSVertexData::default);
        Vec::new()
    } else {
        let vertex_stride = usize::from(vertex_desc.vertex_data_size).saturating_mul(4);
        for _index in 0..num_vertices {
            let (i, raw_vertex) = take(vertex_stride)(data)?;
            let (_, bsdata) = BSVertexData::parse_with_flags(raw_vertex, &vertex_desc)?;
            data = i;
            vertex_data.push(bsdata);
        }
        let (i, triangles) = count(parse_u16_vec3, num_triangles as usize)(data)?;
        data = i;
        triangles
    };

    // SSE stores a secondary particle payload after ordinary geometry. Its
    // size is expressed as a count of 16-bit values.
    let data = if data.len() >= 4 {
        let (data, particle_data_size) = le_u32(data)?;
        let particle_bytes = (particle_data_size as usize).saturating_mul(2);
        let (data, _) = take(particle_bytes)(data)?;
        data
    } else {
        data
    };

    Ok((
        data,
        BSTriShape {
            av,
            bounding_sphere,
            skin,
            shader_property,
            alpha_property,
            vertex_desc,
            num_triangles,
            num_vertices,
            data_size,
            vertex_data,
            triangles,
        },
    ))
}

impl BSTriShape {
    // Get all indices from the triangles
    pub fn get_indices(&self) -> Vec<u16> {
        // Initialize indices vector
        let mut indices = Vec::new();

        // Iterate through all triangles
        for triangle in &self.triangles {
            // Push the indices to the vector
            indices.push(triangle.x);
            indices.push(triangle.y);
            indices.push(triangle.z);
        }

        // Return the indices
        indices
    }

    pub fn get_indices_as_bytes(&self) -> Vec<u8> {
        let mut indices = Vec::new();
        for index in self.get_indices() {
            indices.extend_from_slice(&index.to_le_bytes());
        }
        indices
    }

    // Get the min and max vertices from the vertex data
    pub fn get_vertices_min_max(&self) -> (Vec3, Vec3) {
        // Initialize min and max to 0.0
        let mut min = Vec3::ZERO;
        let mut max = Vec3::ZERO;

        // Iterate through all vertices
        for vertex in &self.vertex_data {
            // If the vertex has a half vertex
            if let Some(v) = &vertex.position {
                // If min and max are still 0.0, set them to the first value
                if min.x == 0.0 && max.x == 0.0 {
                    let val = v.x;
                    min.x = val;
                    max.x = val;
                }

                // If min and max are still 0.0, set them to the first value
                if min.y == 0.0 && max.y == 0.0 {
                    let val = v.y;
                    min.y = val;
                    max.y = val;
                }

                // If min and max are still 0.0, set them to the first value
                if min.z == 0.0 && max.z == 0.0 {
                    let val = v.z;
                    min.z = val;
                    max.z = val;
                }

                // If the value is less than the min, set the min to the value
                if v.x < min.x {
                    min.x = v.x;
                }

                // If the value is less than the min, set the min to the value
                if v.y < min.y {
                    min.y = v.y;
                }

                // If the value is less than the min, set the min to the value
                if v.z < min.z {
                    min.z = v.z;
                }

                // If the value is greater than the max, set the max to the value
                if v.x > max.x {
                    max.x = v.x;
                }

                // If the value is greater than the max, set the max to the value
                if v.y > max.y {
                    max.y = v.y;
                }

                // If the value is greater than the max, set the max to the value
                if v.z > max.z {
                    max.z = v.z;
                }
            }
        }

        // Return the min and max
        (min, max)
    }

    // Get all vertices from the vertex data, converting them to f32 if they are f16
    pub fn get_vertices(&self) -> Vec<f32> {
        // Initialize vertices vector
        let mut vertices = Vec::new();

        // Iterate through all vertices
        for vertex in &self.vertex_data {
            // If the vertex has a half vertex
            if let Some(v) = &vertex.position {
                vertices.push(v.x);
                vertices.push(v.y);
                vertices.push(v.z);
            }
            // If the vertex has a full vertex
            else if let Some(v) = &vertex.position {
                vertices.push(v.x);
                vertices.push(v.y);
                vertices.push(v.z);
            }
        }

        // Return the vertices
        vertices
    }

    pub fn get_vertices_as_bytes(&self) -> Vec<u8> {
        let mut vertices = Vec::new();
        for vertex in self.get_vertices() {
            vertices.extend_from_slice(&vertex.to_le_bytes());
        }
        vertices
    }

    // Get all normals from the vertex data, if they exist
    pub fn get_normals(&self) -> Vec<f32> {
        // Initialize normals vector
        let mut normals = Vec::new();

        // Iterate through all vertices
        for vertex in &self.vertex_data {
            // If the vertex has a normal (it should)
            if let Some(v) = &vertex.normal {
                normals.push(v.x);
                normals.push(v.y);
                normals.push(v.z);
            }
        }

        // Return the normals vector
        normals
    }

    pub fn get_normals_as_bytes(&self) -> Vec<u8> {
        let mut normals = Vec::new();
        for normal in self.get_normals() {
            normals.extend_from_slice(&normal.to_le_bytes());
        }
        normals
    }

    // Get all UVs from the vertex data, if they exist
    pub fn get_uvs(&self) -> Vec<f32> {
        // Initialize uvs vector
        let mut uvs = Vec::new();

        // Iterate through all vertices
        for vertex in &self.vertex_data {
            // If the vertex has a uv, push it to the vector
            if let Some(v) = &vertex.uv {
                uvs.push(v.x);
                uvs.push(v.y);
            }
        }

        // Return the uvs vector
        uvs
    }

    pub fn get_uvs_as_bytes(&self) -> Vec<u8> {
        let mut uvs = Vec::new();
        for uv in self.get_uvs() {
            uvs.extend_from_slice(&uv.to_le_bytes());
        }
        uvs
    }

    pub fn get_bone_weights(&self) -> Vec<f32> {
        let mut weights = Vec::new();
        for vertex in &self.vertex_data {
            if let Some(bone_weights) = &vertex.bone_weights {
                let lin: [f32; 4] = bone_weights.normalize().into();
                weights.extend_from_slice(lin.as_slice());
            }
        }

        weights
    }

    pub fn get_bone_weights_as_bytes(&self) -> Vec<u8> {
        let mut weights = Vec::new();
        for weight in self.get_bone_weights() {
            weights.extend_from_slice(&weight.to_le_bytes());
        }
        weights
    }

    pub fn get_bone_indices(&self) -> Vec<&U8Vec4> {
        let mut indices = Vec::new();
        for vertex in &self.vertex_data {
            if let Some(bone_indices) = &vertex.bone_indices {
                indices.push(bone_indices);
            }
        }

        indices
    }

    pub fn get_bone_indices_as_bytes(&self) -> Vec<u8> {
        let mut indices = Vec::new();
        for index in self.get_bone_indices() {
            let lin: [u8; 4] = index.clone().into();
            indices.extend_from_slice(lin.as_slice());
        }
        indices
    }

    pub fn get_vertex_positions(&self) -> Vec<Vec3> {
        let mut positions = Vec::new();
        for vertex in &self.vertex_data {
            if let Some(v) = vertex.position {
                positions.push(v);
            }
        }

        positions
    }

    pub fn get_vertex_normals(&self) -> Vec<Vec3> {
        let mut normals = Vec::new();
        for vertex in &self.vertex_data {
            if let Some(n) = vertex.normal {
                normals.push(n);
            }
        }

        normals
    }

    pub fn get_triangle_indices(&self) -> Vec<U16Vec3> {
        let halves = self.triangles.clone();
        halves
            .iter()
            .map(|x| U16Vec3 {
                x: x.x,
                y: x.y,
                z: x.z,
            })
            .collect::<Vec<U16Vec3>>()
    }

    pub fn get_vertex_uvs(&self) -> Vec<Vec2> {
        let mut uvs = Vec::new();
        for vertex in &self.vertex_data {
            if let Some(v) = vertex.uv {
                uvs.push(v);
            }
        }

        uvs
    }

    pub fn get_vertex_weights(&self) -> Vec<Vec4> {
        let mut weights = Vec::new();
        for vertex in &self.vertex_data {
            if let Some(w) = vertex.bone_weights {
                weights.push(w.normalize());
            }
        }

        weights
    }

    pub fn get_vertex_joints(&self) -> Vec<U8Vec4> {
        let mut joints = Vec::new();
        for vertex in &self.vertex_data {
            if let Some(j) = vertex.bone_indices {
                joints.push(j);
            }
        }

        joints
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(vertex_map: Option<Vec<u16>>) -> ParsedPartition {
        ParsedPartition {
            bone_palette: vec![0],
            weights_per_vertex: 4,
            weights: Vec::new(),
            bone_indices: Vec::new(),
            vertex_map,
            partition_flags: 0,
        }
    }

    fn merged(maps: [Option<Vec<u16>>; 2]) -> NiSkinPartition {
        let (_, desc) = BSVertexDesc::parse(&0u64.to_le_bytes()).unwrap();
        let mut partition = NiSkinPartition::empty(desc, Vec::new(), Vec::new(), 2);
        let [first, second] = maps;
        partition.absorb(parsed(first), true);
        partition.absorb(parsed(second), false);
        partition
    }

    #[test]
    fn absorb_records_a_vertex_map_dropped_by_mixed_presence() {
        for maps in [[None, Some(vec![1])], [Some(vec![0]), None]] {
            let partition = merged(maps);
            assert_eq!(partition.vertex_map, None);
            assert!(partition.vertex_map_discarded);
        }
        let both = merged([Some(vec![0]), Some(vec![1])]);
        assert_eq!(both.vertex_map, Some(vec![0, 1]));
        assert!(!both.vertex_map_discarded);
        assert!(!merged([None, None]).vertex_map_discarded);
    }

    #[test]
    fn particle_size_counts_u16_values_and_preserves_following_data() {
        for particle_size in [0u32, 3] {
            // Counted NiAVObject with an identity transform and no extra data.
            let mut fixture = Vec::new();
            for value in [0u32, 0, u32::MAX, 0] {
                fixture.extend_from_slice(&value.to_le_bytes());
            }
            for value in [
                0.0f32, 0.0, 0.0, // translation
                1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, // rotation
                1.0, // scale
            ] {
                fixture.extend_from_slice(&value.to_le_bytes());
            }
            fixture.extend_from_slice(&u32::MAX.to_le_bytes()); // collision object
            fixture.extend_from_slice(&[0; 16]); // bounding sphere

            for _ in 0..3 {
                fixture.extend_from_slice(&u32::MAX.to_le_bytes());
            }
            fixture.extend_from_slice(&0u64.to_le_bytes()); // vertex descriptor
            fixture.extend_from_slice(&0u16.to_le_bytes()); // triangles
            fixture.extend_from_slice(&0u16.to_le_bytes()); // vertices
            fixture.extend_from_slice(&0u32.to_le_bytes()); // geometry size
            fixture.extend_from_slice(&particle_size.to_le_bytes());
            let particle_start = fixture.len();
            fixture.extend_from_slice(
                &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66][..particle_size as usize * 2],
            );
            let particle_end = fixture.len();
            fixture.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);

            let (rest, shape) = BSTriShape::parse(&fixture).unwrap();
            assert_eq!(rest, &[0xaa, 0xbb, 0xcc, 0xdd]);
            assert_eq!(shape.data_size, 0);
            assert!(shape.vertex_data.is_empty());
            assert!(shape.triangles.is_empty());
            assert!(BSTriShape::parse(&fixture[..particle_end])
                .unwrap()
                .0
                .is_empty());
            for end in particle_start..particle_end {
                assert!(BSTriShape::parse(&fixture[..end]).is_err());
            }
        }
    }
}

#[cfg(test)]
mod skin_tests {
    use super::*;
    use crate::nif_block::NifBlock;

    fn push_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_f32(out: &mut Vec<u8>, value: f32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_transform(out: &mut Vec<u8>, translation: [f32; 3], scale: f32) {
        for value in [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
            push_f32(out, value);
        }
        for value in translation {
            push_f32(out, value);
        }
        push_f32(out, scale);
    }

    fn partition_body(
        out: &mut Vec<u8>,
        palette: &[u16],
        vertex_map: &[u16],
        weights: &[[f32; 2]],
        bone_indices: &[[u8; 2]],
    ) {
        push_u16(out, weights.len() as u16);
        push_u16(out, 0);
        push_u16(out, palette.len() as u16);
        push_u16(out, 0);
        push_u16(out, 2);
        for bone in palette {
            push_u16(out, *bone);
        }
        out.push(1);
        for index in vertex_map {
            push_u16(out, *index);
        }
        out.push(1);
        for row in weights {
            for weight in row {
                push_f32(out, *weight);
            }
        }
        out.push(0);
        out.push(1);
        for row in bone_indices {
            out.extend_from_slice(row);
        }
    }

    #[test]
    fn legacy_partition_retains_palette_weights_indices_and_map() {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, 1);
        partition_body(
            &mut bytes,
            &[3, 7],
            &[0, 1],
            &[[0.25, 0.75], [0.5, 0.5]],
            &[[0, 1], [1, 0]],
        );
        push_u16(&mut bytes, 0x0101);

        let (rest, partition) = NiSkinPartition::parse_legacy(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(partition.partition_count, 1);
        assert_eq!(partition.bone_palette, vec![3, 7]);
        assert_eq!(partition.weights_per_vertex, 2);
        assert_eq!(partition.weights, vec![vec![0.25, 0.75], vec![0.5, 0.5]]);
        assert_eq!(partition.bone_indices, vec![vec![0, 1], vec![1, 0]]);
        assert_eq!(partition.vertex_map, Some(vec![0, 1]));
        assert_eq!(partition.partition_flags, 0x0101);
        assert!(partition.triangles.is_empty());
    }

    #[test]
    fn sse_partition_retains_palette_weights_indices_and_map() {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, 1);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, 4);
        bytes.extend_from_slice(&1u64.to_le_bytes());
        partition_body(
            &mut bytes,
            &[3, 7],
            &[0, 1],
            &[[0.25, 0.75], [0.5, 0.5]],
            &[[0, 1], [1, 0]],
        );
        bytes.push(0);
        bytes.push(0);
        bytes.extend_from_slice(&1u64.to_le_bytes());

        let (rest, partition) = NiSkinPartition::parse(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(partition.partition_count, 1);
        assert_eq!(partition.bone_palette, vec![3, 7]);
        assert_eq!(partition.weights_per_vertex, 2);
        assert_eq!(partition.weights, vec![vec![0.25, 0.75], vec![0.5, 0.5]]);
        assert_eq!(partition.bone_indices, vec![vec![0, 1], vec![1, 0]]);
        assert_eq!(partition.vertex_map, Some(vec![0, 1]));
        assert_eq!(partition.partition_flags, 0);
        assert!(partition.vertex_data.is_empty());
    }

    #[test]
    fn legacy_partitions_merging_past_255_bones_flag_the_overflow() {
        let first: Vec<u16> = (0..200).collect();
        let second: Vec<u16> = (150..300).collect();
        let mut bytes = Vec::new();
        push_u32(&mut bytes, 2);
        for palette in [&first, &second] {
            partition_body(&mut bytes, palette, &[], &[], &[]);
            push_u16(&mut bytes, 0);
        }

        let (rest, partition) = NiSkinPartition::parse_legacy(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(partition.bone_palette.len(), 300);
        assert!(partition.palette_overflow);
    }

    #[test]
    fn skin_data_round_trips_header_and_bones() {
        let mut bytes = Vec::new();
        push_transform(&mut bytes, [1.0, 2.0, 3.0], 1.0);
        push_u32(&mut bytes, 2);
        bytes.push(1);

        push_transform(&mut bytes, [4.0, 5.0, 6.0], 1.0);
        for value in [0.5f32, 0.25, 0.125, 2.5] {
            push_f32(&mut bytes, value);
        }
        push_u16(&mut bytes, 2);
        push_u16(&mut bytes, 0);
        push_f32(&mut bytes, 0.25);
        push_u16(&mut bytes, 3);
        push_f32(&mut bytes, 0.75);

        push_transform(&mut bytes, [-1.0, -2.0, -3.0], 0.5);
        for value in [0.0f32, 0.0, 0.0, 0.0] {
            push_f32(&mut bytes, value);
        }
        push_u16(&mut bytes, 0);

        let (rest, data) = NiSkinData::parse(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(data.bone_count, 2);
        assert!(data.has_vertex_weights);
        assert_eq!(
            data.skin_transform.translation.0,
            glam::Vec3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(data.bones.len(), 2);
        assert_eq!(
            data.bones[0].skin_transform.translation.0,
            glam::Vec3::new(4.0, 5.0, 6.0)
        );
        assert_eq!(data.bones[0].bounding_sphere.center, [0.5, 0.25, 0.125]);
        assert_eq!(data.bones[0].bounding_sphere.radius, 2.5);
        assert_eq!(data.bones[0].vertex_count, 2);
        assert_eq!(data.bones[0].vertex_weights, vec![(0, 0.25), (3, 0.75)]);
        assert_eq!(data.bones[1].skin_transform.scale, 0.5);
        assert!(data.bones[1].vertex_weights.is_empty());
        assert_eq!(
            data.bones[0].skin_transform.into_matrix4().0.w_axis,
            glam::Vec4::new(4.0, 5.0, 6.0, 1.0)
        );

        assert!(NiSkinData::parse(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn skin_data_without_vertex_weights_does_not_read_pairs() {
        let mut bytes = Vec::new();
        push_transform(&mut bytes, [0.0, 0.0, 0.0], 1.0);
        push_u32(&mut bytes, 1);
        bytes.push(0);
        push_transform(&mut bytes, [0.0, 0.0, 0.0], 1.0);
        for value in [0.0f32, 0.0, 0.0, 0.0] {
            push_f32(&mut bytes, value);
        }
        push_u16(&mut bytes, 4);

        let (rest, data) = NiSkinData::parse(&bytes).unwrap();
        assert!(rest.is_empty());
        assert!(!data.has_vertex_weights);
        assert_eq!(data.bones[0].vertex_count, 4);
        assert!(data.bones[0].vertex_weights.is_empty());
    }

    fn dismember_bytes(partition_count: u32, entries: &[(i16, i16)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, 1);
        push_u32(&mut bytes, 2);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, 2);
        push_u32(&mut bytes, 4);
        push_u32(&mut bytes, 5);
        push_u32(&mut bytes, partition_count);
        for (flag, part) in entries {
            push_u16(&mut bytes, *flag as u16);
            push_u16(&mut bytes, *part as u16);
        }
        bytes
    }

    #[test]
    fn dismember_instance_round_trips_the_trailing_body_parts() {
        let bytes = dismember_bytes(2, &[(257, 32), (1, 38)]);
        let (rest, instance) = BsDismemberSkinInstance::parse(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(instance.skin_instance.bones, vec![4, 5]);
        assert_eq!(instance.body_part_flags, vec![257, 1]);
        assert_eq!(instance.body_parts, vec![32, 38]);

        let (_, block) = NifBlock::parse(&bytes, "BSDismemberSkinInstance".to_string()).unwrap();
        match block {
            NifBlock::BsDismemberSkinInstance(parsed) => {
                assert_eq!(parsed.body_part_flags, vec![257, 1]);
            }
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn dismember_instance_degrades_on_an_implausible_partition_count() {
        let mut bytes = dismember_bytes(0, &[]);
        bytes.truncate(bytes.len() - 4);
        push_u32(&mut bytes, u32::MAX);
        let tail = [0xaa, 0xbb];
        bytes.extend_from_slice(&tail);

        let (rest, instance) = BsDismemberSkinInstance::parse(&bytes).unwrap();
        assert_eq!(rest, &tail);
        assert!(instance.body_part_flags.is_empty());
        assert!(instance.body_parts.is_empty());
        assert_eq!(instance.skin_instance.bones, vec![4, 5]);
    }
}
