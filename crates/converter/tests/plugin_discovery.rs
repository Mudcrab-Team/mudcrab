//! Exercise automatic plugin discovery through the production conversion path.
mod common;

use converter::{
    AssetPipeline, PipelineConfig,
    esm::{EsmParser, load_order::LoadOrder, read_plugins_txt},
};
use dummy_content::layout;
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Encode a synthetic subrecord without relying on the parser under test.
fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Encode a synthetic record header and payload.
fn record(tag: &[u8; 4], id: u32, flags: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}

/// Write a plugin header and an optional newly defined game setting.
fn plugin(root: &Path, name: &str, masters: &[&str], flags: u32, value: Option<f32>) -> PathBuf {
    let mut header = Vec::new();
    for master in masters {
        header.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(sub(b"DATA", &[0; 8]));
    }
    let mut bytes = record(b"TES4", 0, flags, header);
    if let Some(value) = value {
        let payload = [
            sub(b"EDID", b"fJumpHeightMin\0"),
            sub(b"DATA", &value.to_le_bytes()),
        ]
        .concat();
        bytes.extend(record(
            b"GMST",
            ((masters.len() as u32) << 24) | 0x800,
            0,
            payload,
        ));
    }
    let path = root.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// Run the production pipeline while draining progress so workers cannot block.
async fn run(config: PipelineConfig) -> Result<converter::PipelineReport, String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config, tx)
        .await
        .map_err(|error| format!("{error:?}"));
    drain.await.unwrap();
    result
}

#[tokio::test]
async fn fallback_orders_dependencies_and_ignores_nested_plugins_but_keeps_assets() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    plugin(&data, "ZMod.esp", &["Skyrim.esm"], 0, Some(100.0));
    plugin(
        &data,
        "APatch.esp",
        &["Skyrim.esm", "zMOD.esp"],
        0,
        Some(222.0),
    );
    // Header flags, not just the filename suffix, determine master priority.
    plugin(&data, "YMaster.esp", &["Skyrim.esm"], 1, None);
    // Extensions imply early loading even without the ESM header bit.
    plugin(&data, "WMaster.esm", &["Skyrim.esm"], 0, None);
    plugin(&data, "VLight.esl", &["Skyrim.esm"], 0, None);
    plugin(&data, "XLight.esp", &["Skyrim.esm"], 0x200, None);
    plugin(&data, "BIndependent.esp", &["Skyrim.esm"], 0, None);
    fs::create_dir_all(data.join("Optional")).unwrap();
    fs::write(data.join("Optional/ZMod.esp"), b"invalid duplicate backup").unwrap();
    fs::write(data.join("Optional/Unused.esp"), b"invalid optional plugin").unwrap();
    let output = dir.path().join("modern");
    let config = common::cpu_lod_config(&data, &output);
    assert!(config.plugins_file.is_none());
    assert!(run(config.clone()).await.unwrap().complete);
    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        names,
        [
            "Skyrim.esm",
            "VLight.esl",
            "WMaster.esm",
            "YMaster.esp",
            "BIndependent.esp",
            "XLight.esp",
            "ZMod.esp",
            "APatch.esp"
        ]
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get::<_, f64>(0)
        )
        .unwrap(),
        222.0
    );
    assert!(output.join("meshes/generated.glb").is_file());
    assert!(output.join("textures/generated_color.ktx2").is_file());
    drop(conn);
    // Identical fallback ordering on resume keeps asset reuse intact.
    let second = run(config).await.unwrap();
    assert!(second.complete);
    assert_eq!(second.converted, 0);
    assert!(second.cache_hits > 0);
}

/// Nested plugins are not conversion inputs; their advisory must not block assets.
#[tokio::test]
async fn nested_only_plugins_warn_but_asset_conversion_completes_without_database() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    let nested = data.join("Optional");
    fs::create_dir(&nested).unwrap();
    fs::rename(data.join("Skyrim.esm"), nested.join("Skyrim.esm")).unwrap();
    fs::write(nested.join("Backup.esp"), b"invalid ignored plugin").unwrap();
    let output = dir.path().join("modern");
    let report = run(common::cpu_lod_config(&data, &output)).await.unwrap();
    assert!(report.complete, "{report:?}");
    assert!(report.converted > 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(
        report.notices,
        [format!(
            "found 2 plugin files, but none directly in {}; plugins in subfolders are ignored",
            data.display()
        )]
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(!output.join("skyrim_world.db").exists());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["complete"], true);
}

