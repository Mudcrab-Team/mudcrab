//! World/navigation values, modded identities and actual malformed-mod publication.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::load_order::LoadOrder,
    records::{DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{
    esm,
    inhouse_magic::{record, subrecord},
    inhouse_world_extras as world, layout,
};
use rusqlite::Connection;
use std::{fs, path::PathBuf};

/// A native generated world supplies independently encoded base records and target kinds.
struct Fixture {
    directory: tempfile::TempDir,
    header: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Append family source records to a minimal usable engine world.
    fn new(records: &[u8]) -> Self {
        let mut bytes = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        let length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let header = bytes[24..24 + length].to_vec();
        bytes.extend(records);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Skyrim.esm");
        fs::write(&path, bytes).unwrap();
        Self {
            directory,
            header,
            paths: vec![path],
        }
    }

    /// Full/light patches contain one master even when their global slot differs.
    fn patch(&mut self, name: &str, light: bool, records: &[u8]) {
        let mut header = self.header.clone();
        header.extend(subrecord(b"MAST", b"Skyrim.esm\0"));
        header.extend(subrecord(b"DATA", &[0xA7; 8]));
        let mut bytes = record(b"TES4", 0, if light { 0x200 } else { 0 }, &header);
        bytes.extend(records);
        let path = self.directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        self.paths.push(path);
    }

    /// Public APIs exercise compressed scanning, identity, selection and catalog validation.
    fn read(&self) -> ReadResult {
        read_plugins(&self.paths, &LoadOrder::read(&self.paths).unwrap()).unwrap()
    }
}

/// Retrieve one field occurrence by physical signature.
fn field<'a>(record: &'a DecodedRecord, signature: &[u8; 4]) -> &'a Value {
    &record
        .fields
        .iter()
        .find(|field| &field.signature == signature)
        .unwrap()
        .value
}

/// Retrieve a named primitive/array/structure member.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected structure: {value:?}")
    };
    &members.iter().find(|(key, _)| key == name).unwrap().1
}

/// Access one decoded array element while preserving its order.
fn item(value: &Value, index: usize) -> &Value {
    let Value::Array(items) = value else {
        panic!("expected array: {value:?}")
    };
    &items[index]
}

