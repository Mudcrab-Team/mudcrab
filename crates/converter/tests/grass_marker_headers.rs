//! Winning base header flags govern published grass masks through both record readers.
use converter::{AssetPipeline, PipelineConfig, config::RecordReader};
use rusqlite::Connection;
use shared::grass::GrassCache;
use std::{fs, path::Path};

/// Frame independently authored subrecords.
fn field(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Write the header flags literally, without the production marker constant.
fn record(tag: &[u8; 4], id: u32, flags: u32, fields: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(fields.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        &fields,
    ]
    .concat()
}

/// Attach LAND and REFR records to actual world and cell identities.
fn group(kind: u32, label: u32, bytes: &[u8]) -> Vec<u8> {
    [
        b"GRUP".as_slice(),
        &(bytes.len() as u32 + 24).to_le_bytes(),
        &label.to_le_bytes(),
        &kind.to_le_bytes(),
        &[0; 8],
        bytes,
    ]
    .concat()
}

/// Declare masters in file-relative order.
fn header(masters: &[&str]) -> Vec<u8> {
    record(
        b"TES4",
        0,
        0,
        masters
            .iter()
            .flat_map(|master| {
                [
                    field(b"MAST", format!("{master}\0").as_bytes()),
                    field(b"DATA", &[0; 8]),
                ]
                .concat()
            })
            .collect(),
    )
}

/// Both bases use the same ordinary opaque model, outside marker/effect directories.
fn base(id: u32, flags: u32) -> Vec<u8> {
    record(
        b"STAT",
        id,
        flags,
        field(b"MODL", b"architecture/arbitrary.nif\0"),
    )
}

/// Place an enabled reference with an explicit position and no rotation.
fn reference(id: u32, base: u32, x: f32, y: f32) -> Vec<u8> {
    let transform: Vec<_> = [x, y, 0.0, 0.0, 0.0, 0.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    record(
        b"REFR",
        id,
        0,
        [
            field(b"NAME", &base.to_le_bytes()),
            field(b"DATA", &transform),
        ]
        .concat(),
    )
}

/// Prepare two adjacent grass cells and overrides that set and clear the marker bit.
fn fixture(root: &Path, reader: RecordReader) -> PipelineConfig {
    let data = root.join("Data");
    fs::create_dir_all(data.join("meshes/architecture")).unwrap();
    fs::write(
        data.join("Skyrim.esm"),
        dummy_content::esm::plugin(&dummy_content::esm::Plugin {
            author: "Marker header regression",
            worldspace: "MarkerWorld",
            cells: &[
                dummy_content::esm::Cell {
                    grid_x: 0,
                    grid_y: 0,
                },
                dummy_content::esm::Cell {
                    grid_x: 1,
                    grid_y: 0,
                },
            ],
            model_path: "architecture/unavailable.nif",
            diffuse: "textures/unavailable.dds",
            normal_texture: "textures/unavailable_n.dds",
        })
        .unwrap(),
    )
    .unwrap();
    fs::write(data.join("Filler.esm"), header(&[])).unwrap();
    let shape = dummy_content::nif::StaticShape {
        name: "OrdinaryRoof",
        positions: &[
            [-128.0, -128.0, 32.0],
            [128.0, -128.0, 32.0],
            [128.0, 128.0, 32.0],
            [-128.0, 128.0, 32.0],
        ],
        normals: &[[0.0, 0.0, 1.0]; 4],
        uvs: &[[0.0, 0.0]; 4],
        indices: &[[0, 1, 2], [0, 2, 3]],
        diffuse: "textures/unavailable.dds",
        normal_texture: "textures/unavailable_n.dds",
    };
    fs::write(
        data.join("meshes/architecture/arbitrary.nif"),
        dummy_content::nif::static_shape(&shape).unwrap(),
    )
    .unwrap();
    let mut plugin = header(&["Skyrim.esm"]);
    let mut rule = vec![40, 0, 90, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    for value in [0.0f32, 0.0, 0.0, 1.0] {
        rule.extend(value.to_le_bytes());
    }
    rule.extend([0; 4]);
    plugin.extend(record(
        b"GRAS",
        0x01000801,
        0,
        [
            field(b"MODL", b"grass/optional.nif\0"),
            field(b"DATA", &rule),
        ]
        .concat(),
    ));
    // LTEX 4 belongs to Skyrim.esm; only its grass association is replaced.
    plugin.extend(record(
        b"LTEX",
        4,
        0,
        field(b"GNAM", &0x01000801u32.to_le_bytes()),
    ));
    plugin.extend(base(0x01000840, 0));
    plugin.extend(base(0x01000841, 0x00800000));
    plugin.extend(group(
        1,
        1,
        &group(
            6,
            0x10,
            &group(
                9,
                0x10,
                &[
                    reference(0x01000850, 0x01000840, 4080.0, 80.0),
                    reference(0x01000851, 0x01000841, 512.0, 512.0),
                ]
                .concat(),
            ),
        ),
    ));
    fs::write(data.join("Bases.esp"), plugin).unwrap();
    // Filler.esm occupies global slot 1 but is not a master here. Bases.esp is
    // master slot 1 in this patch and global slot 2 in the selected load order.
    let patch = [
        header(&["Skyrim.esm", "Bases.esp"]),
        base(0x01000840, 0x00800000),
        base(0x01000841, 0),
    ]
    .concat();
    fs::write(data.join("Patch.esp"), patch).unwrap();
    let selection = root.join("plugins.txt");
    fs::write(
        &selection,
        "*Skyrim.esm\n*Filler.esm\n*Bases.esp\n*Patch.esp\n",
    )
    .unwrap();
    let mut config = PipelineConfig::new(data, root.join("pack"));
    config.plugins_file = Some(selection);
    config.record_reader = reader;
    config.no_lod = true;
    config.lod_texture_encoder = converter::TextureEncoder::Cpu;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    config
}

/// Publish real conversion output and distinguish setting from clearing the winning flags.
async fn check_marker_headers(reader: RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), reader);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config.clone(), sender)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(report.skipped, 0);
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(config.output_dir.join("conversion-manifest.json")).unwrap(),
    )
    .unwrap();
    assert!(manifest["failures"].as_object().unwrap().is_empty());
    let db = Connection::open(config.output_dir.join("skyrim_world.db")).unwrap();
    for (id, flags) in [(0x02000840u32, 0x00800000u32), (0x02000841, 0)] {
        let row = db.query_row("SELECT s.flags,s.model_path,s.bounds_valid,r.load_order FROM statics s JOIN records r ON r.form_id=s.id WHERE s.id=?1", [id], |row| Ok((row.get::<_,u32>(0)?,row.get::<_,String>(1)?,row.get::<_,u32>(2)?,row.get::<_,u32>(3)?))).unwrap();
        assert_eq!(
            row,
            (flags, "architecture/arbitrary.nif".into(), 1, 3),
            "winning header flags and bounds must survive publication for {id:08x}"
        );
    }
    let bytes = fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap();
    let cache = rkyv::from_bytes::<GrassCache, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(cache.types.len(), 1);
    assert_eq!(
        cache.cells.len(),
        2,
        "valid neighboring cells must survive publication"
    );
    let local = cache
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x10)
        .unwrap();
    let neighbor = cache
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x20)
        .unwrap();
    for cell in [local, neighbor] {
        assert_eq!(cell.quadrants[0].layers[0].grass_ids, [0x02000801]);
    }
    assert!(
        local.quadrants[0].exclusion.iter().any(|&byte| byte != 0),
        "override clearing IsMarker must restore the ordinary blocker"
    );
    assert!(
        local.quadrants[1].exclusion.iter().all(|&byte| byte == 0),
        "override setting IsMarker must remove local mask contributions"
    );
    assert!(
        neighbor
            .quadrants
            .iter()
            .all(|q| q.exclusion.iter().all(|&byte| byte == 0)),
        "override setting IsMarker must remove neighboring mask contributions"
    );
}

/// In-house conversion/publication must retain set/clear overrides and obey the winning flag.
#[tokio::test]
async fn inhouse_publication_obeys_winning_marker_header_overrides() {
    check_marker_headers(RecordReader::Inhouse).await;
}

/// Legacy conversion/publication must retain set/clear overrides and obey the winning flag.
#[tokio::test]
async fn legacy_publication_obeys_winning_marker_header_overrides() {
    check_marker_headers(RecordReader::Legacy).await;
}