/// The launcher only sees progress events, so the nested-plugins notice must arrive there too,
/// marked as a notice rather than a status update.
#[tokio::test]
async fn nested_only_plugin_notice_reaches_the_progress_channel() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    let nested = data.join("Optional");
    fs::create_dir(&nested).unwrap();
    fs::rename(data.join("Skyrim.esm"), nested.join("Skyrim.esm")).unwrap();
    let output = dir.path().join("modern");
    let (tx, mut rx) = tokio::sync::mpsc::channel::<converter::progress::ProgressEvent>(64);
    let collect = tokio::spawn(async move {
        let mut notices = Vec::new();
        while let Some(event) = rx.recv().await {
            if event.notice {
                notices.push(event.message);
            }
        }
        notices
    });
    let report = AssetPipeline::run_async(common::cpu_lod_config(&data, &output), tx)
        .await
        .map_err(|error| format!("{error:?}"))
        .unwrap();
    let notices = collect.await.unwrap();
    assert!(report.complete, "{report:?}");
    let expected = format!(
        "note: found 1 plugin file, but none directly in {}; plugins in subfolders are ignored",
        data.display()
    );
    assert_eq!(
        notices
            .iter()
            .filter(|message| **message == expected)
            .count(),
        1,
        "{notices:?}"
    );
}

/// A deliberately asset-only input needs neither a plugin warning nor a database.
#[tokio::test]
async fn no_plugins_converts_assets_without_warning_or_database() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats {
            esm: false,
            // The LOD settings sidecar describes the plugin's worldspace.
            lodsettings: false,
            ..layout::Formats::all()
        },
    )
    .unwrap();
    let output = dir.path().join("modern");
    let report = run(common::cpu_lod_config(&data, &output)).await.unwrap();
    assert!(report.complete, "{report:?}");
    assert!(report.converted > 0);
    assert_eq!(report.skipped, 0);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    assert!(!output.join("skyrim_world.db").exists());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["complete"], true);
}

#[tokio::test]
async fn fallback_keeps_espfe_in_regular_order_without_changing_light_slots() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    plugin(&data, "ARegular.esp", &["Skyrim.esm"], 0, Some(100.0));
    plugin(&data, "ZLight.esp", &["Skyrim.esm"], 0x200, Some(222.0));
    let output = dir.path().join("modern");
    assert!(
        run(common::cpu_lod_config(&data, &output))
            .await
            .unwrap()
            .complete
    );
    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get::<_, f64>(0)
        )
        .unwrap(),
        222.0
    );
    let order = LoadOrder::read(&[
        data.join("Skyrim.esm"),
        data.join("ARegular.esp"),
        data.join("ZLight.esp"),
    ])
    .unwrap();
    assert_eq!(order.normal["aregular.esp"], 1);
    assert_eq!(order.light["zlight.esp"], 0);
    assert!(!order.normal.contains_key("zlight.esp"));
}

#[tokio::test]
async fn fallback_reports_missing_masters_and_cycles_with_plugin_names() {
    for cycle in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Data");
        fs::create_dir(&data).unwrap();
        plugin(&data, "A.esp", &["B.esp"], 0, None);
        if cycle {
            plugin(&data, "B.esp", &["A.esp"], 0, None);
        }
        let error = run(common::cpu_lod_config(&data, dir.path().join("modern")))
            .await
            .unwrap_err();
        assert!(
            error.contains("A.esp") && error.contains("B.esp"),
            "{error}"
        );
        assert!(
            error.contains(if cycle {
                "cyclic plugin dependencies"
            } else {
                "required master"
            }),
            "{error}"
        );
    }
}

/// Regular plugins keep their list order; one listed before its regular master still fails.
#[tokio::test]
async fn explicit_plugin_order_is_preserved_and_not_silently_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    plugin(&data, "Z.esp", &["Skyrim.esm"], 0, Some(100.0));
    plugin(&data, "A.esp", &["Skyrim.esm"], 0, Some(222.0));
    plugin(&data, "Patch.esp", &["Skyrim.esm", "Z.esp"], 0, None);
    let list = dir.path().join("plugins.txt");
    fs::write(&list, "Skyrim.esm\n*Z.esp\n*A.esp\n").unwrap();
    let output = dir.path().join("modern");
    let mut config = common::cpu_lod_config(&data, &output);
    config.plugins_file = Some(list.clone());
    assert!(run(config.clone()).await.unwrap().complete);
    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get::<_, f64>(0)
        )
        .unwrap(),
        222.0
    );
    drop(conn);
    // No master file needs Z.esp, so normalization leaves this regular inversion invalid.
    fs::write(&list, "Skyrim.esm\n*Patch.esp\n*Z.esp\n").unwrap();
    let error = run(config).await.unwrap_err();
    assert!(error.contains("must precede"), "{error}");
}

