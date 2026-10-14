//! Selection recovery must survive the production publication path.
use converter::{
    AssetPipeline, PipelineConfig,
    esm::{exporter::validate_database, read_plugins_txt},
};
use dummy_content::layout;
use std::{fs, path::Path};

/// Encode a synthetic subrecord independently of the decoder.
fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Encode a native record with a complete bounded header.
fn record(tag: &[u8; 4], id: u32, bytes: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(bytes.len() as u32).to_le_bytes(),
        &[0; 4],
        &id.to_le_bytes(),
        &[0; 8],
        &bytes,
    ]
    .concat()
}

/// Write a mod with distinguishable usable content and explicit master dependencies.
fn plugin(data: &Path, name: &str, masters: &[&str], editor: &str) {
    let mut header = Vec::new();
    for master in masters {
        header.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(sub(b"DATA", &[0; 8]));
    }
    let mut bytes = record(b"TES4", 0, header);
    bytes.extend(record(
        b"GMST",
        ((masters.len() as u32) << 24) | 0x800,
        [
            sub(b"EDID", format!("{editor}\0").as_bytes()),
            sub(b"DATA", &67.0f32.to_le_bytes()),
        ]
        .concat(),
    ));
    fs::write(data.join(name), bytes).unwrap();
}

/// A checked mod loads its implicitly active installed Skyrim master and publishes.
#[tokio::test]
async fn checked_mod_with_implicit_skyrim_master_publishes() {
    assert_publication(false).await;
}

/// Missing or disabled masters remove their transitive dependents, preserving neighbors.
#[tokio::test]
async fn missing_and_disabled_master_closures_preserve_publication() {
    assert_publication(true).await;
}

/// A lower-case Windows-1252 entry selects an upper-case accented filename and publishes its slot.
#[tokio::test]
async fn unicode_case_player_plugin_name_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    plugin(&data, "Good.esp", &["Skyrim.esm"], "fUnicodeNeighbor");
    plugin(&data, "CAFÉ.esp", &["Skyrim.esm"], "fJumpHeightMin");
    let list = dir.path().join("plugins.txt");
    fs::write(&list, b"*Good.esp\n*caf\xe9.esp\n").unwrap();
    let selected = read_plugins_txt(&list, &data).unwrap();
    assert_eq!(selected.len(), 3, "accented active plugin must be selected");
    assert_eq!(selected[2].file_name().unwrap(), "CAFÉ.esp");
    let order = converter::esm::load_order::LoadOrder::read(&selected).unwrap();
    assert_eq!(order.normal["café.esp"], 2);
    let output = dir.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.plugins_file = Some(list);
    config.no_lod = true;
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config, tx).await;
    drain.await.unwrap();
    assert!(
        result.is_ok(),
        "accented player plugin must publish: {result:?}"
    );
    assert!(result.unwrap().complete);
    let conn = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    validate_database(&conn).unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(names, ["Skyrim.esm", "Good.esp", "CAFÉ.esp"]);
    let (form_id, priority): (u32, u32) = conn
        .query_row(
            "SELECT form_id, load_order FROM records WHERE record_type='GMST' AND load_order=2",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    // The source owns local slot 1; Good.esp makes the published slot independently equal 2.
    assert_eq!((form_id, priority), (0x0200_0800, 2));
    let value: f64 = conn
        .query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value, 67.0);
    assert!(output.join("conversion-manifest.json").is_file());
    assert!(output.join("meshes/generated.glb").is_file());
}

