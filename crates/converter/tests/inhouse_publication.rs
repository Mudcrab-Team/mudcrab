//! Synthetic publication checks for the selectable schema frontend.
use converter::{AssetPipeline, PipelineConfig, config::RecordReader};
use dummy_content::{Entry, bsa, esm, layout};
use rusqlite::Connection;
use std::{fs, path::Path};

/// Frame independent supplemental records beside the dummy-content world.
fn record(tag: &[u8; 4], id: u32, flags: u32, fields: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut payload = Vec::new();
    for (signature, bytes) in fields {
        payload.extend_from_slice(*signature);
        payload.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(bytes);
    }
    framed_record(tag, id, flags, &payload)
}

/// Preserve an established record boundary even when its subrecord is broken.
fn framed_record(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_vec();
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(&id.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&44u16.to_le_bytes());
    bytes.extend_from_slice(&[0; 2]);
    bytes.extend_from_slice(payload);
    bytes
}

/// Put supplemental records in ordinary world/cell groups for real cache ownership.
fn group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
    let mut header = vec![0; 24];
    header[..4].copy_from_slice(b"GRUP");
    header[4..8].copy_from_slice(&((24 + payload.len()) as u32).to_le_bytes());
    header[8..12].copy_from_slice(&label.to_le_bytes());
    header[12..16].copy_from_slice(&kind.to_le_bytes());
    header.extend_from_slice(payload);
    header
}