/// Counted geometry, conditional islands, ordered groups and signed bytes retain exact values.
#[test]
fn world_families_decode_complete_asymmetric_values() {
    let result = Fixture::new(&world::records()).read();
    for (index, signature) in world::SIGNATURES.iter().enumerate() {
        let record = &result.records[&(world::FIRST_ID + index as u32)];
        assert_eq!(&record.record_type, signature);
        assert!(record.supported);
        assert!(record.rejected_fields.is_empty(), "{signature:?}");
    }
    let geometry = field(&result.records[&world::FIRST_ID], b"NVNM");
    assert_eq!(member(geometry, "vertices_count"), &Value::Unsigned(3));
    assert_eq!(
        member(item(member(geometry, "vertices"), 1), "z"),
        &Value::Float(-13.5)
    );
    let triangle = item(member(geometry, "triangles"), 0);
    assert_eq!(member(triangle, "edge_2_0"), &Value::Signed(-3));
    assert_eq!(member(triangle, "flags"), &Value::Unsigned(0x0645));
    assert_eq!(member(triangle, "cover_flags"), &Value::Unsigned(0xA753));
    assert_eq!(
        member(
            member(member(geometry, "pathing_cell"), "coordinates"),
            "grid_y"
        ),
        &Value::Signed(-3)
    );
    assert_eq!(
        member(
            member(member(geometry, "pathing_cell"), "coordinates"),
            "grid_x"
        ),
        &Value::Signed(17)
    );
    assert_eq!(
        member(item(member(geometry, "edge_links"), 0), "navmesh"),
        &Value::FormId(world::FIRST_ID)
    );
    assert_eq!(
        member(member(geometry, "grid"), "cell_1"),
        &Value::Array(vec![])
    );
    assert_eq!(
        item(member(member(geometry, "grid"), "cell_0"), 1),
        &Value::Signed(-3)
    );
    let navi = &result.records[&(world::FIRST_ID + 1)];
    let infos: Vec<_> = navi
        .fields
        .iter()
        .filter(|field| field.signature == *b"NVMI")
        .collect();
    assert_eq!(infos.len(), 2);
    assert_eq!(
        member(&infos[0].value, "preferred_merges"),
        &Value::Bytes(vec![0xA7, 0x53, 0xC1, 0x39])
    );
    assert_eq!(
        member(member(&infos[1].value, "pathing_cell"), "parent_cell"),
        &Value::FormId(0x10)
    );
    assert_eq!(
        member(member(member(&infos[1].value, "island"), "bounds"), "max_y"),
        &Value::Float(19.5)
    );
    let paths = field(navi, b"NVPP");
    assert_eq!(member(paths, "paths_count"), &Value::Unsigned(3));
    assert_eq!(
        member(member(paths, "paths"), "path_1"),
        &Value::Array(vec![])
    );
    assert_eq!(
        member(item(member(paths, "road_markers"), 1), "index"),
        &Value::Unsigned(29)
    );
    let region = &result.records[&(world::FIRST_ID + 2)];
    let headers: Vec<_> = region
        .fields
        .iter()
        .filter(|field| field.signature == *b"RDAT")
        .collect();
    assert_eq!(headers.len(), 2);
    assert_eq!(member(&headers[0].value, "priority"), &Value::Unsigned(37));
    assert_eq!(
        member(&headers[1].value, "unknown"),
        &Value::Bytes(vec![0xC1, 0x39])
    );
    assert_eq!(
        member(item(field(region, b"RDOT"), 0), "angle_z"),
        &Value::Unsigned(59)
    );
    assert_eq!(
        member(item(field(region, b"RDOT"), 0), "unknown_48"),
        &Value::Bytes(vec![0xC1, 0x39, 0x87, 0xB3])
    );
    assert_eq!(
        member(
            field(&result.records[&(world::FIRST_ID + 3)], b"DATA"),
            "rank"
        ),
        &Value::Signed(-3)
    );
    let placements: Vec<_> = result.records[&(world::FIRST_ID + 4)]
        .fields
        .iter()
        .filter(|field| field.signature == *b"DATA")
        .collect();
    assert_eq!(placements.len(), 2);
    assert_eq!(
        member(item(&placements[1].value, 0), "scale"),
        &Value::Float(0.625)
    );
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 0);
}

/// Neither a master index nor a light slot may be confused with load-order position.
#[test]
fn navigation_links_use_full_and_light_slots_and_clear_wrong_target_kinds() {
    let targets = record(b"STAT", 0x2600, 0, &subrecord(b"EDID", b"WrongDoorKind\0"));
    let mut fixture = Fixture::new(&targets);
    fixture.patch("Spare.esp", false, &[]);
    fixture.patch("FirstLight.esl", true, &[]);
    fixture.patch(
        "Navigation.esl",
        true,
        &[
            world::navmesh(0x0100_0842, 1, 0x0011_FFFD, 0x0100_0842, 0x2601),
            world::navi(0x0100_0843, 0x0100_0842, 0x2600),
        ]
        .concat(),
    );
    fixture.patch(
        "FullNavigation.esp",
        false,
        &world::navmesh(0x0100_2743, 1, 0x0011_FFFD, 0x0100_2743, 0x11),
    );
    let result = fixture.read();
    let nav = &result.records[&0xFE00_1842];
    assert_eq!(nav.load_order, 3);
    let geometry = field(nav, b"NVNM");
    assert_eq!(
        member(item(member(geometry, "edge_links"), 0), "navmesh"),
        &Value::FormId(0xFE00_1842)
    );
    assert_eq!(
        member(item(member(geometry, "door_triangles"), 0), "door"),
        &Value::FormId(0)
    );
    let info = field(&result.records[&0xFE00_1843], b"NVMI");
    assert_eq!(
        member(item(member(info, "door_links"), 0), "door"),
        &Value::FormId(0)
    );
    assert_eq!(
        member(info, "preferred_merges"),
        &Value::Bytes(vec![0xA7, 0x53, 0xC1, 0x39])
    );
    let full = field(&result.records[&0x0200_2743], b"NVNM");
    assert_eq!(
        member(item(member(full, "edge_links"), 0), "navmesh"),
        &Value::FormId(0x0200_2743)
    );
    assert!(result.diagnostics["navigation.esl"].invalid_links >= 3);
    assert_eq!(result.diagnostics["navigation.esl"].skipped_fields, 0);
}

