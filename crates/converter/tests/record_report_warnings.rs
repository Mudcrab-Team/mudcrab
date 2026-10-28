//! Reader recovery must remain local while making published warning packs visibly incomplete.
use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, config::RecordReader,
    esm::types::ArchivedRecordData,
};
use rusqlite::Connection;
use std::{fs, path::Path, process::Command};

/// Frame a native subrecord independently of either reader's implementation.
fn field(signature: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [
        signature.as_slice(),
        &(bytes.len() as u16).to_le_bytes(),
        bytes,
    ]
    .concat()
}

/// Keep asymmetric header sentinels separate from the independently authored payload.
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

/// Establish a trusted containing boundary so a torn inner record cannot consume its sibling.
fn group(payload: &[u8]) -> Vec<u8> {
    [
        b"GRUP".as_slice(),
        &(payload.len() as u32 + 24).to_le_bytes(),
        b"KYWD".as_slice(),
        &0i32.to_le_bytes(),
        &0x1739_A541u32.to_le_bytes(),
        &0x6248_B913u32.to_le_bytes(),
        payload,
    ]
    .concat()
}

/// Write only small synthetic plugins; no converted meshes or installed game inputs are needed.
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

/// A keyword provides an independently identified usable neighbor with no asset dependencies.
fn keyword(id: u32, editor_id: &[u8]) -> Vec<u8> {
    record(b"KYWD", id, 0, &field(b"EDID", editor_id))
}

/// Valid framing contains data ignored by legacy but diagnosed by the typed reader/adapter.
fn typed_issues() -> Vec<u8> {
    [
        record(
            b"STAT",
            0x804,
            0,
            &[field(b"EDID", b"DamagedBounds\0"), field(b"OBND", &[0xA7])].concat(),
        ),
        record(
            b"FLST",
            0x805,
            0,
            &[
                field(b"EDID", b"MissingOptionalTarget\0"),
                field(b"LNAM", &0x3000u32.to_le_bytes()),
            ]
            .concat(),
        ),
        record(
            b"NPC_",
            0x806,
            0,
            &[
                field(b"EDID", b"MissingLocalizedKey\0"),
                field(b"FULL", &0x1973u32.to_le_bytes()),
            ]
            .concat(),
        ),
    ]
    .concat()
}

/// Combine two distinct parse failures with usable records before and after bounded frames.
fn malformed_fixture(data: &Path) {
    let compressed = record(b"STAT", 0x810, 0x40000, &[7, 0, 0, 0, 0xFF]);
    let mut torn = record(b"KYWD", 0x811, 0, &[]);
    torn[4..8].copy_from_slice(&512u32.to_le_bytes());
    let body = [
        group(&[keyword(0x801, b"BeforeFailure\0"), compressed].concat()),
        group(&torn),
        keyword(0x802, b"AfterFrame\0"),
        typed_issues(),
    ]
    .concat();
    plugin(data, "Broken.esm", 0x81, &[], &body);
}

/// A missing name bank and one valid null sentinel retain a clean neighbor.
fn localized_loss_fixture(data: &Path) {
    let localized = |id| {
        record(
            b"NPC_",
            id,
            0,
            &[
                field(b"EDID", b"LocalizedName\0"),
                field(b"FULL", &17u32.to_le_bytes()),
            ]
            .concat(),
        )
    };
    let null_key = record(
        b"NPC_",
        0x807,
        0,
        &[
            field(b"EDID", b"NullLocalizedName\0"),
            field(b"FULL", &0u32.to_le_bytes()),
        ]
        .concat(),
    );
    plugin(
        data,
        "Lost.esm",
        0x80,
        &[],
        &[
            localized(0x806),
            null_key,
            keyword(0x801, b"HealthyNeighbor\0"),
        ]
        .concat(),
    );
}

