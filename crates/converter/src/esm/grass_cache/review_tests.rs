//! Regressions for initial visibility, skinned spatial filtering and fingerprint-only reuse.

use super::tests::{add_skin, edit_mesh, fixture, read, write_mesh};
use super::*;

/// Publication retains a resolved modelless middle slot before first-N species selection.
#[test]
fn modelless_middle_species_keeps_authored_cap_slot() {
    for model in [None, Some("")] {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute_batch("DELETE FROM landscape_texture_grasses;
            INSERT INTO landscape_texture_grasses(ltex_id,gras_id,ordinal) VALUES(30,21,0),(30,23,1),(30,22,2);").unwrap();
        conn.execute("INSERT INTO grass_types(id,model_path,density,min_slope,max_slope,units_from_water,water_comparison,position_range,height_range,color_range,wave_period,flags,load_order) VALUES(23,?1,63,0,80,0,0,85,0.2,0.1,1.2,1,0)", [model]).unwrap();
        let (report, cache) = bake(&conn, dir.path(), None, "slots.rkyv");
        assert_eq!((report.skipped_rules, report.modelless_rules), (0, 1));
        let ids = &cache.cells[0].quadrants[0].layers[0].grass_ids;
        assert_eq!(
            ids,
            &[21, 23, 22],
            "publication preserves the authored middle slot"
        );
        let emitted: Vec<_> = ids
            .iter()
            .take(2)
            .filter(|id| {
                !cache
                    .types
                    .iter()
                    .find(|rule| rule.form_id == **id)
                    .unwrap()
                    .model_path
                    .is_empty()
            })
            .copied()
            .collect();
        assert_eq!(emitted, [21], "B stays outside the two authored slots");
        let placeholder = cache.types.iter().find(|rule| rule.form_id == 23).unwrap();
        assert_eq!(placeholder.density, 63);
        assert_eq!(placeholder.position_range, 85.0);
    }
}

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

/// Build a tiny sparse payload whose implicit base can describe a much larger accessor.
fn sparse_only_mesh(root: &Path, count: u32, index_accessor: bool) -> Vec<u8> {
    write_mesh(
        root,
        &[[[0.0, 0.0, 0.0], [16.0, 0.0, 0.0], [0.0, 16.0, 0.0]]],
        "OPAQUE",
    );
    edit_mesh(root, |json, bin| {
        let indices_offset = bin.len();
        bin.extend(0u32.to_le_bytes());
        let values_offset = bin.len();
        if index_accessor {
            bin.extend(0u32.to_le_bytes());
        } else {
            bin.extend([1.0f32, 2.0, 3.0].into_iter().flat_map(f32::to_le_bytes));
        }
        json["buffers"][0]["byteLength"] = bin.len().into();
        json["bufferViews"].as_array_mut().unwrap().extend([
            serde_json::json!({"buffer":0,"byteOffset":indices_offset,"byteLength":4}),
            serde_json::json!({"buffer":0,"byteOffset":values_offset,"byteLength":if index_accessor {4} else {12}}),
        ]);
        let sparse = serde_json::json!({"count":1,"indices":{"bufferView":1,"componentType":5125},"values":{"bufferView":2}});
        if index_accessor {
            json["accessors"].as_array_mut().unwrap().push(serde_json::json!({"componentType":5125,"count":count,"type":"SCALAR","sparse":sparse}));
            json["meshes"][0]["primitives"][0]["indices"] = 1.into();
        } else {
            json["accessors"][0]
                .as_object_mut()
                .unwrap()
                .remove("bufferView");
            json["accessors"][0]["count"] = count.into();
            json["accessors"][0]["sparse"] = sparse;
        }
    });
    std::fs::read(root.join("meshes/blocker.glb")).unwrap()
}

/// Reject huge implicit POSITION and index bases before constructing a dependency iterator.
#[test]
fn sparse_only_accessors_obey_geometry_byte_budget_before_decode() {
    let dir = tempfile::tempdir().unwrap();
    for index_accessor in [false, true] {
        let bytes = sparse_only_mesh(dir.path(), u32::MAX, index_accessor);
        let glb = gltf::Gltf::from_slice_without_validation(&bytes).unwrap();
        let accessor = glb.accessors().nth(usize::from(index_accessor)).unwrap();
        let result = validate_accessor(&accessor, glb.blob.as_deref().unwrap());
        assert!(
            result.is_err(),
            "implicit sparse byte budget must reject the crafted count before allocation: indices={index_accessor}"
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("geometry byte budget"));
    }
}

/// A large valid zero base remains independent of the number of sparse overrides.
#[test]
fn sparse_only_accessor_budget_accepts_large_zero_base_with_one_override() {
    let dir = tempfile::tempdir().unwrap();
    for index_accessor in [false, true] {
        let bytes = sparse_only_mesh(dir.path(), 65_535, index_accessor);
        let glb = gltf::Gltf::from_slice_without_validation(&bytes).unwrap();
        let accessor = glb.accessors().nth(usize::from(index_accessor)).unwrap();
        validate_accessor(&accessor, glb.blob.as_deref().unwrap()).unwrap();
    }
}

/// Bounded sparse-model failure publishes healthy neighboring masks and its omission diagnostic.
#[test]
fn sparse_only_large_model_skip_preserves_healthy_neighbor() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    let healthy = dir.path().join("meshes/healthy.glb");
    std::fs::copy(dir.path().join("meshes/blocker.glb"), &healthy).unwrap();
    let healthy_bytes = std::fs::read(&healthy).unwrap();
    let bytes = sparse_only_mesh(dir.path(), u32::MAX, false);
    let glb = gltf::Gltf::from_slice_without_validation(&bytes).unwrap();
    let accessor = glb.accessors().next().unwrap();
    assert!(
        validate_accessor(&accessor, glb.blob.as_deref().unwrap()).is_err(),
        "the crafted sparse model must be refused before the bake can allocate its implicit base"
    );
    conn.execute_batch("INSERT INTO records(form_id,record_type,load_order,data) VALUES(41,'STAT',0,x'01');
        INSERT INTO statics(id,model_path,flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid) SELECT 41,'meshes/healthy.glb',flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid FROM statics WHERE id=40;
        INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale) VALUES(51,12,2,41,1,4224,64,0,0,0,0,1);").unwrap();
    let (report, cache) = bake(&conn, dir.path(), None, "bounded-sparse.rkyv");
    assert_eq!(
        (
            report.baked_cells,
            report.skipped_cells,
            report.skipped_obstruction_models
        ),
        (3, 0, 1)
    );
    assert!(
        report
            .obstruction_diagnostics
            .iter()
            .any(|message| message.contains("meshes/blocker.glb")
                && message.contains("geometry byte budget"))
    );
    assert!(
        report
            .notices()
            .iter()
            .any(|message| message.contains("geometry byte budget"))
    );
    assert_eq!(blocked(&cache.cells[0]) + blocked(&cache.cells[1]), 0);
    assert!(
        blocked(&cache.cells[2]) > 0,
        "the healthy neighboring model still obstructs grass"
    );
    assert_eq!(
        std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap(),
        bytes
    );
    assert_eq!(std::fs::read(healthy).unwrap(), healthy_bytes);
}