/// NUL-terminated author-written fixture text.
fn text(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// Prepare ordinary runtime inputs plus localized names in a synthetic BSA.
fn generate(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let plugin = esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    };
    let light = esm::Light {
        model_path: None,
        enable_parent: Some((0x12, 0xD3_B2_A1_01)),
        ..esm::PRESET_LIGHT
    };
    let mut bytes = esm::plugin_with_lights(&plugin, &light).unwrap();
    for id in [0x900, 0x902] {
        bytes.extend(record(
            b"STAT",
            id,
            0,
            &[
                (b"EDID", text(&format!("Neighbor{id}"))),
                (b"MODL", text(layout::GENERATED_MODEL_PATH)),
            ],
        ));
        if id == 0x900 {
            // The framed record ends safely; its MODL claims more bytes than it holds.
            let mut damaged = record(b"STAT", 0x901, 0, &[(b"EDID", text("DamagedModel"))]);
            let mut payload = damaged.split_off(24);
            payload.extend_from_slice(b"MODL\xff\xffbad");
            bytes.extend(framed_record(b"STAT", 0x901, 0, &payload));
        }
    }
    let mut slots: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"EDID", text("DistinctTextureSlots"))];
    for signature in [
        b"TX00", b"TX01", b"TX02", b"TX03", b"TX04", b"TX05", b"TX06", b"TX07",
    ] {
        slots.push((
            signature,
            text(&format!("textures/slot{}.dds", char::from(signature[3]))),
        ));
    }
    bytes.extend(record(b"TXST", 0x903, 0, &slots));
    let mut grid = Vec::new();
    grid.extend_from_slice(&22i32.to_le_bytes());
    grid.extend_from_slice(&23i32.to_le_bytes());
    grid.extend_from_slice(&0u32.to_le_bytes());
    let mut texture_only_cell = record(
        b"CELL",
        0xA00,
        0,
        &[
            (b"EDID", text("TextureOnlyCell")),
            (b"DATA", vec![0]),
            (b"XCLC", grid),
        ],
    );
    let mut overlay = 4u32.to_le_bytes().to_vec();
    overlay.extend_from_slice(&[2, 0]);
    overlay.extend_from_slice(&3u16.to_le_bytes());
    let mut weight = vec![0; 4];
    weight.extend_from_slice(&0.625f32.to_le_bytes());
    let mut texture_only_land = record(
        b"LAND",
        0xA01,
        0,
        &[
            (b"DATA", 0xCu32.to_le_bytes().to_vec()),
            (b"VCLR", vec![95; 33 * 33 * 3]),
            (b"ATXT", overlay.clone()),
            (b"VTXT", weight),
        ],
    );
    let mut invalid_weight = vec![0; 4];
    invalid_weight.extend_from_slice(&1.25f32.to_le_bytes());
    texture_only_land.extend(record(
        b"LAND",
        0xA02,
        0x800,
        &[
            (b"DATA", 0x4u32.to_le_bytes().to_vec()),
            (b"ATXT", overlay),
            (b"VTXT", invalid_weight),
        ],
    ));
    texture_only_cell.extend(group(8, 0xA00, &texture_only_land));
    bytes.extend(group(1, 1, &texture_only_cell));
    bytes.extend(record(
        b"NPC_",
        0x904,
        0,
        &[
            (b"EDID", text("CP1252Name")),
            (b"RNAM", 10u32.to_le_bytes().to_vec()),
            (b"FULL", b"Kr\xe4he\0".to_vec()),
        ],
    ));
    bytes.extend(record(
        b"RACE",
        0x0001_3746,
        0,
        &[(b"EDID", text("SyntheticPlayerRace"))],
    ));
    for (id, editor_id, value) in [
        (0x0001_EC72, "fMoveCharWalkBase", 100.0f32),
        (0x000A_BEF6, "fJumpHeightMin", 76.8),
    ] {
        bytes.extend(record(
            b"GMST",
            id,
            0,
            &[
                (b"EDID", text(editor_id)),
                (b"DATA", value.to_le_bytes().to_vec()),
            ],
        ));
    }
    fs::write(data.join("Skyrim.esm"), bytes).unwrap();

    let mut header = Vec::new();
    header.extend_from_slice(&1.7f32.to_le_bytes());
    header.extend_from_slice(&4u32.to_le_bytes());
    header.extend_from_slice(&0x800u32.to_le_bytes());
    let mut names = record(
        b"TES4",
        0,
        0x80,
        &[
            (b"HEDR", header),
            (b"MAST", text("Skyrim.esm")),
            (b"DATA", vec![0; 8]),
        ],
    );
    for (id, name_id) in [(0x0100_0800, 17u32), (0x0100_0801, 99), (0x0100_0802, 18)] {
        names.extend(record(
            b"NPC_",
            id,
            0,
            &[
                (b"EDID", text(&format!("Localized{id}"))),
                (b"RNAM", 10u32.to_le_bytes().to_vec()),
                (b"FULL", name_id.to_le_bytes().to_vec()),
            ],
        ));
    }
    names.extend(record(
        b"ARMO",
        0x0100_0803,
        0,
        &[
            (b"EDID", text("UnusedDescription")),
            (b"FULL", 17u32.to_le_bytes().to_vec()),
            (b"DESC", 0x999u32.to_le_bytes().to_vec()),
        ],
    ));
    fs::write(data.join("Names.esp"), names).unwrap();
    let mut strings = Vec::new();
    strings.extend_from_slice(&2u32.to_le_bytes());
    let name = text("Named from archive");
    let cp_name = b"Localized Kr\xe4he\0";
    strings.extend_from_slice(&((name.len() + cp_name.len()) as u32).to_le_bytes());
    strings.extend_from_slice(&17u32.to_le_bytes());
    strings.extend_from_slice(&0u32.to_le_bytes());
    strings.extend_from_slice(&18u32.to_le_bytes());
    strings.extend_from_slice(&(name.len() as u32).to_le_bytes());
    strings.extend_from_slice(&name);
    strings.extend_from_slice(cp_name);
    fs::write(
        data.join("Names.bsa"),
        bsa::v105(
            &[
                Entry::new("Strings/Names_English.STRINGS", &strings),
                Entry::new("Strings/Names_English.DLSTRINGS", &{
                    let description = text("Description from DLSTRINGS");
                    let mut table = Vec::new();
                    table.extend(1u32.to_le_bytes());
                    table.extend((4 + description.len() as u32).to_le_bytes());
                    table.extend(0x999u32.to_le_bytes());
                    table.extend(0u32.to_le_bytes());
                    table.extend((description.len() as u32).to_le_bytes());
                    table.extend(description);
                    table
                }),
            ],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
}

/// Exercise the public pipeline all the way through runtime pack publication.
async fn convert(data: &Path, output: &Path, reader: RecordReader) -> converter::PipelineReport {
    let mut config = PipelineConfig::new(data, output);
    config.record_reader = reader;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    report
}

/// A broken record, missing localized ID and unused XESP bytes preserve valid neighbors.
#[tokio::test]
async fn inhouse_publishes_valid_neighbors_and_correct_typed_projections() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    let output = temp.path().join("inhouse");
    generate(&data);
    convert(&data, &output, RecordReader::Inhouse).await;
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in [0x900, 0x902] {
        assert_eq!(
            db.query_row("SELECT model_path FROM statics WHERE id=?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
            layout::GENERATED_MODEL_PATH
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM records WHERE form_id=2305",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT editor_id, model_path FROM statics WHERE id=2305",
            [],
            |row| { Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)) }
        )
        .unwrap(),
        ("DamagedModel".to_owned(), None)
    );
    let safe: Vec<u8> = db
        .query_row("SELECT data FROM records WHERE form_id=2305", [], |row| {
            row.get(0)
        })
        .unwrap();
    let safe =
        rkyv::from_bytes::<converter::esm::types::ArchivedRecordData, rkyv::rancor::Error>(&safe)
            .unwrap();
    assert_eq!(
        safe.subrecords
            .iter()
            .find(|field| field.tag == *b"EDID")
            .unwrap()
            .data,
        text("DamagedModel")
    );
    assert!(!safe.subrecords.iter().any(|field| field.tag == *b"MODL"));
    let slots: Vec<String> = db.query_row("SELECT mask_path, glow_path, height_path, environment_path, detail_path, specular_path FROM texture_sets WHERE id=2307", [], |row| (0..6).map(|i| row.get(i)).collect()).unwrap();
    assert_eq!(
        slots,
        [2, 3, 4, 5, 6, 7].map(|slot| format!("textures/slot{slot}.dds"))
    );
    assert_eq!(
        db.query_row(
            "SELECT enable_parent_flags FROM \"references\" WHERE enable_parent_id=18",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    let source: Vec<u8> = db.query_row("SELECT records.data FROM records JOIN \"references\" ON records.form_id=\"references\".id WHERE enable_parent_id=18", [], |row| row.get(0)).unwrap();
    let canonical =
        rkyv::from_bytes::<converter::esm::types::ArchivedRecordData, rkyv::rancor::Error>(&source)
            .unwrap();
    assert_eq!(
        canonical
            .subrecords
            .iter()
            .find(|field| field.tag == *b"XESP")
            .unwrap()
            .data,
        [18, 0, 0, 0, 1, 0xA1, 0xB2, 0xD3]
    );
    let source: Vec<u8> = db.query_row("SELECT payload FROM inhouse_source_records JOIN \"references\" ON form_id=\"references\".id WHERE enable_parent_id=18", [], |row| row.get(0)).unwrap();
    assert!(
        source
            .windows(8)
            .any(|bytes| bytes == [18, 0, 0, 0, 1, 0xA1, 0xB2, 0xD3])
    );
    assert_eq!(
        db.query_row("SELECT full_name FROM npcs WHERE id=16779264", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "Named from archive"
    );
    assert_eq!(
        db.query_row("SELECT full_name FROM npcs WHERE id=2308", [], |row| row
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "Krähe"
    );
    assert_eq!(
        db.query_row("SELECT full_name FROM npcs WHERE id=16779266", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "Localized Krähe"
    );
    assert_eq!(
        db.query_row("SELECT full_name FROM npcs WHERE id=16779265", [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .unwrap(),
        None
    );
    let diagnostic: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert!(
        diagnostic["decoder"]["skyrim.esm"]["skipped_fields"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(diagnostic["adapter"]["names.esp"][0].as_u64().unwrap(), 1);
    let armor: Vec<u8> = db
        .query_row(
            "SELECT data FROM records WHERE form_id=16779267",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let armor =
        rkyv::from_bytes::<converter::esm::types::ArchivedRecordData, rkyv::rancor::Error>(&armor)
            .unwrap();
    assert_eq!(
        armor
            .subrecords
            .iter()
            .find(|field| field.tag == *b"DESC")
            .unwrap()
            .data,
        text("Description from DLSTRINGS")
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(output.join("record-reader.json")).unwrap()
        )
        .unwrap()["mode"],
        "inhouse"
    );
    assert!(output.join("cell_cache.rkyv").is_file());
    assert_eq!(
        db.query_row(
            "SELECT length(heightmap), length(vclr) FROM land WHERE cell_id=2560",
            [],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?))
        )
        .unwrap(),
        (0, 3267)
    );
    assert_eq!(
        db.query_row(
            "SELECT record_type,cell_id FROM records WHERE form_id=2561",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
        )
        .unwrap(),
        ("LAND".into(), 2560)
    );
    let original_texture_land: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=2561",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        original_texture_land
            .windows(4)
            .any(|bytes| bytes == b"VCLR")
    );
    assert!(
        !original_texture_land
            .windows(4)
            .any(|bytes| bytes == b"VHGT")
    );
    let cache = rkyv::from_bytes::<converter::shared::CellCache, rkyv::rancor::Error>(
        &fs::read(output.join("cell_cache.rkyv")).unwrap(),
    )
    .unwrap();
    let texture_cell = cache
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0xA00)
        .unwrap();
    assert_eq!(texture_cell.heights, vec![0.0; 33 * 33]);
    assert_eq!(texture_cell.vertex_colors, vec![95; 33 * 33 * 3]);
    let overlay = texture_cell
        .layers
        .iter()
        .find(|layer| !layer.is_base)
        .unwrap();
    assert_eq!(
        (overlay.texture_form_id, overlay.quadrant, overlay.layer),
        (4, 2, 3)
    );
    assert_eq!(overlay.weights[0].opacity, 0.625);
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM records WHERE form_id=2562",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM inhouse_source_records WHERE form_id=2562",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert!(
        diagnostic["decoder"]["skyrim.esm"]["skipped_records"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(
        diagnostic["decoder"]["skyrim.esm"]["first_by_category"]["record"]
            .as_str()
            .unwrap()
            .contains("LAND")
    );
    assert!(!output.join("vfs").exists());
    // The established tool must still decode records.data as rkyv after this export.
    drop(db);
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_movement-profile-annotate"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let annotated = command
        .arg(output.join("skyrim_world.db"))
        .output()
        .unwrap();
    assert!(
        annotated.status.success(),
        "{}",
        String::from_utf8_lossy(&annotated.stderr)
    );
}

/// A reader change invalidates the shared cache proof while legacy hashes retain compatibility.
#[test]
fn reader_identity_separates_configuration_proofs() {
    let mut config = PipelineConfig::new("Data", "output");
    let legacy = converter::cache::configuration_hash(&config).unwrap();
    assert_eq!(config.record_reader, RecordReader::Legacy);
    config.record_reader = RecordReader::Inhouse;
    assert_ne!(
        legacy,
        converter::cache::configuration_hash(&config).unwrap()
    );
    let identity = converter::esm::inhouse::reader_identity(config.record_reader);
    assert_eq!(
        identity["schema_sha256"],
        converter::cache::hash_bytes(converter::records::SCHEMA_BYTES)
    );
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("staged.ktx2");
    fs::write(&path, b"synthetic staged asset").unwrap();
    let staged = converter::cache::StagedOutput {
        schema_version: converter::cache::CONVERTER_SCHEMA_VERSION,
        configuration_hash: legacy.clone(),
        source_hash: "source".into(),
        output_size: 22,
        output_hash: converter::cache::hash_file(&path).unwrap(),
    };
    assert!(staged.is_current(&path, "source", &legacy));
    assert!(!staged.is_current(
        &path,
        "source",
        &converter::cache::configuration_hash(&config).unwrap()
    ));
}

/// Malformed later candidates must not erase usable movement or terrain winners.
#[tokio::test]
async fn inhouse_bad_overrides_preserve_prior_movement_and_authored_heights() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    let output = temp.path().join("inhouse");
    generate(&data);
    let vhgt = |offset: f32| {
        let mut bytes = offset.to_le_bytes().to_vec();
        bytes.extend(vec![0; 33 * 33 + 3]);
        bytes
    };
    let cell = |id: u32, grid_x: i32| {
        let mut grid = grid_x.to_le_bytes().to_vec();
        grid.extend_from_slice(&30i32.to_le_bytes());
        grid.extend_from_slice(&0u32.to_le_bytes());
        record(b"CELL", id, 0, &[(b"DATA", vec![0]), (b"XCLC", grid)])
    };
    let mut baseline = fs::read(data.join("Skyrim.esm")).unwrap();
    for (id, offset) in [
        (0xB00, 3.0f32),
        (0xB10, 5.0),
        (0xB20, 7.0),
        (0xB30, 9.0),
        (0xB40, 11.0),
    ] {
        let mut children = cell(id, id as i32);
        children.extend(group(
            8,
            id,
            &record(b"LAND", id + 1, 0, &[(b"VHGT", vhgt(offset))]),
        ));
        baseline.extend(group(1, 1, &children));
    }
    fs::write(data.join("Skyrim.esm"), baseline).unwrap();
    for (name, bad_value) in [("BadNegative.esp", -1.0f32), ("BadNonfinite.esp", f32::NAN)] {
        let mut header = 1.7f32.to_le_bytes().to_vec();
        let record_count = if name == "BadNegative.esp" { 17u32 } else { 4 };
        header.extend_from_slice(&record_count.to_le_bytes());
        header.extend_from_slice(&0x800u32.to_le_bytes());
        let mut patch = record(
            b"TES4",
            0,
            0,
            &[
                (b"HEDR", header),
                (b"MAST", text("Skyrim.esm")),
                (b"DATA", vec![0; 8]),
            ],
        );
        let mut speeds = [100.0f32; 11];
        speeds[4] = bad_value;
        patch.extend(record(
            b"MOVT",
            0x0003_580D,
            0,
            &[
                (b"EDID", text("NPC_Default_MT")),
                (
                    b"SPED",
                    speeds
                        .iter()
                        .flat_map(|value| value.to_le_bytes())
                        .collect(),
                ),
            ],
        ));
        for (id, editor_id) in [
            (0x0001_EC72, "fMoveCharWalkBase"),
            (0x000A_BEF6, "fJumpHeightMin"),
        ] {
            patch.extend(record(
                b"GMST",
                id,
                0,
                &[
                    (b"EDID", text(editor_id)),
                    (b"DATA", bad_value.to_le_bytes().to_vec()),
                ],
            ));
        }
        patch.extend(record(
            b"STAT",
            0x0100_0810,
            0,
            &[
                (b"EDID", text(name)),
                (b"MODL", text(layout::GENERATED_MODEL_PATH)),
            ],
        ));
        if name == "BadNegative.esp" {
            let mut worlds = Vec::new();
            for (id, height) in [
                (0xB00, vec![0; 10]),
                (0xB10, vhgt(f32::NAN)),
                (0xB20, vhgt(99.0)),
            ] {
                let mut fields: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"VHGT", height)];
                if id == 0xB20 {
                    let mut overlay = 4u32.to_le_bytes().to_vec();
                    overlay.extend_from_slice(&[0, 0, 0, 0]);
                    let mut weight = vec![0; 4];
                    weight.extend_from_slice(&1.25f32.to_le_bytes());
                    fields.extend([(b"ATXT", overlay), (b"VTXT", weight)]);
                }
                let mut children = cell(id, id as i32);
                children.extend(group(8, id, &record(b"LAND", id + 1, 0, &fields)));
                worlds.extend(children);
            }
            // The record boundary is intact, but authored VHGT framing is truncated.
            let mut fractured = b"VHGT".to_vec();
            fractured.extend_from_slice(&1096u16.to_le_bytes());
            fractured.extend_from_slice(&[0; 10]);
            let mut truncated_cell = cell(0xB30, 0xB30);
            truncated_cell.extend(group(
                8,
                0xB30,
                &framed_record(b"LAND", 0xB31, 0, &fractured),
            ));
            worlds.extend(truncated_cell);
            // A recognized authored height field after VCLR is invalid native order,
            // rather than a genuinely heightless record that may use default heights.
            let mut unordered_cell = cell(0xB40, 0xB40);
            unordered_cell.extend(group(
                8,
                0xB40,
                &record(
                    b"LAND",
                    0xB41,
                    0,
                    &[(b"VCLR", vec![123; 33 * 33 * 3]), (b"VHGT", vhgt(999.0))],
                ),
            ));
            worlds.extend(unordered_cell);
            // A new genuinely heightless LAND remains usable beside rejected overrides.
            let mut new_cell = cell(0x0100_0D00, 50);
            new_cell.extend(group(
                8,
                0x0100_0D00,
                &record(
                    b"LAND",
                    0x0100_0D01,
                    0,
                    &[
                        (b"DATA", 0x8u32.to_le_bytes().to_vec()),
                        (b"VCLR", vec![111; 33 * 33 * 3]),
                    ],
                ),
            ));
            worlds.extend(new_cell);
            patch.extend(group(1, 1, &worlds));
            // Real deletion retains normal override behavior.
            patch.extend(record(b"STAT", 0x900, 0x20, &[]));
        }
        fs::write(data.join(name), patch).unwrap();
    }
    convert(&data, &output, RecordReader::Inhouse).await;
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let movement = db
        .query_row(
            "SELECT forward_walk,forward_run,load_order FROM movement_types WHERE id=219149",
            [],
            |row| {
                Ok((
                    row.get::<_, f32>(0)?,
                    row.get::<_, f32>(1)?,
                    row.get::<_, u32>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(movement, (80.1, 370.0, 0));
    // Finite negative GMST values are valid under the existing setting contract;
    // the later NaN patch must preserve this usable prior override.
    for editor_id in ["fMoveCharWalkBase", "fJumpHeightMin"] {
        assert_eq!(
            db.query_row(
                "SELECT value,load_order FROM movement_game_settings WHERE editor_id=?1",
                [editor_id],
                |row| Ok((row.get::<_, f32>(0)?, row.get::<_, u32>(1)?))
            )
            .unwrap(),
            (-1.0, 1)
        );
    }
    let cache = rkyv::from_bytes::<converter::shared::CellCache, rkyv::rancor::Error>(
        &fs::read(output.join("cell_cache.rkyv")).unwrap(),
    )
    .unwrap();
    for (id, offset) in [
        (0xB00, 3.0f32),
        (0xB10, 5.0),
        (0xB20, 7.0),
        (0xB30, 9.0),
        (0xB40, 11.0),
    ] {
        let retained = cache.cells.iter().find(|cell| cell.cell_id == id).unwrap();
        assert_eq!(retained.heights, vec![offset * 8.0; 33 * 33]);
        assert_eq!(
            db.query_row(
                "SELECT load_order FROM records WHERE form_id=?1",
                [id + 1],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT heightmap FROM land WHERE cell_id=?1", [id], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .unwrap(),
            vhgt(offset)
        );
    }
    for editor_id in ["Neighbor2306", "BadNegative.esp", "BadNonfinite.esp"] {
        assert_eq!(
            db.query_row(
                "SELECT model_path FROM statics WHERE editor_id=?1",
                [editor_id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            layout::GENERATED_MODEL_PATH
        );
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM statics WHERE id=2304", [], |row| row
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
    let heightless_cell = db
        .query_row(
            "SELECT cell_id FROM land WHERE length(heightmap)=0 AND vclr=?1",
            [vec![111u8; 33 * 33 * 3]],
            |row| row.get::<_, u32>(0),
        )
        .unwrap();
    let heightless = cache
        .cells
        .iter()
        .find(|cell| cell.cell_id == heightless_cell)
        .unwrap();
    assert_eq!(heightless.heights, vec![0.0; 33 * 33]);
    assert_eq!(heightless.vertex_colors, vec![111; 33 * 33 * 3]);
    assert!(cache.cells.iter().any(|cell| cell.cell_id == 0xA00));
}

/// Repeated metadata rebuilds retain the original asset reader identity and verified bytes.
#[tokio::test]
async fn metadata_reader_switch_preserves_retained_producer_identity() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let original = temp.path().join("legacy");
    convert(&data, &original, RecordReader::Legacy).await;
    let original_manifest: converter::cache::ConversionManifest =
        serde_json::from_slice(&fs::read(original.join("conversion-manifest.json")).unwrap())
            .unwrap();
    let mut source = original.clone();
    for name in ["inhouse-first", "inhouse-second"] {
        let output = temp.path().join(name);
        let mut config = PipelineConfig::new(&data, &output);
        config.record_reader = RecordReader::Inhouse;
        config.no_lod = true;
        config.cpu_jobs = 2;
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let report = AssetPipeline::rebuild_metadata_async(config, &source, tx)
            .await
            .unwrap();
        drain.await.unwrap();
        assert!(report.complete);
        assert!(
            report
                .notices
                .iter()
                .any(|notice| notice.contains("retained models and textures"))
        );
        let manifest: converter::cache::ConversionManifest =
            serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(
            manifest.retained_asset_configuration_hash.as_ref(),
            Some(&original_manifest.configuration_hash)
        );
        let provenance: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap())
                .unwrap();
        assert_eq!(provenance["record_reader"]["mode"], "inhouse");
        assert_eq!(provenance["retained_asset_record_reader"]["mode"], "legacy");
        for entry in original_manifest.entries.values() {
            assert_eq!(
                converter::cache::hash_file(&output.join(&entry.output)).unwrap(),
                entry.output_hash
            );
        }
        source = output;
    }
    let stamp = source.join("record-reader.json");
    let mut identity: serde_json::Value =
        serde_json::from_slice(&fs::read(&stamp).unwrap()).unwrap();
    identity["schema_sha256"] = serde_json::json!("obsolete-schema-identity");
    fs::write(&stamp, serde_json::to_vec(&identity).unwrap()).unwrap();
    let refused = temp.path().join("obsolete-reader-refused");
    let mut config = PipelineConfig::new(&data, &refused);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let error = AssetPipeline::rebuild_metadata_async(config, &source, tx)
        .await
        .unwrap_err();
    drain.await.unwrap();
    assert!(
        error
            .to_string()
            .contains("unsupported source reader producer identity")
    );
    assert!(!refused.exists());
}
