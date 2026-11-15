//! Placement visibility, authored species order and old-cache regressions.

use super::tests::{edit_mesh, fixture, read, write_mesh};
use super::*;

/// Existing databases gain the ordinal column before a subset replaces one texture's list.
#[test]
fn placement_migrates_existing_associations_before_partial_export() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE landscape_texture_grasses(ltex_id INTEGER NOT NULL,gras_id INTEGER NOT NULL,PRIMARY KEY(ltex_id,gras_id));
        INSERT INTO landscape_texture_grasses VALUES(31,99);").unwrap();
    crate::esm::exporter::create_tables(&conn).unwrap();
    assert_eq!(conn.query_row("SELECT count(*) FROM pragma_table_info('landscape_texture_grasses') WHERE name='ordinal'", [], |row| row.get::<_,u32>(0)).unwrap(),1);
    let record = crate::esm::records::RawRecord {
        form_id: 30,
        record_type: *b"LTEX",
        flags: 0,
        subrecords: [23u32, 21, 23, 0, 22]
            .into_iter()
            .map(|id| (b"GNAM".to_vec(), id.to_le_bytes().to_vec()))
            .collect(),
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    };
    crate::esm::exporter::export_to_db(&conn, &HashMap::from([(30, record)])).unwrap();
    let mut query=conn.prepare("SELECT gras_id,ordinal FROM landscape_texture_grasses WHERE ltex_id=30 ORDER BY ordinal").unwrap();
    let links: Vec<_> = query
        .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(links, [(23, 0), (21, 1), (22, 4)]);
    assert_eq!(
        conn.query_row(
            "SELECT gras_id FROM landscape_texture_grasses WHERE ltex_id=31",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        99
    );
}

/// Bake and validate the synthetic neighboring cells through the publication writer.
fn bake(
    conn: &Connection,
    root: &Path,
    previous: Option<&Path>,
    name: &str,
) -> (GrassBakeReport, GrassCache) {
    let path = root.join(name);
    let report =
        write_grass_cache(conn, root, &root.join("terrain.rkyv"), previous, &path).unwrap();
    (report, read(&path))
}

/// Three separate footprints distinguish visible geometry from two invisible materials.
fn visibility_fixture(root: &Path) -> Connection {
    let conn = fixture(root);
    conn.execute_batch("UPDATE \"references\" SET pos_x=0,pos_y=0,scale=1 WHERE id=50;
        UPDATE statics SET bounds_min_x=0,bounds_min_y=0,bounds_min_z=-64,bounds_max_x=320,bounds_max_y=32,bounds_max_z=0 WHERE id=40;").unwrap();
    let triangles =
        [0.0, 128.0, 256.0].map(|x| [[x, 32.0, 0.0], [x + 64.0, 32.0, 0.0], [x, 32.0, -64.0]]);
    write_mesh(root, &triangles, "OPAQUE");
    edit_mesh(root, |json, _| {
        json["accessors"][0]["count"] = 3.into();
        let base = json["accessors"][0].clone();
        for offset in [36, 72] {
            let mut accessor = base.clone();
            accessor["byteOffset"] = offset.into();
            json["accessors"].as_array_mut().unwrap().push(accessor);
        }
        json["materials"] = serde_json::json!([
            {"alphaMode":"OPAQUE"},
            {"alphaMode":"MASK","alphaCutoff":1.0,"pbrMetallicRoughness":{"baseColorFactor":[1.0,1.0,1.0,0.0]}},
            {"alphaMode":"MASK","alphaCutoff":1.0,"pbrMetallicRoughness":{"baseColorFactor":[0.0,0.0,0.0,0.0]},"extras":{"openSkyrim":{"nonRenderingExclusion":true}}}
        ]);
        json["meshes"][0]["primitives"] = serde_json::json!([
            {"attributes":{"POSITION":0},"material":0},
            {"attributes":{"POSITION":1},"material":1},
            {"attributes":{"POSITION":2},"material":2,"extras":{"openSkyrim":{"materialExclusion":"declared BSX EditorMarker"}}}
        ]);
    });
    conn
}

/// Invisible MASK and editor geometry must not erase grass beside a visible triangle.
#[test]
fn placement_invisible_mask_and_editor_marker_leave_only_visible_obstruction() {
    let dir = tempfile::tempdir().unwrap();
    let conn = visibility_fixture(dir.path());
    let (report, cache) = bake(&conn, dir.path(), None, "visibility.rkyv");
    assert_eq!(report.skipped_obstruction_models, 0);
    let mask = &cache.cells[0].quadrants[0].exclusion;
    assert_ne!(mask[0] & 1, 0, "visible neighbor obstructs at (16,16)");
    assert_eq!(
        mask[0] & (1 << 4),
        0,
        "alpha-zero MASK must be clear at (144,16)"
    );
    assert_eq!(mask[1] & 1, 0, "editor marker must be clear at (272,16)");
}

/// An excluded marker with an odd alpha mode cannot discard its valid neighbor or abort baking.
#[test]
fn placement_broken_marker_material_preserves_neighbor_and_publication() {
    let dir = tempfile::tempdir().unwrap();
    let conn = visibility_fixture(dir.path());
    edit_mesh(dir.path(), |json, _| {
        json["materials"][2]["alphaMode"] = "BROKEN_MARKER_MODE".into();
    });
    let (report, cache) = bake(&conn, dir.path(), None, "broken-marker.rkyv");
    assert_eq!(cache.cells.len(), 3, "healthy terrain survives publication");
    assert_eq!(
        report.skipped_obstruction_models, 0,
        "excluded marker is never decoded"
    );
    assert_ne!(
        cache.cells[0].quadrants[0].exclusion[0] & 1,
        0,
        "valid neighboring geometry survives"
    );
}

/// The runtime's primitive declaration works independently of material alpha or display names.
#[test]
fn placement_explicit_marker_skips_invalid_material_link_and_preserves_neighbor() {
    let dir = tempfile::tempdir().unwrap();
    let conn = visibility_fixture(dir.path());
    edit_mesh(dir.path(), |json, _| {
        json["meshes"][0]["primitives"][2]["material"] = 999.into();
        json["nodes"][0]["name"] = "OrdinaryMesh".into();
    });
    let (report, cache) = bake(&conn, dir.path(), None, "invalid-marker-link.rkyv");
    assert_eq!(report.skipped_obstruction_models, 0);
    assert_ne!(cache.cells[0].quadrants[0].exclusion[0] & 1, 0);
    assert_eq!(cache.cells[0].quadrants[0].exclusion[1] & 1, 0);
}

/// An excluded sibling cannot neutralize a material still used by visible geometry.
#[test]
fn placement_excluded_sibling_preserves_shared_live_material() {
    let dir = tempfile::tempdir().unwrap();
    let conn = visibility_fixture(dir.path());
    edit_mesh(dir.path(), |json, _| {
        json["meshes"][0]["primitives"][2]["material"] = 0.into();
    });
    let (report, cache) = bake(&conn, dir.path(), None, "shared-material.rkyv");
    assert_eq!(report.skipped_obstruction_models, 0);
    assert_ne!(cache.cells[0].quadrants[0].exclusion[0] & 1, 0);
    assert_eq!(cache.cells[0].quadrants[0].exclusion[1] & 1, 0);
}

/// Only sequence order changes, with unchanged raw record bytes, still invalidate reuse.
#[test]
fn placement_resolved_species_order_is_hashed_without_raw_record_change() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    conn.execute("UPDATE landscape_texture_grasses SET ordinal=gras_id", [])
        .unwrap();
    let (_, before) = bake(&conn, dir.path(), None, "hash-before.rkyv");
    conn.execute(
        "UPDATE landscape_texture_grasses SET ordinal=1000-gras_id",
        [],
    )
    .unwrap();
    let (report, after) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("hash-before.rkyv")),
        "hash-after.rkyv",
    );
    assert_eq!((report.baked_cells, report.reused_cells), (3, 0));
    assert_eq!(after.cells[0].quadrants[0].layers[0].grass_ids, [22, 21]);
    assert_ne!(before.cells[0].input_hash, after.cells[0].input_hash);
}

