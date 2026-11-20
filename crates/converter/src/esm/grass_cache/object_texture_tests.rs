//! Effective STAT MODS visibility and grass-cache input regressions.

use super::tests::{edit_mesh, fixture, read};
use super::*;

/// A reused visible failure stays scoped while a changed hidden variant keeps its healthy shape.
#[test]
fn grass_mods_partial_reuse_keeps_failed_and_healthy_variants_independent() {
    let dir = tempfile::tempdir().unwrap();
    let conn = tagged_fixture(dir.path());
    edit_mesh(dir.path(), |json, _| {
        json["meshes"][0]["primitives"][0]["attributes"]["POSITION"] = 999.into();
    });
    conn.execute_batch(
        "INSERT INTO records(form_id,record_type,load_order,data) VALUES(41,'STAT',0,x'00');
         INSERT INTO statics(id,model_path,flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid) SELECT 41,model_path,flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid FROM statics WHERE id=40;
         INSERT INTO base_texture_swaps(base_id,position,shape_name,shape_index,texture_set_id,disposition) VALUES(41,0,'ignored',3,40,'null_texture_set');
         INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale) VALUES(51,11,1,41,1,4224,64,0,0,0,0,1);",
    ).unwrap();
    let (_, cold) = bake(&conn, dir.path(), None, "cold.rkyv");
    assert_eq!(cold.cells[0].obstruction_failures.len(), 1);
    assert!(cold.cells[1].obstruction_failures.is_empty());
    conn.execute("UPDATE \"references\" SET pos_x=4256 WHERE id=51", [])
        .unwrap();
    let (report, partial) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("cold.rkyv")),
        "partial.rkyv",
    );
    assert_eq!(
        (
            report.baked_cells,
            report.reused_cells,
            report.geometry_decodes
        ),
        (1, 2, 1)
    );
    assert!(partial.cells[1].obstruction_failures.is_empty());
    assert!(
        blocked(&partial.cells[1]) > 0,
        "the healthy sibling remains an obstruction"
    );
    assert_eq!(report.skipped_obstruction_models, 1);
}

/// Add a second independently tagged primitive to the shared blocker model.
fn tagged_fixture(root: &Path) -> Connection {
    let conn = fixture(root);
    edit_mesh(root, |json, _| {
        json["meshes"][0]["primitives"][0]["extras"] =
            serde_json::json!({"mudcrabGeometryIndex":3});
        let mut sibling = json["meshes"][0].clone();
        sibling["primitives"][0]["extras"] = serde_json::json!({"mudcrabGeometryIndex":7});
        json["meshes"].as_array_mut().unwrap().push(sibling);
        json["nodes"] = serde_json::json!([{"mesh":0},{"mesh":1,"translation":[512,0,0]}]);
        json["scenes"][0]["nodes"] = serde_json::json!([0, 1]);
    });
    conn.execute_batch(
        "UPDATE statics SET bounds_max_x=576 WHERE id=40;
         UPDATE \"references\" SET pos_x=128,pos_y=64,scale=1 WHERE id=50;",
    )
    .unwrap();
    conn
}

/// Bake one output using the same publication path as normal and metadata conversion.
fn bake(
    conn: &Connection,
    root: &Path,
    previous: Option<&Path>,
    name: &str,
) -> (GrassBakeReport, GrassCache) {
    let output = root.join(name);
    let report =
        write_grass_cache(conn, root, &root.join("terrain.rkyv"), previous, &output).unwrap();
    (report, read(&output))
}

/// Count blocked centers in all quadrants of a cell.
fn blocked(cell: &GrassCell) -> u32 {
    cell.quadrants
        .iter()
        .flat_map(|q| &q.exclusion)
        .map(|b| b.count_ones())
        .sum()
}

/// Select a literal tile center in the first quadrant of the synthetic cell.
fn tile(cell: &GrassCell, x: usize, y: usize) -> bool {
    let bit = y * 64 + x;
    cell.quadrants[0].exclusion[bit / 8] & (1 << (bit % 8)) != 0
}

