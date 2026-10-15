//! R5 regressions exercise damaged records and complete diagnostics through publication.
use converter::{
    AssetPipeline, PipelineConfig, config::RecordReader, esm::types::ArchivedRecordData,
};
use rusqlite::Connection;
use std::{io::Write, path::Path};

/// Frame native bytes independently of the converter's schema and decoder.
fn field(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(body.len() as u16).to_le_bytes(), body].concat()
}

/// Append one independently framed record to a synthetic plugin.
fn append(path: &Path, tag: &[u8; 4], id: u32, flags: u32, body: &[u8]) {
    let bytes = [
        tag.as_slice(),
        &(body.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &0x7135_2498u32.to_le_bytes(),
        &44u16.to_le_bytes(),
        &0xA157u16.to_le_bytes(),
        body,
    ]
    .concat();
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}

/// Publish assets and records using the ordinary in-house pipeline with LOD disabled.
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
    // Every caller uses the damaged history fixture, whose usable records still publish.
    assert!(
        !report.complete,
        "recovery diagnostics must make the run incomplete"
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

/// Multiple same-category events retain physical identity and neighboring usable records.
#[tokio::test]
async fn complete_diagnostics_publish_every_cleared_link_and_damaged_field() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    let patch = data.join("Patch.esp");
    // Local supplier index one becomes global slot two: IDs are deliberately asymmetric.
    for id in [0x0100_1881, 0x0100_1882] {
        let body = [
            field(b"EDID", b"R5Links\0"),
            field(b"LNAM", &0x0700_1A31u32.to_le_bytes()),
            field(b"LNAM", &0x0100_1A32u32.to_le_bytes()),
            field(b"LNAM", &0x0700_1A31u32.to_le_bytes()),
        ]
        .concat();
        append(&patch, b"FLST", id, 0, &body);
    }
    for id in [0x0100_1883, 0x0100_1884] {
        append(
            &patch,
            b"STAT",
            id,
            0,
            &[field(b"EDID", b"R5BadBounds\0"), field(b"OBND", &[0x73])].concat(),
        );
    }
    append(&patch, b"STAT", 0x0100_1885, 0x40000, &[7, 0, 0, 0, 0xFF]);
    append(
        &patch,
        b"KYWD",
        0x0100_1886,
        0,
        &field(b"EDID", b"R5Neighbor\0"),
    );
    let output = temp.path().join("pack");
    publish(&data, &output).await;
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let tables: u64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='record_diagnostics'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 1, "R5 must publish the complete diagnostics table");
    for id in [
        0x0200_1881u32,
        0x0200_1882,
        0x0200_1883,
        0x0200_1884,
        0x0200_1886,
    ] {
        let count: u64 = db
            .query_row(
                "SELECT count(*) FROM records WHERE form_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "usable neighbor {id:08X} must survive");
    }
    for id in [0x0200_1881u32, 0x0200_1882] {
        let count: u64 = db.query_row("SELECT count(*) FROM record_diagnostics WHERE form_id=?1 AND category='link' AND action='cleared' AND source_record_offset IS NOT NULL AND field IS NOT NULL AND link_value IS NOT NULL", [id], |row| row.get(0)).unwrap();
        assert_eq!(
            count, 3,
            "all three distinct link occurrences must be traceable"
        );
        let bytes: Vec<u8> = db
            .query_row("SELECT data FROM records WHERE form_id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        let saved = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes).unwrap();
        let links: Vec<_> = saved
            .subrecords
            .iter()
            .filter(|sub| sub.tag == *b"LNAM")
            .collect();
        assert_eq!(links.len(), 3);
        assert!(links.iter().all(|sub| sub.data == [0, 0, 0, 0]));
        let source: Vec<u8> = db
            .query_row(
                "SELECT payload FROM inhouse_source_records WHERE form_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            source,
            [
                field(b"EDID", b"R5Links\0"),
                field(b"LNAM", &0x0700_1A31u32.to_le_bytes()),
                field(b"LNAM", &0x0100_1A32u32.to_le_bytes()),
                field(b"LNAM", &0x0700_1A31u32.to_le_bytes()),
            ]
            .concat()
        );
    }
    for id in [0x0200_1883u32, 0x0200_1884] {
        let count: u64 = db.query_row("SELECT count(*) FROM record_diagnostics WHERE form_id=?1 AND category='field' AND action='skipped' AND field LIKE '%OBND%'", [id], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }
    let count: u64 = db.query_row("SELECT count(*) FROM record_diagnostics WHERE source_form_id=?1 AND category='record' AND action='skipped'", [0x0100_1885u32], |row| row.get(0)).unwrap();
    assert_eq!(
        count, 1,
        "bad zlib must be identified by native source identity"
    );
    let earlier: u64 = db
        .query_row(
            "SELECT count(*) FROM records WHERE form_id=6147",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        earlier, 1,
        "the fixture's bad override must retain its master version"
    );
}

/// A torn override explicitly reports the surviving earlier version without losing its neighbor.
#[tokio::test]
async fn torn_override_diagnostic_reports_kept_earlier_version() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    let last = data.join("Last.esp");
    let body = [field(b"EDID", b"R5Torn\0"), b"OBND\x63\x00\xA7".to_vec()].concat();
    append(&last, b"STAT", 0x1803, 0, &body);
    append(
        &last,
        b"KYWD",
        0x0300_1887,
        0,
        &field(b"EDID", b"R5AfterTorn\0"),
    );
    let output = temp.path().join("pack");
    publish(&data, &output).await;
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let tables: u64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='record_diagnostics'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 1, "R5 must publish the complete diagnostics table");
    let count: u64 = db.query_row("SELECT count(*) FROM record_diagnostics WHERE form_id=6147 AND action='kept_earlier_version' AND reason LIKE '%incomplete override%'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
    let supplier: u32 = db
        .query_row(
            "SELECT load_order FROM inhouse_source_records WHERE form_id=6147",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(supplier, 0);
    let neighbor: u64 = db
        .query_row(
            "SELECT count(*) FROM records WHERE form_id=?1",
            [0x0300_1887u32],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(neighbor, 1);
}

/// Scalar master rows preserve lazy full/light replay through an ordinary published pack.
#[tokio::test]
async fn normalized_masters_publish_native_order_and_preserve_lazy_history() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    let output = temp.path().join("pack");
    publish(&data, &output).await;
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let json_columns: u32 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('record_overrides') WHERE name='plugin_masters'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(
        json_columns, 0,
        "new main-database history has no JSON list column"
    );
    let rows = db.prepare("SELECT plugin_id,master_index,master_name FROM inhouse_plugin_masters ORDER BY plugin_id,master_index").unwrap()
        .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?, row.get::<_, String>(2)?))).unwrap()
        .collect::<rusqlite::Result<Vec<_>>>().unwrap();
    assert_eq!(
        rows,
        vec![
            (1, 0, "Skyrim.esm".into()),
            (2, 0, "Skyrim.esm".into()),
            (3, 0, "Skyrim.esm".into()),
            (3, 1, "Patch.esp".into()),
            (4, 0, "Skyrim.esm".into()),
            (4, 1, "Patch.esp".into()),
            (4, 2, "Light.esp".into()),
        ]
    );
    let history = converter::records::history::read_history(&output, None, 0x1800).unwrap();
    assert_eq!(history.versions.len(), 4);
    assert!(
        history
            .versions
            .iter()
            .all(|version| version.decode_status == "decoded")
    );
    assert_eq!(
        history
            .versions
            .iter()
            .filter(|version| version.winner)
            .map(|version| version.plugin_id)
            .collect::<Vec<_>>(),
        [4]
    );
    let light = converter::records::history::read_history(&output, None, 0xFE00_0A42).unwrap();
    assert_eq!(light.versions.len(), 2);
    assert_eq!(light.versions[0].source_form_id, 0x0200_0A42);
    assert!(
        light
            .versions
            .iter()
            .all(|version| version.decode_status == "decoded")
    );
    assert!(light.versions[1].winner);
}