/// The replay source is complete; the test removes this loose bank only after publication.
fn replay_fixture(data: &Path) {
    plugin(
        data,
        "Replay.esm",
        0x81,
        &[],
        &[keyword(0x801, b"ReplayNeighbor\0"), typed_issues()].concat(),
    );
    let mut directory = Vec::new();
    directory.extend(6515u32.to_le_bytes());
    directory.extend(0u32.to_le_bytes());
    let text = b"Replay localized name\0";
    let bank = [
        1u32.to_le_bytes().as_slice(),
        &(text.len() as u32).to_le_bytes(),
        &directory,
        text,
    ]
    .concat();
    let strings = data.join("Strings");
    fs::create_dir_all(&strings).unwrap();
    fs::write(strings.join("Replay_english.strings"), bank).unwrap();
}

/// Simulate metadata reread after the complete source pack's loose name bank disappears.
fn remove_replay_strings_after_publication(data: &Path, source: &Path, original: &PipelineReport) {
    assert!(original.complete, "{:?}", original.warnings);
    assert!(original.warnings.is_empty(), "{:?}", original.warnings);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(source.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["complete"], true);
    assert_eq!(manifest["record_data_complete"], true);
    assert_eq!(
        fields(source, 0x806, b"FULL"),
        [b"Replay localized name\0".to_vec()]
    );
    let strings = data.join("Strings/Replay_english.strings");
    assert!(
        strings.is_file(),
        "source resolution requires the loose bank"
    );
    fs::remove_file(&strings).unwrap();
    assert!(
        !strings.exists(),
        "metadata reread must see the bank as unavailable"
    );
}

/// Native header ownership compatibility changes no payload or winner identity.
fn compatibility_fixture(data: &Path, mixed: bool) {
    let mut body = [
        record(
            b"ACTI",
            0x0700_0810,
            0,
            &field(b"EDID", b"CompatibilityTarget\0"),
        ),
        record(
            b"GMST",
            0x0600_0820,
            0,
            &[
                field(b"EDID", b"fCompatibilityMetric\0"),
                field(b"DATA", &13.25f32.to_le_bytes()),
            ]
            .concat(),
        ),
        keyword(0x830, b"CompatibilityNeighbor\0"),
    ]
    .concat();
    if mixed {
        body.extend(record(b"STAT", 0x840, 0x40000, &[7, 0, 0, 0, 0xFF]));
        body.extend(keyword(0x850, b"AfterCompatibilityAndLoss\0"));
    }
    plugin(data, "compat.esm", 1, &[], &body);
}

/// Configure a records-only fixture through the real asset publication route.
fn config(data: &Path, output: &Path, reader: RecordReader) -> PipelineConfig {
    let mut config = PipelineConfig::new(data, output);
    config.record_reader = reader;
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    config
}

/// Drain progress while the public pipeline publishes the pack and returns its report.
async fn publish(data: &Path, output: &Path, reader: RecordReader) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config(data, output, reader), tx).await;
    drain.await.unwrap();
    result.unwrap()
}

/// Replay authenticated matching plugin bytes while retaining the valid source pack.
async fn rebuild(data: &Path, source: &Path, output: &Path) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::rebuild_metadata_async(
        config(data, output, RecordReader::Inhouse),
        source,
        tx,
    )
    .await;
    drain.await.unwrap();
    result.unwrap()
}

/// Query canonical rows by explicit identity so malformed bytes cannot displace usable neighbors.
fn record_ids(output: &Path) -> Vec<u32> {
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    db.prepare("SELECT form_id FROM records ORDER BY form_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Inspect the published canonical bytes independently of the reader's summary text.
fn fields(output: &Path, id: u32, signature: &[u8; 4]) -> Vec<Vec<u8>> {
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let bytes: Vec<u8> = db
        .query_row("SELECT data FROM records WHERE form_id=?1", [id], |row| {
            row.get(0)
        })
        .unwrap();
    rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes)
        .unwrap()
        .subrecords
        .into_iter()
        .filter(|subrecord| &subrecord.tag == signature)
        .map(|subrecord| subrecord.data)
        .collect()
}

/// Require one bounded summary of a kind rather than one warning for every damaged occurrence.
fn warning<'a>(report: &'a PipelineReport, plugin: &str, kind: &str) -> &'a str {
    let matches: Vec<_> = report
        .warnings
        .iter()
        .filter(|message| message.to_ascii_lowercase().contains(plugin) && message.contains(kind))
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "one {kind} summary for {plugin}: {:?}",
        report.warnings
    );
    let message = matches[0];
    assert!(
        message.contains("first:"),
        "missing first example: {message}"
    );
    message
}

