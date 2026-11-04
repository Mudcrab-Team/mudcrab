//! Public NIF conversion retains enough authored data for a directional UV frame.
use converter::mesh::MeshConverter;
use project_wormhole_shared::glam::Vec3;

#[test]
fn converted_tangent_frame_follows_mirrored_uv_axes() {
    for (flip_v, expected_sign) in [(false, 1.0), (true, -1.0)] {
        let mut uvs = [[0.2, -0.1], [1.7, 0.6], [0.8, -1.2]];
        if flip_v {
            for uv in &mut uvs {
                uv[1] = -uv[1];
            }
        }
        let bytes = dummy_content::nif::static_shape_with_colors(
            &dummy_content::nif::StaticShape {
                name: "directional grass",
                positions: &[[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 3.0, 0.0]],
                normals: &[[0.0, 0.0, 1.0]; 3],
                uvs: &uvs,
                indices: &[[0, 1, 2]],
                diffuse: "textures/grass.dds",
                normal_texture: "textures/directional.dds",
            },
            &[[255, 128, 64, 0], [64, 255, 128, 64], [128, 64, 255, 255]],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("frame.nif");
        let output = dir.path().join("frame.glb");
        std::fs::write(&input, bytes).unwrap();
        MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
        let bytes = std::fs::read(&output).unwrap();
        let document = gltf::Gltf::from_slice(&bytes).unwrap();
        let blob = document.blob.as_deref().unwrap();
        let primitive = document
            .meshes()
            .next()
            .unwrap()
            .primitives()
            .next()
            .unwrap();
        assert!(
            primitive.material().normal_texture().is_some(),
            "normal reference must survive conversion"
        );
        let reader = primitive.reader(|_| Some(blob));
        let tangents: Vec<_> = reader.read_tangents().unwrap().collect();
        assert_eq!(tangents.len(), 3);
        // For the literal asymmetric UV triangle, U follows (2.2,2.1,0).
        // Half-float UV storage introduces a bounded small angular difference.
        let expected = Vec3::new(2.2, 2.1, 0.0).normalize();
        for tangent in tangents {
            assert!(
                Vec3::from_array([tangent[0], tangent[1], tangent[2]]).distance(expected) < 0.002
            );
            assert_eq!(tangent[3], expected_sign);
        }
        let rgba: Vec<_> = reader.read_colors(0).unwrap().into_rgba_f32().collect();
        assert_eq!(
            rgba.iter().map(|v| v[3]).collect::<Vec<_>>(),
            vec![0.0, 64.0 / 255.0, 1.0]
        );
    }
}