/// Species order is independent of FormID and participates in cell reuse keys.
#[test]
fn placement_texture_species_keep_authored_order_and_invalidate_reorder() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    conn.execute_batch("INSERT INTO grass_types SELECT 23,'Third',model_path,density,min_slope,max_slope,units_from_water,water_comparison,position_range,height_range,color_range,wave_period,flags,load_order FROM grass_types WHERE id=21;").unwrap();
    let export_order = |ids: &[u32]| {
        let record = crate::esm::records::RawRecord {
            form_id: 30,
            record_type: *b"LTEX",
            flags: 0,
            subrecords: ids
                .iter()
                .map(|id| (b"GNAM".to_vec(), id.to_le_bytes().to_vec()))
                .collect(),
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        crate::esm::exporter::export_to_db(&conn, &HashMap::from([(30, record)])).unwrap();
    };
    export_order(&[23, 21, 22]);
    let (_, first) = bake(&conn, dir.path(), None, "ordered.rkyv");
    assert_eq!(
        first.cells[0].quadrants[0].layers[0].grass_ids,
        [23, 21, 22]
    );
    export_order(&[22, 23, 21]);
    let (report, second) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("ordered.rkyv")),
        "reordered.rkyv",
    );
    assert_eq!((report.baked_cells, report.reused_cells), (3, 0));
    assert_eq!(
        second.cells[0].quadrants[0].layers[0].grass_ids,
        [22, 23, 21]
    );
    assert_ne!(first.cells[0].input_hash, second.cells[0].input_hash);
}

/// A structurally valid version-five archive must never reuse its obsolete masks or order.
#[test]
fn placement_version_five_cache_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    let (_, mut old) = bake(&conn, dir.path(), None, "old.rkyv");
    old.version = 5;
    let previous = dir.path().join("old.rkyv");
    std::fs::write(&previous, rkyv::to_bytes::<Error>(&old).unwrap()).unwrap();
    assert!(
        validate_grass_cache(&previous).is_err(),
        "version-five cache must be invalidated"
    );
    let (report, cache) = bake(&conn, dir.path(), Some(&previous), "new.rkyv");
    assert_eq!((report.baked_cells, report.reused_cells), (3, 0));
    assert_eq!(cache.version, 7);
}
