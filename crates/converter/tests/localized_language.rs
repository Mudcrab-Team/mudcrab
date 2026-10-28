//! Localized plugins' names come from the string tables of the configured language (#195), read
//! as UTF-8 or, failing that, Windows-1252. Synthetic plugins and tables; no game data.
use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, config::RecordReader,
    esm::types::ArchivedRecordData,
};
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

/// A localized master whose classes name their `FULL` by string IDs 1, 2, 3 and 4.
fn localized_master(data: &Path) {
    let mut bytes = record(b"TES4", 0, 0x1 | LOCALIZED, sub(b"HEDR", &[0; 12]));
    for (id, string) in [(0x800, 1u32), (0x801, 2), (0x802, 3), (0x803, 4)] {
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

/// Legacy publishes the selected localized text in canonical `records.data`.
fn legacy_full(output: &Path, form_id: u32) -> Option<String> {
    let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT data FROM records WHERE form_id=?1",
            [form_id],
            |row| row.get(0),
        )
        .unwrap();
    let record = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes).unwrap();
    let full = record
        .subrecords
        .iter()
        .find(|field| &field.tag == b"FULL")?;
    let text = full.data.strip_suffix(&[0]).unwrap_or(&full.data);
    Some(String::from_utf8(text.to_vec()).unwrap())
}

