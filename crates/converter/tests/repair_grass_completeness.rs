//! Asset repair refreshes obstruction masks without certifying independent record losses.
use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, config::RecordReader, repair::repair_failed,
};
use dummy_content::{dds, nif, rng::Rng};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Encode ordinary subrecord framing without either frontend's decoder.
fn field(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Frame a top-level record independently of the projection implementation.
fn record(tag: &[u8; 4], id: u32, fields: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(fields.len() as u32).to_le_bytes(),
        &[0; 4],
        &id.to_le_bytes(),
        &[0; 8],
        fields,
    ]
    .concat()
}

/// Both rules have complete 32-byte DATA; only the second water enum is unusable.
fn rule(id: u32, water_comparison: u32) -> Vec<u8> {
    let mut data = vec![40, 0, 90, 0, 0, 0, 0, 0];
    data.extend(water_comparison.to_le_bytes());
    for value in [0.0f32, 0.0, 0.0, 1.0] {
        data.extend(value.to_le_bytes());
    }
    data.extend([0; 4]);
    assert_eq!(data.len(), 32);
    record(
        b"GRAS",
        id,
        &[
            field(b"MODL", b"grass/optional.nif\0"),
            field(b"DATA", &data),
        ]
        .concat(),
    )
}

/// Build two terrain neighbors, a usable rule, and an independently broken texture.
fn fixture(root: &Path, reader: RecordReader, grass_loss: bool) -> PipelineConfig {
    let data = root.join("Data");
    fs::create_dir_all(data.join("textures")).unwrap();
    let plugin = dummy_content::esm::plugin(&dummy_content::esm::Plugin {
        author: "Grass repair completeness",
        worldspace: "GrassRepairWorld",
        cells: &[
            dummy_content::esm::Cell {
                grid_x: 3,
                grid_y: -2,
            },
            dummy_content::esm::Cell {
                grid_x: 4,
                grid_y: -2,
            },
        ],
        model_path: "architecture/unavailable.nif",
        diffuse: "textures/unavailable.dds",
        normal_texture: "textures/unavailable_n.dds",
    })
    .unwrap();
    fs::write(data.join("Skyrim.esm"), plugin).unwrap();
    let mut grass_plugin = record(
        b"TES4",
        0,
        &[
            field(
                b"HEDR",
                &[
                    1.7f32.to_le_bytes(),
                    3u32.to_le_bytes(),
                    0x803u32.to_le_bytes(),
                ]
                .concat(),
            ),
            field(b"MAST", b"Skyrim.esm\0"),
            field(b"DATA", &[0; 8]),
        ]
        .concat(),
    );
    grass_plugin.extend(rule(0x0100_0801, 0));
    grass_plugin.extend(rule(0x0100_0802, if grass_loss { 8 } else { 7 }));
    grass_plugin.extend(record(
        b"LTEX",
        4,
        &[
            field(b"GNAM", &0x0100_0801u32.to_le_bytes()),
            field(b"GNAM", &0x0100_0802u32.to_le_bytes()),
        ]
        .concat(),
    ));
    fs::write(data.join("Grass.esp"), grass_plugin).unwrap();
    fs::write(data.join("textures/repair.dds"), b"independent invalid DDS").unwrap();
    let mut config = PipelineConfig::new(data, root.join("pack"));
    config.record_reader = reader;
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    config
}

/// Drain actual conversion progress so this exercises publication rather than a helper.
async fn publish(config: &PipelineConfig) -> PipelineReport {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config.clone(), sender).await;
    drain.await.unwrap();
    result.unwrap()
}