/// Publication checks catch a model replacement even when later lookups hit cached triangles.
#[test]
fn cached_model_replacement_is_detected_before_publication() {
    let dir = tempfile::tempdir().unwrap();
    let _conn = fixture(dir.path());
    let mut geometry = GeometryCache::new(dir.path()).unwrap();
    let first = geometry.get("meshes/blocker.glb").unwrap();
    geometry.validate_sources().unwrap();
    write_mesh(
        dir.path(),
        &[[[0.0, 32.0, 0.0], [256.0, 32.0, 0.0], [0.0, 32.0, -256.0]]],
        "OPAQUE",
    );
    assert_eq!(geometry.get("meshes/blocker.glb").unwrap(), first);
    let result = geometry.validate_sources();
    assert!(
        result.is_err(),
        "a cached geometry hit must still detect replacement before publication"
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("obstruction changed"));
}

/// Optional unreadable or malformed models remain local omissions at the publication check.
#[test]
fn source_validation_preserves_optional_model_omissions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("meshes")).unwrap();
    std::fs::write(dir.path().join("meshes/broken.glb"), b"broken GLB").unwrap();
    let mut geometry = GeometryCache::new(dir.path()).unwrap();
    for path in ["meshes/broken.glb", "meshes/missing.glb"] {
        geometry.fingerprint(path);
        assert!(geometry.get(path).is_none());
    }
    geometry.validate_sources().unwrap();
    assert_eq!(geometry.failures.len(), 2);
}

