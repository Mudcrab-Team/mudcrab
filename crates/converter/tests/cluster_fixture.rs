//! The shared cluster fixture (ADR-0013 section 8): a deterministic `.mcl` of one eligible
//! primitive (an icosphere of 5,120 triangles with a fixed bump pattern) and a `.mci` block of
//! three instances in two cells, committed under `crates/shared/tests/data/cluster/` with their
//! expected hashes. The engine prototype renders these files; this test is the converter's first
//! acceptance test: rebuilding from the same inputs gives the committed bytes.
//!
//! The fixture uses a fixed test identity, not the real builder identity (which changes with the
//! vendored sources), so engine tests must not compare it with a `cluster_build` row.
//!
//! Regenerate with `MUDCRAB_REGENERATE_CLUSTER_FIXTURE=1 cargo test -p converter --test
//! cluster_fixture`. clusterlod is floating-point code and was checked bit-for-bit only on
//! Windows x86_64 (MSVC); other targets check structure instead of bytes.

use converter::cluster::{
    builder::build_mesh_payload,
    model::{AlphaMode, ModelPrimitive, PrimitiveGeometry},
};
use sha2::{Digest, Sha256};
use shared::cluster::{
    InstanceInput, MciInputs, MeshRef, NO_GROUP, hex, read_mci, read_mcl, write_mci,
};
use std::{collections::HashMap, path::PathBuf};

const FIXTURE_IDENTITY_TEXT: &str = "mudcrab cluster fixture identity v1";
const MESH_HASH_TEXT: &str = "mudcrab cluster fixture mesh v1";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../shared/tests/data/cluster")
}

