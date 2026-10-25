//! Regressions for initial visibility, skinned spatial filtering and fingerprint-only reuse.

use super::tests::{add_skin, edit_mesh, fixture, read, write_mesh};
use super::*;

/// Bake the common fixture with an optional previous archive.
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

/// Count excluded tile centers across a cell's four quadrants.
fn blocked(cell: &GrassCell) -> u32 {
    cell.quadrants
        .iter()
        .flat_map(|q| &q.exclusion)
        .map(|b| b.count_ones())
        .sum()
}

/// REFR header visibility applies before obstruction inspection.
#[test]
fn initially_disabled_blocker_does_not_obstruct() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    conn.execute(
        "UPDATE \"references\" SET header_flags=2048 WHERE id=50",
        [],
    )
    .unwrap();
    let (_, cache) = bake(&conn, dir.path(), None, "disabled.rkyv");
    assert_eq!(
        cache.cells.iter().map(blocked).sum::<u32>(),
        0,
        "initially disabled geometry must leave grass clear"
    );
}

/// An ordinary child follows its hidden parent; the opposite-state sibling remains visible.
#[test]
fn enable_parent_hides_blocker_but_opposite_sibling_obstructs() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    conn.execute_batch("INSERT INTO \"references\"(id,cell_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,header_flags) VALUES(60,12,40,0,0,0,0,0,0,0,2048);
        UPDATE \"references\" SET pos_x=128,scale=1,enable_parent_id=60,enable_parent_flags=0 WHERE id=50;
        INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale,enable_parent_id,enable_parent_flags) VALUES(51,11,1,40,1,4224,64,0,0,0,0,1,60,1);").unwrap();
    let (_, cache) = bake(&conn, dir.path(), None, "siblings.rkyv");
    assert_eq!(
        blocked(&cache.cells[0]),
        0,
        "hidden child must not obstruct"
    );
    assert!(
        blocked(&cache.cells[1]) > 0,
        "opposite-state sibling must obstruct"
    );
}

/// A parent outside the obstruction worldspace invalidates only cells touched by its child.
#[test]
fn enable_parent_flag_change_invalidates_only_affected_cells() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    conn.execute_batch(
        "INSERT INTO \"references\"(id,cell_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,header_flags) VALUES(60,12,40,0,0,0,0,0,0,0,0);
        UPDATE \"references\" SET enable_parent_id=60,enable_parent_flags=0 WHERE id=50;",
    )
    .unwrap();
    let (_, before) = bake(&conn, dir.path(), None, "before.rkyv");
    conn.execute(
        "UPDATE \"references\" SET header_flags=2048 WHERE id=60",
        [],
    )
    .unwrap();
    let (report, after) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("before.rkyv")),
        "after.rkyv",
    );
    assert_eq!(
        (report.baked_cells, report.reused_cells),
        (2, 1),
        "parent changes must invalidate neighboring child cells only"
    );
    for index in 0..2 {
        assert_ne!(
            before.cells[index].input_hash,
            after.cells[index].input_hash
        );
        assert_eq!(blocked(&after.cells[index]), 0);
    }
    assert_eq!(before.cells[2], after.cells[2]);
}