/// Repeated child edges omit the malformed model and retain healthy neighboring masks.
#[test]
fn repeated_model_nodes_skip_locally_and_preserve_healthy_neighbor() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    std::fs::copy(
        dir.path().join("meshes/blocker.glb"),
        dir.path().join("meshes/healthy.glb"),
    )
    .unwrap();
    conn.execute_batch("INSERT INTO records(form_id,record_type,load_order,data) VALUES(41,'STAT',0,x'01');
        INSERT INTO statics(id,model_path,flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid) SELECT 41,'meshes/healthy.glb',flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid FROM statics WHERE id=40;
        INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale) VALUES(51,12,2,41,1,4224,64,0,0,0,0,1);").unwrap();
    for bad_skin in [false, true] {
        edit_mesh(dir.path(), |json, _| {
            json["nodes"] = serde_json::json!([
                {"children":[1,1]}, {"children":[2,2]}, {"children":[3,3]},
                {"children":[4,4]}, {"mesh":0}
            ]);
            if bad_skin {
                json["nodes"][4]["skin"] = 0.into();
                json["skins"] = serde_json::json!([{"joints":[999]}]);
            }
        });
        let bytes = std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap();
        let (report, cache) = bake(&conn, dir.path(), None, "bounded-nodes.rkyv");
        assert_eq!(
            (
                report.baked_cells,
                report.skipped_cells,
                report.skipped_obstruction_models
            ),
            (3, 0, 1),
            "the repeated node is skipped on the evaluated and static fallback paths: bad_skin={bad_skin}"
        );
        assert!(
            report
                .obstruction_diagnostics
                .iter()
                .any(|message| message.contains("meshes/blocker.glb")
                    && message.contains("repeated model node"))
        );
        assert_eq!(blocked(&cache.cells[0]) + blocked(&cache.cells[1]), 0);
        assert!(blocked(&cache.cells[2]) > 0);
        assert_eq!(
            std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap(),
            bytes
        );
    }
}