fn sha(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// An icosphere with 4 subdivisions (5,120 triangles) and a bump pattern from integer hashing, so
/// no platform maths library is involved in the input.
fn fixture_primitive() -> ModelPrimitive {
    let t = (1.0 + 5.0f32.sqrt()) / 2.0;
    let mut vertices: Vec<[f32; 3]> = vec![
        [-1.0, t, 0.0],
        [1.0, t, 0.0],
        [-1.0, -t, 0.0],
        [1.0, -t, 0.0],
        [0.0, -1.0, t],
        [0.0, 1.0, t],
        [0.0, -1.0, -t],
        [0.0, 1.0, -t],
        [t, 0.0, -1.0],
        [t, 0.0, 1.0],
        [-t, 0.0, -1.0],
        [-t, 0.0, 1.0],
    ];
    let mut triangles: Vec<[u32; 3]> = vec![
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    let normalise = |v: [f32; 3]| {
        let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / length, v[1] / length, v[2] / length]
    };
    for vertex in &mut vertices {
        *vertex = normalise(*vertex);
    }
    for _ in 0..4 {
        let mut midpoints: HashMap<(u32, u32), u32> = HashMap::new();
        let mut next = Vec::with_capacity(triangles.len() * 4);
        for &[a, b, c] in &triangles {
            let mut middle = |x: u32, y: u32, vertices: &mut Vec<[f32; 3]>| {
                let key = (x.min(y), x.max(y));
                *midpoints.entry(key).or_insert_with(|| {
                    let (p, q) = (vertices[x as usize], vertices[y as usize]);
                    vertices.push(normalise([
                        (p[0] + q[0]) / 2.0,
                        (p[1] + q[1]) / 2.0,
                        (p[2] + q[2]) / 2.0,
                    ]));
                    vertices.len() as u32 - 1
                })
            };
            let (ab, bc, ca) = (
                middle(a, b, &mut vertices),
                middle(b, c, &mut vertices),
                middle(c, a, &mut vertices),
            );
            next.extend([[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
        }
        triangles = next;
    }
    let mut geometry = PrimitiveGeometry::default();
    for (index, vertex) in vertices.iter().enumerate() {
        // Integer hash in 0..8 scaled to a gentle bump, then a 100-unit sphere.
        let bump = ((index as u32).wrapping_mul(2_654_435_761) >> 29) as f32 * 0.004;
        let radius = 100.0 * (1.0 + bump);
        geometry
            .positions
            .push([vertex[0] * radius, vertex[1] * radius, vertex[2] * radius]);
        geometry.normals.push(*vertex);
        geometry
            .uvs
            .push([(vertex[0] * 0.5 + 0.5) * 4.0, (vertex[1] * 0.5 + 0.5) * 4.0]);
    }
    geometry.indices = triangles.iter().flatten().copied().collect();
    let triangle_count = geometry.indices.len() / 3;
    assert_eq!(triangle_count, 5120);
    ModelPrimitive {
        index: 0,
        geometry,
        alpha: AlphaMode::Opaque,
        alpha_cutoff: None,
        double_sided: false,
        material_key: hex(&sha("fixture material")),
        material_index: Some(0),
        bounds: ([-110.0; 3], [110.0; 3]),
        reject: None,
    }
}

fn fixture_instances(mesh_hash: [u8; 32]) -> Vec<u8> {
    let mesh = MeshRef {
        mesh_hash,
        primitive_index: 0,
    };
    let instance = |x: f32, cell_x: i32, cell_y: i32, scale: f32| InstanceInput {
        mesh,
        transform: [
            scale, 0.0, 0.0, x, 0.0, scale, 0.0, 0.0, 0.0, 0.0, scale, 0.0,
        ],
        cell_x,
        cell_y,
        stable_id: [0; 16],
        base_form_id: 0x0001_A2B3,
        snow_material: if cell_x == 0 { 0 } else { 0x0002_5129 },
        source_worldspace_id: if cell_x == 1 { 0x0001_A22F } else { 0 },
        world_min: [x - 110.0 * scale, -110.0 * scale, 0.0],
        world_max: [x + 110.0 * scale, 110.0 * scale, 220.0 * scale],
    };
    write_mci(&MciInputs {
        builder_identity: sha(FIXTURE_IDENTITY_TEXT),
        worldspace_id: 0x3c,
        tier: 16,
        anchor_x: 0,
        anchor_y: 0,
        instances: &[
            instance(5000.0, 1, 0, 1.0),
            instance(9000.0, 2, 0, 2.0),
            instance(1000.0, 0, 0, 1.5),
        ],
    })
    .unwrap()
}

fn build_fixture() -> (Vec<u8>, Vec<u8>) {
    let mesh_hash = sha(MESH_HASH_TEXT);
    let built = build_mesh_payload(&fixture_primitive(), sha(FIXTURE_IDENTITY_TEXT), mesh_hash)
        .unwrap_or_else(|reject| panic!("{}: {}", reject.reason, reject.detail));
    (built.bytes, fixture_instances(mesh_hash))
}

#[test]
fn the_committed_fixture_is_what_the_converter_builds() {
    let (mcl, mci) = build_fixture();
    let dir = fixture_dir();
    let expected = format!(
        "{{\n  \"mcl_sha256\": \"{}\",\n  \"mci_sha256\": \"{}\",\n  \"mcl_bytes\": {},\n  \"mci_bytes\": {}\n}}\n",
        hex(&Sha256::digest(&mcl)),
        hex(&Sha256::digest(&mci)),
        mcl.len(),
        mci.len()
    );
    if std::env::var_os("MUDCRAB_REGENERATE_CLUSTER_FIXTURE").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fixture.mcl"), &mcl).unwrap();
        std::fs::write(dir.join("fixture.mci"), &mci).unwrap();
        std::fs::write(dir.join("expected.json"), &expected).unwrap();
        return;
    }
    let committed_mcl = std::fs::read(dir.join("fixture.mcl")).expect("fixture.mcl is committed");
    let committed_mci = std::fs::read(dir.join("fixture.mci")).expect("fixture.mci is committed");
    // The instance block uses no floating-point library: identical on every platform.
    assert_eq!(mci, committed_mci, "fixture.mci differs");
    let committed = read_mcl(&committed_mcl).expect("the committed payload validates");
    let rebuilt = read_mcl(&mcl).expect("the rebuilt payload validates");
    assert_eq!(rebuilt.cluster_count > 1, committed.cluster_count > 1);
    if cfg!(all(windows, target_arch = "x86_64")) {
        assert_eq!(mcl, committed_mcl, "fixture.mcl differs");
        // A Windows checkout with autocrlf turns the text file's line ends into CRLF.
        assert_eq!(
            std::fs::read_to_string(dir.join("expected.json"))
                .unwrap()
                .replace("\r\n", "\n"),
            expected,
            "expected.json differs"
        );
    }
}

#[test]
fn the_fixture_has_a_real_hierarchy() {
    let (mcl, mci) = build_fixture();
    let view = read_mcl(&mcl).unwrap();
    assert!(
        view.cluster_count > 40 && view.group_count >= 5,
        "{} clusters in {} groups",
        view.cluster_count,
        view.group_count
    );
    assert!((0..view.cluster_count as usize).any(|i| view.cluster(i).refined_group != NO_GROUP));
    assert!(view.root_error > 0.0);
    let block = read_mci(&mci).unwrap();
    assert_eq!((block.instance_count, block.mesh_count), (3, 1));
    let cells: Vec<(i32, i32)> = (0..3)
        .map(|i| (block.instance(i).cell_x, block.instance(i).cell_y))
        .collect();
    assert_eq!(cells, vec![(0, 0), (1, 0), (2, 0)]);
}

#[test]
fn identical_inputs_give_identical_payloads() {
    assert_eq!(build_fixture(), build_fixture());
}
