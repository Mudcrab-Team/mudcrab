//! Asset repair preserves reader-loss and historical completeness uncertainty.
use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, config::RecordReader,
    esm::types::ArchivedRecordData, repair::repair_failed,
};
use dummy_content::{dds, rng::Rng};
use rusqlite::Connection;
use serde_json::Value;
use std::{fs, path::Path, process::Command};

/// Frame a subrecord independently of either reader's schema or decoder.
fn field(signature: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    [
        signature.as_slice(),
        &(payload.len() as u16).to_le_bytes(),
        payload,
    ]
    .concat()
}

/// Encode a record boundary with asymmetric header sentinels.
fn record(signature: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    [
        signature.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &0x7351_294Bu32.to_le_bytes(),
        &44u16.to_le_bytes(),
        &0xA913u16.to_le_bytes(),
        payload,
    ]
    .concat()
}

/// Include valid neighbors around a safely framed but invalid compressed STAT.
fn plugin_bytes(record_loss: bool) -> Vec<u8> {
    let header = field(
        b"HEDR",
        &[
            1.7f32.to_le_bytes(),
            (2 + u32::from(record_loss)).to_le_bytes(),
            0x900u32.to_le_bytes(),
        ]
        .concat(),
    );
    let mut bytes = record(b"TES4", 0, 1, &header);
    bytes.extend(record(
        b"KYWD",
        0x801,
        0,
        &field(b"EDID", b"BeforeReaderLoss\0"),
    ));
    if record_loss {
        bytes.extend(record(b"STAT", 0x802, 0x40000, &[7, 0, 0, 0, 0xFF]));
    }
    bytes.extend(record(
        b"KYWD",
        0x803,
        0,
        &field(b"EDID", b"AfterReaderLoss\0"),
    ));
    bytes
}

/// Keep a known valid replacement texture separate from the initially broken source.
fn texture_bytes() -> Vec<u8> {
    dds::generate(
        &dds::Spec::new(dds::Format::Bc1Unorm, 4, 4),
        &mut Rng::new(0x1389_74A5),
    )
    .unwrap()
}

/// Create one independent asset failure and optional reader loss without game inputs.
fn fixture(root: &Path, record_loss: bool, reader: Option<RecordReader>) -> PipelineConfig {
    let data = root.join("Data");
    fs::create_dir_all(data.join("textures")).unwrap();
    fs::write(data.join("repair.esm"), plugin_bytes(record_loss)).unwrap();
    fs::write(
        data.join("textures/repair.dds"),
        b"invalid DDS before repair",
    )
    .unwrap();
    let mut config = PipelineConfig::new(data, root.join("pack"));
    if let Some(reader) = reader {
        config.record_reader = reader;
    }
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    config
}

/// Publish the actual default pipeline while draining its progress channel.
async fn publish(config: &PipelineConfig) -> PipelineReport {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config.clone(), sender).await;
    drain.await.unwrap();
    result.unwrap()
}

/// Read persisted JSON without referencing fields absent from the unfixed Rust types.
fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Inspect usable records by fixed IDs rather than deriving expected rows from the decoder.
fn assert_neighbors(output: &Path) {
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    let rows: Vec<(u32, String, Vec<u8>)> = database
        .prepare("SELECT form_id, record_type, data FROM records ORDER BY form_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let names = rows
        .into_iter()
        .map(|(id, signature, bytes)| {
            assert_eq!(signature, "KYWD");
            let decoded =
                rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes).unwrap();
            let editor_ids = decoded
                .subrecords
                .into_iter()
                .filter(|subrecord| subrecord.tag == *b"EDID")
                .map(|subrecord| subrecord.data)
                .collect::<Vec<_>>();
            (id, editor_ids)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            (0x801, vec![b"BeforeReaderLoss\0".to_vec()]),
            (0x803, vec![b"AfterReaderLoss\0".to_vec()]),
        ]
    );
}

/// Require the independent texture failure before repairing only its source bytes.
async fn publish_and_fix_texture(config: &PipelineConfig, record_loss: bool) {
    let report = publish(config).await;
    assert!(!report.complete);
    assert_eq!(report.skipped, 1);
    let reader_warnings = report
        .warnings
        .iter()
        .filter(|warning| warning.to_ascii_lowercase().contains("repair.esm"))
        .count();
    assert_eq!(reader_warnings, usize::from(record_loss), "{report:?}");
    let manifest = json(&config.output_dir.join("conversion-manifest.json"));
    let failures = manifest["failures"].as_object().unwrap();
    assert_eq!(failures.len(), 1);
    assert!(failures.contains_key("textures/repair.dds"));
    assert_neighbors(&config.output_dir);
    fs::write(config.data_dir.join("textures/repair.dds"), texture_bytes()).unwrap();
}