/// Non-emitting terrain preserves rule warnings while skipping unrelated obstruction models.
#[test]
fn grassless_terrain_skips_obstruction_model_reads() {
    for case in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute("UPDATE statics SET model_path='meshes/missing.glb'", [])
            .unwrap();
        match case {
            0 => {
                conn.execute_batch("UPDATE grass_types SET density=-1; ")
                    .unwrap();
            }
            1 => {
                conn.execute("DELETE FROM landscape_texture_grasses", [])
                    .unwrap();
            }
            2 => {
                conn.execute("UPDATE landscape_texture_grasses SET ltex_id=999", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let (report, cache) = bake(&conn, dir.path(), None, "grassless.rkyv");
        // Empty model paths identify retained non-emitting selection slots.
        assert!(
            cache
                .cells
                .iter()
                .flat_map(|cell| &cell.quadrants)
                .flat_map(|quadrant| &quadrant.layers)
                .flat_map(|layer| &layer.grass_ids)
                .all(|id| cache
                    .types
                    .iter()
                    .any(|rule| { rule.form_id == *id && rule.model_path.is_empty() })),
            "any retained grass slots must be non-emitting: case={case}"
        );
        assert!(cache.cells.iter().all(|cell| {
            cell.quadrants
                .iter()
                .all(|quadrant| quadrant.exclusion.iter().all(|&byte| byte == 0))
        }));
        assert_eq!(report.geometry_decodes, 0);
        assert_eq!(
            report.skipped_obstruction_models, 0,
            "no unrelated obstruction model should be read: case={case}"
        );
        assert!(report.obstruction_diagnostics.is_empty());
        if case == 0 {
            assert_eq!(report.skipped_rules, 2);
            assert!(!report.warnings().is_empty());
        }
    }
}

/// Pre-recipe duplicate-node masks rebuild once, omit the malformed model and then reuse safely.
#[test]
fn unique_node_recipe_rebuilds_legacy_masks_before_warm_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    edit_mesh(dir.path(), |json, _| {
        json["nodes"] = serde_json::json!([{"children":[1,1]}, {"mesh":0}]);
    });
    let (_, mut previous) = bake(&conn, dir.path(), None, "seed.rkyv");
    let (types, ..) = read_types(&conn).unwrap();
    let valid_types = types.iter().map(|rule| rule.form_id).collect();
    let (links, ..) = links::read_links(&conn, &valid_types).unwrap();
    let inputs = BTreeMap::from([(21, vec![1, 2]), (22, vec![3, 4]), (30, vec![5, 6])]);
    let (references, _) = read_reference_bounds(&conn).unwrap();
    let model_hash: [u8; 32] =
        Sha256::digest(std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap()).into();
    let terrain_map =
        super::super::cell_cache::validate_cell_cache(&dir.path().join("terrain.rkyv")).unwrap();
    let terrain = rkyv::access::<shared::ArchivedCellCache, Error>(&terrain_map).unwrap();
    for (index, cell) in previous.cells.iter_mut().enumerate() {
        let land = rkyv::deserialize::<CachedLand, Error>(&terrain.cells[index]).unwrap();
        // This is the previous raw-input recipe, before unique-node inspection was part of it.
        // The single static reference crosses cells 10/11; cell 12 is in another worldspace.
        let refs = if index < 2 {
            vec![&references[0]]
        } else {
            vec![]
        };
        let models = if index < 2 { vec![model_hash] } else { vec![] };
        cell.input_hash = input_hasher(
            &land,
            (cell.worldspace_id, cell.grid_x, cell.grid_y),
            &refs,
            &links,
            &inputs,
            None,
            &models,
        )
        .unwrap()
        .finalize()
        .into();
        cell.obstruction_failures.clear();
        cell.expanded_sampling = 0;
        for (quadrant, layer) in cell.quadrants.iter_mut().enumerate() {
            layer.exclusion.fill(0);
            if index == 0 && quadrant == 1 {
                for offset in [7, 15, 23, 31, 39] {
                    layer.exclusion[offset] = 0xf0;
                }
            }
            if index == 1 && quadrant == 0 {
                for offset in [0, 8, 16, 24, 32] {
                    layer.exclusion[offset] = 0x0f;
                }
            }
        }
    }
    assert_eq!(previous.cells.iter().map(blocked).sum::<u32>(), 40);
    let old = dir.path().join("legacy-recipe.rkyv");
    std::fs::write(&old, rkyv::to_bytes::<Error>(&previous).unwrap()).unwrap();
    let (rebuilt, current) = bake(&conn, dir.path(), Some(&old), "current.rkyv");
    assert_eq!((rebuilt.baked_cells, rebuilt.reused_cells), (3, 0));
    assert_eq!(rebuilt.skipped_obstruction_models, 1);
    assert_eq!(current.cells.len(), 3);
    assert_eq!(current.cells.iter().map(blocked).sum::<u32>(), 0);
    assert!(rebuilt.obstruction_diagnostics[0].contains("repeated model node"));
    let (warm, after) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("current.rkyv")),
        "warm-recipe.rkyv",
    );
    assert_eq!(
        (warm.baked_cells, warm.reused_cells, warm.geometry_decodes),
        (0, 3, 0)
    );
    assert_eq!(after, current);
    assert_eq!(warm.notices(), rebuilt.notices());
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
    let (warm, reused) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("rest.rkyv")),
        "rest-warm.rkyv",
    );
    assert_eq!(
        (warm.baked_cells, warm.reused_cells, warm.geometry_decodes),
        (0, 3, 0)
    );
    assert_eq!(warm.notices(), report.notices());
    assert_eq!(reused, cache);
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

/// Missing geometry remains an advisory omission when every mask is reused.
#[test]
fn missing_obstruction_notices_survive_fully_reused_masks() {
    reusable_obstruction_notices(false);
}

/// Readable decode failures are reported without decoding a reused mask again.
#[test]
fn malformed_obstruction_notices_survive_fully_reused_masks() {
    reusable_obstruction_notices(true);
}

