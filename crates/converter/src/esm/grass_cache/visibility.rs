//! Remove proven invisible primitives before typed glTF validation follows their dependencies.

use super::*;
use serde_json::Value;
use shared::model_visibility::{
    declared_non_rendering, explicit_material_exclusion, mask_cannot_render,
};
use std::{borrow::Cow, collections::BTreeSet};

/// Keep node/joint indices and child transforms, isolating excluded materials from live ones.
/// Errors remain optional per-model omissions at GeometryCache::get, never publication failures.
pub(super) fn prepare(bytes: &[u8]) -> Result<Cow<'_, [u8]>> {
    let mut container = gltf::binary::Glb::from_slice(bytes)?;
    let mut json: Value = serde_json::from_slice(&container.json)?;
    let materials = json
        .get("materials")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut removed_materials = BTreeSet::new();
    let mut live_materials = BTreeSet::new();
    let mut changed = false;
    if let Some(meshes) = json.get_mut("meshes").and_then(Value::as_array_mut) {
        for mesh in meshes {
            if let Some(primitives) = mesh.get_mut("primitives").and_then(Value::as_array_mut) {
                primitives.retain(|primitive| {
                    let index = primitive
                        .get("material")
                        .and_then(Value::as_u64)
                        .and_then(|i| usize::try_from(i).ok());
                    let material = index.and_then(|i| materials.get(i));
                    let excluded = primitive
                        .get("extras")
                        .is_some_and(explicit_material_exclusion)
                        || material.is_some_and(|m| {
                            declared_non_rendering(m)
                                || mask_cannot_render(
                                    m,
                                    primitive.pointer("/attributes/COLOR_0").is_some(),
                                )
                        });
                    if let Some(index) = index {
                        if excluded {
                            removed_materials.insert(index);
                        } else {
                            live_materials.insert(index);
                        }
                    }
                    changed |= excluded;
                    !excluded
                });
            }
        }
    }
    if !changed {
        return Ok(Cow::Borrowed(bytes));
    }
    // gltf-json validates unused materials too. Neutralize only materials whose every use
    // was excluded; a live sibling still owns and validates its original material.
    if let Some(materials) = json.get_mut("materials").and_then(Value::as_array_mut) {
        for index in removed_materials.difference(&live_materials) {
            if let Some(material) = materials.get_mut(*index) {
                *material = serde_json::json!({});
            }
        }
    }
    isolate_unused_dependencies(&mut json);
    container.json = Cow::Owned(serde_json::to_vec(&json)?);
    Ok(Cow::Owned(container.to_vec()?))
}

/// Extensions can own indices outside the core graph. Leave their resources to normal
/// validation (and optional per-model omission) rather than guessing their ownership.
fn has_extensions(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            (key == "extensions" && value.as_object().is_none_or(|v| !v.is_empty()))
                || (key != "extras" && has_extensions(value))
        }),
        Value::Array(values) => values.iter().any(has_extensions),
        _ => false,
    }
}

/// Record an authored index without changing malformed live references or their validation.
fn keep_index(indices: &mut BTreeSet<usize>, value: &Value) {
    if let Some(index) = value.as_u64().and_then(|i| usize::try_from(i).ok()) {
        indices.insert(index);
    }
}

/// Replace unreferenced slots without shifting any retained index. An invalid live entry
/// remains invalid; copying it cannot hide corruption from whole-document validation.
fn neutralize_unused(json: &mut Value, field: &str, live: &BTreeSet<usize>) {
    let Some(entries) = json.get_mut(field).and_then(Value::as_array_mut) else {
        return;
    };
    if live.is_empty() {
        entries.clear();
    } else if let Some(replacement) = live.iter().find_map(|&i| entries.get(i)).cloned() {
        for (index, entry) in entries.iter_mut().enumerate() {
            if !live.contains(&index) {
                *entry = replacement.clone();
            }
        }
    }
}