/// Read the persisted contract rather than relying on Rust struct defaults.
fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Inspect serialized diagnostics so this regression also compiles against pre-refresh repair.
fn assert_repair_diagnostics(
    repair: &converter::repair::RepairReport,
    grass_loss: bool,
    unbounded_references: usize,
) {
    let report = serde_json::to_value(repair).unwrap();
    let warnings = report["warnings"].as_array().cloned().unwrap_or_default();
    assert_eq!(warnings.len(), usize::from(grass_loss), "{report}");
    assert!(
        warnings.iter().all(|warning| warning
            .as_str()
            .unwrap()
            .contains("incomplete or invalid grass rules")),
        "{report}"
    );
    let notices = report["notices"].as_array().cloned().unwrap_or_default();
    let obstruction_notices = notices
        .iter()
        .filter(|notice| {
            notice
                .as_str()
                .unwrap()
                .contains("without validated model bounds")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        obstruction_notices.len(),
        usize::from(unbounded_references > 0),
        "{report}"
    );
    if unbounded_references > 0 {
        assert_eq!(
            obstruction_notices[0].as_str().unwrap(),
            format!(
                "grass exclusion skipped {unbounded_references} exterior references without validated model bounds"
            )
        );
    }
}

/// Require healthy neighbors and rules to survive actual conversion and asset repair.
fn assert_cache(output: &Path, grass_loss: bool) {
    converter::esm::grass_cache::validate_grass_cache(&output.join("grass_cache.rkyv")).unwrap();
    let bytes = fs::read(output.join("grass_cache.rkyv")).unwrap();
    let cache = rkyv::from_bytes::<shared::grass::GrassCache, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(cache.cells.len(), 2);
    let ids: Vec<_> = cache.types.iter().map(|rule| rule.form_id).collect();
    assert_eq!(
        ids,
        if grass_loss {
            vec![0x0100_0801]
        } else {
            vec![0x0100_0801, 0x0100_0802]
        }
    );
    let mut coordinates = cache
        .cells
        .iter()
        .map(|cell| (cell.grid_x, cell.grid_y))
        .collect::<Vec<_>>();
    coordinates.sort_unstable();
    assert_eq!(coordinates, [(3, -2), (4, -2)]);
}

/// A successful DDS repair must preserve the independently omitted grass-data status.
async fn check_repair(reader: RecordReader, grass_loss: bool) {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), reader, grass_loss);
    let report = publish(&config).await;
    assert!(!report.complete);
    assert_eq!(report.skipped, 1, "{report:?}");
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|warning| warning.contains("incomplete or invalid grass rules"))
            .count(),
        usize::from(grass_loss),
        "{report:?}"
    );
    assert!(
        !report.warnings.iter().any(|warning| {
            let warning = warning.to_ascii_lowercase();
            warning.contains("skyrim.esm") || warning.contains("grass.esp")
        }),
        "the fixture must reach grass baking without reader losses: {report:?}"
    );
    let manifest = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(manifest["failures"].as_object().unwrap().len(), 1);
    assert!(manifest["failures"].get("textures/repair.dds").is_some());
    assert_cache(&config.output_dir, grass_loss);
    let cache_before = fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap();
    let plugin_before = fs::read(config.data_dir.join("Skyrim.esm")).unwrap();
    let grass_plugin_before = fs::read(config.data_dir.join("Grass.esp")).unwrap();
    let replacement = dds::generate(
        &dds::Spec::new(dds::Format::Bc1Unorm, 4, 4),
        &mut Rng::new(0x1389_74A5),
    )
    .unwrap();
    fs::write(config.data_dir.join("textures/repair.dds"), replacement).unwrap();
    let repair = repair_failed(&config, true).unwrap();
    assert!(repair.published);
    assert_eq!(repair.converted, 1);
    assert!(repair.failures.is_empty());
    assert_repair_diagnostics(&repair, grass_loss, 2);
    assert!(config.output_dir.join("textures/repair.ktx2").is_file());
    assert_eq!(
        fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap(),
        cache_before
    );
    assert_eq!(
        fs::read(config.data_dir.join("Skyrim.esm")).unwrap(),
        plugin_before
    );
    assert_eq!(
        fs::read(config.data_dir.join("Grass.esp")).unwrap(),
        grass_plugin_before
    );
    assert_cache(&config.output_dir, grass_loss);
    let manifest = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(
        manifest["complete"], !grass_loss,
        "successful asset repair must not certify omitted grass data"
    );
    assert_eq!(manifest["record_data_complete"], !grass_loss);
    let check =
        converter::check_output(&config.output_dir, converter::CheckMode::Full, |_, _| {}).unwrap();
    assert_eq!(
        check.problems,
        if grass_loss {
            vec![converter::CheckProblem::Incomplete]
        } else {
            vec![]
        }
    );
}

