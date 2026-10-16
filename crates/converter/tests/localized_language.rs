//! Localized plugins' names come from the string tables of the configured language (#195), read
//! as UTF-8 or, failing that, Windows-1252. Synthetic plugins and tables; no game data.
use converter::{AssetPipeline, PipelineConfig, PipelineReport};
use dummy_content::{Entry, bsa};
use rusqlite::Connection;
use std::{fs, path::Path};

const LOCALIZED: u32 = 0x80;

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// A record with form version 44, as Skyrim SE writes them.
fn record(tag: &[u8; 4], id: u32, flags: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 4],
        &44u16.to_le_bytes(),
        &[0; 2],
        &payload,
    ]
    .concat()
}

/// A localized master whose classes name their `FULL` by string ID 1, 2 and 3.
fn localized_master(data: &Path) {
    let mut bytes = record(b"TES4", 0, 0x1 | LOCALIZED, sub(b"HEDR", &[0; 12]));
    for (id, string) in [(0x800, 1u32), (0x801, 2), (0x802, 3)] {
        bytes.extend(record(
            b"CLAS",
            id,
            0,
            [
                sub(b"EDID", format!("Class{id:X}\0").as_bytes()),
                sub(b"FULL", &string.to_le_bytes()),
            ]
            .concat(),
        ));
    }
    fs::write(data.join("Base.esm"), bytes).unwrap();
}

/// A `.STRINGS` table holding each `(id, bytes)` entry NUL-terminated, in the on-disk layout.
fn table(entries: &[(u32, &[u8])]) -> Vec<u8> {
    let mut directory = Vec::new();
    let mut data = Vec::new();
    for (id, text) in entries {
        directory.extend(id.to_le_bytes());
        directory.extend((data.len() as u32).to_le_bytes());
        data.extend(*text);
        data.push(0);
    }
    [
        (entries.len() as u32).to_le_bytes().as_slice(),
        &(data.len() as u32).to_le_bytes(),
        &directory,
        &data,
    ]
    .concat()
}

async fn convert(data: &Path, output: &Path, language: &str) -> PipelineReport {
    let mut config = PipelineConfig::new(data, output);
    config.no_lod = true;
    config.language = language.to_owned();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    report
}

/// The resolved `FULL` of each class, in FormID order.
fn names(output: &Path) -> Vec<Option<String>> {
    Connection::open(output.join("skyrim_world.db"))
        .unwrap()
        .prepare(
            "SELECT resolved_text FROM inhouse_localized_fields
             WHERE signature='FULL' ORDER BY form_id",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn reader_language(output: &Path) -> serde_json::Value {
    let identity: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("record-reader.json")).unwrap()).unwrap();
    identity["language"].clone()
}

/// The language picks the table inside the plugin's archive, a loose table overrides it, UTF-8
/// text decodes as UTF-8 and other text as Windows-1252, and a language with no table publishes
/// empty names and an incomplete run.
#[tokio::test]
async fn names_come_from_the_tables_of_the_configured_language() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    fs::create_dir_all(&data).unwrap();
    localized_master(&data);
    let english = table(&[(1, b"Lydia"), (2, b"Faendal"), (3, b"Prisoner")]);
    // "Müller" in Windows-1252, "Jäger" and "Gefangener" in UTF-8.
    let german = table(&[
        (1, b"Lydia M\xfcller"),
        (2, "Faendal der Jäger".as_bytes()),
        (3, b"Gefangener"),
    ]);
    let russian = table(&[
        (1, "Лидия".as_bytes()),
        (2, "Фаендал".as_bytes()),
        (3, "Пленник".as_bytes()),
    ]);
    let archive = bsa::v105(
        &[
            Entry::new("strings/base_english.strings", &english),
            Entry::new("strings/base_german.strings", &german),
            Entry::new("strings/base_russian.strings", &russian),
        ],
        bsa::Compression::None,
    )
    .unwrap();
    fs::write(data.join("Base.bsa"), archive).unwrap();

    let output = dir.path().join("english");
    let report = convert(&data, &output, "english").await;
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(
        names(&output),
        [
            Some("Lydia".into()),
            Some("Faendal".into()),
            Some("Prisoner".into())
        ]
    );
    assert_eq!(reader_language(&output), "english");

    let output = dir.path().join("german");
    let report = convert(&data, &output, "german").await;
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(
        names(&output),
        [
            Some("Lydia Müller".into()),
            Some("Faendal der Jäger".into()),
            Some("Gefangener".into())
        ]
    );
    assert_eq!(reader_language(&output), "german");

    let output = dir.path().join("russian");
    let report = convert(&data, &output, "russian").await;
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(
        names(&output),
        [
            Some("Лидия".into()),
            Some("Фаендал".into()),
            Some("Пленник".into())
        ]
    );

    // A loose table in Data/Strings wins over the archive's, matched without regard to case.
    fs::create_dir_all(data.join("Strings")).unwrap();
    fs::write(
        data.join("Strings/Base_German.STRINGS"),
        table(&[
            (1, b"Lydia die Huscarl"),
            (2, b"Faendal"),
            (3, b"Gefangener"),
        ]),
    )
    .unwrap();
    let output = dir.path().join("loose");
    let report = convert(&data, &output, "german").await;
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(names(&output)[0], Some("Lydia die Huscarl".into()));

    let output = dir.path().join("klingon");
    let report = convert(&data, &output, "klingon").await;
    assert!(!report.complete);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("base.esm")),
        "{:?}",
        report.warnings
    );
    assert_eq!(names(&output), [None, None, None]);
}

/// The language changes the database, never the converted assets: it is part of the reader
/// identity and the metadata proof, not of the asset settings.
#[test]
fn the_language_changes_the_metadata_proof_but_not_the_asset_settings() {
    let english = PipelineConfig::new("Data", "output");
    let mut german = english.clone();
    german.language = "german".into();
    assert_ne!(
        converter::cache::configuration_hash(&english).unwrap(),
        converter::cache::configuration_hash(&german).unwrap()
    );
    for kind in ["nif", "dds", "pex"] {
        assert_eq!(
            converter::cache::asset_configuration_hash_for_schema(
                &english,
                converter::cache::CONVERTER_SCHEMA_VERSION,
                kind
            )
            .unwrap(),
            converter::cache::asset_configuration_hash_for_schema(
                &german,
                converter::cache::CONVERTER_SCHEMA_VERSION,
                kind
            )
            .unwrap(),
            "{kind}"
        );
    }
}

/// English tables keep the identity English-only readers stamped, so English packs are unchanged;
/// another language, read as UTF-8, has an identity of its own.
#[test]
fn only_another_language_changes_the_reader_identity() {
    use converter::{config::RecordReader, esm::inhouse};
    let english = inhouse::reader_identity(RecordReader::Inhouse);
    assert_eq!(english["language"], "english");
    assert!(english.get("string_table_encoding").is_none());
    assert_eq!(
        inhouse::reader_identity_in_language(RecordReader::Inhouse, "english"),
        english
    );
    let german = inhouse::reader_identity_in_language(RecordReader::Inhouse, "german");
    assert_eq!(german["language"], "german");
    assert_eq!(german["string_table_encoding"], "utf-8-or-windows-1252");
    assert_eq!(
        inhouse::reader_identity_in_language(RecordReader::Legacy, "german"),
        inhouse::reader_identity(RecordReader::Legacy)
    );
}
