//! Ordinary authored-shape regression: segment tables cannot multiply primitives.
use converter::mesh::MeshConverter;
use dummy_content::nif::{StaticShape, static_shape, static_shape_with_segment_table};
use serde_json::Value;
use std::fs;

const DIFFUSE: &str = "textures/generated/shape.dds";
const NORMAL: &str = "textures/generated/shape_n.dds";

/// Six unshared triangles with three nonempty authored segment entries.
fn segmented_shape<'a>(diffuse: &'a str, normal: &'a str) -> StaticShape<'a> {
    StaticShape {
        name: "GeneratedSegmentedShape",
        positions: &[
            [0.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [4.0, 4.0, 0.0],
            [10.0, 0.0, 0.0],
            [14.0, 0.0, 0.0],
            [14.0, 4.0, 0.0],
            [20.0, 0.0, 0.0],
            [24.0, 0.0, 0.0],
            [24.0, 4.0, 0.0],
            [30.0, 0.0, 0.0],
            [34.0, 0.0, 0.0],
            [34.0, 4.0, 0.0],
            [40.0, 0.0, 0.0],
            [44.0, 0.0, 0.0],
            [44.0, 4.0, 0.0],
            [50.0, 0.0, 0.0],
            [54.0, 0.0, 0.0],
            [54.0, 4.0, 0.0],
        ],
        normals: &[[0.0, 0.0, 1.0]; 18],
        uvs: &[[0.0, 0.0]; 18],
        indices: &[
            [0, 1, 2],
            [3, 4, 5],
            [6, 7, 8],
            [9, 10, 11],
            [12, 13, 14],
            [15, 16, 17],
        ],
        diffuse,
        normal_texture: normal,
    }
}

/// Authored segment entries for [`segmented_shape`]. The ordinary converter must ignore
/// these partitions and export the complete shape once.
const SEGMENTED_COUNTS: [u32; 16] = [1, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3];

fn accessor_indices(bytes: &[u8], document: &Value, accessor_index: u64) -> Vec<u16> {
    let accessor = &document["accessors"][accessor_index as usize];
    assert_eq!(
        accessor["componentType"], 5123,
        "expected a u16 index accessor"
    );
    let view = &document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let count = accessor["count"].as_u64().unwrap() as usize;
    let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let binary = 20 + json_length.next_multiple_of(4) + 8;
    let start = binary
        + view["byteOffset"].as_u64().unwrap_or(0) as usize
        + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
    let mut values = Vec::with_capacity(count);
    for chunk in bytes[start..start + count * 2].as_chunks::<2>().0 {
        values.push(u16::from_le_bytes(*chunk));
    }
    values
}

fn glb_json(bytes: &[u8]) -> Value {
    assert_eq!(&bytes[..4], b"glTF", "invalid GLB signature");
    assert_eq!(&bytes[16..20], b"JSON", "missing GLB JSON chunk");
    let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    serde_json::from_slice(&bytes[20..20 + json_length]).expect("invalid glTF JSON")
}

#[test]
fn ordinary_segment_table_preserves_one_primitive_and_every_triangle() {
    let directory = tempfile::tempdir().unwrap();
    let shape = segmented_shape(DIFFUSE, NORMAL);
    let input = directory.path().join("segmented.nif");
    let output = directory.path().join("segmented.glb");
    fs::write(
        &input,
        static_shape_with_segment_table(&shape, &SEGMENTED_COUNTS).unwrap(),
    )
    .unwrap();
    MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
    let bytes = fs::read(&output).unwrap();
    let document = glb_json(&bytes);
    let primitives = document["meshes"][0]["primitives"].as_array().unwrap();
    assert_eq!(primitives.len(), 1);
    assert!(
        primitives[0]
            .pointer("/extras/openSkyrim/lodSegment")
            .is_none()
    );
    let indices = accessor_indices(
        &bytes,
        &document,
        primitives[0]["indices"].as_u64().unwrap(),
    );
    assert_eq!(indices, (0u16..18).collect::<Vec<_>>());
    let unsplit_input = directory.path().join("unsplit.nif");
    let unsplit_output = directory.path().join("unsplit.glb");
    fs::write(&unsplit_input, static_shape(&shape).unwrap()).unwrap();
    MeshConverter::convert_nif_to_glb(&unsplit_input, &unsplit_output).unwrap();
    let bounds = MeshConverter::glb_bounds(&output).unwrap();
    let unsplit_bounds = MeshConverter::glb_bounds(&unsplit_output).unwrap();
    assert_eq!(bounds.min, unsplit_bounds.min);
    assert_eq!(bounds.max, unsplit_bounds.max);
}