async fn convert_with(
    data: &Path,
    output: &Path,
    language: &str,
    reader: RecordReader,
) -> PipelineReport {
    let mut config = PipelineConfig::new(data, output);
    config.no_lod = true;
    config.language = language.to_owned();
    config.record_reader = reader;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    report
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
    let english = table(&[(1, b"Lydia"), (2, b"Faendal"), (3, b"Prisoner"), (4, b"")]);
    // "Müller" in Windows-1252, "Jäger" and "Gefangener" in UTF-8.
    let german = table(&[
        (1, b"Lydia M\xfcller"),
        (2, "Faendal der Jäger".as_bytes()),
        (3, b"Gefangener"),
        (4, b""),
    ]);
    let russian = table(&[
        (1, "Лидия".as_bytes()),
        (2, "Фаендал".as_bytes()),
        (3, "Пленник".as_bytes()),
        (4, b""),
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
            Some("Prisoner".into()),
            Some("".into()),
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
            Some("Gefangener".into()),
            Some("".into()),
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
            Some("Пленник".into()),
            Some("".into()),
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
            (4, b""),
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
    assert_eq!(names(&output), [None, None, None, None]);

    // A corrupt selected bank keeps its parser reason in the legacy warning.
    let broken = dir.path().join("BrokenData");
    fs::create_dir_all(&broken).unwrap();
    localized_master(&broken);
    fs::write(
        broken.join("Base.bsa"),
        bsa::v105(
            &[Entry::new("strings/base_english.strings", b"bad")],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
    let broken_output = dir.path().join("broken-legacy");
    let report = convert_with(&broken, &broken_output, "english", RecordReader::Legacy).await;
    assert!(!report.complete);
    assert!(
        report.warnings.iter().any(|warning| {
            warning.contains("unreadable strings table")
                && warning.contains("truncated STRINGS header")
        }),
        "{:?}",
        report.warnings
    );
}

/// The language changes the database, never the converted assets: it is part of the reader
/// identity and the metadata proof, not of the asset settings.
#[test]
fn the_language_changes_the_metadata_proof_but_not_the_asset_settings() {
    for reader in [RecordReader::Inhouse, RecordReader::Legacy] {
        let mut english = PipelineConfig::new("Data", "output");
        english.record_reader = reader;
        let mut german = english.clone();
        german.language = "german".into();
        assert_ne!(
            converter::esm::inhouse::reader_identity_in_language(reader, &english.language),
            converter::esm::inhouse::reader_identity_in_language(reader, &german.language),
            "{reader:?} metadata identity"
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
                "{reader:?} {kind}"
            );
        }
    }
}

/// Both reader identities bind the configured language and its table encoding.
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
    let legacy_english = inhouse::reader_identity(RecordReader::Legacy);
    let legacy_german = inhouse::reader_identity_in_language(RecordReader::Legacy, "german");
    assert_eq!(legacy_english["language"], "english");
    assert!(legacy_english.get("string_table_encoding").is_none());
    assert_eq!(legacy_german["language"], "german");
    assert_eq!(
        legacy_german["string_table_encoding"],
        "utf-8-or-windows-1252"
    );
    assert_ne!(legacy_english, legacy_german);
}

/// A warm database follows the selected language and the winning German bank, for both readers:
/// unchanged reruns restore, while a language switch, an edited loose winner, or the archive
/// fallback after removing that winner each rebuild the database.
#[tokio::test]
async fn database_cache_follows_language_and_winning_bank_for_both_readers() {
    for reader in [RecordReader::Inhouse, RecordReader::Legacy] {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Data");
        fs::create_dir_all(&data).unwrap();
        localized_master(&data);
        let english = table(&[(1, b"Lydia"), (2, b"Faendal"), (3, b"Prisoner"), (4, b"")]);
        let german = table(&[
            (1, b"Lydia M\xfcller"),
            (2, "Faendal der Jäger".as_bytes()),
            (3, b"Gefangener"),
            (4, b""),
        ]);
        let archive = bsa::v105(
            &[
                Entry::new("strings/base_english.strings", &english),
                Entry::new("strings/base_german.strings", &german),
            ],
            bsa::Compression::None,
        )
        .unwrap();
        fs::write(data.join("Base.bsa"), archive).unwrap();
        let output = dir.path().join("modern");

        let cold = convert_with(&data, &output, "english", reader).await;
        assert!(
            cold.complete && !cold.database_cache_hit,
            "{reader:?} cold english"
        );
        assert!(cold.warnings.is_empty(), "{reader:?}: {:?}", cold.warnings);
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["record_data_complete"], true);
        let warm = convert_with(&data, &output, "english", reader).await;
        assert!(
            warm.database_cache_hit,
            "{reader:?} unchanged english restores"
        );

        let switched = convert_with(&data, &output, "german", reader).await;
        assert!(
            !switched.database_cache_hit,
            "{reader:?} english to german must rebuild"
        );
        let german_warm = convert_with(&data, &output, "german", reader).await;
        assert!(
            german_warm.database_cache_hit,
            "{reader:?} unchanged german restores: complete={} key={:?} warnings={:?} notices={:?}",
            german_warm.complete,
            german_warm.database_cache_key,
            german_warm.warnings,
            german_warm.notices
        );
        if reader == RecordReader::Inhouse {
            assert_eq!(names(&output)[0], Some("Lydia Müller".into()));
            assert_eq!(names(&output)[3], Some("".into()));
        } else {
            assert_eq!(legacy_full(&output, 0x800), Some("Lydia Müller".into()));
            assert_eq!(legacy_full(&output, 0x803), Some("".into()));
        }

        // The loose German winner overrides the archive; editing it misses the cache.
        fs::create_dir_all(data.join("Strings")).unwrap();
        let loose = data.join("Strings/Base_German.STRINGS");
        fs::write(
            &loose,
            table(&[
                (1, b"Lydia die Huscarl"),
                (2, b"Faendal"),
                (3, b"Gefangener"),
                (4, b""),
            ]),
        )
        .unwrap();
        let added = convert_with(&data, &output, "german", reader).await;
        assert!(
            !added.database_cache_hit,
            "{reader:?} loose winner must rebuild"
        );
        if reader == RecordReader::Inhouse {
            assert_eq!(names(&output)[0], Some("Lydia die Huscarl".into()));
        } else {
            assert_eq!(
                legacy_full(&output, 0x800),
                Some("Lydia die Huscarl".into())
            );
        }
        let repeated = convert_with(&data, &output, "german", reader).await;
        assert!(
            repeated.database_cache_hit,
            "{reader:?} unchanged loose winner restores"
        );
        fs::write(
            &loose,
            table(&[
                (1, b"Lydia die Wache"),
                (2, b"Faendal"),
                (3, b"Gefangener"),
                (4, b""),
            ]),
        )
        .unwrap();
        let edited = convert_with(&data, &output, "german", reader).await;
        assert!(
            !edited.database_cache_hit,
            "{reader:?} edited loose winner must miss"
        );

        // Removing the loose winner falls back to the archive bank: a new origin, so a miss.
        fs::remove_file(&loose).unwrap();
        let fallback = convert_with(&data, &output, "german", reader).await;
        assert!(
            !fallback.database_cache_hit,
            "{reader:?} archive fallback must miss"
        );
        if reader == RecordReader::Inhouse {
            assert_eq!(names(&output)[0], Some("Lydia Müller".into()));
        } else {
            assert_eq!(legacy_full(&output, 0x800), Some("Lydia Müller".into()));
        }
    }
}