/// Repairing a pre-ordinal package recovers authored order from its winning raw LTEX.
#[tokio::test]
async fn asset_repair_recovers_legacy_database_gnam_order() {
    for (migrate_before_repair, corrupt_field) in [(false, false), (true, false), (false, true)] {
        let directory = tempfile::tempdir().unwrap();
        let config = fixture(directory.path(), RecordReader::Legacy, false);
        let plugin_path = config.data_dir.join("Grass.esp");
        let mut plugin = fs::read(&plugin_path).unwrap();
        plugin.extend(record(
            b"LTEX",
            4,
            &[
                field(b"GNAM", &0x0100_0802u32.to_le_bytes()),
                field(b"GNAM", &0x0100_0801u32.to_le_bytes()),
            ]
            .concat(),
        ));
        fs::write(&plugin_path, plugin).unwrap();
        let report = publish(&config).await;
        assert_eq!(
            report.skipped, 1,
            "the independent DDS failure must be repairable"
        );
        let database = config.output_dir.join("skyrim_world.db");
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute_batch("ALTER TABLE landscape_texture_grasses DROP COLUMN ordinal")
            .unwrap();
        if corrupt_field {
            let data = converter::esm::extractors::serialize_subrecords(&[
                (b"GNAM".to_vec(), 0x0100_0802u32.to_le_bytes().to_vec()),
                (b"GNAM".to_vec(), vec![1, 2, 3]),
                (b"GNAM".to_vec(), 0x0100_0801u32.to_le_bytes().to_vec()),
            ]);
            conn.execute("UPDATE records SET data=?1 WHERE form_id=4", [data])
                .unwrap();
        }
        if migrate_before_repair {
            converter::esm::exporter::create_tables(&conn).unwrap();
        }
        drop(conn);
        let manifest_path = config.output_dir.join("conversion-manifest.json");
        let mut manifest = converter::cache::ConversionManifest::load(&manifest_path).unwrap();
        for entry in manifest
            .entries
            .values_mut()
            .filter(|entry| entry.output == "skyrim_world.db")
        {
            entry.output_hash = converter::cache::hash_file(&database).unwrap();
            entry.output_size = fs::metadata(&database).unwrap().len();
        }
        manifest.save(&manifest_path).unwrap();
        let replacement = dds::generate(
            &dds::Spec::new(dds::Format::Bc1Unorm, 4, 4),
            &mut Rng::new(0x1389_74A5),
        )
        .unwrap();
        fs::write(config.data_dir.join("textures/repair.dds"), replacement).unwrap();
        let result = repair_failed(&config, true);
        assert!(
            result.is_ok(),
            "a legacy association schema must not abort asset repair: {result:?}"
        );
        let repair = result.unwrap();
        assert!(repair.published);
        assert!(repair.failures.is_empty());
        assert_eq!(
            repair.warnings.len(),
            usize::from(corrupt_field),
            "{:?}",
            repair.warnings
        );
        if corrupt_field {
            assert!(
                repair.warnings[0].contains(
                    "unreadable legacy LTEX grass data from 1 lists; first LTEX 00000004"
                )
            );
        }
        let bytes = fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap();
        let cache =
            rkyv::from_bytes::<shared::grass::GrassCache, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(cache.version, 6);
        assert_eq!(cache.cells.len(), 2);
        assert_eq!(
            cache
                .types
                .iter()
                .map(|rule| rule.form_id)
                .collect::<Vec<_>>(),
            [0x0100_0801, 0x0100_0802]
        );
        for cell in &cache.cells {
            let layers = cell
                .quadrants
                .iter()
                .flat_map(|q| &q.layers)
                .collect::<Vec<_>>();
            assert!(
                !layers.is_empty(),
                "legacy recovery must retain grass-bearing layers in cell {}",
                cell.cell_id
            );
            for layer in layers {
                assert_eq!(layer.grass_ids, [0x0100_0802, 0x0100_0801]);
            }
        }
        assert_eq!(json(&manifest_path)["complete"], !corrupt_field);
    }
}