/// Huge counts, truncation and trailing bytes reject one geometry override, retaining neighbors.
#[test]
fn malformed_navigation_layouts_preserve_old_winner_and_local_neighbors() {
    for defect in 0..3 {
        let mut fixture = Fixture::new(&world::records());
        let mut geometry = world::geometry(1, 0x0011_FFFD, world::FIRST_ID, 0x11);
        match defect {
            0 => geometry[16..20].copy_from_slice(&u32::MAX.to_le_bytes()),
            1 => {
                geometry.pop();
            }
            _ => geometry.push(0xA7),
        }
        let bad = record(b"NAVM", world::FIRST_ID, 0, &subrecord(b"NVNM", &geometry));
        fixture.patch(
            "DamagedWorld.esp",
            false,
            &[world::region(0x0100_2750), bad, world::region(0x0100_2751)].concat(),
        );
        let result = fixture.read();
        assert_eq!(result.records[&world::FIRST_ID].load_order, 0);
        assert!(result.records.contains_key(&0x0100_2750));
        assert!(result.records.contains_key(&0x0100_2751));
        assert_eq!(result.diagnostics["damagedworld.esp"].skipped_records, 1);
        assert!(
            result.diagnostics["damagedworld.esp"]
                .first_example
                .as_ref()
                .unwrap()
                .contains("geometry")
        );
        fixture.patch(
            "DeletedWorld.esp",
            false,
            &record(b"NAVM", world::FIRST_ID, 0x20, &[]),
        );
        assert!(!fixture.read().records.contains_key(&world::FIRST_ID));
    }
}

/// Valid empty layouts differ from truncated/overflow selectors in the public reader.
#[test]
fn zero_navigation_counts_decode_and_each_partial_layout_is_bounded() {
    use converter::records::deciders::world_extras::layout as descriptor;
    let empty = world::empty_geometry(1, 0x0011_FFFD);
    let fixture = Fixture::new(&record(b"NAVM", 0x2500, 0, &subrecord(b"NVNM", &empty)));
    let result = fixture.read();
    let value = field(&result.records[&0x2500], b"NVNM");
    assert_eq!(member(value, "vertices"), &Value::Array(vec![]));
    assert_eq!(member(value, "grid"), &Value::Struct(vec![]));
    assert_eq!(member(value, "max_y_distance"), &Value::Float(47.5));
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 0);
    let paths: Vec<_> = [0u32, 0].into_iter().flat_map(u32::to_le_bytes).collect();
    let layouts = [
        (
            "world_navmesh",
            world::geometry(1, 0x0011_FFFD, world::FIRST_ID, 0x11),
        ),
        (
            "world_navmesh_info",
            world::info(world::FIRST_ID, true, 1, 0x0011_FFFD, 0x11),
        ),
        ("world_navmesh_paths", paths),
    ];
    for (name, bytes) in layouts {
        assert!(descriptor(name, &bytes).unwrap().is_some());
        for length in 0..bytes.len() {
            assert!(
                descriptor(name, &bytes[..length]).is_err(),
                "{name} length{length}"
            );
        }
    }
    let mut grid = empty;
    grid[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(descriptor("world_navmesh", &grid).is_err());
    let mut paths = u32::MAX.to_le_bytes().to_vec();
    paths.extend([0; 4]);
    assert!(descriptor("world_navmesh_paths", &paths).is_err());
}