/// Rest joints move a blocker out of raw bounds across both a CELL and quadrant boundary.
#[test]
fn skinned_rest_pose_outside_raw_bounds_clears_cells_and_quadrants() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    write_mesh(
        dir.path(),
        &[[[0.0, 32.0, 0.0], [256.0, 32.0, 0.0], [0.0, 32.0, -256.0]]],
        "OPAQUE",
    );
    add_skin(dir.path(), false);
    edit_mesh(dir.path(), |json, bin| {
        json["nodes"] = serde_json::json!([{"mesh":0,"skin":0},{"translation":[0,0,0]}, {"translation":[4000,0,-1984]}]);
        json["scenes"][0]["nodes"] = serde_json::json!([0, 1, 2]);
        json["skins"] = serde_json::json!([{"joints":[2]}]);
        let joint_offset = json["bufferViews"][1]["byteOffset"].as_u64().unwrap() as usize;
        bin[joint_offset..joint_offset + 24].fill(0);
        let weight_offset = json["bufferViews"][2]["byteOffset"].as_u64().unwrap() as usize;
        for weight in bin[weight_offset..weight_offset + 48]
            .as_chunks_mut::<16>()
            .0
        {
            weight.copy_from_slice(
                &[1.0f32, 0.0, 0.0, 0.0]
                    .into_iter()
                    .flat_map(f32::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
        }
    });
    conn.execute_batch("UPDATE \"references\" SET pos_x=0,pos_y=0,scale=1 WHERE id=50;
        UPDATE statics SET bounds_min_x=0,bounds_min_y=32,bounds_min_z=-256,bounds_max_x=256,bounds_max_y=32,bounds_max_z=0 WHERE id=40;").unwrap();
    let (report, cache) = bake(&conn, dir.path(), None, "rest.rkyv");
    assert_eq!(
        (
            report.expanded_sampling_cells,
            report.expanded_sampling_quadrants
        ),
        // CELL 10 was already a raw-bound candidate; only CELL 11 is newly admitted.
        (1, 4)
    );
    // Literal triangle: [4000,1984,32], [4256,1984,32], [4000,2240,32].
    for (cell, quadrant, x, y) in [(0, 1, 61, 62), (0, 3, 61, 0), (1, 0, 0, 62), (1, 2, 0, 0)] {
        let bit = y * 64 + x;
        assert_ne!(
            cache.cells[cell].quadrants[quadrant].exclusion[bit / 8] & (1 << (bit % 8)),
            0,
            "cell {cell}, quadrant {quadrant}: rest-pose sample must be blocked"
        );
    }
}

/// A completely reusable warm bake reads model bytes but never decodes geometry.
#[test]
fn fully_reusable_warm_run_decodes_no_geometry_and_preserves_keys() {
    for skinned in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        if skinned {
            add_skin(dir.path(), true);
        }
        let (cold, before) = bake(&conn, dir.path(), None, "cold.rkyv");
        assert_eq!(cold.geometry_decodes, 1);
        let (warm, after) = bake(
            &conn,
            dir.path(),
            Some(&dir.path().join("cold.rkyv")),
            "warm.rkyv",
        );
        assert_eq!((warm.baked_cells, warm.reused_cells), (0, 3));
        assert_eq!(before, after);
        assert_eq!(
            warm.geometry_decodes, 0,
            "fingerprinting reusable cells must not decode model triangles (skin={skinned})"
        );
    }
}

/// Splitting fingerprints from decoding preserves the legacy readable-byte and unavailable keys.
#[test]
fn fingerprint_split_preserves_readable_malformed_and_unavailable_keys() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    for (path, bytes) in [
        ("meshes/blocker.glb", None),
        ("meshes/bad.glb", Some(b"malformed GLB".as_slice())),
    ] {
        if let Some(bytes) = bytes {
            std::fs::write(dir.path().join(path), bytes).unwrap();
        }
        let expected: [u8; 32] =
            Sha256::digest(std::fs::read(dir.path().join(path)).unwrap()).into();
        let mut legacy = GeometryCache::new(dir.path()).unwrap();
        legacy.get(path);
        let mut split = GeometryCache::new(dir.path()).unwrap();
        assert_eq!(split.fingerprint(path), legacy.hashes[path]);
        assert_eq!(split.fingerprint(path), expected);
        assert_eq!(split.decodes, 0);
    }
    for path in [
        "meshes/missing.glb",
        "meshes/../outside.glb",
        "meshes/Blocker.glb",
    ] {
        let mut legacy = GeometryCache::new(dir.path()).unwrap();
        legacy.get(path);
        let mut split = GeometryCache::new(dir.path()).unwrap();
        assert_eq!(split.fingerprint(path), legacy.hashes[path]);
        assert_eq!(
            split.fingerprint(path),
            <[u8; 32]>::from(Sha256::digest(b"unavailable obstruction model"))
        );
        assert_eq!(split.decodes, 0);
    }
}

/// The unchanged static box has the exact forty-bit mask produced before these fixes.
#[test]
fn static_obstruction_masks_remain_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    let (_, cache) = bake(&conn, dir.path(), None, "static.rkyv");
    for (index, cell) in cache.cells.iter().enumerate() {
        for (quadrant, actual) in cell.quadrants.iter().enumerate() {
            let mut expected = vec![0u8; 512];
            if index == 0 && quadrant == 1 {
                for offset in [7, 15, 23, 31, 39] {
                    expected[offset] = 0xf0;
                }
            }
            if index == 1 && quadrant == 0 {
                for offset in [0, 8, 16, 24, 32] {
                    expected[offset] = 0x0f;
                }
            }
            assert_eq!(
                actual.exclusion, expected,
                "cell {index}, quadrant {quadrant}"
            );
        }
    }
}

/// Broken parent metadata hides only that blocker while healthy neighboring cells still bake.
#[test]
fn malformed_enable_parent_is_local_and_healthy_neighbor_survives() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    conn.execute_batch("UPDATE \"references\" SET enable_parent_id=60,enable_parent_flags=NULL WHERE id=50;
        INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale) VALUES(51,12,2,40,1,4224,64,0,0,0,0,1);").unwrap();
    let (report, cache) = bake(&conn, dir.path(), None, "broken-enable.rkyv");
    assert_eq!(
        (
            report.baked_cells,
            report.skipped_cells,
            report.unresolved_enable_references
        ),
        (3, 0, 1)
    );
    assert!(report.warnings().is_empty());
    assert!(
        report
            .notices()
            .iter()
            .any(|notice| notice.contains("1 unresolved enable states"))
    );
    assert_eq!(blocked(&cache.cells[0]) + blocked(&cache.cells[1]), 0);
    assert!(blocked(&cache.cells[2]) > 0);
    conn.execute_batch("UPDATE \"references\" SET enable_parent_flags=0 WHERE id=50;
        INSERT INTO \"references\"(id,cell_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,header_flags) VALUES(60,10,40,0,0,0,0,0,0,0,0);").unwrap();
    let (repaired, next) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("broken-enable.rkyv")),
        "repaired-enable.rkyv",
    );
    assert_eq!(
        (
            repaired.baked_cells,
            repaired.reused_cells,
            repaired.unresolved_enable_references
        ),
        (2, 1, 0)
    );
    assert!(blocked(&next.cells[0]) > 0 && blocked(&next.cells[1]) > 0);
    assert_eq!(cache.cells[2], next.cells[2]);
}
