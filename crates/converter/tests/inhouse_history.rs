//! Override history is tested through the public conversion and pack publication path.
use converter::{AssetPipeline, PipelineConfig, config::RecordReader};
use rusqlite::Connection;
use std::path::Path;

/// H2 must preserve the original early validation even when a later file is unreadable.
#[test]
fn load_order_reports_earlier_fault_before_opening_later_plugins() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    let missing = data.join("Unreadable.esp");
    let error =
        converter::esm::load_order::LoadOrder::read(&[data.join("Patch.esp"), missing.clone()])
            .err()
            .expect("missing earlier master must be rejected");
    assert!(error.to_string().contains("must precede"), "{error}");
    let base = data.join("Skyrim.esm");
    let error = converter::esm::load_order::LoadOrder::read(&[base.clone(), base, missing])
        .err()
        .expect("earlier duplicate must be rejected");
    assert!(error.to_string().contains("duplicate plugin"), "{error}");
}

/// Run the real pipeline with an explicit list and --no-lod configuration.
async fn publish(data: &Path, output: &Path) {
    let mut config = PipelineConfig::new(data, output);
    config.plugins_file = Some(data.join("plugins.txt"));
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    // The shared history fixture deliberately contains rejected Patch.esp versions.
    assert!(
        !report.complete,
        "recovered input must be visible in the report"
    );
    assert_eq!(report.skipped, 0);
    assert!(
        report.warnings.iter().any(|warning| {
            warning.contains("patch.esp") && warning.contains("in-house reader skipped")
        }),
        "{:?}",
        report.warnings
    );
}

/// Lazy replay preserves source versions and per-plugin checksum failures stay local.
#[tokio::test]
async fn history_replay_marks_differences_and_changed_plugin_without_losing_neighbors() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    let output = temp.path().join("pack");
    publish(&data, &output).await;
    let before = converter::records::history::read_history(&output, None, 0x1800).unwrap();
    assert_eq!(before.versions.len(), 4);
    assert!(before.versions.iter().all(|v| v.decode_status == "decoded"));
    assert_eq!(
        before
            .versions
            .iter()
            .filter(|v| v.winner)
            .map(|v| v.plugin_id)
            .collect::<Vec<_>>(),
        [4]
    );
    assert!(
        before
            .fields
            .iter()
            .any(|f| f.field.starts_with("EDID:") && f.different)
    );
    let broken = converter::records::history::read_history(&output, Some(&data), 0x1803).unwrap();
    assert_eq!(broken.versions[0].decode_status, "decoded");
    assert_eq!(broken.versions[1].status, "skipped");
    assert_eq!(broken.versions[1].decode_status, "undecodable");
    assert!(
        broken.versions[1]
            .decode_reason
            .as_ref()
            .is_some_and(|s| s.contains("zlib") || s.contains("stream"))
    );
    let broken_setting = converter::records::history::read_history(&output, None, 0x1820).unwrap();
    assert_eq!(broken_setting.versions.len(), 2);
    assert_eq!(broken_setting.versions[1].source_form_id, 0x0100_182F);
    assert_eq!(broken_setting.versions[1].status, "skipped");
    assert_eq!(broken_setting.versions[1].decode_status, "undecodable");
    assert!(broken_setting.versions[0].winner);
    let broken_editor =
        converter::records::history::read_history(&output, None, 0x0200_1811).unwrap();
    assert_eq!(broken_editor.versions.len(), 1);
    assert_eq!(broken_editor.versions[0].status, "skipped");
    assert_eq!(broken_editor.versions[0].decode_status, "undecodable");
    let file = data.join("Patch.esp");
    let mut bytes = std::fs::read(&file).unwrap();
    bytes.push(0xC7);
    std::fs::write(file, bytes).unwrap();
    let changed = converter::records::history::read_history(&output, None, 0x1800).unwrap();
    assert_eq!(
        changed
            .versions
            .iter()
            .filter(|v| v.decode_status == "decoded")
            .count(),
        3
    );
    assert!(
        changed.versions[1]
            .decode_reason
            .as_ref()
            .unwrap()
            .contains("plugin changed")
    );
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_inhouse-records"))
        .arg("history")
        .arg(&output)
        .arg("00001800")
        .arg("--json")
        .output()
        .unwrap();
    assert!(cli.status.success());
    let report: serde_json::Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(report["versions"][1]["decode_status"], "undecodable");
    assert_eq!(report["versions"][3]["decode_status"], "decoded");
}

/// Original metadata rejection is a clean command diagnostic, independent of runtime tables.
#[tokio::test]
async fn history_rejects_bad_stored_masters_cleanly() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    let output = temp.path().join("pack");
    publish(&data, &output).await;
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    db.execute("UPDATE inhouse_plugin_masters SET master_name='Absent.esm' WHERE plugin_id=2 AND master_index=0", []).unwrap();
    drop(db);
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_inhouse-records"))
        .arg("history")
        .arg(&output)
        .arg("00001800")
        .output()
        .unwrap();
    assert!(cli.status.success());
    let message = String::from_utf8(cli.stdout).unwrap();
    assert!(message.contains("Absent.esm") && message.contains("must precede"));
}