/// Reader-loss recovery remains incomplete after the unrelated DDS is successfully repaired.
async fn assert_mixed_repair(reader: RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), true, Some(reader));
    publish_and_fix_texture(&config, true).await;
    let input_before = fs::read(config.data_dir.join("repair.esm")).unwrap();
    let diagnostics_path = config.output_dir.join("inhouse-reader-diagnostics.json");
    let diagnostics_before = fs::read(&diagnostics_path).ok();
    let repaired = repair_failed(&config, true).unwrap();
    assert!(repaired.published);
    assert_eq!(repaired.converted, 1);
    assert!(repaired.failures.is_empty());
    let manifest = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(
        manifest["complete"], false,
        "repairing a DDS must not certify unresolved reader data loss"
    );
    assert_eq!(manifest["record_data_complete"], false);
    let report = json(&repaired.directory.join("repair-report.json"));
    assert_eq!(report["record_data_complete"], false);
    assert_neighbors(&config.output_dir);
    assert_eq!(
        fs::read(config.data_dir.join("repair.esm")).unwrap(),
        input_before
    );
    assert_eq!(fs::read(diagnostics_path).ok(), diagnostics_before);
    assert!(config.output_dir.join("textures/repair.ktx2").is_file());
    let check =
        converter::check_output(&config.output_dir, converter::CheckMode::Full, |_, _| {}).unwrap();
    assert_eq!(check.problems, [converter::CheckProblem::Incomplete]);

    // A full conversion can establish record completeness after fixing that source.
    // It must retain the successfully repaired texture through ordinary cache reuse.
    fs::write(config.data_dir.join("repair.esm"), plugin_bytes(false)).unwrap();
    let recovered = publish(&config).await;
    assert!(recovered.complete, "{recovered:?}");
    assert!(recovered.warnings.is_empty());
    assert_eq!(recovered.converted, 0);
    assert_eq!(recovered.cache_hits, 1);
    let manifest = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(manifest["record_data_complete"], true);
    assert_eq!(manifest["complete"], true);
    assert_neighbors(&config.output_dir);
}

/// The default frontend cannot lose its unresolved record-loss status during asset repair.
#[tokio::test]
async fn inhouse_reader_loss_survives_successful_asset_repair() {
    assert_mixed_repair(RecordReader::Inhouse).await;
}

/// Explicit legacy shares the same persisted reader-loss completeness contract.
#[tokio::test]
async fn legacy_reader_loss_survives_successful_asset_repair() {
    assert_mixed_repair(RecordReader::Legacy).await;
}

/// A healthy record source still becomes complete after its sole asset failure is repaired.
#[tokio::test]
async fn asset_only_repair_becomes_complete() {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), false, None);
    publish_and_fix_texture(&config, false).await;
    let repaired = repair_failed(&config, true).unwrap();
    assert!(repaired.published);
    assert!(repaired.failures.is_empty());
    let manifest = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(manifest["complete"], true);
    assert_neighbors(&config.output_dir);
    let check =
        converter::check_output(&config.output_dir, converter::CheckMode::Full, |_, _| {}).unwrap();
    assert!(check.problems.is_empty(), "{:?}", check.problems);
}

/// Missing historical proof cannot be inferred from the successful repair of asset failures.
#[tokio::test]
async fn historical_manifest_without_reader_proof_stays_incomplete() {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), false, None);
    publish_and_fix_texture(&config, false).await;
    let path = config.output_dir.join("conversion-manifest.json");
    let mut original = json(&path);
    original
        .as_object_mut()
        .unwrap()
        .remove("record_data_complete");
    fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();
    let repaired = repair_failed(&config, true).unwrap();
    assert!(repaired.published);
    assert!(repaired.failures.is_empty());
    let manifest = json(&path);
    assert_eq!(
        manifest["complete"], false,
        "asset repair cannot establish missing historical record completeness"
    );
    assert!(manifest.get("record_data_complete").is_none());
    let report = json(&repaired.directory.join("repair-report.json"));
    assert_eq!(report["record_data_complete"], false);
    assert_neighbors(&config.output_dir);
}

/// Run native CLI repair with its actual default and capture both result streams.
fn repair_cli(config: &PipelineConfig) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_converter"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
        .arg("repair-failed")
        .arg(&config.data_dir)
        .arg(&config.output_dir)
        .args(["--no-lod", "--apply"])
        .output()
        .unwrap()
}

/// Native default-reader repair publishes usable assets, reports record uncertainty and exits 1.
#[tokio::test]
async fn cli_repair_reports_remaining_reader_loss_with_asset_only_success_control() {
    for record_loss in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let config = fixture(directory.path(), record_loss, None);
        assert_eq!(config.record_reader, RecordReader::Inhouse);
        publish_and_fix_texture(&config, record_loss).await;
        let result = repair_cli(&config);
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let manifest = json(&config.output_dir.join("conversion-manifest.json"));
        assert_eq!(
            manifest["complete"], !record_loss,
            "native repair must preserve record completeness: {combined}"
        );
        assert!(manifest["failures"].as_object().unwrap().is_empty());
        assert!(config.output_dir.join("textures/repair.ktx2").is_file());
        assert!(combined.contains("Repair published"), "{combined}");
        assert_eq!(
            result.status.code(),
            Some(i32::from(record_loss)),
            "{combined}"
        );
        if record_loss {
            let lower = combined.to_ascii_lowercase();
            assert!(
                lower.contains("record data") && lower.contains("incomplete"),
                "{combined}"
            );
        }
        assert_neighbors(&config.output_dir);
    }
}