/// Published warnings follow the existing completeness contract without becoming asset skips.
fn incomplete(report: &PipelineReport, output: &Path, warning_count: usize) {
    assert_eq!(
        report.warnings.len(),
        warning_count,
        "{:?}",
        report.warnings
    );
    assert_eq!(
        report.skipped, 0,
        "reader issues do not change the asset-skip counter"
    );
    assert!(
        !report.complete,
        "published reader warnings must mark the report incomplete"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(
        manifest["complete"], false,
        "manifest and report completeness must agree"
    );
    assert_eq!(manifest["record_data_complete"], false);
    assert!(
        output.join("cell_cache.rkyv").is_file(),
        "usable pack must publish"
    );
}

/// Match a compatibility category without confusing it with a data-loss summary.
fn compatibility_kind(reader: RecordReader) -> &'static str {
    match reader {
        RecordReader::Legacy => "FormID occurrences name a master index past",
        RecordReader::Inhouse => "used supplier-self runtime compatibility",
    }
}

/// Require one counted compatibility notice while retaining unrelated metadata advisories.
fn compatibility_notice(report: &PipelineReport, reader: RecordReader) {
    let kind = compatibility_kind(reader);
    let messages: Vec<_> = report
        .notices
        .iter()
        .filter(|message| message.contains("compat.esm") && message.contains(kind))
        .collect();
    assert_eq!(
        messages.len(),
        1,
        "one compatibility notice: {:?}",
        report.notices
    );
    let message = messages[0];
    let count = match reader {
        RecordReader::Legacy => "2 FormID occurrences",
        RecordReader::Inhouse => "for 1 headers",
    };
    assert!(message.contains(count), "{message}");
    assert!(message.contains("first:"), "{message}");
    assert!(message.contains("07000810"), "{message}");
}

/// Benign header compatibility preserves a usable complete pack and the warning bucket stays empty.
fn complete_notice_pack(report: &PipelineReport, output: &Path, reader: RecordReader) {
    compatibility_notice(report, reader);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.skipped, 0);
    assert!(
        report.complete,
        "compatibility alone must not make the run incomplete"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["complete"], true);
    assert_eq!(manifest["record_data_complete"], true);
    assert!(output.join("cell_cache.rkyv").is_file());
}

/// Exercise authenticated metadata replay with either existing reader through its public route.
async fn rebuild_with_reader(
    data: &Path,
    source: &Path,
    output: &Path,
    reader: RecordReader,
) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result =
        AssetPipeline::rebuild_metadata_async(config(data, output, reader), source, tx).await;
    drain.await.unwrap();
    result.unwrap()
}

/// Invoke the normal or reuse-assets CLI without creating a second interpretation of report policy.
fn compatibility_cli(
    data: &Path,
    output: &Path,
    report_path: &Path,
    reader: RecordReader,
    source: Option<&Path>,
) -> (std::process::Output, PipelineReport) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_converter"));
    command
        .arg(data)
        .arg(output)
        .arg("--no-lod")
        .arg("--report-json")
        .arg(report_path);
    command.arg("--record-reader").arg(match reader {
        RecordReader::Legacy => "legacy",
        RecordReader::Inhouse => "inhouse",
    });
    if let Some(source) = source {
        command.arg("--reuse-assets").arg(source);
    }
    let run = command.output().unwrap();
    let report = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    (run, report)
}

/// Malformed records and frames aggregate once while trusted neighboring records publish.
#[tokio::test]
async fn legacy_malformed_frames_warn_once_and_publish_neighbors() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("pack");
    malformed_fixture(&data);
    let report = publish(&data, &output, RecordReader::Legacy).await;
    assert_eq!(record_ids(&output), [0x801, 0x802, 0x804, 0x805, 0x806]);
    assert_eq!(
        fields(&output, 0x801, b"EDID"),
        [b"BeforeFailure\0".to_vec()]
    );
    assert_eq!(fields(&output, 0x802, b"EDID"), [b"AfterFrame\0".to_vec()]);
    let message = warning(&report, "broken.esm", "malformed record/frame");
    assert!(
        message.contains("2 malformed record/frame occurrence(s)"),
        "{message}"
    );
    assert!(message.contains("00000810"), "{message}");
    let localization = warning(
        &report,
        "broken.esm",
        "legacy reader left 1 localized fields without text",
    );
    assert!(localization.contains("00000806"), "{localization}");
    assert!(
        localization.contains("FULL ID 6515: unreadable strings table"),
        "{localization}"
    );
    incomplete(&report, &output, 2);
}

