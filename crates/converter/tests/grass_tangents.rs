//! Authored directional frames survive the NIF model and glTF accessor path.
use project_wormhole_nif::{bs::prelude::BSTriShape, nif_file::tri_shape_to_mesh};
use project_wormhole_shared::glam::{Vec3, u8::U8Vec3};

#[test]
fn authored_tangent_handedness_survives_model_conversion() {
    // Use a real parsed synthetic NIF to avoid manufacturing parser defaults.
    let bytes = dummy_content::nif::static_shape(&dummy_content::nif::StaticShape {
        name: "directional grass",
        positions: &[[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 3.0, 0.0]],
        normals: &[[0.0, 0.0, 1.0]; 3],
        uvs: &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        indices: &[[0, 1, 2]],
        diffuse: "textures/grass.dds",
        normal_texture: "textures/directional.dds",
    })
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("frame.nif");
    std::fs::write(&path, bytes).unwrap();
    let nif = project_wormhole_nif::nif_file::NifFile::open(path.to_str().unwrap()).unwrap();
    let mut shape: BSTriShape = nif
        .blocks
        .iter()
        .find_map(|block| match block {
            project_wormhole_nif::nif_block::NifBlock::BSTriShape(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap();
    for vertex in &mut shape.vertex_data {
        vertex.normal = Some(Vec3::Z);
        vertex.tangent = Some(U8Vec3::new(255, 128, 128));
        vertex.bitangent_x = Some(0.0);
        vertex.bitangent_y = Some(0.0); // negative Y: mirrored frame
        vertex.bitangent_z = Some(128.0);
    }
    let mesh = tri_shape_to_mesh(&shape, Some("grass".into()));
    assert_eq!(mesh.tangents.len(), 3);
    assert!(mesh.tangents.iter().all(|t| t.x > 0.99 && t.w == -1.0));
    let mut model = project_wormhole_nif::nif_file::nif_to_static_model(&nif).unwrap();
    model.static_meshes[0] = mesh;
    let (json, bin) = model.to_gltf("test".into()).unwrap();
    let json = serde_json::to_vec(&json).unwrap();
    let document = gltf::Gltf::from_slice(&json).unwrap();
    let primitive = document
        .meshes()
        .next()
        .unwrap()
        .primitives()
        .next()
        .unwrap();
    let reader = primitive.reader(|_| Some(bin.as_slice()));
    let tangents: Vec<_> = reader.read_tangents().unwrap().collect();
    assert_eq!(tangents.len(), 3);
    assert!(tangents.iter().all(|t| t[0] > 0.99 && t[3] == -1.0));
}
