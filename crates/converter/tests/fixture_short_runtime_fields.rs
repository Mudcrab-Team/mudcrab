//! Unusable DATA components are omitted with diagnostics; readable GRAS scalars survive.

mod common;

use converter::{AssetPipeline, config::RecordReader};
use dummy_content::layout;
use rusqlite::Connection;
use std::{fs, path::Path};

/// Writes a Skyrim SE record with explicit independent subrecord bytes.
fn record(tag: &[u8; 4], id: u32, fields: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let mut payload = Vec::new();
    for (name, bytes) in fields {
        payload.extend_from_slice(*name);
        payload.extend_from_slice(&u16::try_from(bytes.len()).unwrap().to_le_bytes());
        payload.extend_from_slice(bytes);
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(tag);
    bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&id.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&44u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Wraps the added references with the dummy world's existing WRLD/CELL parent context.
fn group(label: u32, kind: i32, content: &[u8]) -> Vec<u8> {
    let mut bytes = b"GRUP".to_vec();
    bytes.extend_from_slice(&u32::try_from(content.len() + 24).unwrap().to_le_bytes());
    bytes.extend_from_slice(&label.to_le_bytes());
    bytes.extend_from_slice(&kind.to_le_bytes());
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(content);
    bytes
}

/// Builds missing, short and valid records for each projection beside the stock usable world.
fn generate(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm,lodsettings").unwrap(),
    )
    .unwrap();
    let mut grass_data = [0u8; 32];
    grass_data[..3].copy_from_slice(&[67, 11, 73]);
    grass_data[4..6].copy_from_slice(&512u16.to_le_bytes());
    grass_data[8..12].copy_from_slice(&3u32.to_le_bytes());
    for (offset, value) in [(12, 13.5f32), (16, 2.25), (20, 0.45), (24, 4.75)] {
        grass_data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    grass_data[28] = 5;
    let transform: Vec<u8> = [129.5f32, 241.25, 37.75, 0.13, -0.24, 0.47]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    let mut lighting = [0u8; 92];
    lighting[..3].copy_from_slice(&[17, 41, 97]);
    let path = data.join("Skyrim.esm");
    let mut plugin = fs::read(&path).unwrap();
    for (id, data) in [
        (0x8001, None),
        (0x8002, Some(&grass_data[..3])),
        (0x8003, Some(grass_data.as_slice())),
    ] {
        let mut fields = vec![
            (b"EDID", b"SyntheticGrass\0".as_slice()),
            (b"MODL", b"generated.nif\0".as_slice()),
        ];
        if let Some(data) = data {
            fields.push((b"DATA", data));
        }
        plugin.extend(record(b"GRAS", id, &fields));
    }
    for (id, data) in [
        (0x8021, None),
        (0x8022, Some(&lighting[..63])),
        (0x8023, Some(lighting.as_slice())),
    ] {
        let mut fields = vec![(b"EDID", b"SyntheticLighting\0".as_slice())];
        if let Some(data) = data {
            fields.push((b"DATA", data));
        }
        plugin.extend(record(b"LGTM", id, &fields));
    }
    let base = 3u32.to_le_bytes();
    let mut references = Vec::new();
    for (id, data) in [
        (0x8011, None),
        (0x8012, Some(&transform[..20])),
        (0x8013, Some(transform.as_slice())),
    ] {
        let mut fields = vec![(b"NAME", base.as_slice())];
        if let Some(data) = data {
            fields.push((b"DATA", data));
        }
        references.extend(record(b"REFR", id, &fields));
    }
    plugin.extend(group(1, 1, &group(0x10, 8, &references)));
    fs::write(path, plugin).unwrap();
}

/// All three bad fields warn, preserve canonical data, and leave valid neighboring rows intact.
#[tokio::test]
async fn short_fields_publish_usable_neighbors_and_report_each_omission() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    generate(&data);
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let output = directory.path().join(format!("{reader:?}"));
        let mut config = common::cpu_lod_config(&data, &output);
        config.record_reader = reader;
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
        let report = AssetPipeline::run_async(config, sender).await.unwrap();
        drain.await.unwrap();
        let db = Connection::open(output.join("skyrim_world.db")).unwrap();
        let omissions: Vec<(String, u32)> = db.prepare("SELECT category,count FROM projection_omission_stats WHERE category IN ('grass_unusable_data','reference_unusable_data','lighting_template_unusable_data') ORDER BY category").unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?))).unwrap().collect::<Result<_,_>>().unwrap();
        assert_eq!(
            omissions,
            vec![
                ("grass_unusable_data".into(), 2),
                ("lighting_template_unusable_data".into(), 2),
                ("reference_unusable_data".into(), 2)
            ],
            "{reader:?}: each missing/short field must reach the stored report"
        );
        let short_reference_reason: String = db
            .query_row(
                "SELECT reason FROM projection_omission_samples WHERE category='reference_unusable_data' AND form_id=32786",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let extent = match reader {
            RecordReader::Legacy => "20 bytes in the runtime projection",
            RecordReader::Inhouse => "unavailable in the runtime projection",
        };
        assert!(
            short_reference_reason.contains(extent),
            "{reader:?}: {short_reference_reason}"
        );
        if reader == RecordReader::Inhouse {
            // The typed decoder dropped this rejected field before runtime adaptation;
            // its original twenty-byte DATA remains in source provenance, not the view.
            let payload: Vec<u8> = db
                .query_row(
                    "SELECT payload FROM inhouse_source_records WHERE form_id=32786",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(payload.windows(6).any(|header| header == b"DATA\x14\x00"));
        }
        let missing_grass: u32 = db.query_row("SELECT count(*) FROM grass_types WHERE id=32769 AND density IS NULL AND min_slope IS NULL AND max_slope IS NULL AND units_from_water IS NULL AND water_comparison IS NULL AND position_range IS NULL AND height_range IS NULL AND color_range IS NULL AND wave_period IS NULL AND flags IS NULL AND model_path='meshes/generated.glb'", [], |row| row.get(0)).unwrap();
        assert_eq!(
            missing_grass, 1,
            "grass model/identity survive without made-up DATA"
        );
        let short_grass: (u32, u32, u32) = db
            .query_row(
                "SELECT density,min_slope,max_slope FROM grass_types WHERE id=32770",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            short_grass,
            (67, 11, 73),
            "{reader:?}: authored scalar prefix survives"
        );
        let unavailable_grass_tail: u32 = db.query_row("SELECT count(*) FROM grass_types WHERE id=32770 AND units_from_water IS NULL AND water_comparison IS NULL AND position_range IS NULL AND height_range IS NULL AND color_range IS NULL AND wave_period IS NULL AND flags IS NULL AND model_path='meshes/generated.glb'", [], |row| row.get(0)).unwrap();
        assert_eq!(
            unavailable_grass_tail, 1,
            "{reader:?}: absent scalar spans remain NULL"
        );
        let valid_grass: (u32,u32,u32,f32,u32) = db.query_row("SELECT density,min_slope,max_slope,height_range,flags FROM grass_types WHERE id=32771", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).unwrap();
        assert_eq!(valid_grass, (67, 11, 73, 2.25, 5));
        let unusable_placements: u32 = db
            .query_row(
                "SELECT count(*) FROM \"references\" WHERE id IN (32785,32786)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            unusable_placements, 0,
            "short DATA must never publish an invented placement"
        );
        let unusable_spatial: u32 = db
            .query_row(
                "SELECT count(*) FROM exterior_spatial WHERE id IN (32785,32786)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unusable_spatial, 0);
        let valid_placement: (f32, f32, f32) = db
            .query_row(
                "SELECT pos_x,pos_y,pos_z FROM \"references\" WHERE id=32787",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(valid_placement, (129.5, 241.25, 37.75));
        let null_lighting: u32 = db.query_row("SELECT count(*) FROM lighting_templates WHERE id IN (32801,32802) AND data_size IS NULL AND ambient_r IS NULL AND ambient_g IS NULL AND ambient_b IS NULL AND fog_near IS NULL AND fog_far IS NULL", [], |row| row.get(0)).unwrap();
        assert_eq!(
            null_lighting, 2,
            "LGTM identity survives without DATA lighting"
        );
        let valid_lighting: (u32, u32, u32) = db
            .query_row(
                "SELECT ambient_r,ambient_g,ambient_b FROM lighting_templates WHERE id=32803",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(valid_lighting, (17, 41, 97));
        let canonical: u32 = db.query_row("SELECT count(*) FROM records WHERE form_id IN (32769,32770,32771,32785,32786,32787,32801,32802,32803)", [], |row| row.get(0)).unwrap();
        assert_eq!(canonical, 9, "canonical malformed records remain available");
        assert!(output.join("meshes/generated.glb").is_file());
        assert!(output.join("cell_cache.rkyv").is_file());
        assert_eq!(report.skipped, 0);
        assert!(!report.complete);
        for category in [
            "grass_unusable_data",
            "reference_unusable_data",
            "lighting_template_unusable_data",
        ] {
            assert_eq!(
                report
                    .warnings
                    .iter()
                    .filter(|line| line.contains(category))
                    .count(),
                1,
                "{reader:?}: one bounded warning per category"
            );
        }
        let integration: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("integration-report.json")).unwrap())
                .unwrap();
        assert_eq!(
            integration["projection_omissions"]["reference_unusable_data"]["samples"][0]["form_id"],
            32785
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["complete"], false);
        assert!(manifest["failures"].as_object().unwrap().is_empty());
    }
}

/// A malformed subset update removes its placement and dependents, retaining usable neighbors.
#[test]
fn short_reference_subset_update_removes_the_old_dependent_rows() {
    use converter::esm::exporter::{create_tables, insert_reference};
    let db = Connection::open_in_memory().unwrap();
    create_tables(&db).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    let transform: Vec<u8> = [21.0f32, 39.0, 8.0, 0.13, 0.24, 0.37]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    let good = vec![
        (b"NAME".to_vec(), 3u32.to_le_bytes().to_vec()),
        (b"DATA".to_vec(), transform),
    ];
    insert_reference(
        &tx,
        0x9001,
        0x10,
        Some((Some(0), Some(0), Some(1))),
        0,
        &good,
    )
    .unwrap();
    tx.execute_batch(
        "INSERT INTO reference_details(reference_id) VALUES (36865),(36866);
         INSERT INTO reference_linked_refs(reference_id,position,linked_ref_id) VALUES (36865,0,36866),(36866,0,36865);
         INSERT INTO reference_activate_parents(reference_id,position,parent_ref_id) VALUES (36865,0,36866),(36866,0,36867);
         INSERT INTO door_links(ref_id,destination_ref_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,destination_cell_id) VALUES
            (36865,36866,1,2,3,0,0,0,16),(36866,36865,1,2,3,0,0,0,16),(36867,36868,1,2,3,0,0,0,16);
         INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z) VALUES
            (36865,16,1,2,3,0,0,0,4,5,6),(36866,16,1,2,3,0,0,0,4,5,6);
         INSERT INTO room_portals(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_z,room_a,room_b) VALUES
            (36865,16,1,2,3,0,0,0,4,5,36865,36866),(36866,16,1,2,3,0,0,0,4,5,36865,36866);
         INSERT INTO room_links(room_ref,linked_ref) VALUES (36865,36866),(36866,36865),(36866,36867);
         INSERT INTO reference_rooms(reference_id,room_ref,contained) VALUES (36865,36866,1),(36866,36865,1),(36866,36866,1);",
    ).unwrap();
    let bad = vec![
        (b"NAME".to_vec(), 3u32.to_le_bytes().to_vec()),
        (b"DATA".to_vec(), vec![0; 20]),
    ];
    insert_reference(
        &tx,
        0x9001,
        0x10,
        Some((Some(0), Some(0), Some(1))),
        0,
        &bad,
    )
    .unwrap();
    tx.commit().unwrap();
    let retained: u32 = db
        .query_row(
            "SELECT count(*) FROM exterior_spatial WHERE id=36865",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        retained, 0,
        "unusable new DATA must clear the old spatial projection"
    );
    let placements: u32 = db
        .query_row(
            "SELECT count(*) FROM \"references\" WHERE id=36865",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(placements, 0);
    for table in [
        "reference_details",
        "reference_linked_refs",
        "reference_activate_parents",
        "reference_rooms",
    ] {
        let owned: u32 = db
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE reference_id=36865"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owned, 0, "stale owned rows in {table}");
        let neighbor: u32 = db
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE reference_id=36866"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(neighbor, 1, "usable neighbor in {table}");
    }
    let stale_doors: u32 = db
        .query_row(
            "SELECT count(*) FROM door_links WHERE ref_id=36865 OR destination_ref_id=36865",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale_doors, 0);
    let neighbor_doors: u32 = db
        .query_row(
            "SELECT count(*) FROM door_links WHERE ref_id=36867",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(neighbor_doors, 1);
    let stale_rooms: u32 = db
        .query_row(
            "SELECT count(*) FROM room_bounds WHERE ref_id=36865",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale_rooms, 0);
    let portal_sides: (Option<u32>, Option<u32>) = db
        .query_row(
            "SELECT room_a,room_b FROM room_portals WHERE ref_id=36866",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(portal_sides, (None, Some(36866)));
    let room_links: u32 = db
        .query_row(
            "SELECT count(*) FROM room_links WHERE room_ref=36865 OR linked_ref=36865",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(room_links, 0);
}

/// Room publication must not reconstruct an omitted marker as an origin from canonical DATA.
#[test]
fn short_room_marker_is_not_recreated_by_the_room_projection() {
    use converter::esm::{exporter::create_tables, records::RawRecord, rooms};
    use std::collections::{HashMap, HashSet};
    let db = Connection::open_in_memory().unwrap();
    create_tables(&db).unwrap();
    let mut bounds = vec![0; 32];
    for (offset, value) in [(0, 400.0f32), (4, 300.0), (8, 200.0)] {
        bounds[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bounds[28..32].copy_from_slice(&1u32.to_le_bytes());
    let transform: Vec<u8> = [111.0f32, 222.0, 33.0, 0.13, 0.24, 0.37]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    let mut master = HashMap::new();
    for (id, data) in [
        (0x9101, None),
        (0x9102, Some(&transform[..20])),
        (0x9103, Some(transform.as_slice())),
    ] {
        let mut subrecords = vec![
            (
                b"NAME".to_vec(),
                rooms::ROOM_MARKER_BASE.to_le_bytes().to_vec(),
            ),
            (b"XPRM".to_vec(), bounds.clone()),
        ];
        if let Some(data) = data {
            subrecords.push((b"DATA".to_vec(), data.to_vec()));
        }
        master.insert(
            id,
            RawRecord {
                form_id: id,
                record_type: *b"REFR",
                flags: 0,
                subrecords,
                cell_form_id: Some(0x20),
                worldspace_form_id: None,
                load_order: 0,
            },
        );
    }
    let tx = db.unchecked_transaction().unwrap();
    let report = rooms::export_room_graph(&tx, &master, &HashSet::from([0x20])).unwrap();
    assert_eq!(report.rooms, 1);
    let complete: bool = tx
        .query_row(
            "SELECT complete FROM room_graph_cells WHERE cell_id=0x20",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !complete,
        "omitted room markers leave the surviving graph incomplete"
    );
    let rows: Vec<(u32, f32, f32, f32)> = tx
        .prepare("SELECT ref_id,pos_x,pos_y,pos_z FROM room_bounds ORDER BY ref_id")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows, vec![(0x9103, 111.0, 222.0, 33.0)]);
}