/// Reproduce #108 with a valid list whose ordinary override must beat a later master.
#[tokio::test]
async fn explicit_regular_override_beats_a_master_listed_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    let regular = plugin(&data, "A.esp", &["Skyrim.esm"], 0, Some(222.0));
    let master = plugin(&data, "B.esm", &["Skyrim.esm"], 1, Some(100.0));
    // Both override the same base-owned STAT; neither depends on the other.
    append_order_records(&regular, &[(3, "RegularWinner")], None);
    append_order_records(&master, &[(3, "MasterLoser")], None);
    let list = dir.path().join("plugins.txt");
    fs::write(&list, "Skyrim.esm\n*A.esp\n*B.esm\n").unwrap();
    let output = dir.path().join("modern");
    let mut config = common::cpu_lod_config(&data, &output);
    config.plugins_file = Some(list);
    let report = run(config).await.unwrap();
    assert!(report.complete, "{report:?}");
    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    let setting: (f64, String) = conn.query_row(
        "SELECT g.value, p.name FROM movement_game_settings g JOIN plugins p ON p.priority=g.load_order
         WHERE g.editor_id='fJumpHeightMin'",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(setting, (222.0, "A.esp".to_string()));
    let object: (String, String, String) = conn.query_row(
        "SELECT s.editor_id, p.name, f.plugin_name FROM statics s JOIN records r ON r.form_id=s.id
         JOIN plugins p ON p.priority=r.load_order JOIN formid_map f ON f.form_id=s.id WHERE s.id=3",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(
        object,
        (
            "RegularWinner".to_string(),
            "A.esp".to_string(),
            "skyrim.esm".to_string()
        )
    );
}

/// Dependency hoisting must terminate on cycles and preserve validation failures.
#[test]
fn explicit_order_rejects_missing_masters_inversions_and_cycles() {
    let cases: &[&[(&str, &[&str])]] = &[
        &[("MissingDependent.esm", &["Absent.esm"])],
        &[("Dependent.esm", &["Base.esm"]), ("Base.esm", &[])],
        &[
            ("A.esp", &["B.esp"]),
            ("B.esp", &["A.esp"]),
            ("Dependent.esm", &["A.esp"]),
        ],
    ];
    for plugins in cases {
        let dir = tempfile::tempdir().unwrap();
        let mut list = String::new();
        for (name, masters) in *plugins {
            plugin(dir.path(), name, masters, 0, None);
            list.push_str(&format!("*{name}\n"));
        }
        let path = dir.path().join("plugins.txt");
        fs::write(&path, list).unwrap();
        let paths = read_plugins_txt(&path, dir.path()).unwrap();
        let error = format!("{:?}", LoadOrder::read(&paths).err().unwrap());
        assert!(error.contains("must precede"), "{error}");
        assert!(
            error.contains(&plugins[0].0.to_ascii_lowercase()),
            "{error}"
        );
        assert!(error.contains(plugins[0].1[0]), "{error}");
    }
}

/// Encode a group retaining the world/cell context of its children.
fn group(kind: i32, id: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        b"GRUP".as_slice(),
        &(payload.len() as u32 + 24).to_le_bytes(),
        &id.to_le_bytes(),
        &kind.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}

/// Add distinguishable statics and optionally override the fixture's first LAND.
fn append_order_records(path: &Path, statics: &[(u32, &str)], height: Option<f32>) {
    let mut bytes = fs::read(path).unwrap();
    for (id, name) in statics {
        bytes.extend(record(
            b"STAT",
            *id,
            0,
            [
                sub(b"EDID", format!("{name}\0").as_bytes()),
                sub(b"MODL", b"generated.nif\0"),
            ]
            .concat(),
        ));
    }
    if let Some(height) = height {
        let mut vhgt = height.to_le_bytes().to_vec();
        vhgt.extend(vec![0; 33 * 33 + 3]);
        // The generated world owns cell 0x10 and LAND 0x11. Every fixture plugin
        // lists Skyrim.esm first, so those IDs remain master-relative here.
        bytes.extend(group(
            1,
            1,
            group(6, 0x10, record(b"LAND", 0x11, 0, sub(b"VHGT", &vhgt))),
        ));
    }
    fs::write(path, bytes).unwrap();
}