/// Default frontend must preserve a grass-only loss after successful asset repair.
#[tokio::test]
async fn inhouse_grass_loss_survives_asset_repair() {
    check_repair(RecordReader::Inhouse, true).await;
}

/// Explicit legacy must preserve the same derived-data loss.
#[tokio::test]
async fn legacy_grass_loss_survives_asset_repair() {
    check_repair(RecordReader::Legacy, true).await;
}

/// A valid enum boundary supplies the healthy control without suppressing asset repair.
#[tokio::test]
async fn healthy_grass_asset_only_repair_becomes_complete() {
    check_repair(RecordReader::Inhouse, false).await;
}

/// Keep a distant terrain control without another placement of the repaired obstruction.
fn obstruction_fixture(root: &Path, grass_loss: bool, reader_loss: bool) -> PipelineConfig {
    let config = fixture(root, RecordReader::Inhouse, grass_loss);
    let mut plugin = dummy_content::esm::plugin(&dummy_content::esm::Plugin {
        author: "Grass obstruction repair",
        worldspace: "GrassRepairWorld",
        cells: &[
            dummy_content::esm::Cell {
                grid_x: 3,
                grid_y: -2,
            },
            dummy_content::esm::Cell {
                grid_x: 9,
                grid_y: 5,
            },
        ],
        model_path: "architecture/unavailable.nif",
        diffuse: "textures/repair.dds",
        normal_texture: "textures/unavailable_n.dds",
    })
    .unwrap();
    // The dummy writer assigns the second CELL 0x20 and its REFR 0x22.
    // Mark only that placement deleted; the far LAND must remain grass-bearing.
    let far_references = plugin
        .windows(16)
        .enumerate()
        .filter(|(_, header)| &header[..4] == b"REFR" && header[12..16] == 0x22u32.to_le_bytes())
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    assert_eq!(far_references.len(), 1);
    let offset = far_references[0];
    plugin[offset + 8..offset + 12].copy_from_slice(&0x20u32.to_le_bytes());
    fs::write(config.data_dir.join("Skyrim.esm"), plugin).unwrap();
    let texture = dds::generate(
        &dds::Spec::new(dds::Format::Bc1Unorm, 4, 4),
        &mut Rng::new(0x1389_74A5),
    )
    .unwrap();
    fs::write(config.data_dir.join("textures/repair.dds"), texture).unwrap();
    let model = config.data_dir.join("meshes/architecture/unavailable.nif");
    fs::create_dir_all(model.parent().unwrap()).unwrap();
    fs::write(model, b"broken obstruction NIF before repair").unwrap();
    if reader_loss {
        let grass_path = config.data_dir.join("Grass.esp");
        let mut grass_plugin = fs::read(&grass_path).unwrap();
        grass_plugin.extend(record(
            b"KYWD",
            0x0100_0901,
            &field(b"EDID", b"BeforeRepairReaderLoss\0"),
        ));
        let mut broken = record(b"STAT", 0x0100_0902, &[7, 0, 0, 0, 0xFF]);
        broken[8..12].copy_from_slice(&0x40000u32.to_le_bytes());
        grass_plugin.extend(broken);
        grass_plugin.extend(record(
            b"KYWD",
            0x0100_0903,
            &field(b"EDID", b"AfterRepairReaderLoss\0"),
        ));
        fs::write(grass_path, grass_plugin).unwrap();
    }
    config
}