/// An unusable later candidate cannot replace the earlier winner or hide its counted warning.
#[tokio::test]
async fn legacy_unusable_override_warns_and_preserves_earlier_winner() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("pack");
    let setting = |value: f32| {
        record(
            b"GMST",
            0x800,
            0,
            &[
                field(b"EDID", b"fJumpHeightMin\0"),
                field(b"DATA", &value.to_le_bytes()),
            ]
            .concat(),
        )
    };
    plugin(
        &data,
        "Base.esm",
        1,
        &[],
        &[setting(50.0), keyword(0x801, b"BaseNeighbor\0")].concat(),
    );
    plugin(
        &data,
        "Patch.esp",
        0,
        &["Base.esm"],
        &[setting(f32::NAN), keyword(0x0100_0802, b"PatchNeighbor\0")].concat(),
    );
    let report = publish(&data, &output, RecordReader::Legacy).await;
    assert_eq!(record_ids(&output), [0x800, 0x801, 0x0100_0802]);
    assert_eq!(
        fields(&output, 0x800, b"DATA"),
        [50.0f32.to_le_bytes().to_vec()]
    );
    let message = warning(&report, "patch.esp", "unusable record candidate(s)");
    assert!(
        message.contains("1 unusable record candidate(s)"),
        "{message}"
    );
    assert!(message.contains("GMST source 00000800"), "{message}");
    incomplete(&report, &output, 1);
}

/// Decoder skips and missing banks warn while a faithfully cleared link remains a notice.
#[tokio::test]
async fn inhouse_decoder_and_adapter_summaries_reach_report() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("pack");
    malformed_fixture(&data);
    let report = publish(&data, &output, RecordReader::Inhouse).await;
    assert_eq!(record_ids(&output), [0x801, 0x802, 0x804, 0x805, 0x806]);
    assert!(fields(&output, 0x804, b"OBND").is_empty());
    assert_eq!(
        fields(&output, 0x805, b"LNAM"),
        [0u32.to_le_bytes().to_vec()]
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["broken.esm"]["skipped_records"], 2);
    assert_eq!(diagnostics["decoder"]["broken.esm"]["skipped_fields"], 1);
    assert_eq!(diagnostics["decoder"]["broken.esm"]["invalid_links"], 1);
    assert_eq!(diagnostics["adapter"]["broken.esm"][0], 2);
    let decoder = warning(&report, "broken.esm", "in-house reader skipped");
    assert!(decoder.contains("2 records, 1 fields"), "{decoder}");
    assert!(!decoder.contains("links"), "{decoder}");
    let link_notices: Vec<_> = report
        .notices
        .iter()
        .filter(|notice| notice.contains("broken.esm: in-house reader cleared 1 links"))
        .collect();
    assert_eq!(link_notices.len(), 1, "{:?}", report.notices);
    assert!(link_notices[0].contains("00003000"));
    let adapter = warning(&report, "broken.esm", "inhouse adapter reported");
    assert!(
        adapter.contains("reported 2 local decode/lookup issues"),
        "{adapter}"
    );
    assert!(adapter.contains("missing localized ID 6515"), "{adapter}");
    incomplete(&report, &output, 2);
}