/// Master priority determines slots and winners; an ESL-flagged `.esp` remains regular.
#[tokio::test]
async fn explicit_plugin_list_loads_master_files_before_regular_plugins() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    let regular = plugin(
        &data,
        "A.esp",
        &["Skyrim.esm", "B.esm", "Flagged.esp", "Light.esl"],
        0,
        Some(222.0),
    );
    // Neither extension fixture sets ESM; Flagged.esp isolates the header bit.
    let master = plugin(&data, "B.esm", &["Skyrim.esm"], 0, Some(100.0));
    plugin(&data, "Flagged.esp", &["Skyrim.esm"], 0x1, Some(101.0));
    let light = plugin(&data, "Light.esl", &["Skyrim.esm"], 0, Some(102.0));
    let small = plugin(&data, "Small.esp", &["Skyrim.esm"], 0x200, Some(333.0));
    append_order_records(&master, &[(0x01000811, "MasterOriginal")], Some(100.0));
    append_order_records(&light, &[(0x01000812, "LightOriginal")], None);
    append_order_records(
        &regular,
        &[
            (0x01000811, "MasterOverride"),
            (0x03000812, "LightOverride"),
            (0x04000814, "RegularOwned"),
        ],
        Some(222.0),
    );
    append_order_records(&small, &[(0x01000813, "SmallOwned")], Some(333.0));
    let list = dir.path().join("plugins.txt");
    fs::write(
        &list,
        "Skyrim.esm\n*A.esp\n*B.esm\n*Small.esp\n*Flagged.esp\n*Light.esl\n",
    )
    .unwrap();

    let names: Vec<_> = read_plugins_txt(&list, &data)
        .unwrap()
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "Skyrim.esm",
            "B.esm",
            "Flagged.esp",
            "Light.esl",
            "A.esp",
            "Small.esp"
        ]
    );
    let order = LoadOrder::read(&read_plugins_txt(&list, &data).unwrap()).unwrap();
    assert_eq!(
        [
            order.normal["skyrim.esm"],
            order.normal["b.esm"],
            order.normal["flagged.esp"],
            order.normal["a.esp"],
        ],
        [0, 1, 2, 3]
    );
    assert_eq!((order.light["light.esl"], order.light["small.esp"]), (0, 1));

    // The regular plugins now follow all masters; ESL-flagged Small.esp is last.
    let output = dir.path().join("modern");
    let mut config = common::cpu_lod_config(&data, &output);
    config.plugins_file = Some(list.clone());
    let first = run(config.clone()).await.unwrap();
    assert!(first.complete, "{first:?}");
    assert!(first.converted > 0);
    let mesh = fs::read(output.join("meshes/generated.glb")).unwrap();
    let texture = fs::read(output.join("textures/generated_color.ktx2")).unwrap();

    for (regular_names, winner, height) in [
        (["A.esp", "Small.esp"], "Small.esp", 333.0f32),
        (["Small.esp", "A.esp"], "A.esp", 222.0f32),
    ] {
        if winner == "A.esp" {
            // Only the explicit list changes; cached assets must survive while
            // the database and terrain adopt the new last regular override.
            fs::write(
                &list,
                "Skyrim.esm\n*Small.esp\n*B.esm\n*A.esp\n*Flagged.esp\n*Light.esl\n",
            )
            .unwrap();
            let resumed = run(config.clone()).await.unwrap();
            assert!(resumed.complete, "{resumed:?}");
            assert_eq!(resumed.converted, 0);
            assert_eq!(resumed.cache_hits, first.converted);
            assert_eq!(fs::read(output.join("meshes/generated.glb")).unwrap(), mesh);
            assert_eq!(
                fs::read(output.join("textures/generated_color.ktx2")).unwrap(),
                texture
            );
        }
        let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
        let priorities: Vec<(String, u32)> = conn
            .prepare("SELECT name, priority FROM plugins ORDER BY priority")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let expected = [
            "Skyrim.esm",
            "B.esm",
            "Flagged.esp",
            "Light.esl",
            regular_names[0],
            regular_names[1],
        ];
        assert_eq!(
            priorities,
            expected
                .iter()
                .enumerate()
                .map(|(i, name)| (name.to_string(), i as u32))
                .collect::<Vec<_>>()
        );
        let setting: (f64, String, String) = conn
            .query_row(
                "SELECT g.value, p.name, f.plugin_name FROM movement_game_settings g
             JOIN plugins p ON p.priority=g.load_order
             JOIN formid_map f ON f.form_id=g.id WHERE g.editor_id='fJumpHeightMin'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            setting,
            (f64::from(height), winner.to_string(), "b.esm".to_string())
        );
        for (id, owner, local, editor, source) in [
            (0x01000811u32, "b.esm", 0x811, "MasterOverride", "A.esp"),
            (0xfe000812, "light.esl", 0x812, "LightOverride", "A.esp"),
            (0xfe001813, "small.esp", 0x813, "SmallOwned", "Small.esp"),
            (0x03000814, "a.esp", 0x814, "RegularOwned", "A.esp"),
        ] {
            let actual: (String, u32, String, String) = conn
                .query_row(
                    "SELECT f.plugin_name, f.internal_id, s.editor_id, p.name FROM formid_map f
                 JOIN statics s ON s.id=f.form_id JOIN records r ON r.form_id=f.form_id
                 JOIN plugins p ON p.priority=r.load_order WHERE f.form_id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                actual,
                (
                    owner.to_string(),
                    local,
                    editor.to_string(),
                    source.to_string()
                )
            );
        }
        let heightmap: Vec<u8> = conn
            .query_row("SELECT heightmap FROM land WHERE cell_id=16", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            f32::from_le_bytes(heightmap[..4].try_into().unwrap()),
            height
        );
        let mmap = converter::esm::cell_cache::validate_cell_cache(&output.join("cell_cache.rkyv"))
            .unwrap();
        let cache = rkyv::access::<shared::ArchivedCellCache, rkyv::rancor::Error>(&mmap).unwrap();
        let land = cache
            .cells
            .iter()
            .find(|land| land.cell_id == 0x10)
            .unwrap();
        assert!(
            land.heights
                .iter()
                .all(|value| value.to_native() == height * 8.0)
        );
    }
}