/// Compare the complete bounded notice list, its unique-model count and repeated reuse.
fn reusable_obstruction_notices(malformed: bool) {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    if malformed {
        std::fs::write(dir.path().join("meshes/blocker.glb"), b"malformed GLB").unwrap();
    } else {
        conn.execute("UPDATE statics SET model_path='meshes/missing.glb'", [])
            .unwrap();
    }
    let (cold, before) = bake(&conn, dir.path(), None, "cold.rkyv");
    assert_eq!(cold.skipped_obstruction_models, 1);
    assert_eq!(cold.obstruction_diagnostics.len(), 1);
    assert_eq!(cold.geometry_decodes, usize::from(malformed));
    assert!(cold.warnings().is_empty());
    assert_eq!(
        before.cells.len(),
        3,
        "healthy neighboring cells still bake"
    );
    for (previous, output) in [("cold.rkyv", "warm.rkyv"), ("warm.rkyv", "warm2.rkyv")] {
        let (warm, after) = bake(&conn, dir.path(), Some(&dir.path().join(previous)), output);
        assert_eq!((warm.baked_cells, warm.reused_cells), (0, 3));
        assert_eq!(
            warm.geometry_decodes, 0,
            "reused masks must decode no geometry"
        );
        assert_eq!(before, after);
        assert_eq!(
            warm.skipped_obstruction_models,
            cold.skipped_obstruction_models
        );
        assert_eq!(warm.obstruction_diagnostics, cold.obstruction_diagnostics);
        assert_eq!(warm.notices(), cold.notices());
    }
}

/// Reused and rebuilt cells share failures without retaining removed or repaired omissions.
#[test]
fn obstruction_notices_follow_partial_reuse_removal_and_repair() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    std::fs::write(dir.path().join("meshes/blocker.glb"), b"malformed GLB").unwrap();
    conn.execute_batch("INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale) VALUES(51,12,2,40,1,4224,64,0,0,0,0,1);").unwrap();
    let (cold, _) = bake(&conn, dir.path(), None, "cold.rkyv");
    assert_eq!(cold.skipped_obstruction_models, 1);
    conn.execute("UPDATE \"references\" SET pos_x=4256 WHERE id=51", [])
        .unwrap();
    let (partial, _) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("cold.rkyv")),
        "partial.rkyv",
    );
    assert_eq!(
        (
            partial.baked_cells,
            partial.reused_cells,
            partial.geometry_decodes
        ),
        (1, 2, 0)
    );
    assert_eq!(partial.notices(), cold.notices());
    conn.execute("DELETE FROM \"references\" WHERE id=51", [])
        .unwrap();
    let (removed, _) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("partial.rkyv")),
        "removed.rkyv",
    );
    assert_eq!((removed.baked_cells, removed.reused_cells), (1, 2));
    assert_eq!(removed.notices(), cold.notices());
    conn.execute(
        "UPDATE \"references\" SET header_flags=2048 WHERE id=50",
        [],
    )
    .unwrap();
    let (hidden, _) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("removed.rkyv")),
        "hidden.rkyv",
    );
    let (fresh, _) = bake(&conn, dir.path(), None, "hidden-fresh.rkyv");
    assert_eq!(hidden.notices(), fresh.notices());
    assert_eq!(hidden.skipped_obstruction_models, 0);
    conn.execute("UPDATE \"references\" SET header_flags=0 WHERE id=50", [])
        .unwrap();
    write_mesh(
        dir.path(),
        &[[[0.0, 32.0, 0.0], [256.0, 32.0, 0.0], [0.0, 32.0, -256.0]]],
        "OPAQUE",
    );
    let (repaired, cache) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("hidden.rkyv")),
        "repaired.rkyv",
    );
    let (fresh, expected) = bake(&conn, dir.path(), None, "repaired-fresh.rkyv");
    assert_eq!(repaired.notices(), fresh.notices());
    assert_eq!(repaired.skipped_obstruction_models, 0);
    assert_eq!(cache, expected);
}