/// Missing required string banks reach report and manifest; ID zero stays a valid null sentinel.
#[tokio::test]
async fn missing_localized_text_marks_both_readers_incomplete_without_losing_neighbors() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    localized_loss_fixture(&data);
    for (name, reader) in [
        ("legacy-localized", RecordReader::Legacy),
        ("inhouse-localized", RecordReader::Inhouse),
    ] {
        let output = directory.path().join(name);
        let report = publish(&data, &output, reader).await;
        assert_eq!(record_ids(&output), [0x801, 0x806, 0x807]);
        let expected = match reader {
            RecordReader::Legacy => "FULL ID 17: unreadable strings table",
            RecordReader::Inhouse => "missing localized ID 17",
        };
        assert!(
            report
                .warnings
                .iter()
                .any(|line| { line.contains("lost.esm") && line.contains(expected) }),
            "{:?}",
            report.warnings
        );
        assert!(
            report.warnings.iter().all(|line| !line.contains("ID 0")),
            "null localized IDs are not data loss: {:?}",
            report.warnings
        );
        incomplete(&report, &output, 1);
    }
}

/// Readable banks distinguish absent keys from malformed entries without losing neighbors.
#[tokio::test]
async fn readable_localized_gaps_are_notices_but_malformed_entries_remain_warnings() {
    let directory = tempfile::tempdir().unwrap();
    for malformed in [false, true] {
        let case = directory
            .path()
            .join(if malformed { "malformed" } else { "absent" });
        let data = case.join("Data");
        let mut body = keyword(0x801, b"BeforeSubtitle\0");
        for (id, key) in [(0x806, 17u32), (0x807, 33), (0x808, 0), (0x809, 34)] {
            body.extend(record(
                b"SNDR",
                id,
                0,
                &[
                    field(b"EDID", format!("Sound{id:X}\0").as_bytes()),
                    field(b"FNAM", &key.to_le_bytes()),
                ]
                .concat(),
            ));
        }
        body.extend(keyword(0x80A, b"AfterSubtitle\0"));
        plugin(&data, "Subtitles.esm", 0x81, &[], &body);
        let mut entries = vec![(33u32, 0u32), (34, 9)];
        if malformed {
            entries.push((17, u32::MAX));
        }
        let mut bank = (entries.len() as u32).to_le_bytes().to_vec();
        bank.extend(10u32.to_le_bytes());
        for (key, offset) in entries {
            bank.extend(key.to_le_bytes());
            bank.extend(offset.to_le_bytes());
        }
        bank.extend(b"Resolved\0\0");
        fs::create_dir(data.join("Strings")).unwrap();
        fs::write(data.join("Strings/Subtitles_english.strings"), bank).unwrap();
        let mut reader_rows = Vec::new();
        for (name, reader) in [
            ("legacy", RecordReader::Legacy),
            ("inhouse", RecordReader::Inhouse),
        ] {
            let output = case.join(name);
            let report = publish(&data, &output, reader).await;
            assert_eq!(
                record_ids(&output),
                [0x801, 0x806, 0x807, 0x808, 0x809, 0x80A]
            );
            assert_eq!(fields(&output, 0x806, b"FNAM"), [b"\0".to_vec()]);
            assert_eq!(fields(&output, 0x807, b"FNAM"), [b"Resolved\0".to_vec()]);
            assert_eq!(fields(&output, 0x808, b"FNAM"), [b"\0".to_vec()]);
            assert_eq!(fields(&output, 0x809, b"FNAM"), [b"\0".to_vec()]);
            let manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
                    .unwrap();
            assert_eq!(report.complete, !malformed, "{reader:?}: {report:?}");
            assert_eq!(manifest["complete"], !malformed);
            assert_eq!(manifest["record_data_complete"], !malformed);
            assert_eq!(report.skipped, 0);
            let notices: Vec<_> = report
                .notices
                .iter()
                .filter(|line| line.contains("localized IDs missing from readable string tables"))
                .collect();
            if malformed {
                assert_eq!(report.warnings.len(), 1, "{report:?}");
                assert!(report.warnings[0].contains("localized ID 17"), "{report:?}");
                assert!(notices.is_empty(), "{report:?}");
            } else {
                assert!(report.warnings.is_empty(), "{report:?}");
                assert_eq!(notices.len(), 1, "{report:?}");
                assert!(
                    notices[0].contains("1 localized IDs missing from readable string tables"),
                    "{report:?}"
                );
                assert!(notices[0].contains("00000806"), "{report:?}");
                assert!(notices[0].contains("localized ID 17"), "{report:?}");
            }
            let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
            let rows: Vec<(u32, Vec<u8>)> = connection
                .prepare("SELECT form_id,data FROM records ORDER BY form_id")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            reader_rows.push(rows);
        }
        assert_eq!(
            reader_rows[0], reader_rows[1],
            "both readers publish identical rows"
        );
    }
}

