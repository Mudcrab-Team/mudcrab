//! Broken RACE authoring is reported once per race while usable neighbors publish.

mod common;

use converter::{AssetPipeline, config::RecordReader};
use dummy_content::layout;
use rusqlite::Connection;
use std::{fs, path::Path};

/// Writes a bounded Skyrim SE record from explicit subrecord bytes.
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

/// Adds two broken races, one valid race, and two NPC users of the first bad race.
fn generate(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm,lodsettings").unwrap(),
    )
    .unwrap();
    let path = data.join("Skyrim.esm");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend(record(
        b"RACE",
        0x0801,
        &[(b"EDID", b"NoSexSelector\0"), (b"ANAM", b"generated.nif\0")],
    ));
    bytes.extend(record(
        b"RACE",
        0x0802,
        &[
            (b"EDID", b"InvalidUtf8\0"),
            (b"MNAM", b""),
            (b"ANAM", &[0xff, 0]),
        ],
    ));
    bytes.extend(record(
        b"RACE",
        0x0803,
        &[
            (b"EDID", b"ReadableNeighbor\0"),
            (b"MNAM", b""),
            (b"ANAM", b"generated.nif\0"),
        ],
    ));
    for id in [0x0811, 0x0812] {
        bytes.extend(record(
            b"NPC_",
            id,
            &[
                (b"EDID", b"BadRaceUser\0"),
                (b"ACBS", &[0; 24]),
                (b"RNAM", &0x0801u32.to_le_bytes()),
            ],
        ));
    }
    fs::write(path, bytes).unwrap();
}

/// A terminal-only warning cannot satisfy the persisted report or completeness assertions.
#[tokio::test]
async fn broken_race_authoring_publishes_neighbors_and_persisted_omissions() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("converted");
    generate(&data);
    // The legacy path retains the invalid UTF-8 bytes, proving the authoring reader's
    // recovery independently of the schema decoder's own invalid-string diagnostics.
    let mut config = common::cpu_lod_config(&data, &output);
    config.record_reader = RecordReader::Legacy;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, sender).await.unwrap();
    drain.await.unwrap();
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let omissions: Vec<(u32, String)> = db
        .prepare("SELECT form_id, reason FROM projection_omission_samples WHERE category='race_unreadable_authoring' ORDER BY form_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(omissions.len(), 2, "one persisted diagnostic per bad RACE");
    assert_eq!(omissions[0].0, 0x0801);
    assert!(omissions[0].1.contains("no sex selector"));
    assert_eq!(omissions[1].0, 0x0802);
    assert!(omissions[1].1.contains("utf-8"));
    let count: u32 = db.query_row("SELECT count FROM projection_omission_stats WHERE category='race_unreadable_authoring'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 2, "NPC users must not multiply a race omission");
    let authored: (Option<String>, Option<String>) = db.query_row("SELECT skeleton_nif,project_hkx FROM race_animation_sources WHERE race_id=2051 AND sex='male'", [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(authored, (Some("meshes/generated.nif".into()), None));
    let retained: u32 = db
        .query_row(
            "SELECT count(*) FROM records WHERE form_id IN (2049,2050,2051)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        retained, 3,
        "canonical RACE data survives failed authoring projection"
    );
    assert!(output.join("meshes/generated.glb").is_file());
    assert!(output.join("cell_cache.rkyv").is_file());
    assert_eq!(report.skipped, 0);
    assert!(!report.complete);
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|line| line.contains("race_unreadable_authoring"))
            .count(),
        1
    );
    let integration: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("integration-report.json")).unwrap()).unwrap();
    assert_eq!(
        integration["projection_omissions"]["race_unreadable_authoring"]["count"],
        2
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["complete"], false);
    assert!(manifest["failures"].as_object().unwrap().is_empty());
}

/// Standalone actor-model consumers receive the same RACE error once, with its details.
#[test]
fn actor_catalog_retains_unique_race_authoring_errors() {
    let directory = tempfile::tempdir().unwrap();
    generate(directory.path());
    let records = converter::esm::binary::parse_plugin_file(&directory.path().join("Skyrim.esm"))
        .unwrap()
        .into_iter()
        .map(|record| (record.form_id, record))
        .collect();
    let catalog = converter::actor_models::resolve_actor_models(&records, &Default::default());
    assert_eq!(catalog.race_authoring_errors.len(), 1);
    assert!(catalog.race_authoring_errors[&0x0801].contains("no sex selector"));
    assert_eq!(
        catalog
            .models
            .iter()
            .filter(|row| [0x0811, 0x0812].contains(&row.id))
            .count(),
        2
    );
}
