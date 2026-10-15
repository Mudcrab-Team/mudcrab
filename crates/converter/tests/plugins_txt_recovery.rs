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

/// Convert a synthetic player selection and prove complete publication of its usable records.
async fn publish_final_fixture(
    config: PipelineConfig,
) -> (converter::PipelineReport, rusqlite::Connection) {
    let output = config.output_dir.clone();
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
        "recoverable final selection must publish: {result:?}"
    );
    let report = result.unwrap();
    assert!(report.complete, "{report:?}");
    for notice in report.notices.iter().filter(|notice| {
        notice.contains("cyclic plugin dependencies") || notice.contains("plugin slot overflow")
    }) {
        assert_eq!(
            progress_notices
                .iter()
                .filter(|message| **message == format!("warning: {notice}"))
                .count(),
            1,
            "{progress_notices:?}"
        );
    }
    let conn = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    validate_database(&conn).unwrap();
    assert!(output.join("conversion-manifest.json").is_file());
    assert!(output.join("meshes/generated.glb").is_file());
    (report, conn)
}

/// Prepare generated assets and one explicit player list without any installed game inputs.
fn final_fixture() -> (tempfile::TempDir, PipelineConfig) {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    let mut config = PipelineConfig::new(&data, dir.path().join("pack"));
    config.plugins_file = Some(dir.path().join("plugins.txt"));
    config.no_lod = true;
    (dir, config)
}

/// Read exact published plugin provenance in assigned priority order.
fn final_names(conn: &rusqlite::Connection) -> Vec<String> {
    conn.prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Decode generic published settings so absent rejected records cannot pass a movement-only query.
fn final_editors(conn: &rusqlite::Connection) -> Vec<String> {
    conn.prepare("SELECT data FROM records WHERE record_type='GMST'")
        .unwrap()
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|blob| {
            let data = rkyv::from_bytes::<
                converter::esm::types::ArchivedRecordData,
                rkyv::rancor::Error,
            >(&blob.unwrap())
            .unwrap();
            let editor = &data
                .subrecords
                .iter()
                .find(|sub| sub.tag == *b"EDID")
                .unwrap()
                .data;
            String::from_utf8(editor.strip_suffix(&[0]).unwrap_or(editor).to_vec()).unwrap()
        })
        .collect()
}