/// A NullTextureSet hides only its tagged shape; another base sharing the GLB remains visible.
#[test]
fn grass_mods_hides_only_the_matching_shape_and_keeps_shared_model_neighbors() {
    let dir = tempfile::tempdir().unwrap();
    let conn = tagged_fixture(dir.path());
    conn.execute_batch(
        "INSERT INTO base_texture_swaps(base_id,position,shape_name,shape_index,texture_set_id,disposition) VALUES(40,0,'ignored:99',3,40,'null_texture_set');
         INSERT INTO records(form_id,record_type,load_order,data) VALUES(41,'STAT',0,x'00');
         INSERT INTO statics(id,model_path,flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid) SELECT 41,model_path,flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid FROM statics WHERE id=40;
         INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale) VALUES(51,11,1,41,1,4224,64,0,0,0,0,1);",
    ).unwrap();
    let (report, cache) = bake(&conn, dir.path(), None, "hidden.rkyv");
    assert!(
        !tile(&cache.cells[0], 3, 1),
        "the tagged hidden shape leaves its grass centers clear"
    );
    assert!(
        tile(&cache.cells[0], 19, 1),
        "its visible sibling still obstructs"
    );
    assert!(
        tile(&cache.cells[1], 3, 1),
        "a distinct base keeps the same model's first shape"
    );
    assert_eq!(report.skipped_obstruction_models, 0);
    assert!(report.warnings().is_empty());
}

/// MODS-only hide and reveal updates rebuild affected cells while unrelated cells reuse masks.
#[test]
fn grass_mods_hide_and_reveal_invalidates_only_affected_warm_cells() {
    let dir = tempfile::tempdir().unwrap();
    let conn = tagged_fixture(dir.path());
    let model = dir.path().join("meshes/blocker.glb");
    let original = std::fs::read(&model).unwrap();
    let (_, before) = bake(&conn, dir.path(), None, "before.rkyv");
    conn.execute_batch("INSERT INTO base_texture_swaps(base_id,position,shape_name,shape_index,texture_set_id,disposition) VALUES(40,0,'ignored',3,40,'null_texture_set');").unwrap();
    let (hidden_report, hidden) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("before.rkyv")),
        "hidden.rkyv",
    );
    assert_eq!(
        (hidden_report.baked_cells, hidden_report.reused_cells),
        (1, 2)
    );
    assert_ne!(before.cells[0].input_hash, hidden.cells[0].input_hash);
    assert!(!tile(&hidden.cells[0], 3, 1));
    assert!(tile(&hidden.cells[0], 19, 1));
    assert_eq!(&before.cells[1..], &hidden.cells[1..]);
    conn.execute_batch("UPDATE base_texture_swaps SET disposition='texture_set',texture_set_id=1234 WHERE base_id=40;").unwrap();
    let (revealed_report, revealed) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("hidden.rkyv")),
        "revealed.rkyv",
    );
    assert_eq!(
        (revealed_report.baked_cells, revealed_report.reused_cells),
        (1, 2)
    );
    assert_eq!(revealed, before);
    let (warm, _) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("revealed.rkyv")),
        "warm.rkyv",
    );
    assert_eq!(
        (warm.baked_cells, warm.reused_cells, warm.geometry_decodes),
        (0, 3, 0)
    );
    assert_eq!(
        std::fs::read(model).unwrap(),
        original,
        "visibility filtering leaves the source model immutable"
    );
}

