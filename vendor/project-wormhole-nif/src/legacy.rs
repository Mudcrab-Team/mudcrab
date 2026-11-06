//! Legacy Gamebryo geometry still present in a small number of Skyrim assets.

use project_wormhole_shared::glam::{Mat3, U16Vec3, Vec2, Vec3, Vec4};

use crate::{dev::*, nif_block::NiAVObjectFlags};

#[derive(Debug, Clone)]
pub struct NiTriShape {
    pub name: u32,
    pub flags: NiAVObjectFlags,
    pub translation: Vec3,
    pub rotation: Mat3,
    pub scale: f32,
    pub data: u32,
    pub shader_property: u32,
    pub alpha_property: u32,
}

impl Parse<&[u8]> for NiTriShape {
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, name) = le_u32(i)?;
        let (mut i, extra_data_count) = le_u32(i)?;
        if extra_data_count as usize > i.len() / 4 {
            return invalid(i);
        }
        for _ in 0..extra_data_count {
            (i, _) = le_u32(i)?;
        }
        let (i, _) = le_u32(i)?; // controller
        let (i, flags) = NiAVObjectFlags::parse(i)?;
        let (i, translation) = vec3(i)?;
        let (i, rotation) = matrix3(i)?;
        let (i, scale) = le_f32(i)?;
        let (i, _) = le_u32(i)?;
        let (i, data) = le_u32(i)?;
        let (i, _) = le_u32(i)?;
        let (mut i, material_count) = le_u32(i)?;
        if material_count as usize > i.len() / 8 {
            return invalid(i);
        }
        for _ in 0..material_count {
            (i, _) = le_u32(i)?;
            (i, _) = le_u32(i)?;
        }
        let (i, _) = le_u32(i)?;
        let (i, _) = le_u8(i)?;
        let (i, shader_property) = le_u32(i)?;
        let (i, alpha_property) = le_u32(i)?;
        Ok((
            i,
            Self {
                name,
                flags,
                translation,
                rotation: Mat3::from_cols_array(&rotation),
                scale,
                data,
                shader_property,
                alpha_property,
            },
        ))
    }
}

#[derive(Debug, Clone)]
pub struct NiTriShapeData {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<Vec2>,
    pub colors: Vec<Vec4>,
    pub triangles: Vec<U16Vec3>,
}

impl Parse<&[u8]> for NiTriShapeData {
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, _) = le_u32(i)?;
        let (i, vertex_count) = le_u16(i)?;
        let vertex_count = usize::from(vertex_count);
        let (i, _) = le_u16(i)?; // Bethesda maximum vertex count
        let (mut i, has_vertices) = bool8(i)?;
        if vertex_count > i.len() / 12 {
            return invalid(i);
        }
        let mut positions = Vec::new();
        if has_vertices {
            (i, positions) = count(vec3, vertex_count)(i)?;
        }
        let (i, data_flags) = le_u16(i)?;
        let (i, _) = le_u32(i)?; // Bethesda vector flags
        let (mut i, has_normals) = bool8(i)?;
        let mut normals = Vec::new();
        if has_normals {
            if vertex_count > i.len() / 12 {
                return invalid(i);
            }
            (i, normals) = count(vec3, vertex_count)(i)?;
            if data_flags & 0x1000 != 0 {
                let tangent_bytes = vertex_count.saturating_mul(24);
                if tangent_bytes > i.len() {
                    return invalid(i);
                }
                (i, _) = take(tangent_bytes)(i)?;
            }
        }
        let (i, _) = vec3(i)?;
        let (i, _) = le_f32(i)?;
        let (mut i, has_colors) = bool8(i)?;
        let mut colors = Vec::new();
        if has_colors {
            if vertex_count > i.len() / 16 {
                return invalid(i);
            }
            (i, colors) = count(vec4, vertex_count)(i)?;
        }
        let uv_set_count = usize::from(data_flags & 0x003f);
        let uv_count = vertex_count.saturating_mul(uv_set_count);
        if uv_count > i.len() / 8 {
            return invalid(i);
        }
        let (next, uv_sets) = count(vec2, uv_count)(i)?;
        i = next;
        let uvs = uv_sets.into_iter().take(vertex_count).collect();
        let (i, _) = le_u16(i)?; // consistency flags
        let (i, _) = le_u32(i)?; // additional data
        let (i, triangle_count) = le_u16(i)?;
        let triangle_count = usize::from(triangle_count);
        let (i, triangle_point_count) = le_u32(i)?;
        if triangle_point_count as usize != triangle_count.saturating_mul(3) {
            return invalid(i);
        }
        let (mut i, has_triangles) = bool8(i)?;
        let mut triangles = Vec::new();
        if has_triangles {
            if triangle_count > i.len() / 6 {
                return invalid(i);
            }
            (i, triangles) = count(triangle, triangle_count)(i)?;
        }
        if i.len() >= 2 {
            let (mut rest, group_count) = le_u16(i)?;
            for _ in 0..group_count {
                let (next, count) = le_u16(rest)?;
                (rest, _) = take(usize::from(count).saturating_mul(2))(next)?;
            }
            i = rest;
        }
        if positions.len() != vertex_count
            || triangles.iter().any(|triangle| {
                usize::from(triangle.x) >= vertex_count
                    || usize::from(triangle.y) >= vertex_count
                    || usize::from(triangle.z) >= vertex_count
            })
        {
            return invalid(i);
        }
        Ok((
            i,
            Self {
                positions,
                normals,
                uvs,
                colors,
                triangles,
            },
        ))
    }
}