/// Every assigned type survives bad compressed overrides through actual conversion/publication.
#[tokio::test]
async fn broken_world_mod_publishes_prior_winners_neighbors_and_link_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let base_path = data.join("Skyrim.esm");
    let mut base = fs::read(&base_path).unwrap();
    let header_size = u32::from_le_bytes(base[4..8].try_into().unwrap()) as usize;
    let mut header = base[24..24 + header_size].to_vec();
    header.extend(subrecord(b"MAST", b"Skyrim.esm\0"));
    header.extend(subrecord(b"DATA", &[0xA7; 8]));
    base.extend(world::records());
    base.extend(record(
        b"STAT",
        0x2600,
        0,
        &subrecord(b"EDID", b"WrongLinkKind\0"),
    ));
    fs::write(&base_path, base).unwrap();
    let earlier = read_plugins(
        std::slice::from_ref(&base_path),
        &LoadOrder::read(std::slice::from_ref(&base_path)).unwrap(),
    )
    .unwrap();
    let mut patch = record(b"TES4", 0, 0, &header);
    let mut neighbors = Vec::new();
    for (index, signature) in world::SIGNATURES.iter().enumerate() {
        let index = index as u32;
        let before = 0x0100_2800 + index * 2;
        let after = before + 1;
        neighbors.extend([before, after]);
        patch.extend(world::region(before));
        patch.extend(record(
            signature,
            world::FIRST_ID + index,
            0x40000,
            &[24, 0, 0, 0, 99],
        ));
        patch.extend(world::region(after));
    }
    let mut bad_geometry = world::geometry(1, 0x0011_FFFD, world::FIRST_ID, 0x11);
    bad_geometry[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    patch.extend(record(
        b"NAVM",
        world::FIRST_ID,
        0,
        &subrecord(b"NVNM", &bad_geometry),
    ));
    patch.extend(world::navmesh(
        0x0100_2900,
        1,
        0x0011_FFFD,
        world::FIRST_ID,
        0x2601,
    ));
    patch.extend(world::navi(0x0100_2901, world::FIRST_ID, 0x2600));
    let patch_path = data.join("DamagedWorld.esp");
    fs::write(&patch_path, patch).unwrap();
    let paths = [base_path, patch_path];
    let result = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
    assert_eq!(result.diagnostics["damagedworld.esp"].skipped_records, 12);
    assert!(result.diagnostics["damagedworld.esp"].invalid_links >= 3);
    for id in &neighbors {
        assert!(result.records.contains_key(id));
    }
    let recovered_info = field(&result.records[&0x0100_2901], b"NVMI");
    assert_eq!(
        member(item(member(recovered_info, "door_links"), 0), "door"),
        &Value::FormId(0)
    );
    assert_eq!(
        member(recovered_info, "preferred_merges"),
        &Value::Bytes(vec![0xA7, 0x53, 0xC1, 0x39])
    );
    let plugins = temp.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n*DamagedWorld.esp\n").unwrap();
    let output = temp.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.plugins_file = Some(plugins);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    for index in 0..world::SIGNATURES.len() {
        let id = world::FIRST_ID + index as u32;
        let (load_order, payload): (i64, Vec<u8>) = db.query_row("SELECT source.load_order,source.payload FROM inhouse_source_records source JOIN records USING(form_id) WHERE source.form_id=?", [id], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(load_order, 0);
        assert_eq!(payload, earlier.records[&id].raw_payload);
    }
    for id in neighbors.into_iter().chain([0x0100_2900, 0x0100_2901]) {
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["damagedworld.esp"]["skipped_records"],
        12
    );
    assert!(
        diagnostics["decoder"]["damagedworld.esp"]["invalid_links"]
            .as_u64()
            .unwrap()
            >= 3
    );
}