/// Signed and duplicate indices follow runtime ordering; a later missing or texture entry clears a hide.
#[test]
fn grass_mods_uses_last_entry_per_index_and_ignores_negative_or_untagged_indices() {
    let dir = tempfile::tempdir().unwrap();
    let conn = tagged_fixture(dir.path());
    let (_, baseline) = bake(&conn, dir.path(), None, "baseline.rkyv");
    for (earlier, later, hidden) in [
        ("texture_set", "null_texture_set", true),
        ("null_texture_set", "texture_set", false),
        ("null_texture_set", "missing_texture_set", false),
    ] {
        conn.execute_batch("DELETE FROM base_texture_swaps;")
            .unwrap();
        for (position, index, disposition) in [
            (0, 3, earlier),
            (1, 3, later),
            (2, -1, "null_texture_set"),
            (3, 99, "null_texture_set"),
        ] {
            conn.execute("INSERT INTO base_texture_swaps(base_id,position,shape_name,shape_index,texture_set_id,disposition) VALUES(40,?1,'ignored',?2,40,?3)",rusqlite::params![position,index,disposition]).unwrap();
        }
        let (_, cache) = bake(&conn, dir.path(), None, "ordered.rkyv");
        assert_eq!(
            tile(&cache.cells[0], 3, 1),
            !hidden,
            "{earlier} then {later}"
        );
        assert!(tile(&cache.cells[0], 19, 1));
        if !hidden {
            assert_eq!(cache.cells[0].quadrants, baseline.cells[0].quadrants);
        }
    }
}

/// Malformed geometry in a hidden shape is omitted before validation while a healthy sibling survives.
#[test]
fn grass_mods_hidden_bad_primitive_preserves_healthy_shape() {
    let dir = tempfile::tempdir().unwrap();
    let conn = tagged_fixture(dir.path());
    edit_mesh(dir.path(), |json, _| {
        json["meshes"][0]["primitives"][0]["attributes"]["POSITION"] = 999.into();
    });
    conn.execute_batch("INSERT INTO base_texture_swaps(base_id,position,shape_name,shape_index,texture_set_id,disposition) VALUES(40,0,'ignored',3,40,'null_texture_set');").unwrap();
    let (report, cache) = bake(&conn, dir.path(), None, "healthy.rkyv");
    assert_eq!(report.skipped_obstruction_models, 0);
    assert!(!tile(&cache.cells[0], 3, 1));
    assert!(tile(&cache.cells[0], 19, 1));
    assert!(blocked(&cache.cells[0]) > 0);
}

/// Version-nine masks lack effective MODS visibility and must be regenerated.
#[test]
fn grass_mods_rebuilds_version_nine_masks() {
    let dir = tempfile::tempdir().unwrap();
    let conn = tagged_fixture(dir.path());
    let (_, mut old) = bake(&conn, dir.path(), None, "old.rkyv");
    old.version = 9;
    std::fs::write(
        dir.path().join("old.rkyv"),
        rkyv::to_bytes::<Error>(&old).unwrap(),
    )
    .unwrap();
    let (report, _) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("old.rkyv")),
        "new.rkyv",
    );
    assert_eq!((report.baked_cells, report.reused_cells), (3, 0));
}

/// Current runtime material bindings determine whether a geometry tag can be hidden by MODS.
#[test]
fn grass_mods_material_binding_eligibility_matches_published_fields() {
    use serde_json::json;
    for material in [
        json!({}),
        json!({"extras":{"openSkyrim":{"normalConvention":"directx","shaderFlags1":2147483648u64,"shaderFlags2":1}}}),
        json!({"extras":{"openSkyrim":{"normalConvention":"model_space"}}}),
        json!({"extensions":{"OPEN_SKYRIM_material":{"shaderFamily":"effect","shaderFlags1":0}}}),
    ] {
        assert!(
            standard_mods_surface(Some(&material)),
            "standard binding stays eligible: {material}"
        );
    }
    for material in [
        json!({"extras":{"openSkyrim":{"shaderFlags1":2684354560u64,"shaderFlags2":1}}}),
        json!({"extras":{"openSkyrim":{"shaderFlags1":0,"shaderFlags2":1}}}),
        json!({"extras":{"openSkyrim":{"shaderFlags1":2147483648u64,"shaderFlags2":0}}}),
        json!({"extras":{"openSkyrim":{"shaderFlags1":2214592512u64,"shaderFlags2":1}}}),
        json!({"extras":{"openSkyrim":{"normalConvention":"model_space"}},"normalTexture":{"index":0}}),
        json!({"extensions":{"OPEN_SKYRIM_material":{"lightingShaderType":"hair_tint","tintColor":[0.3,0.7,0.2]}}}),
        json!({"extensions":{"OPEN_SKYRIM_material":{"shaderFamily":"effect","shaderFlags1":16,"textureSlots":[{"semantic":"greyscale","texture":0}]}}}),
    ] {
        assert!(
            !standard_mods_surface(Some(&material)),
            "wrapped/external binding stays visible: {material}"
        );
    }
}