/// Database allocation can vary elsewhere; every ordered history cell remains identical.
#[tokio::test]
async fn history_ordered_table_bytes_are_deterministic() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    let mut snapshots = Vec::new();
    for name in ["first", "second"] {
        let output = temp.path().join(name);
        publish(&data, &output).await;
        let db = Connection::open(output.join("skyrim_world.db")).unwrap();
        let mut query = db
            .prepare("SELECT * FROM record_overrides ORDER BY form_id,chain_position")
            .unwrap();
        let count = query.column_count();
        let rows = query
            .query_map([], |r| {
                (0..count)
                    .map(|i| r.get::<_, rusqlite::types::Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        snapshots.push(rows);
    }
    assert_eq!(snapshots[0], snapshots[1]);
}

/// Independently append a framed native header to an existing synthetic input.
fn append_frame(path: &Path, id: u32, flags: u32, size: u32, payload: &[u8]) {
    use std::io::Write;
    let mut bytes = b"STAT".to_vec();
    bytes.extend(size.to_le_bytes());
    bytes.extend(flags.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend([0; 8]);
    bytes.extend(payload);
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}

/// Duplicate, unmappable and truncated headers stay inspectable while valid winners survive.
#[tokio::test]
async fn history_keeps_duplicate_unmappable_and_truncated_headers() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    append_frame(&data.join("Patch.esp"), 0x1800, 0, 0, &[]);
    append_frame(&data.join("Patch.esp"), 0x0100_1890, 0x40000, 5, &[0xFF; 5]);
    append_frame(&data.join("Patch.esp"), 0x0100_1891, 0, 0, &[]);
    append_frame(&data.join("Light.esp"), 0x0200_1880, 0, 0, &[]);
    append_frame(&data.join("Last.esp"), 0x0300_1950, 0, 100, &[0xC7; 3]);
    let output = temp.path().join("pack");
    publish(&data, &output).await;
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let duplicates: (u32,u32) = db.query_row("SELECT count(*),sum(status='skipped') FROM record_overrides WHERE form_id=6144 AND plugin_id=2", [], |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(duplicates, (2, 1));
    let unmappable: (Option<u32>,String) = db.query_row("SELECT form_id,status FROM record_overrides WHERE plugin_id=3 AND source_form_id=33560704", [], |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(unmappable, (None, "skipped".into()));
    let truncated: String = db
        .query_row(
            "SELECT reason FROM record_overrides WHERE plugin_id=4 AND source_form_id=50338128",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(truncated.contains("truncated"));
    let winner: u32 = db
        .query_row(
            "SELECT load_order FROM inhouse_source_records WHERE form_id=6144",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(winner, 4);
    let truncated = converter::records::history::read_history(&output, None, 0x0300_1950).unwrap();
    assert_eq!(truncated.versions[0].decode_status, "undecodable");
    assert!(
        truncated.versions[0]
            .decode_reason
            .as_ref()
            .unwrap()
            .contains("truncated")
    );
    let oversized = converter::records::history::read_history(&output, None, 0x0200_1890).unwrap();
    assert!(
        oversized.versions[0]
            .decode_reason
            .as_ref()
            .unwrap()
            .contains("exceeds bound")
    );
    let neighbor = converter::records::history::read_history(&output, None, 0x0200_1891).unwrap();
    assert_eq!(neighbor.versions[0].decode_status, "decoded");
}

/// Fail on a behavioral assertion on baseline, before relying on new APIs or table columns.
#[tokio::test]
async fn history_publication_keeps_every_version_and_usable_neighbors() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    let output = temp.path().join("pack");
    publish(&data, &output).await;
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let tables: u32 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='record_overrides'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        tables, 1,
        "published pack must retain losing and skipped native record headers"
    );
    for (id, count) in [
        (0x1800u32, 4),
        (0x1801, 3),
        (0x1802, 2),
        (0x1803, 2),
        (0x1810, 4),
        (0x1820, 2),
        (0x1860, 2),
        (0xFE00_0A42, 2),
    ] {
        let got: u32 = db
            .query_row(
                "SELECT count(*) FROM record_overrides WHERE form_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(got, count, "chain {id:08X}");
    }
    for id in [0x1803u32, 0x1820] {
        let state: String = db
            .query_row(
                "SELECT status FROM record_overrides WHERE form_id=?1 AND plugin_id=2",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "skipped");
        let winner: u32 = db
            .query_row(
                "SELECT load_order FROM inhouse_source_records WHERE form_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(winner, 0);
    }
    let raw_alias: String = db
        .query_row(
            "SELECT status FROM record_overrides WHERE form_id=?1 AND source_form_id=?2",
            [0x0200_1811u32, 0x0100_1811],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(raw_alias, "skipped");
    let deletes: Vec<(u32, bool)> = db.prepare("SELECT plugin_id,deleted FROM record_overrides WHERE form_id=6145 ORDER BY chain_position").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(deletes, [(0, false), (2, true), (3, false)]);
    let absent: u32 = db
        .query_row(
            "SELECT count(*) FROM inhouse_source_records WHERE form_id=6146",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(absent, 0);
    let inconsistencies: u32 = db.query_row("SELECT count(*) FROM inhouse_source_records s LEFT JOIN record_overrides h ON h.form_id=s.form_id AND h.status='accepted' AND h.chain_position=(SELECT max(x.chain_position) FROM record_overrides x WHERE x.form_id=s.form_id AND x.status='accepted') WHERE h.plugin_id IS NULL OR h.deleted!=0 OR h.plugin_id!=s.load_order OR h.source_form_id!=s.source_form_id OR h.record_type!=s.record_type OR h.flags!=s.flags", [], |r| r.get(0)).unwrap();
    assert_eq!(inconsistencies, 0);
    let neighbor: u32 = db
        .query_row(
            "SELECT count(*) FROM inhouse_source_records WHERE form_id=33560688",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(neighbor, 1);
    let fk: u32 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk, 0);
}