/// Supply an asymmetric solid roof covering the grass-bearing southwest quadrant's last tile.
fn restore_obstruction(config: &PipelineConfig) {
    fs::write(
        config.data_dir.join("meshes/architecture/unavailable.nif"),
        nif::static_shape(&nif::StaticShape {
            name: "RepairedGrassRoof",
            positions: &[
                [-768.0, -384.0, 128.0],
                [896.0, -384.0, 128.0],
                [-768.0, 640.0, 128.0],
            ],
            normals: &[[0.0, 0.0, 1.0]; 3],
            uvs: &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            indices: &[[0, 1, 2]],
            diffuse: "textures/repair.dds",
            normal_texture: "",
        })
        .unwrap(),
    )
    .unwrap();
}

/// Decode a validated published archive, keeping comparisons independent of bake internals.
fn grass_cache(output: &Path) -> shared::grass::GrassCache {
    let path = output.join("grass_cache.rkyv");
    converter::esm::grass_cache::validate_grass_cache(&path).unwrap();
    rkyv::from_bytes::<shared::grass::GrassCache, rkyv::rancor::Error>(&fs::read(path).unwrap())
        .unwrap()
}

/// Repair an actual failed NIF and require only overlapping terrain masks to change.
async fn check_obstruction_repair(grass_loss: bool, reader_loss: bool) {
    let directory = tempfile::tempdir().unwrap();
    let config = obstruction_fixture(directory.path(), grass_loss, reader_loss);
    let initial = publish(&config).await;
    assert!(!initial.complete);
    assert_eq!(initial.skipped, 1, "{initial:?}");
    assert_eq!(
        initial
            .warnings
            .iter()
            .any(|warning| warning.to_ascii_lowercase().contains("grass.esp")),
        reader_loss,
        "{initial:?}"
    );
    let manifest = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(manifest["failures"].as_object().unwrap().len(), 1);
    assert!(
        manifest["failures"]
            .get("meshes/architecture/unavailable.nif")
            .is_some()
    );
    let before = grass_cache(&config.output_dir);
    assert_eq!(before.cells.len(), 2);
    let near_before = before
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x10)
        .unwrap();
    let far_before = before
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x20)
        .unwrap();
    assert_eq!((near_before.grid_x, near_before.grid_y), (3, -2));
    assert_eq!((far_before.grid_x, far_before.grid_y), (9, 5));
    assert!(before.cells.iter().all(|cell| {
        cell.quadrants
            .iter()
            .all(|quadrant| quadrant.exclusion.iter().all(|byte| *byte == 0))
    }));
    assert!(
        before
            .cells
            .iter()
            .all(|cell| !cell.quadrants[0].layers.is_empty())
    );
    let plugin_before = fs::read(config.data_dir.join("Skyrim.esm")).unwrap();
    let grass_plugin_before = fs::read(config.data_dir.join("Grass.esp")).unwrap();
    restore_obstruction(&config);
    let repair = repair_failed(&config, true).unwrap();
    assert!(repair.published);
    assert_eq!(repair.converted, 1);
    assert!(repair.failures.is_empty());
    assert!(
        config
            .output_dir
            .join("meshes/architecture/unavailable.glb")
            .is_file()
    );
    let after = grass_cache(&config.output_dir);
    let near_after = after
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x10)
        .unwrap();
    let far_after = after
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x20)
        .unwrap();
    assert_ne!(
        near_after.input_hash, near_before.input_hash,
        "repair must invalidate the affected cell after restoring model bounds and content"
    );
    // REFR 0x12 is at the near CELL center. The last southwest tile center
    // is 16 units west and south of it, inside the authored roof triangle.
    assert_ne!(near_after.quadrants[0].exclusion[511] & 0x80, 0);
    assert_eq!(
        rkyv::to_bytes::<rkyv::rancor::Error>(far_after)
            .unwrap()
            .as_slice(),
        rkyv::to_bytes::<rkyv::rancor::Error>(far_before)
            .unwrap()
            .as_slice(),
        "distant cell must retain its input hash, terrain, species and mask bytes"
    );
    assert_eq!(after.types, before.types);
    assert_repair_diagnostics(&repair, grass_loss, 0);
    assert_eq!(
        fs::read(config.data_dir.join("Skyrim.esm")).unwrap(),
        plugin_before
    );
    assert_eq!(
        fs::read(config.data_dir.join("Grass.esp")).unwrap(),
        grass_plugin_before
    );
    let manifest = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(manifest["complete"], !(grass_loss || reader_loss));
    assert_eq!(
        manifest["record_data_complete"],
        !(grass_loss || reader_loss)
    );
    assert_eq!(repair.record_data_complete, !(grass_loss || reader_loss));
    let database = rusqlite::Connection::open(config.output_dir.join("skyrim_world.db")).unwrap();
    if reader_loss {
        let surviving: usize = database
            .query_row(
                "SELECT count(*) FROM records WHERE form_id IN (16779521,16779523)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            surviving, 2,
            "valid records surrounding reader loss must survive repair"
        );
    }
    drop(database);
    let check =
        converter::check_output(&config.output_dir, converter::CheckMode::Full, |_, _| {}).unwrap();
    assert_eq!(
        check.problems,
        if grass_loss || reader_loss {
            vec![converter::CheckProblem::Incomplete]
        } else {
            vec![]
        }
    );
}