/// Clean input remains complete under both readers without any new warning lines.
#[tokio::test]
async fn clean_legacy_and_inhouse_reports_remain_complete() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    plugin(
        &data,
        "Clean.esm",
        1,
        &[],
        &keyword(0x801, b"CleanNeighbor\0"),
    );
    for (name, reader) in [
        ("legacy", RecordReader::Legacy),
        ("inhouse", RecordReader::Inhouse),
    ] {
        let output = directory.path().join(name);
        let report = publish(&data, &output, reader).await;
        assert_eq!(record_ids(&output), [0x801]);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(report.complete);
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["complete"], true);
    }
}

/// The legacy CLI publishes usable data and writes an incomplete report with exit code one.
#[test]
fn legacy_cli_report_json_marks_published_warning_pack_incomplete() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("pack");
    let report_path = directory.path().join("report.json");
    malformed_fixture(&data);
    let run = Command::new(env!("CARGO_BIN_EXE_converter"))
        .arg(&data)
        .arg(&output)
        .arg("--record-reader")
        .arg("legacy")
        .arg("--no-lod")
        .arg("--report-json")
        .arg(&report_path)
        .output()
        .unwrap();
    let report: PipelineReport = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(record_ids(&output), [0x801, 0x802, 0x804, 0x805, 0x806]);
    assert_eq!(
        run.status.code(),
        Some(1),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    warning(&report, "broken.esm", "malformed record/frame");
    let localization = warning(
        &report,
        "broken.esm",
        "legacy reader left 1 localized fields without text",
    );
    assert!(localization.contains("00000806"), "{localization}");
    assert!(
        localization.contains("FULL ID 6515: unreadable strings table"),
        "{localization}"
    );
    incomplete(&report, &output, 2);
}

/// Complete source assets survive while metadata reread sees the removed loose name bank.
#[tokio::test]
async fn metadata_rebuild_replays_reader_warnings_without_changing_source() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    replay_fixture(&data);
    let original = publish(&data, &source, RecordReader::Legacy).await;
    remove_replay_strings_after_publication(&data, &source, &original);
    let plugin_before = fs::read(data.join("Replay.esm")).unwrap();
    let manifest_before = fs::read(source.join("conversion-manifest.json")).unwrap();
    let database_before = fs::read(source.join("skyrim_world.db")).unwrap();
    let report = rebuild(&data, &source, &output).await;
    assert_eq!(record_ids(&output), [0x801, 0x804, 0x805, 0x806]);
    assert_eq!(fs::read(data.join("Replay.esm")).unwrap(), plugin_before);
    assert_eq!(
        fs::read(source.join("conversion-manifest.json")).unwrap(),
        manifest_before
    );
    assert_eq!(
        fs::read(source.join("skyrim_world.db")).unwrap(),
        database_before
    );
    warning(&report, "replay.esm", "in-house reader skipped");
    let adapter = warning(&report, "replay.esm", "inhouse adapter reported");
    assert!(adapter.contains("missing localized ID 6515"), "{adapter}");
    incomplete(&report, &output, 2);
}

/// --reuse-assets sees unavailable text after publishing a complete, localized source pack.
#[tokio::test]
async fn reuse_assets_cli_report_json_replays_reader_warnings() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    let report_path = directory.path().join("report.json");
    replay_fixture(&data);
    let original = publish(&data, &source, RecordReader::Legacy).await;
    remove_replay_strings_after_publication(&data, &source, &original);
    let run = Command::new(env!("CARGO_BIN_EXE_converter"))
        .arg(&data)
        .arg(&output)
        .arg("--no-lod")
        .arg("--reuse-assets")
        .arg(&source)
        .arg("--record-reader")
        .arg("inhouse")
        .arg("--report-json")
        .arg(&report_path)
        .output()
        .unwrap();
    let report: PipelineReport = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(record_ids(&output), [0x801, 0x804, 0x805, 0x806]);
    assert_eq!(
        run.status.code(),
        Some(1),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    warning(&report, "replay.esm", "in-house reader skipped");
    let adapter = warning(&report, "replay.esm", "inhouse adapter reported");
    assert!(adapter.contains("missing localized ID 6515"), "{adapter}");
    incomplete(&report, &output, 2);
}

