//! A NIF shape with one non-finite UV keeps its UV attribute after conversion.
//!
//! Retail `WRGuardHouse01` stores one NaN half-float in its 1,095-vertex wall shape. The converter
//! used to drop the whole UV attribute for such a shape, so every vertex sampled one texel and the
//! wall rendered as a flat colour. Only the bad component is zeroed now. See
//! [the NIF spec](../../../docs/specs/converters/nif-to-gltf.md#61-non-finite-uvs).

use converter::mesh::MeshConverter;
use dummy_content::nif::{StaticShape, static_shape};
use std::fs;

#[test]
fn one_nan_uv_does_not_remove_the_uv_attribute() {
    // 0.375 and 0.625 are exact half floats: 0x3600 and 0x3900, little endian `00 36 00 39`.
    let uvs = [[0.0, 0.0], [0.375, 0.625], [0.0, 1.0]];
    let mut bytes = static_shape(&StaticShape {
        name: "wall",
        positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: &[[0.0, 0.0, 1.0]; 3],
        uvs: &uvs,
        indices: &[[0, 1, 2]],
        diffuse: "textures/fix/wall.dds",
        normal_texture: "textures/fix/wall_n.dds",
    })
    .unwrap();
    let pattern = [0x00, 0x36, 0x00, 0x39];
    let at = bytes
        .windows(4)
        .position(|window| window == pattern)
        .expect("the fixture stores vertex 1's UV as two halves");
    assert_eq!(
        bytes.windows(4).filter(|window| *window == pattern).count(),
        1,
        "the patched bytes must be the UV and nothing else"
    );
    // Half-float NaN: exponent all ones, non-zero mantissa. Replace u, keep v.
    bytes[at..at + 2].copy_from_slice(&0x7E00u16.to_le_bytes());

    let directory = tempfile::tempdir().unwrap();
    let nif = directory.path().join("wall.nif");
    let glb = directory.path().join("wall.glb");
    fs::write(&nif, bytes).unwrap();
    MeshConverter::convert_nif_to_glb(&nif, &glb).unwrap();

    let glb = fs::read(&glb).unwrap();
    let json_length = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    let document: serde_json::Value = serde_json::from_slice(&glb[20..20 + json_length]).unwrap();
    let primitive = &document["meshes"][0]["primitives"][0];
    assert!(
        primitive["attributes"]["TEXCOORD_0"].is_u64(),
        "the shape lost its UVs: {primitive}"
    );
}