/// Records-only archives publish diagnostics/tooling while deferring scalar script views.
#[test]
fn record_archives_publish_complete_diagnostics_and_explicit_scalar_scope() {
    use converter::esm::inhouse::{RecordBundleMode, export_record_bundle_typed_with_options};
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    let output = temp.path().join("archive");
    export_record_bundle_typed_with_options(
        &data,
        &data.join("plugins.txt"),
        &output,
        &[],
        None,
        RecordBundleMode::RecordArchiveOnly,
    )
    .unwrap();
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let json_columns: u32 = db
        .query_row(
            "SELECT count(*) FROM pragma_table_info('scripts') WHERE name='properties_json'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        json_columns, 0,
        "even deferred in-house script schemas have no JSON lists"
    );
    for table in ["scripts", "script_properties", "script_property_values"] {
        let count: u32 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    let count: u32 = db
        .query_row(
            "SELECT count(*) FROM record_diagnostics WHERE action='kept_earlier_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(count > 0);
    let scope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("record-bundle-scope.json")).unwrap())
            .unwrap();
    for table in [
        "record_diagnostics",
        "inhouse_plugin_masters",
        "inhouse_unresolved_terrain_textures",
    ] {
        assert!(
            scope["preserved_sql_tables"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == table)
        );
    }
    for table in ["scripts", "script_properties", "script_property_values"] {
        assert!(
            scope["deferred_runtime_tables"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == table)
        );
    }
    let identity: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("record-reader.json")).unwrap()).unwrap();
    assert_eq!(identity["diagnostics_version"], 1);
}