/// Every failed model is counted while the path-sorted examples remain capped at 32.
#[test]
fn obstruction_notice_limit_and_first_example_survive_reuse_and_removal() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    conn.execute("DELETE FROM \"references\"", []).unwrap();
    for index in 0..35u32 {
        let path = format!("meshes/bad{index:02}.glb");
        std::fs::write(dir.path().join(&path), b"malformed GLB").unwrap();
        conn.execute(
            "INSERT INTO records(form_id,record_type,load_order,data) VALUES(?1,'STAT',0,x'00')",
            [100 + index],
        )
        .unwrap();
        conn.execute("INSERT INTO statics(id,model_path,flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid) VALUES(?1,?2,0,-64,-16,-48,64,32,48,1)", rusqlite::params![100+index,path]).unwrap();
        conn.execute("INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale) VALUES(?1,10,1,?2,1,4090,64,0,0,0,0,2)", rusqlite::params![200+index,100+index]).unwrap();
    }
    let (cold, _) = bake(&conn, dir.path(), None, "cold.rkyv");
    assert_eq!(
        (
            cold.skipped_obstruction_models,
            cold.obstruction_diagnostics.len()
        ),
        (35, 32)
    );
    assert!(cold.obstruction_diagnostics[0].starts_with("obstruction meshes/bad00.glb:"));
    let (warm, _) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("cold.rkyv")),
        "warm.rkyv",
    );
    assert_eq!(
        (warm.baked_cells, warm.reused_cells, warm.geometry_decodes),
        (0, 3, 0)
    );
    assert_eq!(cold.notices(), warm.notices());
    conn.execute("DELETE FROM \"references\" WHERE id IN (200,201)", [])
        .unwrap();
    let (removed, _) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("warm.rkyv")),
        "removed.rkyv",
    );
    let (fresh, _) = bake(&conn, dir.path(), None, "fresh.rkyv");
    assert_eq!(removed.skipped_obstruction_models, 33);
    assert!(removed.obstruction_diagnostics[0].starts_with("obstruction meshes/bad02.glb:"));
    assert_eq!(removed.notices(), fresh.notices());
}

/// A genuine version-four layout is rejected before rebuilding and reusing current-version masks.
#[test]
fn version_four_archive_layout_rebuilds_before_current_version_reuse() {
    #[derive(rkyv::Archive, rkyv::Serialize)]
    #[rkyv(bytecheck())]
    struct VersionFourCell {
        cell_id: u32,
        worldspace_id: u32,
        grid_x: i32,
        grid_y: i32,
        input_hash: [u8; 32],
        heights: Vec<f32>,
        water_height: Option<f32>,
        quadrants: Vec<GrassQuadrant>,
    }
    #[derive(rkyv::Archive, rkyv::Serialize)]
    #[rkyv(bytecheck())]
    struct VersionFourCache {
        version: u32,
        types: Vec<GrassType>,
        cells: Vec<VersionFourCell>,
    }
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    let (_, expected) = bake(&conn, dir.path(), None, "expected.rkyv");
    assert_eq!(expected.version, shared::grass::GRASS_CACHE_VERSION);
    let old = VersionFourCache {
        version: 4,
        types: expected.types.clone(),
        cells: expected
            .cells
            .iter()
            .map(|c| VersionFourCell {
                cell_id: c.cell_id,
                worldspace_id: c.worldspace_id,
                grid_x: c.grid_x,
                grid_y: c.grid_y,
                input_hash: c.input_hash,
                heights: c.heights.clone(),
                water_height: c.water_height,
                quadrants: c.quadrants.clone(),
            })
            .collect(),
    };
    let bytes = rkyv::to_bytes::<Error>(&old).unwrap();
    assert_eq!(
        rkyv::access::<ArchivedVersionFourCache, Error>(&bytes)
            .unwrap()
            .version,
        4
    );
    let previous = dir.path().join("v4.rkyv");
    std::fs::write(&previous, bytes).unwrap();
    assert!(validate_grass_cache(&previous).is_err());
    let (rebuilt, after) = bake(&conn, dir.path(), Some(&previous), "current.rkyv");
    assert_eq!((rebuilt.baked_cells, rebuilt.reused_cells), (3, 0));
    assert_eq!(after, expected);
    let (warm, after) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("current.rkyv")),
        "current-warm.rkyv",
    );
    assert_eq!(
        (warm.baked_cells, warm.reused_cells, warm.geometry_decodes),
        (0, 3, 0)
    );
    assert_eq!(after, expected);
}