/// Out-of-range depth words keep the standard binding and still honor other wrapper tags.
#[test]
fn grass_mods_overflow_depth_flags_fall_through_to_other_bindings() {
    use serde_json::json;
    for (flags1, flags2) in [(4294967296u64, 1u64), (2147483648, 4294967296)] {
        let standard =
            json!({"extras":{"openSkyrim":{"shaderFlags1":flags1,"shaderFlags2":flags2}}});
        assert!(
            standard_mods_surface(Some(&standard)),
            "overflow keeps standard binding: {standard}"
        );
        let mut wrapped = standard;
        wrapped["extras"]["openSkyrim"]["normalConvention"] = json!("model_space");
        wrapped["normalTexture"] = json!({"index":0});
        assert!(
            !standard_mods_surface(Some(&wrapped)),
            "valid normal wrapper remains visible"
        );
    }
    let valid_depth = json!({"extras":{"openSkyrim":{"shaderFlags1":0,"shaderFlags2":1}}});
    assert!(
        !standard_mods_surface(Some(&valid_depth)),
        "valid depth wrapper remains visible"
    );
}

/// Unsupported material bindings keep both their geometry and their effective input identity.
#[test]
fn grass_mods_keeps_external_and_depth_wrapped_shapes_visible() {
    for (flags1, flags2) in [
        (2684354560u64, 1),
        (0, 1),
        (2147483648u64, 0),
        (2214592512u64, 1),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let conn = tagged_fixture(dir.path());
        edit_mesh(dir.path(), |json, _| {
            json["materials"][0]["extras"] =
                serde_json::json!({"openSkyrim":{"shaderFlags1":flags1,"shaderFlags2":flags2}});
        });
        let (_, before) = bake(&conn, dir.path(), None, "before.rkyv");
        conn.execute_batch("INSERT INTO base_texture_swaps(base_id,position,shape_name,shape_index,texture_set_id,disposition) VALUES(40,0,'ignored',3,40,'null_texture_set');").unwrap();
        let (report, after) = bake(
            &conn,
            dir.path(),
            Some(&dir.path().join("before.rkyv")),
            "after.rkyv",
        );
        assert_eq!((report.baked_cells, report.reused_cells), (0, 3));
        assert_eq!(before, after);
        assert!(tile(&after.cells[0], 3, 1));
    }
}

/// Irrelevant material changes and unmatched signed indices leave grass keys reusable.
#[test]
fn grass_mods_ignores_texture_only_and_unmatched_index_changes() {
    let dir = tempfile::tempdir().unwrap();
    let conn = tagged_fixture(dir.path());
    let (_, before) = bake(&conn, dir.path(), None, "before.rkyv");
    conn.execute_batch("INSERT INTO base_texture_swaps(base_id,position,shape_name,shape_index,texture_set_id,disposition) VALUES(40,0,'ignored',3,1234,'texture_set'),(40,1,'ignored',-1,40,'null_texture_set'),(40,2,'ignored',99,40,'null_texture_set');").unwrap();
    let (report, after) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("before.rkyv")),
        "after.rkyv",
    );
    assert_eq!(
        (
            report.baked_cells,
            report.reused_cells,
            report.geometry_decodes
        ),
        (0, 3, 0)
    );
    assert_eq!(before, after);
}