/// A master may depend on regular plugins; hoist its prerequisite chain without
/// letting unrelated regular plugins override their explicit relative order.
#[tokio::test]
async fn explicit_master_keeps_its_transitive_regular_prerequisites() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    plugin(&data, "First.esp", &["Skyrim.esm"], 0, Some(40.0));
    plugin(&data, "Dependency.esp", &["Skyrim.esm"], 0, Some(10.0));
    plugin(
        &data,
        "Chain.esp",
        &["Skyrim.esm", "Dependency.esp"],
        0,
        Some(20.0),
    );
    plugin(
        &data,
        "Dependent.esm",
        &["Skyrim.esm", "Chain.esp"],
        1,
        Some(30.0),
    );
    plugin(&data, "Last.esp", &["Skyrim.esm"], 0, Some(50.0));
    let list = dir.path().join("plugins.txt");
    fs::write(
        &list,
        "Skyrim.esm\n*First.esp\n*Dependency.esp\n*Chain.esp\n*Dependent.esm\n*Last.esp\n",
    )
    .unwrap();
    let output = dir.path().join("modern");
    let mut config = common::cpu_lod_config(&data, &output);
    config.plugins_file = Some(list);
    let report = run(config).await.unwrap();
    assert!(report.complete, "{report:?}");
    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        names,
        [
            "Skyrim.esm",
            "Dependency.esp",
            "Chain.esp",
            "Dependent.esm",
            "First.esp",
            "Last.esp"
        ]
    );
    let setting: (f64, String) = conn.query_row(
        "SELECT g.value, p.name FROM movement_game_settings g JOIN plugins p ON p.priority=g.load_order
         WHERE g.editor_id='fJumpHeightMin'",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(setting, (50.0, "Last.esp".to_string()));
}

#[test]
fn header_and_record_errors_include_the_plugin_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Broken.esp");
    fs::write(&path, b"TES4").unwrap();
    let error = LoadOrder::read(std::slice::from_ref(&path)).err().unwrap();
    assert!(format!("{error:?}").contains(path.to_str().unwrap()));
    let malformed = record(b"STAT", 0x800, 0, b"DATA\x04\x00\x01".to_vec());
    fs::write(&path, [record(b"TES4", 0, 0, vec![]), malformed].concat()).unwrap();
    let error = EsmParser::merge_plugins(std::slice::from_ref(&path)).unwrap_err();
    let message = format!("{error:?}");
    assert!(
        message.contains(path.to_str().unwrap()) && message.contains("truncated DATA"),
        "{message}"
    );
}
