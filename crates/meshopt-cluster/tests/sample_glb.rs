//! Runs the builder on a generated GLB with no private files or game data.

use meshopt_cluster::{Config, Mesh, build};

type Primitive = (Vec<[f32; 3]>, Vec<u32>);

fn load_primitives(bytes: &[u8]) -> Vec<Primitive> {
    // Decode the generated geometry buffers without adding image-loader dependencies.
    let gltf::Gltf { document, blob } = gltf::Gltf::from_slice(bytes).expect("gltf open");
    let buffers = gltf::import_buffers(&document, None, blob).expect("gltf buffers");
    let mut out = Vec::new();
    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            let reader = primitive.reader(|b| Some(buffers[b.index()].0.as_slice()));
            let positions: Vec<[f32; 3]> = reader.read_positions().expect("POSITION").collect();
            let indices: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };
            out.push((positions, indices));
        }
    }
    out
}

/// An indexed, nonplanar 32 x 32 grid exercises multiple clusters and simplification.
/// The GLB is assembled here so the integration also exercises glTF buffer/accessor decoding.
fn generated_glb() -> Vec<u8> {
    let mut bin = Vec::new();
    for y in 0..=32 {
        for x in 0..=32 {
            for value in [x as f32, y as f32, ((x * y) as f32 * 0.03).sin()] {
                bin.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    let position_bytes = bin.len();
    for y in 0..32_u32 {
        for x in 0..32_u32 {
            let a = y * 33 + x;
            for index in [a, a + 1, a + 33, a + 1, a + 34, a + 33] {
                bin.extend_from_slice(&index.to_le_bytes());
            }
        }
    }
    let mut json = format!(
        r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":{}}}],
        "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{}}},{{"buffer":0,"byteOffset":{},"byteLength":{}}}],
        "accessors":[{{"bufferView":0,"componentType":5126,"count":1089,"type":"VEC3","min":[0,0,-1],"max":[32,32,1]}},
        {{"bufferView":1,"componentType":5125,"count":6144,"type":"SCALAR"}}],
        "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1}}]}}]}}"#,
        bin.len(), position_bytes, position_bytes, bin.len() - position_bytes,
    ).into_bytes();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut glb = Vec::new();
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2_u32.to_le_bytes());
    glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin);
    glb
}

#[test]
fn sample_glb_hierarchy_invariants() {
    let primitives = load_primitives(&generated_glb());
    assert_eq!(primitives.len(), 1);
    assert_eq!(primitives[0].1.len() / 3, 2048);
    let input_triangles: usize = primitives.iter().map(|(_, i)| i.len() / 3).sum();

    for (label, config) in [("classic", Config::classic(128)), ("rt", Config::rt(128))] {
        let mut groups = 0usize;
        let mut max_depth = 0u32;
        let mut source_triangles = 0usize;

        for (positions, indices) in &primitives {
            let mesh = Mesh {
                positions,
                indices,
                attributes: None,
                vertex_lock: None,
            };

            let h = build(&mesh, config).expect("build");
            groups += h.groups().len();
            max_depth = max_depth.max(h.groups().iter().map(|g| g.depth).max().unwrap_or(0));
            for c in h.clusters() {
                assert!(c.triangle_count() <= 128);
                if c.refined.is_none() {
                    source_triangles += c.triangle_count();
                }
            }
        }

        assert!(groups > 1, "{label}: fixture must build a hierarchy");
        assert!(max_depth > 0, "{label}: fixture must simplify");
        assert_eq!(
            source_triangles, input_triangles,
            "{label}: source triangles not all covered"
        );
    }
}