fn bool8(i: &[u8]) -> IResult<&[u8], bool, nom::error::Error<&[u8]>> {
    let (i, value) = le_u8(i)?;
    if value > 1 {
        return invalid(i);
    }
    Ok((i, value != 0))
}

fn vec2(i: &[u8]) -> IResult<&[u8], Vec2, nom::error::Error<&[u8]>> {
    let (i, x) = le_f32(i)?;
    let (i, y) = le_f32(i)?;
    Ok((i, Vec2::new(x, y)))
}

fn vec3(i: &[u8]) -> IResult<&[u8], Vec3, nom::error::Error<&[u8]>> {
    let (i, x) = le_f32(i)?;
    let (i, y) = le_f32(i)?;
    let (i, z) = le_f32(i)?;
    Ok((i, Vec3::new(x, y, z)))
}

fn vec4(i: &[u8]) -> IResult<&[u8], Vec4, nom::error::Error<&[u8]>> {
    let (i, x) = le_f32(i)?;
    let (i, y) = le_f32(i)?;
    let (i, z) = le_f32(i)?;
    let (i, w) = le_f32(i)?;
    Ok((i, Vec4::new(x, y, z, w)))
}

fn matrix3(i: &[u8]) -> IResult<&[u8], [f32; 9], nom::error::Error<&[u8]>> {
    let mut output = [0.0; 9];
    let mut input = i;
    for value in &mut output {
        (input, *value) = le_f32(input)?;
    }
    Ok((input, output))
}

fn triangle(i: &[u8]) -> IResult<&[u8], U16Vec3, nom::error::Error<&[u8]>> {
    let (i, x) = le_u16(i)?;
    let (i, y) = le_u16(i)?;
    let (i, z) = le_u16(i)?;
    Ok((i, U16Vec3::new(x, y, z)))
}

fn invalid<T>(i: &[u8]) -> IResult<&[u8], T, nom::error::Error<&[u8]>> {
    Err(nom::Err::Failure(nom::error::Error::new(
        i,
        nom::error::ErrorKind::Verify,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_f32(output: &mut Vec<u8>, value: f32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn keeps_the_legacy_shapes_flags() {
        for flags in [0x0008_000E_u32, 0x0008_000F] {
            let mut raw = Vec::new();
            for word in [7, 0, u32::MAX, flags] {
                raw.extend_from_slice(&word.to_le_bytes());
            }
            for value in [
                1.0, 2.0, 3.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0,
            ] {
                push_f32(&mut raw, value);
            }
            // Properties, data, skin, no materials, active material, dirty flag, shader, alpha.
            for word in [0, 4, u32::MAX, 0, u32::MAX] {
                raw.extend_from_slice(&word.to_le_bytes());
            }
            raw.push(0);
            for word in [5_u32, 6] {
                raw.extend_from_slice(&word.to_le_bytes());
            }
            let (remaining, shape) = NiTriShape::parse(&raw).unwrap();
            assert!(remaining.is_empty());
            assert_eq!(shape.flags.hidden(), flags & 1 != 0);
            assert_eq!(
                (shape.translation, shape.data),
                (Vec3::new(1.0, 2.0, 3.0), 4)
            );
            assert_eq!((shape.shader_property, shape.alpha_property), (5, 6));
        }
    }

    #[test]
    fn parses_legacy_triangle_geometry() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&3u16.to_le_bytes());
        raw.extend_from_slice(&0u16.to_le_bytes());
        raw.push(1);
        for position in [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for value in position {
                push_f32(&mut raw, value);
            }
        }
        raw.extend_from_slice(&1u16.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.push(0);
        for _ in 0..4 {
            push_f32(&mut raw, 0.0);
        }
        raw.push(0);
        for uv in [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]] {
            for value in uv {
                push_f32(&mut raw, value);
            }
        }
        raw.extend_from_slice(&0u16.to_le_bytes());
        raw.extend_from_slice(&u32::MAX.to_le_bytes());
        raw.extend_from_slice(&1u16.to_le_bytes());
        raw.extend_from_slice(&3u32.to_le_bytes());
        raw.push(1);
        raw.extend_from_slice(&0u16.to_le_bytes());
        raw.extend_from_slice(&1u16.to_le_bytes());
        raw.extend_from_slice(&2u16.to_le_bytes());
        raw.extend_from_slice(&0u16.to_le_bytes());

        let (remaining, data) = NiTriShapeData::parse(&raw).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(data.positions.len(), 3);
        assert_eq!(data.uvs.len(), 3);
        assert_eq!(data.triangles, vec![U16Vec3::new(0, 1, 2)]);
    }
}