/// One cycle and its entire dependent closure are omitted while a neighboring plugin publishes.
#[tokio::test]
async fn final_cycle_closure_preserves_player_publication() {
    let (_dir, config) = final_fixture();
    for (name, masters, editor) in [
        ("CycleA.esp", vec!["CycleC.esp"], "fCycleA"),
        ("CycleB.esp", vec!["CycleA.esp"], "fCycleB"),
        ("CycleC.esp", vec!["CycleB.esp"], "fCycleC"),
        ("Child.esp", vec!["CycleB.esp"], "fCycleChild"),
        ("Grandchild.esp", vec!["Child.esp"], "fCycleGrandchild"),
        ("Good.esp", vec!["Skyrim.esm"], "fJumpHeightMin"),
    ] {
        plugin(&config.data_dir, name, &masters, editor);
    }
    fs::write(
        config.plugins_file.as_ref().unwrap(),
        "*CycleA.esp\n*CycleB.esp\n*CycleC.esp\n*Child.esp\n*Grandchild.esp\n*Good.esp\n",
    )
    .unwrap();
    let (report, conn) = publish_final_fixture(config).await;
    assert_eq!(final_names(&conn), ["Skyrim.esm", "Good.esp"]);
    let editors = final_editors(&conn);
    assert!(editors.contains(&"fJumpHeightMin".to_owned()));
    assert!(
        !editors.iter().any(|editor| editor.starts_with("fCycle")),
        "{editors:?}"
    );
    let summaries: Vec<_> = report
        .notices
        .iter()
        .filter(|notice| notice.contains("cyclic plugin dependencies"))
        .collect();
    assert_eq!(summaries.len(), 1, "{:?}", report.notices);
    assert!(!summaries[0].contains(['\r', '\n']));
    for name in [
        "CycleA.esp",
        "CycleB.esp",
        "CycleC.esp",
        "Child.esp",
        "Grandchild.esp",
    ] {
        assert!(
            report.skipped_plugins.iter().any(|skipped| skipped == name),
            "{name}: {:?}",
            report.skipped_plugins
        );
    }
    assert_eq!(report.skipped_plugins.len(), 5);
    assert_eq!(
        serde_json::to_value(&report).unwrap()["skipped_plugins"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}

/// A generated 256-full-plugin selection publishes its first 254 and omits overflow dependents.
#[tokio::test]
async fn final_full_slot_overflow_preserves_player_publication() {
    let (_dir, config) = final_fixture();
    let mut entries = String::new();
    for index in 0..255 {
        let name = format!("Full{index:03}.esp");
        let editor = if index == 0 {
            "fJumpHeightMin".to_owned()
        } else {
            format!("fFull{index:03}")
        };
        plugin(&config.data_dir, &name, &["Skyrim.esm"], &editor);
        entries.push_str(&format!("*{name}\n"));
    }
    plugin(
        &config.data_dir,
        "Dependent.esp",
        &["Full254.esp"],
        "fOverflowDependent",
    );
    let dependent = config.data_dir.join("Dependent.esp");
    let mut bytes = fs::read(&dependent).unwrap();
    bytes[8..12].copy_from_slice(&0x200u32.to_le_bytes());
    fs::write(dependent, bytes).unwrap();
    entries.push_str("*Dependent.esp\n");
    fs::write(config.plugins_file.as_ref().unwrap(), entries).unwrap();
    let (report, conn) = publish_final_fixture(config).await;
    let expected: Vec<_> = std::iter::once("Skyrim.esm".to_owned())
        .chain((0..253).map(|index| format!("Full{index:03}.esp")))
        .collect();
    assert_eq!(final_names(&conn), expected);
    let editors = final_editors(&conn);
    assert!(editors.contains(&"fFull252".to_owned()));
    for omitted in ["fFull253", "fFull254", "fOverflowDependent"] {
        assert!(!editors.iter().any(|editor| editor == omitted), "{omitted}");
    }
    let summaries: Vec<_> = report
        .notices
        .iter()
        .filter(|notice| notice.contains("full plugin slot overflow"))
        .collect();
    assert_eq!(summaries.len(), 1, "{:?}", report.notices);
    assert!(
        summaries[0].contains('2') && summaries[0].contains("Full253.esp"),
        "{summaries:?}"
    );
    assert_eq!(
        report.skipped_plugins,
        ["Full253.esp", "Full254.esp", "Dependent.esp"]
    );
}

/// Light-slot overflow is recoverable through publication, including full plugins depending on it.
#[tokio::test]
async fn final_light_slot_overflow_preserves_player_publication() {
    let (_dir, config) = final_fixture();
    let mut entries = String::new();
    for index in 0..4097 {
        let name = format!("Light{index:04}.esl");
        let editor = if index == 0 {
            "fJumpHeightMin".to_owned()
        } else {
            format!("fLight{index:04}")
        };
        plugin(&config.data_dir, &name, &["Skyrim.esm"], &editor);
        entries.push_str(&format!("*{name}\n"));
    }
    plugin(
        &config.data_dir,
        "FullDependent.esp",
        &["Light4096.esl"],
        "fLightOverflowDependent",
    );
    plugin(
        &config.data_dir,
        "Good.esp",
        &["Skyrim.esm"],
        "fFullNeighbor",
    );
    entries.push_str("*FullDependent.esp\n*Good.esp\n");
    fs::write(config.plugins_file.as_ref().unwrap(), entries).unwrap();
    let (report, conn) = publish_final_fixture(config).await;
    let expected: Vec<_> = std::iter::once("Skyrim.esm".to_owned())
        .chain((0..4096).map(|index| format!("Light{index:04}.esl")))
        .chain(std::iter::once("Good.esp".to_owned()))
        .collect();
    assert_eq!(final_names(&conn), expected);
    let editors = final_editors(&conn);
    assert!(
        editors.contains(&"fLight4095".to_owned()) && editors.contains(&"fFullNeighbor".to_owned())
    );
    assert!(
        !editors
            .iter()
            .any(|editor| ["fLight4096", "fLightOverflowDependent"].contains(&editor.as_str()))
    );
    assert_eq!(
        report.skipped_plugins,
        ["Light4096.esl", "FullDependent.esp"]
    );
    let summaries: Vec<_> = report
        .notices
        .iter()
        .filter(|notice| notice.contains("light plugin slot overflow"))
        .collect();
    assert_eq!(summaries.len(), 1, "{:?}", report.notices);
    assert!(summaries[0].contains("Light4096.esl") && !summaries[0].contains(['\r', '\n']));
}

/// Automatic discovery uses the same cycle recovery and retains usable publication.
#[tokio::test]
async fn final_cycle_closure_preserves_automatic_publication() {
    let (_dir, mut config) = final_fixture();
    config.plugins_file = None;
    for (name, master, editor) in [
        ("CycleA.esp", "CycleC.esp", "fAutoCycleA"),
        ("CycleB.esp", "CycleA.esp", "fAutoCycleB"),
        ("CycleC.esp", "CycleB.esp", "fAutoCycleC"),
        ("Child.esp", "CycleB.esp", "fAutoCycleChild"),
        ("Good.esp", "Skyrim.esm", "fJumpHeightMin"),
    ] {
        plugin(&config.data_dir, name, &[master], editor);
    }
    let (report, conn) = publish_final_fixture(config).await;
    assert_eq!(final_names(&conn), ["Skyrim.esm", "Good.esp"]);
    assert_eq!(
        report.skipped_plugins,
        ["Child.esp", "CycleA.esp", "CycleB.esp", "CycleC.esp"]
    );
    assert!(
        !final_editors(&conn)
            .iter()
            .any(|editor| editor.starts_with("fAutoCycle"))
    );
    assert_eq!(
        report
            .notices
            .iter()
            .filter(|notice| notice.contains("cyclic plugin dependencies"))
            .count(),
        1
    );
}

/// A Unicode player entry and differently cased TES4 master share the installed owner's runtime slot.
#[tokio::test]
async fn final_unicode_player_names_and_master_override_publish() {
    let (_dir, config) = final_fixture();
    plugin(
        &config.data_dir,
        "CAF\u{c9}.esp",
        &["Skyrim.esm"],
        "fJumpHeightMin",
    );
    let header = [sub(b"MAST", b"cAf\xe9.ESP\0"), sub(b"DATA", &[0; 8])].concat();
    let bytes = [
        record(b"TES4", 0, header),
        record(
            b"GMST",
            0x800,
            [
                sub(b"EDID", b"fJumpHeightMin\0"),
                sub(b"DATA", &93.0f32.to_le_bytes()),
            ]
            .concat(),
        ),
    ]
    .concat();
    fs::write(config.data_dir.join("Patch.esp"), bytes).unwrap();
    fs::write(
        config.plugins_file.as_ref().unwrap(),
        b"*caf\xe9.esp\n*Patch.esp\n",
    )
    .unwrap();
    let (_report, conn) = publish_final_fixture(config).await;
    assert_eq!(
        final_names(&conn),
        ["Skyrim.esm", "CAF\u{c9}.esp", "Patch.esp"]
    );
    let value: f64 = conn
        .query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value, 93.0);
    let ids: Vec<(u32, u32)> = conn
        .prepare("SELECT form_id, load_order FROM records WHERE record_type='GMST'")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(ids.contains(&(0x0100_0800, 2)), "{ids:?}");
}
