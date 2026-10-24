//! Public publication and replay preserve the severity of independent in-house recoveries.
use converter::{AssetPipeline, PipelineConfig, PipelineReport, config::RecordReader};
use rusqlite::Connection;
use std::{fs, path::Path, process::Command};

/// Frame a synthetic native field without using either reader's serializer.
fn field(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Supply native record framing with asymmetric nonidentity header sentinels.
fn record(tag: &[u8; 4], id: u32, flags: u32, bytes: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(bytes.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &0x1739_A541u32.to_le_bytes(),
        &44u16.to_le_bytes(),
        &0xB913u16.to_le_bytes(),
        bytes,
    ]
    .concat()
}

/// Write a valid header with an independently supplied master list and body.
fn plugin(data: &Path, name: &str, flags: u32, masters: &[&str], body: &[u8]) {
    fs::create_dir_all(data).unwrap();
    let mut header = field(
        b"HEDR",
        &[
            1.7f32.to_le_bytes(),
            8u32.to_le_bytes(),
            0x900u32.to_le_bytes(),
        ]
        .concat(),
    );
    for master in masters {
        header.extend(field(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(field(b"DATA", &[0; 8]));
    }
    fs::write(
        data.join(name),
        [record(b"TES4", 0, flags, &header), body.to_vec()].concat(),
    )
    .unwrap();
}

/// Author a healthy record whose canonical identity can be checked after publication.
fn keyword(id: u32) -> Vec<u8> {
    record(b"KYWD", id, 0, &field(b"EDID", b"HealthyNeighbor\0"))
}

/// Distinguish the patch's local self index one from its global load-order slot two.
fn link_fixture(data: &Path, skipped_target: bool) {
    plugin(data, "A.esm", 1, &[], &keyword(0x810));
    plugin(data, "B.esm", 1, &[], &keyword(0x820));
    let mut body = [
        keyword(0x0100_0801),
        record(
            b"FLST",
            0x0100_0802,
            0,
            &[
                field(b"EDID", b"AbsentTarget\0"),
                field(b"LNAM", &0x0100_0900u32.to_le_bytes()),
            ]
            .concat(),
        ),
    ]
    .concat();
    if skipped_target {
        body.extend(record(b"STAT", 0x0100_0900, 0x40000, &[7, 0, 0, 0, 0xFF]));
    }
    plugin(data, "Patch.esp", 0, &["A.esm"], &body);
    fs::write(data.join("plugins.txt"), "*A.esm\n*B.esm\n*Patch.esp\n").unwrap();
}

/// Create a localized NPC and optionally a readable bank containing a different key.
fn localized_fixture(data: &Path, bank: Option<&[u8]>) {
    plugin(
        data,
        "Text.esm",
        0x81,
        &[],
        &[
            keyword(0x801),
            record(
                b"NPC_",
                0x802,
                0,
                &[
                    field(b"EDID", b"MissingName\0"),
                    field(b"FULL", &6515u32.to_le_bytes()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    if let Some(bytes) = bank {
        fs::create_dir(data.join("Strings")).unwrap();
        fs::write(data.join("Strings/Text_english.strings"), bytes).unwrap();
    }
}

/// Encode one valid STRINGS entry whose ID differs from the requested localized ID.
fn readable_bank() -> Vec<u8> {
    [
        1u32.to_le_bytes().as_slice(),
        &9u32.to_le_bytes(),
        &7u32.to_le_bytes(),
        &0u32.to_le_bytes(),
        b"Existing\0",
    ]
    .concat()
}

/// Keep one healthy STRINGS entry beside one directory entry with an invalid offset.
fn malformed_bank() -> Vec<u8> {
    [
        2u32.to_le_bytes().as_slice(),
        &9u32.to_le_bytes(),
        &7u32.to_le_bytes(),
        &0u32.to_le_bytes(),
        &6515u32.to_le_bytes(),
        &u32::MAX.to_le_bytes(),
        b"Existing\0",
    ]
    .concat()
}

/// Keep asset work minimal while using the normal public conversion configuration.
fn config(data: &Path, output: &Path, reader: RecordReader) -> PipelineConfig {
    let mut config = PipelineConfig::new(data, output);
    config.record_reader = reader;
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    config
}

/// Drain progress while publishing fresh bytes through the public pipeline API.
async fn publish(data: &Path, output: &Path, reader: RecordReader) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config(data, output, reader), tx)
        .await
        .unwrap();
    drain.await.unwrap();
    report
}

/// Run the default reader's real CLI, optionally authenticating an existing asset pack.
fn cli(
    data: &Path,
    output: &Path,
    report_path: &Path,
    source: Option<&Path>,
) -> (i32, PipelineReport) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_converter"));
    command
        .arg(data)
        .arg(output)
        .arg("--no-lod")
        .arg("--texture-encoder")
        .arg("cpu")
        .arg("--report-json")
        .arg(report_path);
    if let Some(source) = source {
        command.arg("--reuse-assets").arg(source);
    }
    let run = command.output().unwrap();
    let report = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    (run.status.code().unwrap(), report)
}

/// Read exact published identities without relying on diagnostic wording.
fn ids(output: &Path) -> Vec<u32> {
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    db.prepare("SELECT form_id FROM records ORDER BY form_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Require one bounded notice for the specified plugin and recovery kind.
fn notice(report: &PipelineReport, plugin: &str, kind: &str, count: &str, example: &str) {
    let messages: Vec<_> = report
        .notices
        .iter()
        .filter(|line| line.contains(plugin) && line.contains(kind))
        .collect();
    assert_eq!(messages.len(), 1, "{:?}", report.notices);
    let line = messages[0];
    assert!(line.contains(count), "{line}");
    assert!(line.contains("first:"), "{line}");
    assert!(line.contains(example), "{line}");
}

/// Require manifest proof and report status to agree on whether records were faithfully retained.
fn completeness(report: &PipelineReport, output: &Path, expected: bool) {
    assert_eq!(report.complete, expected, "{:?}", report.warnings);
    assert_eq!(report.skipped, 0);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["complete"], expected);
    assert_eq!(manifest["record_data_complete"], expected);
    assert!(output.join("cell_cache.rkyv").is_file());
}

/// A single absent target is faithful recovery and the default CLI must succeed.
#[test]
fn dangling_link_only_is_complete_notice_and_cli_zero() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("pack");
    link_fixture(&data, false);
    let (status, report) = cli(&data, &output, &directory.path().join("report.json"), None);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    notice(
        &report,
        "patch.esp",
        "in-house reader cleared",
        "1 links",
        "02000900",
    );
    assert_eq!(ids(&output), [0x810, 0x0100_0820, 0x0200_0801, 0x0200_0802]);
    completeness(&report, &output, true);
    assert_eq!(status, 0);
}

/// Readable empty and populated tables with a missing ID retain complete notice-only packs.
#[tokio::test]
async fn missing_id_in_readable_table_is_complete_notice() {
    for bank in [vec![0; 8], readable_bank()] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let output = directory.path().join("pack");
        localized_fixture(&data, Some(&bank));
        let report = publish(&data, &output, RecordReader::Inhouse).await;
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        notice(
            &report,
            "text.esm",
            "localized IDs missing from readable string tables",
            "1 localized IDs",
            "6515",
        );
        assert_eq!(ids(&output), [0x801, 0x802]);
        completeness(&report, &output, true);
    }
}

/// A referenced malformed entry warns and makes CLI incomplete while healthy text survives.
#[test]
fn malformed_entry_warns_cli_one_and_preserves_healthy_neighbor_and_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("pack");
    plugin(
        &data,
        "Text.esm",
        0x81,
        &[],
        &[
            keyword(0x801),
            record(
                b"NPC_",
                0x802,
                0,
                &[
                    field(b"EDID", b"MalformedName\0"),
                    field(b"FULL", &6515u32.to_le_bytes()),
                ]
                .concat(),
            ),
            record(
                b"NPC_",
                0x803,
                0,
                &[
                    field(b"EDID", b"HealthyName\0"),
                    field(b"FULL", &7u32.to_le_bytes()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    fs::create_dir(data.join("Strings")).unwrap();
    fs::write(data.join("Strings/Text_english.strings"), malformed_bank()).unwrap();
    let (status, report) = cli(&data, &output, &directory.path().join("report.json"), None);
    assert_eq!(status, 1);
    completeness(&report, &output, false);
    assert_eq!(ids(&output), [0x801, 0x802, 0x803]);
    assert_eq!(
        report.notices,
        ["Terrain LOD compilation disabled by --no-lod".to_owned()]
    );
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("malformed strings entry for localized ID 6515"));
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    type TextProvenance = (u32, u32, Option<String>, String);
    let provenance: Vec<TextProvenance> = db
        .prepare("SELECT form_id,string_id,resolved_text,status FROM inhouse_localized_fields ORDER BY form_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        provenance,
        [
            (0x802, 6515, None, "missing".into()),
            (0x803, 7, Some("Existing".into()), "resolved".into()),
        ]
    );
    let source_records: Vec<(u32, u64)> = db
        .prepare("SELECT form_id,length(payload) FROM inhouse_source_records WHERE form_id IN (?1,?2) ORDER BY form_id")
        .unwrap()
        .query_map([0x802u32, 0x803], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(source_records.len(), 2);
    assert!(source_records.iter().all(|(_, bytes)| *bytes > 0));
}

/// Null IDs need no bank; a later nonzero reference still reports the missing bank.
#[test]
fn null_only_missing_bank_is_complete_but_later_reference_warns() {
    for include_nonzero in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let output = directory.path().join("pack");
        let mut body = keyword(0x801);
        body.extend(record(
            b"NPC_",
            0x802,
            0,
            &[
                field(b"EDID", b"NullName\0"),
                field(b"FULL", &0u32.to_le_bytes()),
            ]
            .concat(),
        ));
        if include_nonzero {
            body.extend(record(
                b"NPC_",
                0x803,
                0,
                &[
                    field(b"EDID", b"MissingName\0"),
                    field(b"FULL", &6515u32.to_le_bytes()),
                ]
                .concat(),
            ));
        }
        plugin(&data, "Text.esm", 0x81, &[], &body);
        let (status, report) = cli(&data, &output, &directory.path().join("report.json"), None);
        assert_eq!(status, if include_nonzero { 1 } else { 0 });
        completeness(&report, &output, !include_nonzero);
        let db = Connection::open(output.join("skyrim_world.db")).unwrap();
        let null_status: String = db
            .query_row(
                "SELECT status FROM inhouse_localized_fields WHERE form_id=?1",
                [0x802u32],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(null_status, "null_id");
        assert_eq!(
            report.notices,
            ["Terrain LOD compilation disabled by --no-lod".to_owned()]
        );
        if include_nonzero {
            assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
            assert!(report.warnings[0].contains("2 local decode/lookup issues"));
            assert!(!report.warnings[0].contains("null_id"));
        } else {
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        }
    }
}

/// Clearing a link to a record we skipped cannot hide the actual record-loss warning.
#[test]
fn skipped_target_keeps_warning_and_notice_and_healthy_neighbor_with_cli_one() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("pack");
    link_fixture(&data, true);
    let (status, report) = cli(&data, &output, &directory.path().join("report.json"), None);
    assert_eq!(ids(&output), [0x810, 0x0100_0820, 0x0200_0801, 0x0200_0802]);
    completeness(&report, &output, false);
    assert_eq!(status, 1);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    let warning = &report.warnings[0];
    assert!(
        warning.contains("1 records, 0 fields, 0 unexpected subrecords"),
        "{warning}"
    );
    assert!(
        warning.contains("first:") && warning.contains("01000900"),
        "{warning}"
    );
    assert!(!warning.contains("links"), "{warning}");
    notice(
        &report,
        "patch.esp",
        "in-house reader cleared",
        "1 links",
        "02000900",
    );
}

/// Missing banks and malformed banks both remain bounded warnings and retain healthy rows.
#[tokio::test]
async fn missing_or_unreadable_table_remains_warning() {
    for bank in [None, Some(&b"bad"[..])] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let output = directory.path().join("pack");
        localized_fixture(&data, bank);
        let report = publish(&data, &output, RecordReader::Inhouse).await;
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(
            report.warnings[0].contains("reported 2 local decode/lookup issues")
                && report.warnings[0].contains("missing localized ID 6515"),
            "{:?}",
            report.warnings
        );
        assert!(report.warnings[0].contains("first:"));
        assert!(
            !report
                .notices
                .iter()
                .any(|line| line.contains("readable string tables"))
        );
        assert_eq!(ids(&output), [0x801, 0x802]);
        completeness(&report, &output, false);
    }
}

/// Authenticated API rebuild and CLI reuse replay the same notice and preserve their source.
#[tokio::test]
async fn metadata_rebuild_and_reuse_replay_link_notice_and_complete_proof() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    link_fixture(&data, false);
    let original = publish(&data, &source, RecordReader::Legacy).await;
    completeness(&original, &source, true);
    let input_before = fs::read(data.join("Patch.esp")).unwrap();
    let manifest_before = fs::read(source.join("conversion-manifest.json")).unwrap();
    let database_before = fs::read(source.join("skyrim_world.db")).unwrap();
    let output = directory.path().join("api");
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::rebuild_metadata_async(
        config(&data, &output, RecordReader::Inhouse),
        &source,
        tx,
    )
    .await
    .unwrap();
    drain.await.unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    notice(
        &report,
        "patch.esp",
        "in-house reader cleared",
        "1 links",
        "02000900",
    );
    completeness(&report, &output, true);
    let output = directory.path().join("cli");
    let (status, report) = cli(
        &data,
        &output,
        &directory.path().join("report.json"),
        Some(&source),
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    notice(
        &report,
        "patch.esp",
        "in-house reader cleared",
        "1 links",
        "02000900",
    );
    completeness(&report, &output, true);
    assert_eq!(status, 0);
    assert_eq!(fs::read(data.join("Patch.esp")).unwrap(), input_before);
    assert_eq!(
        fs::read(source.join("conversion-manifest.json")).unwrap(),
        manifest_before
    );
    assert_eq!(
        fs::read(source.join("skyrim_world.db")).unwrap(),
        database_before
    );
}

/// Readable and failed banks in one plugin keep separate severities and exact text provenance.
#[tokio::test]
async fn readable_missing_id_and_failed_description_bank_keep_both_severities() {
    for broken_bank in [None, Some(&b"bad"[..])] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let output = directory.path().join("pack");
        // ARMO FULL/DESC use STRINGS/DLSTRINGS, as in the Names publication fixture.
        plugin(
            &data,
            "Text.esm",
            0x81,
            &[],
            &[
                keyword(0x801),
                record(
                    b"ARMO",
                    0x802,
                    0,
                    &[
                        field(b"EDID", b"MixedTextBanks\0"),
                        field(b"FULL", &6515u32.to_le_bytes()),
                        field(b"DESC", &9u32.to_le_bytes()),
                    ]
                    .concat(),
                ),
                record(
                    b"NPC_",
                    0x803,
                    0,
                    &[
                        field(b"EDID", b"ResolvedNeighbor\0"),
                        field(b"FULL", &7u32.to_le_bytes()),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        );
        fs::create_dir(data.join("Strings")).unwrap();
        fs::write(data.join("Strings/Text_english.strings"), readable_bank()).unwrap();
        if let Some(bytes) = broken_bank {
            fs::write(data.join("Strings/Text_english.dlstrings"), bytes).unwrap();
        }
        let report = publish(&data, &output, RecordReader::Inhouse).await;
        assert_eq!(ids(&output), [0x801, 0x802, 0x803]);
        completeness(&report, &output, false);
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(
            report.warnings[0].contains("2 local decode/lookup issues"),
            "{:?}",
            report.warnings
        );
        assert!(
            report.warnings[0].contains("missing localized ID 9"),
            "{:?}",
            report.warnings
        );
        assert!(report.warnings[0].contains("first:"));
        notice(
            &report,
            "text.esm",
            "localized IDs missing from readable string tables",
            "1 localized IDs",
            "6515",
        );
        let db = Connection::open(output.join("skyrim_world.db")).unwrap();
        type TextProvenance = (u32, String, String, u32, Option<String>, String);
        let provenance: Vec<TextProvenance> = db
            .prepare("SELECT form_id,signature,string_table,string_id,resolved_text,status FROM inhouse_localized_fields ORDER BY form_id,signature")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            provenance,
            [
                (
                    0x802,
                    "DESC".into(),
                    "dlstrings".into(),
                    9,
                    None,
                    "missing".into()
                ),
                (
                    0x802,
                    "FULL".into(),
                    "strings".into(),
                    6515,
                    None,
                    "missing".into()
                ),
                (
                    0x803,
                    "FULL".into(),
                    "strings".into(),
                    7,
                    Some("Existing".into()),
                    "resolved".into()
                ),
            ]
        );
    }
}