/// Both frontends keep bounded ownership compatibility separate from every failure counter.
#[tokio::test]
async fn severity_compatibility_only_is_notice_and_complete_for_both_readers() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    compatibility_fixture(&data, false);
    for (name, reader) in [
        ("legacy", RecordReader::Legacy),
        ("inhouse", RecordReader::Inhouse),
    ] {
        let output = directory.path().join(name);
        let report = publish(&data, &output, reader).await;
        assert_eq!(record_ids(&output), [0x810, 0x820, 0x830]);
        assert_eq!(
            fields(&output, 0x810, b"EDID"),
            [b"CompatibilityTarget\0".to_vec()]
        );
        assert_eq!(
            fields(&output, 0x820, b"DATA"),
            [13.25f32.to_le_bytes().to_vec()]
        );
        complete_notice_pack(&report, &output, reader);
    }
}

/// Compatibility notices remain visible in report JSON and the normal CLI succeeds.
#[test]
fn severity_compatibility_cli_is_complete_and_exits_zero_for_both_readers() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    compatibility_fixture(&data, false);
    for (name, reader) in [
        ("legacy", RecordReader::Legacy),
        ("inhouse", RecordReader::Inhouse),
    ] {
        let output = directory.path().join(name);
        let report_path = directory.path().join(format!("{name}.json"));
        let (run, report) = compatibility_cli(&data, &output, &report_path, reader, None);
        assert_eq!(record_ids(&output), [0x810, 0x820, 0x830]);
        assert_eq!(
            run.status.code(),
            Some(0),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        complete_notice_pack(&report, &output, reader);
        assert!(String::from_utf8_lossy(&run.stdout).contains(compatibility_kind(reader)));
    }
}

/// An ownership notice never hides real record loss or changes the retained usable neighbors.
#[tokio::test]
async fn severity_mixed_compatibility_and_record_loss_keep_separate_buckets() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    compatibility_fixture(&data, true);
    for (name, reader) in [
        ("legacy", RecordReader::Legacy),
        ("inhouse", RecordReader::Inhouse),
    ] {
        let output = directory.path().join(name);
        let report = publish(&data, &output, reader).await;
        assert_eq!(record_ids(&output), [0x810, 0x820, 0x830, 0x850]);
        assert_eq!(
            fields(&output, 0x820, b"DATA"),
            [13.25f32.to_le_bytes().to_vec()]
        );
        assert_eq!(
            fields(&output, 0x850, b"EDID"),
            [b"AfterCompatibilityAndLoss\0".to_vec()]
        );
        compatibility_notice(&report, reader);
        let kind = match reader {
            RecordReader::Legacy => "malformed record/frame",
            RecordReader::Inhouse => "in-house reader skipped",
        };
        let loss = warning(&report, "compat.esm", kind);
        assert!(loss.contains("00000840"), "{loss}");
        incomplete(&report, &output, 1);
    }
}

/// Authenticated unchanged plugin bytes replay compatibility notices through both metadata readers.
#[tokio::test]
async fn severity_metadata_rebuild_replays_compatibility_notices_without_source_changes() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    compatibility_fixture(&data, false);
    let original = publish(&data, &source, RecordReader::Legacy).await;
    // Metadata replay requires a complete source manifest and authenticated unchanged plugin bytes.
    complete_notice_pack(&original, &source, RecordReader::Legacy);
    let input_before = fs::read(data.join("compat.esm")).unwrap();
    let manifest_before = fs::read(source.join("conversion-manifest.json")).unwrap();
    let database_before = fs::read(source.join("skyrim_world.db")).unwrap();
    for (name, reader) in [
        ("legacy", RecordReader::Legacy),
        ("inhouse", RecordReader::Inhouse),
    ] {
        let output = directory.path().join(name);
        let report = rebuild_with_reader(&data, &source, &output, reader).await;
        assert_eq!(record_ids(&output), [0x810, 0x820, 0x830]);
        complete_notice_pack(&report, &output, reader);
        if reader == RecordReader::Inhouse {
            assert!(
                report
                    .notices
                    .iter()
                    .any(|notice| notice.contains("retained models and textures"))
            );
        }
        assert_eq!(fs::read(data.join("compat.esm")).unwrap(), input_before);
        assert_eq!(
            fs::read(source.join("conversion-manifest.json")).unwrap(),
            manifest_before
        );
        assert_eq!(
            fs::read(source.join("skyrim_world.db")).unwrap(),
            database_before
        );
    }
}

