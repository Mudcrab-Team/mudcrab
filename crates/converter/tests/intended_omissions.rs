//! A retail record with no usable value is omitted and listed, and it does not make the
//! conversion incomplete (#811, #909). A real reader failure still does.
mod common;

use converter::{AssetPipeline, PipelineConfig, PipelineReport, config::RecordReader};
use dummy_content::{
    inhouse_magic::{record, subrecord},
    layout,
};
use rusqlite::Connection;
use std::{fs, path::Path};

/// Frame a group using native sizes and types, independently of converter internals.
fn group(kind: i32, label: u32, content: &[u8]) -> Vec<u8> {
    let mut bytes = b"GRUP".to_vec();
    bytes.extend((content.len() as u32 + 24).to_le_bytes());
    bytes.extend(label.to_le_bytes());
    bytes.extend(kind.to_le_bytes());
    bytes.extend([0; 8]);
    bytes.extend(content);
    bytes
}

/// Both groups name cell 0x1000 of the generated master.
fn cell_children(content: &[u8]) -> Vec<u8> {
    group(6, 0x1000, &group(9, 0x1000, content))
}

/// A LAND as the reader sees it in a plugin that carries normals and colours but no `VHGT`.
fn land_without_heights(id: u32) -> Vec<u8> {
    let mut fields = subrecord(b"VNML", &vec![7; 33 * 33 * 3]);
    fields.extend(subrecord(b"VCLR", &vec![8; 33 * 33 * 3]));
    record(b"LAND", id, 0, &fields)
}

/// A generated world plus one patch plugin holding `content` in cell 0x1000.
fn fixture(root: &Path, content: &[u8], reader: RecordReader) -> PipelineConfig {
    let data = root.join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let base = fs::read(data.join("Skyrim.esm")).unwrap();
    let length = u32::from_le_bytes(base[4..8].try_into().unwrap()) as usize;
    let mut header = base[24..24 + length].to_vec();
    header.extend(subrecord(b"MAST", b"Skyrim.esm\0"));
    header.extend(subrecord(b"DATA", &[0xA7; 8]));
    let mut patch = record(b"TES4", 0, 0, &header);
    patch.extend(cell_children(content));
    fs::write(data.join("Patch.esp"), patch).unwrap();
    let list = root.join("plugins.txt");
    fs::write(&list, "*Skyrim.esm\n*Patch.esp\n").unwrap();
    let mut config = PipelineConfig::new(&data, root.join("pack"));
    config.plugins_file = Some(list);
    config.record_reader = reader;
    config.no_lod = true;
    config.cpu_jobs = 2;
    config
}

async fn publish(config: PipelineConfig) -> PipelineReport {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, sender).await.unwrap();
    drain.await.unwrap();
    report
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// A LAND whose `VHGT` is present with the given bytes.
fn land_with_vhgt(id: u32, vhgt: &[u8]) -> Vec<u8> {
    let mut fields = subrecord(b"VHGT", vhgt);
    fields.extend(subrecord(b"VNML", &vec![7; 33 * 33 * 3]));
    record(b"LAND", id, 0, &fields)
}

const READERS: [RecordReader; 2] = [RecordReader::Inhouse, RecordReader::Legacy];

/// The no-false-data rule: a height-less LAND is omitted and listed, and the pack is complete.
/// Both readers apply it.
#[tokio::test]
async fn land_without_heights_is_listed_by_both_readers_and_the_pack_stays_complete() {
    for reader in READERS {
        let directory = tempfile::tempdir().unwrap();
        let config = fixture(directory.path(), &land_without_heights(0x0100_08B0), reader);
        let output = config.output_dir.clone();
        let report = publish(config).await;
        assert!(report.complete, "{reader:?}: {report:?}");
        assert!(
            report.warnings.is_empty(),
            "{reader:?}: {:?}",
            report.warnings
        );
        let manifest = json(&output.join("conversion-manifest.json"));
        assert_eq!(manifest["complete"], true, "{reader:?}");
        assert_eq!(manifest["record_data_complete"], true, "{reader:?}");
        let listed = manifest["intended_omissions"].as_array().unwrap();
        assert_eq!(listed.len(), 1, "{reader:?}: {listed:?}");
        assert_eq!(listed[0]["source"], "patch.esp", "{reader:?}");
        assert_eq!(listed[0]["record_type"], "LAND");
        assert_eq!(listed[0]["class"], "land_missing_heights");
        assert_eq!(listed[0]["count"], 1);
        assert!(
            report
                .notices
                .iter()
                .any(|n| n.contains("patch.esp") && n.contains("land_missing_heights")),
            "{reader:?}: {:?}",
            report.notices
        );
    }
}