/// Run real conversion, inspect published data/provenance, and retain recovery notices.
async fn assert_publication(broken: bool) {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    plugin(&data, "Good.esp", &["Skyrim.esm"], "fJumpHeightMin");
    let mut entries = String::from("*Good.esp\n");
    if broken {
        plugin(&data, "Broken.esp", &["Missing.esm"], "fRecoveryBad");
        plugin(&data, "Child.esp", &["Broken.esp"], "fRecoveryChild");
        plugin(&data, "Disabled.esm", &[], "fDisabledMaster");
        plugin(
            &data,
            "DisabledChild.esp",
            &["Disabled.esm"],
            "fDisabledChild",
        );
        plugin(
            &data,
            "Grandchild.esp",
            &["DisabledChild.esp"],
            "fDisabledGrandchild",
        );
        fs::write(data.join("Corrupt.esp"), b"TES4 truncated").unwrap();
        plugin(&data, "CorruptChild.esp", &["Corrupt.esp"], "fCorruptChild");
        entries.push_str("*Broken.esp\n*Child.esp\nDisabled.esm\n*DisabledChild.esp\n*Grandchild.esp\n*NotInstalled.esp\n*Corrupt.esp\n*CorruptChild.esp\n*Good.esp\n*Good.esp\n*Good.esp\n");
    }
    let list = dir.path().join("plugins.txt");
    fs::write(&list, entries).unwrap();
    let output = dir.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.plugins_file = Some(list);
    config.no_lod = true;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<converter::ProgressEvent>(128);
    let drain = tokio::spawn(async move {
        let mut notices = Vec::new();
        while let Some(event) = rx.recv().await {
            if event.notice {
                notices.push(event.message);
            }
        }
        notices
    });
    let result = AssetPipeline::run_async(config, tx).await;
    let progress_notices = drain.await.unwrap();
    assert!(
        result.is_ok(),
        "recoverable player list must publish: {result:?}"
    );
    let report = result.unwrap();
    assert!(report.complete, "{report:?}");
    let db = output.join("skyrim_world.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    validate_database(&conn).unwrap();
    let good: f64 = conn
        .query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(good, 67.0);
    let plugins: Vec<String> = conn
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(plugins, ["Skyrim.esm", "Good.esp"]);
    assert!(output.join("conversion-manifest.json").is_file());
    assert!(output.join("meshes/generated.glb").is_file());
    if broken {
        assert!(
            report
                .notices
                .iter()
                .all(|notice| !notice.contains(['\r', '\n']))
        );
        assert!(
            progress_notices
                .iter()
                .any(|notice| notice.starts_with("warning: skipping Broken.esp:")
                    && notice.contains("Missing.esm"))
        );
        for name in [
            "Broken.esp",
            "Child.esp",
            "DisabledChild.esp",
            "Grandchild.esp",
        ] {
            let notices: Vec<_> = report
                .notices
                .iter()
                .filter(|notice| notice.starts_with(&format!("skipping {name}:")))
                .collect();
            assert_eq!(notices.len(), 1, "{name}: {notices:?}");
            let missing = if name == "Broken.esp" || name == "Child.esp" {
                "Missing.esm"
            } else {
                "Disabled.esm"
            };
            assert!(notices[0].contains(missing), "{notices:?}");
        }
        assert!(
            report
                .notices
                .iter()
                .any(|notice| notice.contains("duplicate") && notice.contains("good.esp"))
        );
        assert!(
            report
                .notices
                .iter()
                .any(|notice| notice.contains("notinstalled.esp"))
        );
        assert!(
            report
                .notices
                .iter()
                .any(|notice| notice.starts_with("skipping Corrupt.esp:")
                    && notice.contains("unusable plugin header"))
        );
        assert!(
            report
                .notices
                .iter()
                .any(|notice| notice.starts_with("skipping CorruptChild.esp:")
                    && notice.contains("Corrupt.esp"))
        );
        assert!(
            report
                .notices
                .iter()
                .any(|notice| notice.contains("duplicate") && notice.contains("3 occurrences"))
        );
        let rejected: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM records WHERE load_order >= 2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rejected, 0);
    }
}

/// Empty recovery has a clear error instead of publishing a misleading empty selection.
#[test]
fn empty_final_selection_reports_no_usable_plugins() {
    let dir = tempfile::tempdir().unwrap();
    let list = dir.path().join("plugins.txt");
    fs::write(&list, "*Missing.esp\n").unwrap();
    let error = read_plugins_txt(&list, dir.path()).unwrap_err();
    assert!(
        error.to_string().contains("no usable active plugins"),
        "{error:?}"
    );
}