/// Follow the core glTF graph after exclusions. Keep all remaining materials, all mesh
/// attributes/morphs, skin matrices and animation inputs/outputs, including off-scene users.
/// Nodes, skins, animations, mesh indices and binary payloads are never rewritten.
fn isolate_unused_dependencies(json: &mut Value) {
    if has_extensions(json) {
        return;
    }
    let mut textures = BTreeSet::new();
    for material in json["materials"].as_array().into_iter().flatten() {
        for path in [
            "/pbrMetallicRoughness/baseColorTexture/index",
            "/pbrMetallicRoughness/metallicRoughnessTexture/index",
            "/normalTexture/index",
            "/occlusionTexture/index",
            "/emissiveTexture/index",
        ] {
            if let Some(index) = material.pointer(path) {
                keep_index(&mut textures, index);
            }
        }
    }
    let mut images = BTreeSet::new();
    let mut samplers = BTreeSet::new();
    for &index in &textures {
        keep_index(&mut images, &json["textures"][index]["source"]);
        keep_index(&mut samplers, &json["textures"][index]["sampler"]);
    }
    let mut accessors = BTreeSet::new();
    for mesh in json["meshes"].as_array().into_iter().flatten() {
        for primitive in mesh["primitives"].as_array().into_iter().flatten() {
            keep_index(&mut accessors, &primitive["indices"]);
            for attributes in std::iter::once(&primitive["attributes"])
                .chain(primitive["targets"].as_array().into_iter().flatten())
            {
                for index in attributes.as_object().into_iter().flat_map(|v| v.values()) {
                    keep_index(&mut accessors, index);
                }
            }
        }
    }
    for skin in json["skins"].as_array().into_iter().flatten() {
        keep_index(&mut accessors, &skin["inverseBindMatrices"]);
    }
    for animation in json["animations"].as_array().into_iter().flatten() {
        for sampler in animation["samplers"].as_array().into_iter().flatten() {
            keep_index(&mut accessors, &sampler["input"]);
            keep_index(&mut accessors, &sampler["output"]);
        }
    }
    let mut views = BTreeSet::new();
    for &index in &accessors {
        let accessor = &json["accessors"][index];
        keep_index(&mut views, &accessor["bufferView"]);
        keep_index(&mut views, &accessor["sparse"]["indices"]["bufferView"]);
        keep_index(&mut views, &accessor["sparse"]["values"]["bufferView"]);
    }
    for &index in &images {
        keep_index(&mut views, &json["images"][index]["bufferView"]);
    }
    let mut buffers = BTreeSet::new();
    for &index in &views {
        keep_index(&mut buffers, &json["bufferViews"][index]["buffer"]);
    }
    for (field, live) in [
        ("textures", textures),
        ("images", images),
        ("samplers", samplers),
        ("accessors", accessors),
        ("bufferViews", views),
        ("buffers", buffers),
    ] {
        neutralize_unused(json, field, &live);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{add_skin, edit_mesh, fixture, read, write_mesh};
    use super::*;

    /// A marker parent and translated visible child share healthy resources at nonzero indices.
    /// Only the marker owns the appended malformed resource.
    fn dependency_fixture(root: &Path, bad_accessor: bool) -> Connection {
        let conn = fixture(root);
        conn.execute_batch("UPDATE \"references\" SET pos_x=0,pos_y=0,scale=1 WHERE id=50;
            UPDATE statics SET bounds_min_x=0,bounds_min_y=0,bounds_min_z=-64,bounds_max_x=320,bounds_max_y=32,bounds_max_z=0 WHERE id=40;").unwrap();
        write_mesh(
            root,
            &[[[0.0, 32.0, 0.0], [64.0, 32.0, 0.0], [0.0, 32.0, -64.0]]],
            "OPAQUE",
        );
        edit_mesh(root, |json, _| {
            let position = json["accessors"][0].clone();
            json["accessors"] = serde_json::json!([position, position, position]);
            if bad_accessor {
                json["accessors"][1]["componentType"] = "BAD_MARKER_COMPONENT".into();
            }
            json["textures"] = serde_json::json!([{"source":0,"sampler":0},{"source":999}]);
            json["images"] = serde_json::json!([{"uri":"healthy.png"},{"bufferView":999}]);
            json["samplers"] = serde_json::json!([{}, {"magFilter":"BAD_MARKER_FILTER"}]);
            if bad_accessor {
                // Make the malformed accessor the only invalid dependency in this case.
                json["textures"][1] = serde_json::json!({"source":0});
                json["images"][1] = serde_json::json!({"uri":"unused.png"});
                json["samplers"][1] = serde_json::json!({});
            }
            json["materials"] = serde_json::json!([
                {"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}},
                {"pbrMetallicRoughness":{"baseColorTexture":{"index":if bad_accessor {0} else {1}}}}
            ]);
            json["meshes"] = serde_json::json!([
                {"primitives":[{"attributes":{"POSITION":1},"material":1,"extras":{"openSkyrim":{"materialExclusion":"declared BSX EditorMarker"}}}]},
                {"primitives":[{"attributes":{"POSITION":2},"material":0}]}
            ]);
            json["nodes"] = serde_json::json!([
                {"mesh":0,"translation":[128,0,0],"children":[1]},
                {"mesh":1,"translation":[-128,0,0]}
            ]);
        });
        conn
    }

    /// Exercise both decoding and publication with literal child geometry/mask expectations.
    fn assert_dependency_recovery(bad_accessor: bool) {
        let dir = tempfile::tempdir().unwrap();
        let conn = dependency_fixture(dir.path(), bad_accessor);
        let bytes = std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap();
        let triangles = model_triangles(&bytes).expect("excluded dependencies cannot lose child");
        assert_eq!(triangles.len(), 1);
        assert_eq!(triangles[0][0], DVec3::new(0.0, 32.0, 0.0));
        assert_eq!(triangles[0][1], DVec3::new(64.0, 32.0, 0.0));
        let path = dir.path().join("recovered.rkyv");
        let report = write_grass_cache(
            &conn,
            dir.path(),
            &dir.path().join("terrain.rkyv"),
            None,
            &path,
        )
        .unwrap();
        assert_eq!(report.skipped_obstruction_models, 0);
        let cache = read(&path);
        assert_eq!(cache.cells.len(), 3);
        let mask = &cache.cells[0].quadrants[0].exclusion;
        assert_ne!(mask[0] & 1, 0, "visible child obstructs at (16,16)");
        assert_eq!(mask[0] & (1 << 4), 0, "marker at (144,16) is omitted");
    }

    /// Baseline-compatible red: a marker-only invalid texture must not lose a visible child.
    #[test]
    fn excluded_bad_texture_preserves_child_and_publication() {
        assert_dependency_recovery(false);
    }

    /// Baseline-compatible red: malformed accessor JSON must be isolated before typed decode.
    #[test]
    fn excluded_bad_accessor_preserves_child_and_publication() {
        assert_dependency_recovery(true);
    }

    /// Shared live resources and authored node/accessor identities survive isolation unchanged.
    #[test]
    fn dependency_isolation_preserves_shared_resources_and_indices() {
        let dir = tempfile::tempdir().unwrap();
        dependency_fixture(dir.path(), false);
        edit_mesh(dir.path(), |json, _| {
            json["meshes"][0]["primitives"][0]["attributes"]["POSITION"] = 2.into();
            json["meshes"][0]["primitives"][0]["material"] = 0.into();
        });
        let bytes = std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap();
        let prepared = prepare(&bytes).unwrap();
        let glb = gltf::binary::Glb::from_slice(&prepared).unwrap();
        let json: Value = serde_json::from_slice(&glb.json).unwrap();
        assert_eq!(
            json["meshes"][1]["primitives"][0]["attributes"]["POSITION"],
            2
        );
        assert_eq!(json["accessors"].as_array().unwrap().len(), 3);
        assert_eq!(
            json["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["index"],
            0
        );
        assert_eq!(
            json["textures"][0],
            serde_json::json!({"source":0,"sampler":0})
        );
        assert_eq!(json["images"][0]["uri"], "healthy.png");
        assert_eq!(json["nodes"][0]["children"], serde_json::json!([1]));
        assert_eq!(model_triangles(&prepared).unwrap().len(), 1);
    }

    /// Matrices and joint attributes remain reachable even when no primitive directly names them.
    #[test]
    fn dependency_isolation_preserves_skin_pose() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        add_skin(dir.path(), true);
        let path = dir.path().join("meshes/blocker.glb");
        let before = model_triangles(&std::fs::read(&path).unwrap()).unwrap();
        edit_mesh(dir.path(), |json, _| {
            let mut marker = json["meshes"][0]["primitives"][0].clone();
            marker["extras"] =
                serde_json::json!({"openSkyrim":{"materialExclusion":"declared BSX EditorMarker"}});
            json["meshes"][0]["primitives"]
                .as_array_mut()
                .unwrap()
                .push(marker);
        });
        assert_eq!(
            model_triangles(&std::fs::read(path).unwrap()).unwrap(),
            before
        );
    }

    /// Exclusion recovery never repairs a dependency still owned by visible geometry.
    #[test]
    fn dependency_isolation_keeps_live_corruption_and_unmodified_models() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let path = dir.path().join("meshes/blocker.glb");
        let original = std::fs::read(&path).unwrap();
        assert!(matches!(prepare(&original).unwrap(), Cow::Borrowed(_)));
        dependency_fixture(dir.path(), false);
        edit_mesh(dir.path(), |json, _| {
            json["textures"][0]["source"] = 999.into();
        });
        assert!(model_triangles(&std::fs::read(path).unwrap()).is_err());
    }

    /// Unknown extension links are retained for ordinary validation and per-model recovery.
    #[test]
    fn dependency_isolation_retains_unknown_extensions_and_animation_resources() {
        let mut json = serde_json::json!({
            "accessors":[{"bufferView":1},{"bufferView":0},{"bufferView":2}],
            "bufferViews":[{"buffer":1},{"buffer":0},{"buffer":2}],
            "buffers":[{"byteLength":4},{"byteLength":8},{"byteLength":12}],
            "animations":[{"samplers":[{"input":0,"output":1}]}]
        });
        let before = json.clone();
        isolate_unused_dependencies(&mut json);
        for field in ["accessors", "bufferViews", "buffers"] {
            assert_eq!(json[field][0], before[field][0]);
            assert_eq!(json[field][1], before[field][1]);
        }
        let mut extension = before.clone();
        extension["extensions"] = serde_json::json!({"UNKNOWN_resources":{"accessor":2}});
        let expected = extension.clone();
        isolate_unused_dependencies(&mut extension);
        assert_eq!(extension, expected);
    }
}