/// An authored `VHGT` that is empty or malformed is not an absent field: it stays a reader
/// failure and the pack stays incomplete, under both readers.
#[tokio::test]
async fn empty_or_malformed_vhgt_stays_a_reader_failure() {
    for reader in READERS {
        for (label, vhgt) in [("empty", Vec::new()), ("short", vec![0u8; 100])] {
            let directory = tempfile::tempdir().unwrap();
            let config = fixture(
                directory.path(),
                &land_with_vhgt(0x0100_08B0, &vhgt),
                reader,
            );
            let output = config.output_dir.clone();
            let report = publish(config).await;
            assert!(!report.complete, "{reader:?} {label}: {report:?}");
            assert!(!report.warnings.is_empty(), "{reader:?} {label}");
            assert!(report.intended_omissions.is_empty(), "{reader:?} {label}");
            let manifest = json(&output.join("conversion-manifest.json"));
            assert_eq!(manifest["complete"], false, "{reader:?} {label}");
            assert_eq!(
                manifest["record_data_complete"], false,
                "{reader:?} {label}"
            );
            assert!(
                manifest.get("intended_omissions").is_none(),
                "{reader:?} {label}"
            );
        }
    }
}

/// The in-house diagnostics, journal and rows of the absent-`VHGT` case.
#[tokio::test]
async fn land_without_heights_is_journaled_by_the_inhouse_reader() {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(
        directory.path(),
        &land_without_heights(0x0100_08B0),
        RecordReader::Inhouse,
    );
    let output = config.output_dir.clone();
    let report = publish(config).await;

    assert!(report.complete, "{report:?}");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.skipped, 0);
    let manifest = json(&output.join("conversion-manifest.json"));
    assert_eq!(manifest["complete"], true);
    assert_eq!(manifest["record_data_complete"], true);
    assert!(manifest["failures"].as_object().unwrap().is_empty());

    // One counted row per plugin, record type and reason class, with the first example.
    let listed = manifest["intended_omissions"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["source"], "patch.esp");
    assert_eq!(listed[0]["record_type"], "LAND");
    assert_eq!(listed[0]["class"], "land_missing_heights");
    assert_eq!(listed[0]["count"], 1);
    assert!(
        listed[0]["first_example"]
            .as_str()
            .unwrap()
            .contains("no VHGT height field"),
        "{listed:?}"
    );
    assert_eq!(report.intended_omissions.len(), 1);
    assert_eq!(
        serde_json::to_value(&report.intended_omissions).unwrap(),
        manifest["intended_omissions"]
    );

    // It stays visible: a notice, the diagnostics counters, and the event journal.
    assert!(
        report
            .notices
            .iter()
            .any(|notice| notice.contains("patch.esp") && notice.contains("land_missing_heights")),
        "{:?}",
        report.notices
    );
    let diagnostics = json(&output.join("inhouse-reader-diagnostics.json"));
    let plugin = &diagnostics["decoder"]["patch.esp"];
    assert_eq!(plugin["omitted_records"], 1);
    assert_eq!(plugin["skipped_records"], 0);
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    let journaled: u32 = database
        .query_row(
            "SELECT COUNT(*) FROM record_diagnostics WHERE category='omission' AND record_type='LAND'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(journaled, 1);
    // No height was invented for it.
    let rows: u32 = database
        .query_row(
            "SELECT COUNT(*) FROM land WHERE cell_id=4096 AND length(heightmap)>0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0, "the generated master has no LAND in this cell");
}

/// A real reader failure (a damaged record) is not an intended omission: still incomplete.
#[tokio::test]
async fn a_damaged_land_is_still_a_reader_failure() {
    let directory = tempfile::tempdir().unwrap();
    let damaged = record(b"LAND", 0x0100_08B0, 0x40000, &[24, 0, 0, 0, 99]);
    let config = fixture(directory.path(), &damaged, RecordReader::Inhouse);
    let output = config.output_dir.clone();
    let report = publish(config).await;

    common::assert_reader_warning_publication(&report, &output);
    let manifest = json(&output.join("conversion-manifest.json"));
    assert!(manifest.get("intended_omissions").is_none(), "{manifest}");
    assert!(report.intended_omissions.is_empty());
    let diagnostics = json(&output.join("inhouse-reader-diagnostics.json"));
    assert_eq!(diagnostics["decoder"]["patch.esp"]["skipped_records"], 1);
    assert!(
        diagnostics["decoder"]["patch.esp"]
            .get("omitted_records")
            .is_none()
    );
}

/// Both together: the failure alone decides completeness, and the omission stays listed.
#[tokio::test]
async fn a_failure_beside_an_omission_keeps_the_pack_incomplete() {
    let directory = tempfile::tempdir().unwrap();
    let mut content = land_without_heights(0x0100_08B0);
    content.extend(record(b"LAND", 0x0100_08B1, 0x40000, &[24, 0, 0, 0, 99]));
    let config = fixture(directory.path(), &content, RecordReader::Inhouse);
    let output = config.output_dir.clone();
    let report = publish(config).await;

    assert!(!report.complete, "{report:?}");
    let manifest = json(&output.join("conversion-manifest.json"));
    assert_eq!(manifest["complete"], false);
    assert_eq!(manifest["record_data_complete"], false);
    assert_eq!(manifest["intended_omissions"].as_array().unwrap().len(), 1);
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| !warning.contains("land_missing_heights")),
        "{:?}",
        report.warnings
    );
}
