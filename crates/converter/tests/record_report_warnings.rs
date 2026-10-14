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

/// The same safely framed bytes form a complete legacy source for reader-switch replay.
fn replay_fixture(data: &Path) {
    plugin(
        data,
        "Replay.esm",
        0x81,
        &[],
        &[keyword(0x801, b"ReplayNeighbor\0"), typed_issues()].concat(),
    );
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
    assert!(
        output.join("cell_cache.rkyv").is_file(),
        "usable pack must publish"
    );
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
    incomplete(&report, &output, 1);
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

/// Decoder skips, native optional-link recovery and adapter lookup issues all reach the report.
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
    assert_eq!(diagnostics["adapter"]["broken.esm"][0], 1);
    let decoder = warning(&report, "broken.esm", "in-house reader skipped");
    assert!(decoder.contains("2 records, 1 fields"), "{decoder}");
    assert!(decoder.contains("cleared 1 invalid links"), "{decoder}");
    let adapter = warning(&report, "broken.esm", "inhouse adapter reported");
    assert!(
        adapter.contains("reported 1 local decode/lookup issues"),
        "{adapter}"
    );
    assert!(adapter.contains("missing localized ID 6515"), "{adapter}");
    incomplete(&report, &output, 2);
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

/// The default CLI publishes usable data and writes an incomplete report with exit code one.
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
    incomplete(&report, &output, 1);
}

/// Reader-switch metadata replay reports issues in unchanged checksum-matched plugin bytes.
#[tokio::test]
async fn metadata_rebuild_replays_reader_warnings_without_changing_source() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    replay_fixture(&data);
    let original = publish(&data, &source, RecordReader::Legacy).await;
    assert!(original.complete, "{:?}", original.warnings);
    assert!(original.warnings.is_empty());
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
    warning(&report, "replay.esm", "inhouse adapter reported");
    incomplete(&report, &output, 2);
}

/// --reuse-assets uses the same matched replay warning contract and CLI failure status.
#[tokio::test]
async fn reuse_assets_cli_report_json_replays_reader_warnings() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    let report_path = directory.path().join("report.json");
    replay_fixture(&data);
    let original = publish(&data, &source, RecordReader::Legacy).await;
    assert!(original.complete, "{:?}", original.warnings);
    assert!(original.warnings.is_empty());
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
    warning(&report, "replay.esm", "inhouse adapter reported");
    incomplete(&report, &output, 2);
}
