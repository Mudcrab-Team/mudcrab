//! Independent numeric and malformed-input regressions for skin obstruction decoding.

use super::tests::{add_skin, edit_mesh, write_mesh};
use super::*;

/// Append one tightly packed, aligned fixture accessor to the embedded buffer.
fn skin_attribute(
    json: &mut serde_json::Value,
    bin: &mut Vec<u8>,
    bytes: &[u8],
    component: u32,
    normalized: bool,
) -> usize {
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let offset = bin.len();
    bin.extend_from_slice(bytes);
    let view = json["bufferViews"].as_array().unwrap().len();
    json["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"buffer":0,"byteOffset":offset,"byteLength":bytes.len()}));
    let accessor = json["accessors"].as_array().unwrap().len();
    json["accessors"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "bufferView":view,"componentType":component,"count":3,"type":"VEC4",
            "normalized":normalized
        }));
    json["buffers"][0]["byteLength"] = bin.len().into();
    accessor
}

/// Global joint scale and inverse-bind translation do not commute.
#[test]
fn rest_palette_uses_global_joint_times_inverse_bind_literal_points() {
    let dir = tempfile::tempdir().unwrap();
    write_mesh(
        dir.path(),
        &[[[1.0, 4.0, 6.0], [2.0, 4.0, 6.0], [1.0, 5.0, 6.0]]],
        "OPAQUE",
    );
    add_skin(dir.path(), false);
    edit_mesh(dir.path(), |json, bin| {
        json["nodes"] = serde_json::json!([
            {"mesh":0,"skin":0,"translation":[600,700,800]},
            {"translation":[40,7,11],"children":[3]},
            {}, {"scale":[2,3,1]}
        ]);
        json["scenes"][0]["nodes"] = serde_json::json!([0, 1, 2]);
        let offset = json["bufferViews"][2]["byteOffset"].as_u64().unwrap() as usize;
        for weights in bin[offset..offset + 48].as_chunks_mut::<16>().0 {
            for (slot, value) in weights
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip([1.0f32, 0.0, 0.0, 0.0])
            {
                slot.copy_from_slice(&value.to_le_bytes());
            }
        }
        let offset = json["bufferViews"][3]["byteOffset"].as_u64().unwrap() as usize;
        let bind = [
            1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -5.0, 2.0, -3.0, 1.0,
        ];
        for (slot, value) in bin[offset..offset + 64]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(bind)
        {
            slot.copy_from_slice(&value.to_le_bytes());
        }
    });
    let triangles =
        model_triangles(&std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap()).unwrap();
    assert_eq!(
        triangles,
        vec![[
            DVec3::new(32.0, 25.0, 14.0),
            DVec3::new(34.0, 25.0, 14.0),
            DVec3::new(32.0, 28.0, 14.0),
        ]]
    );
}

/// Five influences across two sets normalize together; omitted binds are identities.
#[test]
fn all_influence_sets_and_normalized_integer_formats_use_identity_bind_defaults() {
    for format in [5126, 5121, 5123] {
        let dir = tempfile::tempdir().unwrap();
        write_mesh(
            dir.path(),
            &[[[1.0, 4.0, 6.0], [2.0, 4.0, 6.0], [1.0, 5.0, 6.0]]],
            "OPAQUE",
        );
        edit_mesh(dir.path(), |json, bin| {
            json["nodes"] = serde_json::json!([
                {"mesh":0,"skin":0,"translation":[600,700,800]},
                {"children":[2,3,4,5,6]}, {}, {},
                {"translation":[192,0,-128]}, {}, {}
            ]);
            json["scenes"][0]["nodes"] = serde_json::json!([0, 1]);
            json["skins"] = serde_json::json!([{"joints":[6,2,5,3,4]}]);
            for (set, joints, weights) in [
                (0, [0u16, 1, 2, 3], [1u16, 2, 1, 1]),
                (1, [4u16, 0, 0, 0], [5u16, 0, 0, 0]),
            ] {
                let joint_component = if format == 5121 { 5121 } else { 5123 };
                let joint_bytes: Vec<u8> = joints
                    .repeat(3)
                    .into_iter()
                    .flat_map(|v| {
                        if joint_component == 5121 {
                            vec![v as u8]
                        } else {
                            v.to_le_bytes().to_vec()
                        }
                    })
                    .collect();
                let weight_bytes: Vec<u8> = weights
                    .repeat(3)
                    .into_iter()
                    .flat_map(|v| match format {
                        5121 => vec![v as u8],
                        5123 => v.to_le_bytes().to_vec(),
                        _ => (f32::from(v) / 10.0).to_le_bytes().to_vec(),
                    })
                    .collect();
                let joints = skin_attribute(json, bin, &joint_bytes, joint_component, false);
                let weights = skin_attribute(json, bin, &weight_bytes, format, format != 5126);
                json["meshes"][0]["primitives"][0]["attributes"][format!("JOINTS_{set}")] =
                    joints.into();
                json["meshes"][0]["primitives"][0]["attributes"][format!("WEIGHTS_{set}")] =
                    weights.into();
            }
        });
        let triangles =
            model_triangles(&std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap())
                .unwrap();
        assert_eq!(triangles.len(), 1);
        for (actual, expected) in triangles[0].iter().zip([
            DVec3::new(97.0, 4.0, -58.0),
            DVec3::new(98.0, 4.0, -58.0),
            DVec3::new(97.0, 5.0, -58.0),
        ]) {
            assert!(
                actual.abs_diff_eq(expected, 1e-5),
                "format {format}: {actual:?}"
            );
        }
    }
}

/// The spec permits more inverse-bind matrices than joints; trailing entries are unused.
#[test]
fn trailing_inverse_bind_matrices_do_not_force_static_fallback() {
    let dir = tempfile::tempdir().unwrap();
    write_mesh(
        dir.path(),
        &[[[1.0, 4.0, 6.0], [2.0, 4.0, 6.0], [1.0, 5.0, 6.0]]],
        "OPAQUE",
    );
    add_skin(dir.path(), false);
    edit_mesh(dir.path(), |json, bin| {
        bin.extend([0u8; 64]);
        json["bufferViews"][3]["byteLength"] = 192.into();
        json["accessors"][3]["count"] = 3.into();
        json["buffers"][0]["byteLength"] = bin.len().into();
    });
    let triangles =
        model_triangles(&std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap()).unwrap();
    assert_eq!(
        triangles,
        vec![[
            DVec3::new(18.0, 15.0, -3.0),
            DVec3::new(19.0, 15.0, -3.0),
            DVec3::new(18.0, 16.0, -3.0),
        ]]
    );
}

/// Invalid POSITION references must return locally even in meshes outside the chosen scene.
#[test]
fn malformed_position_accessor_reference_survives_validation_without_panic() {
    for unreachable in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        write_mesh(
            dir.path(),
            &[[[1.0, 4.0, 6.0], [2.0, 4.0, 6.0], [1.0, 5.0, 6.0]]],
            "OPAQUE",
        );
        add_skin(dir.path(), false);
        edit_mesh(dir.path(), |json, _| {
            if unreachable {
                let mut bad_mesh = json["meshes"][0].clone();
                bad_mesh["primitives"][0]["attributes"]["POSITION"] = 999.into();
                json["meshes"].as_array_mut().unwrap().push(bad_mesh);
            } else {
                json["meshes"][0]["primitives"][0]["attributes"]["POSITION"] = 999.into();
            }
        });
        assert!(
            model_triangles(&std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap(),)
                .is_err(),
            "unreachable mesh: {unreachable}"
        );
    }
}