/// A repaired mesh must publish fresh masks while reusing a far grass-bearing cell.
#[tokio::test]
async fn repaired_obstruction_refreshes_affected_grass_only() {
    check_obstruction_repair(false, false).await;
}

/// Restored model geometry cannot certify an independently invalid modeled grass rule.
#[tokio::test]
async fn repaired_obstruction_preserves_grass_loss() {
    check_obstruction_repair(true, false).await;
}

/// A healthy grass re-bake cannot clear an independent frontend decoding loss.
#[tokio::test]
async fn repaired_obstruction_preserves_reader_loss() {
    check_obstruction_repair(false, true).await;
}

/// Snapshot published files while excluding retained repair previews and staging directories.
fn published_files(output: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(output)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with(".repair-")
        })
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry.path().strip_prefix(output).unwrap().to_path_buf(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

/// Missing authoritative terrain must fail before publishing any repaired mesh or grass bytes.
#[tokio::test]
async fn missing_preserved_terrain_aborts_repair_without_changing_published_pack() {
    let directory = tempfile::tempdir().unwrap();
    let config = obstruction_fixture(directory.path(), false, false);
    let initial = publish(&config).await;
    assert_eq!(initial.skipped, 1, "{initial:?}");
    restore_obstruction(&config);
    // Reconstruct an older pack without the complete LAND-layer projection.
    // Its capped runtime terrain cannot safely replace the preserved archive.
    fs::remove_file(config.output_dir.join("cell_cache_preserved.rkyv")).unwrap();
    let manifest_path = config.output_dir.join("conversion-manifest.json");
    let mut manifest = converter::cache::ConversionManifest::load(&manifest_path).unwrap();
    manifest
        .entries
        .retain(|_, entry| entry.output != "cell_cache_preserved.rkyv");
    manifest.save(&manifest_path).unwrap();
    let check =
        converter::check_output(&config.output_dir, converter::CheckMode::Full, |_, _| {}).unwrap();
    assert_eq!(check.problems.len(), 2, "{check:?}");
    assert!(check.problems.iter().all(|problem| matches!(
        problem,
        converter::CheckProblem::Incomplete
            | converter::CheckProblem::RecordedFailures { count: 1 }
    )));
    let before = published_files(&config.output_dir);
    assert!(before.contains_key(Path::new("grass_cache.rkyv")));
    assert!(before.contains_key(Path::new("textures/repair.ktx2")));
    let repair = repair_failed(&config, true);
    assert!(
        repair.is_err(),
        "repair must require preserved terrain: {repair:?}"
    );
    let error = format!("{:#}", repair.unwrap_err());
    assert!(error.contains("cell_cache_preserved.rkyv"), "{error}");
    assert!(error.contains("normal conversion"), "{error}");
    assert!(
        error.contains("metadata") && error.contains("complete verified assets"),
        "metadata recovery must identify its asset completeness prerequisite: {error}"
    );
    assert_eq!(
        published_files(&config.output_dir),
        before,
        "a grass-bake I/O failure must preserve every published file, including manifest, database, grass and textures"
    );
    assert!(
        !config
            .output_dir
            .join("meshes/architecture/unavailable.glb")
            .exists()
    );
    grass_cache(&config.output_dir);
}

/// Current-layout retained assets without prior grass do not certify newly omitted grass data.
#[tokio::test]
async fn metadata_rebuild_without_prior_grass_retains_loss_completeness() {
    let directory = tempfile::tempdir().unwrap();
    let source_config = fixture(directory.path(), RecordReader::Legacy, true);
    let texture = dds::generate(
        &dds::Spec::new(dds::Format::Bc1Unorm, 4, 4),
        &mut Rng::new(0x1389_74A5),
    )
    .unwrap();
    fs::write(source_config.data_dir.join("textures/repair.dds"), texture).unwrap();
    let initial = publish(&source_config).await;
    assert_eq!(initial.skipped, 0, "{initial:?}");
    assert!(!initial.complete);
    assert_eq!(initial.warnings.len(), 1, "{initial:?}");
    assert!(initial.warnings[0].contains("incomplete or invalid grass rules"));
    assert_cache(&source_config.output_dir, true);

    // Main's current producer can certify these assets without a grass artifact.
    // Keep its texture layout and exact reader/asset identities: older texture
    // producers now require normal conversion before metadata-only rebuilding.
    // Missing prior record proof still cannot certify the invalid grass rule.
    let manifest_path = source_config.output_dir.join("conversion-manifest.json");
    let mut historical = converter::cache::ConversionManifest::load(&manifest_path).unwrap();
    assert!(historical.failures.is_empty());
    historical.complete = true;
    historical.record_data_complete = None;
    historical.save(&manifest_path).unwrap();
    fs::remove_file(source_config.output_dir.join("grass_cache.rkyv")).unwrap();

    let source_manifest_before = fs::read(&manifest_path).unwrap();
    let source_database_before =
        converter::cache::hash_file(&source_config.output_dir.join("skyrim_world.db")).unwrap();
    let texture_before = fs::read(source_config.output_dir.join("textures/repair.ktx2")).unwrap();
    let plugin_before = fs::read(source_config.data_dir.join("Skyrim.esm")).unwrap();
    let grass_plugin_before = fs::read(source_config.data_dir.join("Grass.esp")).unwrap();
    let mut config = PipelineConfig::new(
        source_config.data_dir.clone(),
        directory.path().join("metadata-derived"),
    );
    assert_eq!(config.record_reader, RecordReader::Inhouse);
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result =
        AssetPipeline::rebuild_metadata_async(config.clone(), &source_config.output_dir, sender)
            .await;
    drain.await.unwrap();
    let report = result.unwrap();
    assert!(!report.complete, "{report:?}");
    assert_eq!(report.skipped, 0, "{report:?}");
    assert_eq!(report.warnings.len(), 1, "{report:?}");
    assert!(report.warnings[0].contains("incomplete or invalid grass rules"));
    assert_eq!(report.converted, 0);
    assert_cache(&config.output_dir, true);
    let derived = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(derived["complete"], false);
    assert_eq!(
        derived["record_data_complete"], false,
        "metadata grass losses must clear proof independently of healthy reader decoding"
    );
    assert_eq!(
        fs::read(config.output_dir.join("textures/repair.ktx2")).unwrap(),
        texture_before
    );
    assert_eq!(fs::read(&manifest_path).unwrap(), source_manifest_before);
    assert_eq!(
        converter::cache::hash_file(&source_config.output_dir.join("skyrim_world.db")).unwrap(),
        source_database_before
    );
    assert_eq!(
        fs::read(source_config.output_dir.join("textures/repair.ktx2")).unwrap(),
        texture_before
    );
    assert_eq!(
        fs::read(config.data_dir.join("Skyrim.esm")).unwrap(),
        plugin_before
    );
    assert_eq!(
        fs::read(config.data_dir.join("Grass.esp")).unwrap(),
        grass_plugin_before
    );
    assert!(!source_config.output_dir.join("grass_cache.rkyv").exists());
}