/// Reuse-assets CLI replay retains notice evidence and succeeds for both frontends.
#[tokio::test]
async fn severity_reuse_assets_cli_replays_compatibility_notices_and_exits_zero() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    compatibility_fixture(&data, false);
    let original = publish(&data, &source, RecordReader::Legacy).await;
    complete_notice_pack(&original, &source, RecordReader::Legacy);
    let input_before = fs::read(data.join("compat.esm")).unwrap();
    let manifest_before = fs::read(source.join("conversion-manifest.json")).unwrap();
    let database_before = fs::read(source.join("skyrim_world.db")).unwrap();
    for (name, reader) in [
        ("legacy", RecordReader::Legacy),
        ("inhouse", RecordReader::Inhouse),
    ] {
        let output = directory.path().join(name);
        let report_path = directory.path().join(format!("{name}.json"));
        let (run, report) = compatibility_cli(&data, &output, &report_path, reader, Some(&source));
        assert_eq!(record_ids(&output), [0x810, 0x820, 0x830]);
        assert_eq!(
            run.status.code(),
            Some(0),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        complete_notice_pack(&report, &output, reader);
        assert_eq!(fs::read(data.join("compat.esm")).unwrap(), input_before);
        assert_eq!(
            fs::read(source.join("conversion-manifest.json")).unwrap(),
            manifest_before
        );
        assert_eq!(
            fs::read(source.join("skyrim_world.db")).unwrap(),
            database_before
        );
    }
}

/// Real ESL-width truncation and cleared optional ownership remain warnings, with no compatibility notice.
#[tokio::test]
async fn severity_legacy_light_truncation_and_optional_clear_remain_warnings() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("pack");
    let body = [
        record(
            b"ACTI",
            0x1821,
            0,
            &field(b"EDID", b"TruncatedLightIdentity\0"),
        ),
        record(b"ACTI", 0x822, 0, &field(b"EDID", b"LightNeighbor\0")),
        record(
            b"CELL",
            0x830,
            0,
            &[
                field(b"DATA", &[1, 0]),
                field(b"XOWN", &0x0700_0900u32.to_le_bytes()),
            ]
            .concat(),
        ),
    ]
    .concat();
    plugin(&data, "loss.esl", 0x201, &[], &body);
    let report = publish(&data, &output, RecordReader::Legacy).await;
    assert_eq!(record_ids(&output), [0xFE00_0821, 0xFE00_0822, 0xFE00_0830]);
    assert_eq!(
        fields(&output, 0xFE00_0830, b"XOWN"),
        [0u32.to_le_bytes().to_vec()]
    );
    assert!(
        !report.notices.iter().any(|notice| {
            notice.contains(compatibility_kind(RecordReader::Legacy))
                || notice.contains(compatibility_kind(RecordReader::Inhouse))
        }),
        "unexpected compatibility notice: {:?}",
        report.notices
    );
    let truncation = warning(&report, "loss.esl", "light-plugin ID occurrences");
    assert!(
        truncation.contains("1 light-plugin ID occurrences"),
        "{truncation}"
    );
    assert!(truncation.contains("00001821"), "{truncation}");
    let optional = warning(&report, "loss.esl", "invalid optional FormID links");
    assert!(
        optional.contains("1 invalid optional FormID links"),
        "{optional}"
    );
    assert!(optional.contains("07000900"), "{optional}");
    incomplete(&report, &output, 2);
}