/// Malformed stored provenance is rejected before it can supply reused notices.
#[test]
fn invalid_obstruction_provenance_rebuilds_healthy_cells() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    let (_, expected) = bake(&conn, dir.path(), None, "expected.rkyv");
    for case in 0..4 {
        let mut invalid = expected.clone();
        let cell = &mut invalid.cells[0];
        match case {
            0 => cell.expanded_sampling = 0x20,
            1 => cell.expanded_sampling = 0x10,
            2 => cell.obstruction_failures.push(GrassObstructionFailure {
                model_path: "meshes/bad.glb".into(),
                reason: String::new(),
            }),
            3 => {
                let failure = GrassObstructionFailure {
                    model_path: "meshes/bad.glb".into(),
                    reason: "bad model".into(),
                };
                cell.obstruction_failures = vec![failure.clone(), failure];
            }
            _ => unreachable!(),
        }
        let previous = dir.path().join(format!("invalid-{case}.rkyv"));
        std::fs::write(&previous, rkyv::to_bytes::<Error>(&invalid).unwrap()).unwrap();
        assert!(validate_grass_cache(&previous).is_err());
        let (report, actual) = bake(
            &conn,
            dir.path(),
            Some(&previous),
            &format!("rebuilt-{case}.rkyv"),
        );
        assert_eq!((report.baked_cells, report.reused_cells), (3, 0));
        assert_eq!(actual, expected);
    }
}

/// A path that exists but cannot be read as a file retains its original I/O reason.
#[test]
fn unreadable_obstruction_notice_survives_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    std::fs::create_dir(dir.path().join("meshes/unreadable.glb")).unwrap();
    conn.execute("UPDATE statics SET model_path='meshes/unreadable.glb'", [])
        .unwrap();
    let (cold, _) = bake(&conn, dir.path(), None, "cold.rkyv");
    assert_eq!(cold.skipped_obstruction_models, 1);
    assert_eq!(cold.geometry_decodes, 0);
    assert!(cold.obstruction_diagnostics[0].starts_with("obstruction meshes/unreadable.glb:"));
    let (warm, _) = bake(
        &conn,
        dir.path(),
        Some(&dir.path().join("cold.rkyv")),
        "warm.rkyv",
    );
    assert_eq!(
        (warm.baked_cells, warm.reused_cells, warm.geometry_decodes),
        (0, 3, 0)
    );
    assert_eq!(warm.notices(), cold.notices());
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

/// Pick-up clutter never leaves permanent holes; fixed bases still obstruct.
#[test]
fn only_static_like_bases_bake_obstructions() {
    for kind in [
        "STAT", "MSTT", "ACTI", "FURN", "CONT", "DOOR", "MISC", "ALCH", "WEAP", "INGR", "ARMO",
        "BOOK", "AMMO", "SLGM", "SCRL", "KEYM", "TREE", "FLOR",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute("UPDATE records SET record_type=?1 WHERE form_id=40", [kind])
            .unwrap();
        let (_, cache) = bake(&conn, dir.path(), None, "policy.rkyv");
        let count: u32 = cache.cells.iter().map(blocked).sum();
        assert_eq!(
            count > 0,
            matches!(kind, "STAT" | "MSTT" | "ACTI" | "FURN" | "CONT" | "DOOR"),
            "{kind}"
        );
    }
}

/// Corrupt containers do not become worldspace-wide skinned candidates.
#[test]
fn corrupt_container_is_not_potentially_skinned() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    std::fs::write(dir.path().join("meshes/blocker.glb"), b"corrupt").unwrap();
    let mut geometry = GeometryCache::new(dir.path()).unwrap();
    geometry.fingerprint("meshes/blocker.glb");
    assert!(!geometry.potential_skins["meshes/blocker.glb"]);
}

/// A replacement between fingerprint and decode cannot publish a mask under stale bytes.
#[test]
fn changed_model_rejects_fingerprint_decode_race() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut geometry = GeometryCache::new(dir.path()).unwrap();
    geometry.fingerprint("meshes/blocker.glb");
    std::fs::write(dir.path().join("meshes/blocker.glb"), b"replacement").unwrap();
    assert!(geometry.get("meshes/blocker.glb").is_none());
    assert!(geometry.source_changed);
}

/// Cancelled bakes leave an existing archive untouched.
#[test]
fn cancelled_bake_preserves_output() {
    let dir = tempfile::tempdir().unwrap();
    let conn = fixture(dir.path());
    let output = dir.path().join("cancelled.rkyv");
    std::fs::write(&output, b"previous").unwrap();
    let cancellation = crate::pipeline::Cancellation::new();
    cancellation.cancel();
    assert!(
        write_grass_cache_with_cancel(
            &conn,
            dir.path(),
            &dir.path().join("terrain.rkyv"),
            None,
            &output,
            &cancellation
        )
        .is_err()
    );
    assert_eq!(std::fs::read(output).unwrap(), b"previous");
}
