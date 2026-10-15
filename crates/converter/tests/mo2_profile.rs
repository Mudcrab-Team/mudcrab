//! MO2 selection through the real converter, including cache and resume boundaries.
use converter::{
    AssetPipeline, PipelineConfig, cache::configuration_hash, esm::exporter::validate_database,
    pipeline::Cancellation,
};
use dummy_content::layout;
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Writes a fixture file relative to a root, creating its parent directories.
fn write(root: &Path, relative: &str, contents: impl AsRef<[u8]>) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Encodes a synthetic plugin subrecord with a tag and 16-bit payload length.
fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Encodes a synthetic plugin record with its identifier, flags, and payload.
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

/// Writes a synthetic plugin with a master dependency and a jump-height game setting.
fn plugin(path: &Path, master: &str, value: f32) {
    plugin_setting(path, master, "fJumpHeightMin", value);
}

/// Writes a synthetic plugin with a distinguishable setting and a native local identifier.
fn plugin_setting(path: &Path, master: &str, editor: &str, value: f32) {
    let header = [
        sub(b"MAST", format!("{master}\0").as_bytes()),
        sub(b"DATA", &[0; 8]),
    ]
    .concat();
    let mut bytes = record(b"TES4", 0, 0, header);
    bytes.extend(record(
        b"GMST",
        0x01000800,
        0,
        [
            sub(b"EDID", format!("{editor}\0").as_bytes()),
            sub(b"DATA", &value.to_le_bytes()),
        ]
        .concat(),
    ));
    fs::write(path, bytes).unwrap();
}

/// Writes a localized class and an independently identified usable neighbor.
fn localized_plugin(path: &Path, editor: &str) {
    let header = [sub(b"MAST", b"Skyrim.esm\0"), sub(b"DATA", &[0; 8])].concat();
    let mut bytes = record(b"TES4", 0, 0x80, header);
    bytes.extend(record(
        b"CLAS",
        0x0100_0900,
        0,
        [
            sub(b"EDID", format!("{editor}\0").as_bytes()),
            sub(b"FULL", &17u32.to_le_bytes()),
            sub(b"DESC", &17u32.to_le_bytes()),
        ]
        .concat(),
    ));
    bytes.extend(record(
        b"KYWD",
        0x0100_0901,
        0,
        sub(b"EDID", format!("{editor}Neighbor\0").as_bytes()),
    ));
    fs::write(path, bytes).unwrap();
}

/// Frames one native Windows-1252 string independently of the converter's bank parser.
fn single_string_bank(text: &[u8], length_prefixed: bool) -> Vec<u8> {
    let mut text = text.to_vec();
    text.push(0);
    let payload = if length_prefixed {
        [(text.len() as u32).to_le_bytes().to_vec(), text].concat()
    } else {
        text
    };
    [
        1u32.to_le_bytes().to_vec(),
        (payload.len() as u32).to_le_bytes().to_vec(),
        17u32.to_le_bytes().to_vec(),
        0u32.to_le_bytes().to_vec(),
        payload,
    ]
    .concat()
}

/// Creates synthetic game data and an MO2 profile with conflicting plugins across priority layers.
fn fixture() -> (tempfile::TempDir, PipelineConfig, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    let instance = dir.path().join("MO2");
    for folder in [
        "mods/Low",
        "mods/High",
        "mods/Disabled",
        "profiles/Default",
        "overwrite",
    ] {
        fs::create_dir_all(instance.join(folder)).unwrap();
    }
    write(
        &instance,
        "ModOrganizer.ini",
        "[General]\ngameName=Skyrim Special Edition\n",
    );
    write(
        &instance,
        "profiles/Default/modlist.txt",
        "+High\n-Disabled\n+Low\n",
    );
    write(
        &instance,
        "profiles/Default/plugins.txt",
        "*Patch.esp\n*Middle.esp\nInactive.esm\n",
    );
    write(
        &instance,
        "profiles/Default/loadorder.txt",
        "Skyrim.esm\nMiddle.esp\nPatch.esp\nInactive.esm\n",
    );
    plugin(&instance.join("mods/Low/Middle.esp"), "Skyrim.esm", 100.0);
    plugin(&instance.join("mods/Low/Patch.esp"), "Skyrim.esm", 111.0);
    plugin(&instance.join("mods/High/Patch.esp"), "Skyrim.esm", 222.0);
    plugin(&instance.join("overwrite/PATCH.esp"), "Skyrim.esm", 444.0);
    write(
        &instance,
        "mods/Disabled/Inactive.esm",
        "invalid inactive plugin",
    );
    write(
        &instance,
        "mods/Low/Inactive.bsa",
        "invalid inactive archive",
    );
    let mut config = PipelineConfig::new(&data, dir.path().join("modern"));
    config.mo2 = Some(mo2::Selection {
        instance_path: instance.clone(),
        profile: "Default".into(),
    });
    (dir, config, instance)
}

/// Runs conversion with cancellation support while draining progress events to avoid blocking.
async fn run(
    config: PipelineConfig,
    stop: Cancellation,
) -> Result<converter::PipelineReport, converter::pipeline::PipelineFailure> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::run_async_with_cancel(config, tx, stop).await;
    drain.await.unwrap();
    result
}

/// Requires a recoverable profile to complete the real conversion and publication path.
async fn publish(config: &PipelineConfig) -> converter::PipelineReport {
    let result = run(config.clone(), Cancellation::new()).await;
    assert!(
        result.is_ok(),
        "recoverable MO2 profile must publish: {result:?}"
    );
    let report = result.unwrap();
    assert!(report.complete, "{report:?}");
    report
}

/// Reads the winning jump-height game setting from the converted world database.
fn value(config: &PipelineConfig) -> f64 {
    Connection::open(config.output_dir.join("skyrim_world.db"))
        .unwrap()
        .query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// Checks published plugin provenance, database validity, and surviving converted assets.
fn assert_publication(config: &PipelineConfig, expected: &[&str], jump_height: f64) {
    let conn = Connection::open(config.output_dir.join("skyrim_world.db")).unwrap();
    validate_database(&conn).unwrap();
    let names = conn
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|name| name.unwrap().to_lowercase())
        .collect::<Vec<_>>();
    assert_eq!(names, expected);
    assert_eq!(value(config), jump_height);
    assert!(config.output_dir.join("conversion-manifest.json").is_file());
    assert!(config.output_dir.join("meshes/generated.glb").is_file());
}

/// Reads synthetic GMST editors, values and winning priorities from published generic records.
fn published_settings(config: &PipelineConfig) -> Vec<(String, f32, u32)> {
    let conn = Connection::open(config.output_dir.join("skyrim_world.db")).unwrap();
    let mut query = conn
        .prepare("SELECT data, load_order FROM records WHERE record_type='GMST'")
        .unwrap();
    query
        .query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, u32>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (blob, priority) = row.unwrap();
            let data = rkyv::from_bytes::<
                converter::esm::types::ArchivedRecordData,
                rkyv::rancor::Error,
            >(&blob)
            .unwrap();
            let editor = &data
                .subrecords
                .iter()
                .find(|sub| &sub.tag == b"EDID")
                .unwrap()
                .data;
            let value = &data
                .subrecords
                .iter()
                .find(|sub| &sub.tag == b"DATA")
                .unwrap()
                .data;
            (
                String::from_utf8(editor.strip_suffix(&[0]).unwrap_or(editor).to_vec()).unwrap(),
                f32::from_le_bytes(value.as_slice().try_into().unwrap()),
                priority,
            )
        })
        .collect()
}

/// Checks a retained generic setting's independent value and winning-plugin priority.
fn assert_setting(config: &PipelineConfig, editor: &str, expected: f64, priority: u32) {
    let settings = published_settings(config);
    let actual: Vec<_> = settings
        .iter()
        .filter(|(name, _, _)| name == editor)
        .collect();
    assert_eq!(actual.len(), 1, "{editor}: {actual:?}");
    assert_eq!(
        (f64::from(actual[0].1), actual[0].2),
        (expected, priority),
        "{editor}"
    );
}

/// Checks one bounded profile notice contains all identifying details for a recovered input.
fn assert_notice(report: &converter::PipelineReport, details: &[&str]) {
    let matches = report
        .notices
        .iter()
        .filter(|notice| {
            let lower = notice.to_ascii_lowercase();
            details
                .iter()
                .all(|detail| lower.contains(&detail.to_ascii_lowercase()))
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{details:?}: {:?}", report.notices);
    assert!(matches[0].contains("MO2 profile Default"), "{matches:?}");
    assert!(!matches[0].contains(['\r', '\n']), "{matches:?}");
}

/// Verifies plugin order, overwrite precedence, cache reuse, and changes to enabled mods.
#[tokio::test]
async fn consumes_merged_profile_plugin_order_overwrite_and_reuses_conversion() {
    let (_dir, config, instance) = fixture();
    assert!(
        run(config.clone(), Cancellation::new())
            .await
            .unwrap()
            .complete
    );
    assert_eq!(value(&config), 444.0);
    let conn = Connection::open(config.output_dir.join("skyrim_world.db")).unwrap();
    let names = conn
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert_eq!(
        names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<Vec<_>>(),
        ["skyrim.esm", "middle.esp", "patch.esp"]
    );
    drop(conn);
    assert!(config.output_dir.join("meshes/generated.glb").is_file());
    assert!(!config.output_dir.join(".mo2-input").exists());
    let second = run(config.clone(), Cancellation::new()).await.unwrap();
    assert!(second.complete);
    assert_eq!(second.converted, 0);
    assert!(second.cache_hits > 0);
    fs::remove_file(instance.join("overwrite/PATCH.esp")).unwrap();
    run(config.clone(), Cancellation::new()).await.unwrap();
    assert_eq!(value(&config), 222.0);
    write(
        &instance,
        "profiles/Default/modlist.txt",
        "-High\n-Disabled\n+Low\n",
    );
    run(config.clone(), Cancellation::new()).await.unwrap();
    assert_eq!(value(&config), 111.0);
    // Source files were not rewritten or lowercased by staging.
    assert!(instance.join("mods/High/Patch.esp").exists());
    assert!(!instance.join("mods/High/patch.esp.mohidden").exists());
}

/// Irrelevant profile edits retain per-source cache proofs, while profile identity remains distinct.
#[tokio::test]
async fn irrelevant_profile_changes_reuse_assets() {
    let (_dir, config, instance) = fixture();
    let first = run(config.clone(), Cancellation::new()).await.unwrap();
    assert!(first.complete);
    let hash = configuration_hash(&config).unwrap();
    write(
        &instance,
        "ModOrganizer.ini",
        "[General]\ngameName=Skyrim Special Edition\n[Settings]\nwindowWidth=1200\n",
    );
    write(
        &instance,
        "profiles/Default/modlist.txt",
        "# cosmetic comment\n+High\n-Disabled\n+Low\n",
    );
    write(
        &instance,
        "profiles/Default/plugins.txt",
        "# cosmetic comment\n*Patch.esp\n*Middle.esp\nInactive.esm\n",
    );
    write(
        &instance,
        "profiles/Default/loadorder.txt",
        "# cosmetic comment\nSkyrim.esm\nMiddle.esp\nPatch.esp\nInactive.esm\n",
    );
    assert_eq!(hash, configuration_hash(&config).unwrap());
    let second = run(config.clone(), Cancellation::new()).await.unwrap();
    assert!(second.complete);
    assert_eq!(second.converted, 0);
    assert_eq!(second.cache_hits, first.converted);
    fs::create_dir_all(instance.join("profiles/Other")).unwrap();
    let mut other = config;
    other.mo2.as_mut().unwrap().profile = "Other".into();
    assert_ne!(hash, configuration_hash(&other).unwrap());
}

/// Skeleton winners participate in mesh cache proofs even when the mesh itself is unchanged.
#[tokio::test]
async fn winning_skeleton_changes_invalidate_dependent_mesh() {
    let (_dir, config, instance) = fixture();
    let mesh = fs::read(config.data_dir.join("meshes/generated.nif")).unwrap();
    let body = "meshes/actors/character/armor/body.nif";
    let skeleton = "meshes/actors/character/character assets/skeleton.nif";
    write(&instance, &format!("mods/Low/{body}"), &mesh);
    write(&instance, &format!("mods/Low/{skeleton}"), &mesh);
    let first = run(config.clone(), Cancellation::new()).await.unwrap();
    assert!(first.complete);
    let manifest = converter::cache::ConversionManifest::load(
        &config.output_dir.join("conversion-manifest.json"),
    )
    .unwrap();
    let original = manifest.entries[body].source_hash.clone();
    assert_eq!(
        original,
        format!(
            "{}:{}",
            converter::cache::hash_bytes(&mesh),
            converter::cache::hash_bytes(&mesh)
        )
    );
    let mut replacement = mesh.clone();
    let name = replacement
        .windows(13)
        .position(|bytes| bytes == b"GeneratedQuad")
        .unwrap();
    replacement[name..name + 13].copy_from_slice(b"GeneratedBody");
    write(&instance, &format!("overwrite/{skeleton}"), &replacement);
    let second = run(config.clone(), Cancellation::new()).await.unwrap();
    assert!(second.complete);
    let manifest = converter::cache::ConversionManifest::load(
        &config.output_dir.join("conversion-manifest.json"),
    )
    .unwrap();
    assert_eq!(
        manifest.entries[body].source_hash,
        format!(
            "{}:{}",
            converter::cache::hash_bytes(&mesh),
            converter::cache::hash_bytes(&replacement)
        )
    );
    assert_ne!(manifest.entries[body].source_hash, original);
    assert!(second.converted >= 2);

    // The body still physically neighbors Low's skeleton, but overwrite wins.
    // Changing that losing source must not invalidate either winning asset.
    replacement[name..name + 13].copy_from_slice(b"GeneratedLoss");
    write(&instance, &format!("mods/Low/{skeleton}"), &replacement);
    let third = run(config.clone(), Cancellation::new()).await.unwrap();
    assert!(third.complete);
    assert_eq!(third.converted, 0);
    assert_eq!(third.cache_hits, second.converted + second.cache_hits);
    let unchanged = converter::cache::ConversionManifest::load(
        &config.output_dir.join("conversion-manifest.json"),
    )
    .unwrap();
    assert_eq!(
        unchanged.entries[body].source_hash,
        manifest.entries[body].source_hash
    );
}

/// Verifies conversion reads winning MO2 sources directly without copying them into staging.
#[tokio::test]
async fn reads_sources_without_materializing_mo2_inputs() {
    let (_dir, config, instance) = fixture();
    let output = config.output_dir.clone();
    let data = config.data_dir.clone();
    let original = fs::read(data.join("meshes/generated.nif")).unwrap();
    write(&instance, "mods/High/meshes/loose-only.nif", &original);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<converter::ProgressEvent>(1);
    let inspect = tokio::spawn(async move {
        let mut saw_meshes = false;
        while let Some(event) = rx.recv().await {
            if event.stage == converter::ProgressStage::Meshes {
                saw_meshes = true;
                let (staging, _) = converter::config::find_resumable_staging(&output).unwrap();
                assert!(!staging.join(".mo2-input").exists());
                assert!(!staging.join("vfs/meshes/loose-only.nif").exists());
                assert!(!staging.join("vfs/skyrim.esm").exists());
            }
        }
        saw_meshes
    });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    assert!(report.complete);
    assert!(inspect.await.unwrap());
    assert_eq!(
        fs::read(data.join("meshes/generated.nif")).unwrap(),
        original
    );
}

/// Game-folder CCC entries beyond the formerly fixed Creations publish without profile entries.
#[tokio::test]
async fn extra_ccc_creation_and_profile_neighbors_publish() {
    let (_dir, config, instance) = fixture();
    plugin_setting(
        &instance.join("mods/Low/ExtraCreation.esl"),
        "Skyrim.esm",
        "fExtraCreation",
        87.0,
    );
    fs::write(
        config.data_dir.parent().unwrap().join("Skyrim.ccc"),
        "ExtraCreation.esl\n",
    )
    .unwrap();
    let _report = publish(&config).await;
    assert_publication(
        &config,
        &["skyrim.esm", "extracreation.esl", "middle.esp", "patch.esp"],
        444.0,
    );
    assert_setting(&config, "fExtraCreation", 87.0, 1);
}

/// Both profile lists decode Windows-1252 names and retain the distinct setting's owner.
#[tokio::test]
async fn windows_1252_profile_lists_and_neighbors_publish() {
    let (_dir, config, instance) = fixture();
    plugin_setting(
        &instance.join("mods/Low/Café.esp"),
        "Skyrim.esm",
        "fAccentSetting",
        63.0,
    );
    write(
        &instance,
        "profiles/Default/plugins.txt",
        b"*Caf\xe9.esp\n*Patch.esp\n*Middle.esp\n",
    );
    write(
        &instance,
        "profiles/Default/loadorder.txt",
        b"Skyrim.esm\nMiddle.esp\nCaf\xe9.esp\nPatch.esp\n",
    );
    let _report = publish(&config).await;
    assert_publication(
        &config,
        &["skyrim.esm", "middle.esp", "café.esp", "patch.esp"],
        444.0,
    );
    assert_setting(&config, "fAccentSetting", 63.0, 2);
}

/// Accented profile names share the winning Data key across casing and publish the overwrite source.
#[tokio::test]
async fn unicode_case_mo2_plugin_name_and_overwrite_publish() {
    let (_dir, mut config, instance) = fixture();
    config.no_lod = true;
    for (relative, value) in [
        ("mods/Low/café.esp", 11.0),
        ("mods/High/CAFÉ.esp", 29.0),
        ("overwrite/CAFÉ.esp", 83.0),
    ] {
        plugin_setting(
            &instance.join(relative),
            "Skyrim.esm",
            "fUnicodeAccentSetting",
            value,
        );
    }
    write(
        &instance,
        "profiles/Default/plugins.txt",
        b"*caf\xe9.esp\n*Patch.esp\n*Middle.esp\n",
    );
    write(
        &instance,
        "profiles/Default/loadorder.txt",
        b"Skyrim.esm\nMiddle.esp\nCAF\xc9.esp\nPatch.esp\n",
    );
    let _report = publish(&config).await;
    assert_publication(
        &config,
        &["skyrim.esm", "middle.esp", "café.esp", "patch.esp"],
        444.0,
    );
    // The accented file owns native slot 1, whereas Middle.esp places it at published slot 2.
    assert_setting(&config, "fUnicodeAccentSetting", 83.0, 2);
    let conn = Connection::open(config.output_dir.join("skyrim_world.db")).unwrap();
    let actual: String = conn
        .query_row("SELECT name FROM plugins WHERE priority=2", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(actual, "CAFÉ.esp");
}

/// MO2's effective loose banks win independently of the plugin's parent and broken banks stay local.
#[tokio::test]
async fn inhouse_mo2_localized_banks_follow_loose_winners_and_preserve_neighbors() {
    let (_dir, mut config, instance) = fixture();
    config.no_lod = true;
    config.record_reader = converter::config::RecordReader::Inhouse;
    for (name, editor) in [
        ("CAFÉ.esp", "Mo2Localized"),
        ("BrokenNames.esp", "Mo2Broken"),
    ] {
        localized_plugin(&instance.join("mods/Low").join(name), editor);
    }
    for (root, name, text) in [
        ("mods/Low", "CAFÉ", b"Losing low name".as_slice()),
        ("mods/High", "café", b"Losing high name".as_slice()),
        ("overwrite", "café", b"Winning overwrite Caf\xe9".as_slice()),
        (
            "mods/Low",
            "BrokenNames",
            b"Losing broken-bank name".as_slice(),
        ),
        (
            "mods/High",
            "BrokenNames",
            b"Winning high name \x80".as_slice(),
        ),
    ] {
        write(
            &instance,
            &format!("{root}/Strings/{name}_english.strings"),
            single_string_bank(text, false),
        );
    }
    for (root, name, text) in [
        ("mods/Low", "CAFÉ", b"Losing low description".as_slice()),
        (
            "mods/High",
            "café",
            b"Winning high description \x80".as_slice(),
        ),
        (
            "mods/Low",
            "BrokenNames",
            b"Losing valid description".as_slice(),
        ),
    ] {
        write(
            &instance,
            &format!("{root}/Strings/{name}_english.dlstrings"),
            single_string_bank(text, true),
        );
    }
    write(
        &instance,
        "overwrite/Strings/BrokenNames_english.dlstrings",
        b"bad",
    );
    write(
        &instance,
        "profiles/Default/plugins.txt",
        b"*Middle.esp\n*caf\xe9.esp\n*BrokenNames.esp\n*Patch.esp\n",
    );
    write(
        &instance,
        "profiles/Default/loadorder.txt",
        b"Skyrim.esm\nMiddle.esp\nCAF\xc9.esp\nBrokenNames.esp\nPatch.esp\n",
    );
    // The deliberately broken winning bank loses a description, so usable output
    // publishes with a bounded data-loss warning and an incomplete manifest.
    let report = run(config.clone(), Cancellation::new()).await.unwrap();
    assert!(!report.complete, "{report:?}");
    assert_eq!(report.skipped, 0);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("brokennames.esp"));
    assert!(report.warnings[0].contains("local decode/lookup issues"));
    assert!(report.warnings[0].contains("missing localized ID 17"));
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(config.output_dir.join("conversion-manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["complete"], false);
    assert_publication(
        &config,
        &[
            "skyrim.esm",
            "middle.esp",
            "café.esp",
            "brokennames.esp",
            "patch.esp",
        ],
        444.0,
    );
    let conn = Connection::open(config.output_dir.join("skyrim_world.db")).unwrap();
    let lookups: Vec<(u32, String, String, Option<String>, String)> = conn
        .prepare("SELECT form_id,signature,string_table,resolved_text,status FROM inhouse_localized_fields WHERE form_id IN (33556736,50333952) ORDER BY form_id,field_index")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        lookups,
        [
            (
                0x0200_0900,
                "FULL".into(),
                "strings".into(),
                Some("Winning overwrite Café".into()),
                "resolved".into()
            ),
            (
                0x0200_0900,
                "DESC".into(),
                "dlstrings".into(),
                Some("Winning high description €".into()),
                "resolved".into()
            ),
            (
                0x0300_0900,
                "FULL".into(),
                "strings".into(),
                Some("Winning high name €".into()),
                "resolved".into()
            ),
            (
                0x0300_0900,
                "DESC".into(),
                "dlstrings".into(),
                None,
                "missing".into()
            ),
        ]
    );
    for (id, expected_full, expected_description) in [
        (
            0x0200_0900u32,
            "Winning overwrite Café",
            "Winning high description €",
        ),
        (0x0300_0900, "Winning high name €", ""),
    ] {
        let bytes: Vec<u8> = conn
            .query_row("SELECT data FROM records WHERE form_id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        let runtime = rkyv::from_bytes::<
            converter::esm::types::ArchivedRecordData,
            rkyv::rancor::Error,
        >(&bytes)
        .unwrap();
        for (tag, text) in [(b"FULL", expected_full), (b"DESC", expected_description)] {
            let value = runtime
                .subrecords
                .iter()
                .find(|field| &field.tag == tag)
                .unwrap();
            assert_eq!(value.data, format!("{text}\0").as_bytes());
        }
    }
    for id in [0x0200_0901u32, 0x0300_0901] {
        let count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "usable neighbor must survive broken localized bank"
        );
    }
    let notices: u32 = conn.query_row("SELECT COUNT(*) FROM record_diagnostics WHERE plugin_id=3 AND stage='localization_bank' AND field='dlstrings' AND action='unresolved' AND reason LIKE '%truncated STRINGS header%'", [], |row| row.get(0)).unwrap();
    assert_eq!(
        notices, 1,
        "broken winning bank must produce one contextual diagnostic"
    );
    let source: Vec<u8> = conn
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=?1",
            [0x0200_0900u32],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        source
            .windows(10)
            .any(|bytes| bytes == sub(b"FULL", &17u32.to_le_bytes()))
    );
    assert_eq!(
        converter::cache::hash_bytes(&fs::read(instance.join("mods/Low/CAFÉ.esp")).unwrap()),
        conn.query_row(
            "SELECT lower(hex(checksum)) FROM plugins WHERE priority=2",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "plugin provenance must still hash the original low-layer source"
    );
}

/// Last duplicate checkboxes and loadorder positions win while usable records publish.
#[tokio::test]
async fn duplicate_profile_entries_keep_last_state_and_position_and_publish() {
    let (_dir, config, instance) = fixture();
    plugin_setting(
        &instance.join("mods/Low/Neighbor.esp"),
        "Skyrim.esm",
        "fDuplicateNeighbor",
        73.0,
    );
    write(
        &instance,
        "profiles/Default/plugins.txt",
        "*Patch.esp\nMiddle.esp\n*Neighbor.esp\n*Middle.esp\nPatch.esp\n",
    );
    write(
        &instance,
        "profiles/Default/loadorder.txt",
        "Skyrim.esm\nMiddle.esp\nNeighbor.esp\nMiddle.esp\nPatch.esp\n",
    );
    let report = publish(&config).await;
    assert_publication(
        &config,
        &["skyrim.esm", "neighbor.esp", "middle.esp"],
        100.0,
    );
    assert_setting(&config, "fDuplicateNeighbor", 73.0, 1);
    assert_notice(&report, &["duplicate plugins.txt", "patch.esp"]);
    assert_notice(&report, &["duplicate plugins.txt", "middle.esp"]);
    assert_notice(&report, &["duplicate loadorder.txt", "middle.esp"]);
}

/// A listed active plugin missing from merged Data is omitted while its neighbors publish.
#[tokio::test]
async fn active_plugin_missing_from_data_preserves_publication() {
    let (_dir, config, instance) = fixture();
    write(
        &instance,
        "profiles/Default/plugins.txt",
        "*Patch.esp\n*NotInstalled.esp\n*Middle.esp\nInactive.esm\n",
    );
    write(
        &instance,
        "profiles/Default/loadorder.txt",
        "Skyrim.esm\nMiddle.esp\nNotInstalled.esp\nPatch.esp\nInactive.esm\n",
    );
    let report = publish(&config).await;
    assert_publication(&config, &["skyrim.esm", "middle.esp", "patch.esp"], 444.0);
    assert_notice(&report, &["notinstalled.esp", "not installed"]);
}

/// Active entries absent from loadorder append in profile order after all ordered plugins.
#[tokio::test]
async fn absent_loadorder_entries_append_in_profile_order_and_publish() {
    let (_dir, config, instance) = fixture();
    for (name, editor, value) in [
        ("SecondFallback.esp", "fSecondFallback", 53.0),
        ("FirstFallback.esp", "fFirstFallback", 59.0),
    ] {
        plugin_setting(
            &instance.join("mods/Low").join(name),
            "Skyrim.esm",
            editor,
            value,
        );
    }
    write(
        &instance,
        "profiles/Default/plugins.txt",
        "*SecondFallback.esp\n*Patch.esp\n*FirstFallback.esp\n*Middle.esp\nInactive.esm\n",
    );
    let report = publish(&config).await;
    assert_publication(
        &config,
        &[
            "skyrim.esm",
            "middle.esp",
            "patch.esp",
            "secondfallback.esp",
            "firstfallback.esp",
        ],
        444.0,
    );
    assert_setting(&config, "fSecondFallback", 53.0, 3);
    assert_setting(&config, "fFirstFallback", 59.0, 4);
    for name in ["secondfallback.esp", "firstfallback.esp"] {
        assert_notice(&report, &[name, "absent from loadorder.txt"]);
    }
}

/// Missing and disabled master closures warn with profile context and preserve published neighbors.
#[tokio::test]
async fn missing_and_disabled_master_closures_preserve_profile_publication() {
    let (_dir, config, instance) = fixture();
    for (relative, master, editor) in [
        ("overwrite/PATCH.esp", "Missing.esm", "fBrokenPatch"),
        ("mods/Low/Child.esp", "Patch.esp", "fBrokenChild"),
        ("mods/Low/Disabled.esm", "Skyrim.esm", "fDisabledMaster"),
        (
            "mods/Low/DisabledChild.esp",
            "Disabled.esm",
            "fDisabledChild",
        ),
        (
            "mods/Low/Grandchild.esp",
            "DisabledChild.esp",
            "fDisabledGrandchild",
        ),
    ] {
        plugin_setting(&instance.join(relative), master, editor, 17.0);
    }
    write(
        &instance,
        "profiles/Default/plugins.txt",
        "*Patch.esp\n*Child.esp\nDisabled.esm\n*DisabledChild.esp\n*Grandchild.esp\n*Middle.esp\n",
    );
    write(
        &instance,
        "profiles/Default/loadorder.txt",
        "Skyrim.esm\nPatch.esp\nChild.esp\nDisabled.esm\nDisabledChild.esp\nGrandchild.esp\nMiddle.esp\n",
    );
    let report = publish(&config).await;
    assert_publication(&config, &["skyrim.esm", "middle.esp"], 100.0);
    assert_setting(&config, "fJumpHeightMin", 100.0, 1);
    for (name, missing) in [
        ("patch.esp", "Missing.esm"),
        ("child.esp", "Missing.esm"),
        ("disabledchild.esp", "Disabled.esm"),
        ("grandchild.esp", "Disabled.esm"),
    ] {
        assert_notice(&report, &[&format!("skipping {name}:"), missing]);
    }
    let settings = published_settings(&config);
    let bad = settings
        .iter()
        .filter(|(editor, _, _)| {
            [
                "fBrokenPatch",
                "fBrokenChild",
                "fDisabledMaster",
                "fDisabledChild",
                "fDisabledGrandchild",
            ]
            .contains(&editor.as_str())
        })
        .count();
    assert_eq!(bad, 0);
}

/// Unicode profile keys and a differently cased TES4 master resolve the same winning owner.
#[tokio::test]
async fn final_unicode_profile_master_override_publishes() {
    let (_dir, config, instance) = fixture();
    plugin(&instance.join("mods/Low/CAF\u{c9}.esp"), "Skyrim.esm", 67.0);
    let header = [sub(b"MAST", b"cAf\xe9.ESP\0"), sub(b"DATA", &[0; 8])].concat();
    let bytes = [
        record(b"TES4", 0, 0, header),
        record(
            b"GMST",
            0x800,
            0,
            [
                sub(b"EDID", b"fJumpHeightMin\0"),
                sub(b"DATA", &93.0f32.to_le_bytes()),
            ]
            .concat(),
        ),
    ]
    .concat();
    write(&instance, "overwrite/PATCH.esp", bytes);
    write(
        &instance,
        "profiles/Default/plugins.txt",
        b"*Middle.esp\n*caf\xe9.esp\n*Patch.esp\n",
    );
    write(
        &instance,
        "profiles/Default/loadorder.txt",
        b"Skyrim.esm\nMiddle.esp\ncaf\xe9.esp\nPatch.esp\n",
    );
    let report = publish(&config).await;
    let conn = Connection::open(config.output_dir.join("skyrim_world.db")).unwrap();
    validate_database(&conn).unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        names,
        ["Skyrim.esm", "Middle.esp", "CAF\u{c9}.esp", "PATCH.esp"]
    );
    assert_eq!(value(&config), 93.0);
    let winning: Vec<(u32, u32)> = conn
        .prepare("SELECT form_id, load_order FROM records WHERE record_type='GMST'")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    // GMST retains the first owner's identity despite overrides from both later plugins.
    assert!(winning.contains(&(0x0100_0800, 3)), "{winning:?}");
    assert!(
        report
            .notices
            .iter()
            .all(|notice| !notice.contains("required master")),
        "{:?}",
        report.notices
    );
    assert!(config.output_dir.join("conversion-manifest.json").is_file());
    assert!(config.output_dir.join("meshes/generated.glb").is_file());
}

/// Cyclic profile plugins and their transitive dependents are omitted while neighbors publish.
#[tokio::test]
async fn final_cycle_closure_preserves_profile_publication() {
    let (_dir, config, instance) = fixture();
    for (name, master, editor) in [
        ("CycleA.esp", "CycleC.esp", "fProfileCycleA"),
        ("CycleB.esp", "CycleA.esp", "fProfileCycleB"),
        ("CycleC.esp", "CycleB.esp", "fProfileCycleC"),
        ("Child.esp", "CycleB.esp", "fProfileCycleChild"),
        ("Grandchild.esp", "Child.esp", "fProfileCycleGrandchild"),
    ] {
        plugin_setting(&instance.join("mods/Low").join(name), master, editor, 17.0);
    }
    write(
        &instance,
        "profiles/Default/plugins.txt",
        "*CycleA.esp\n*CycleB.esp\n*CycleC.esp\n*Child.esp\n*Grandchild.esp\n*Middle.esp\n",
    );
    write(
        &instance,
        "profiles/Default/loadorder.txt",
        "Skyrim.esm\nCycleA.esp\nCycleB.esp\nCycleC.esp\nChild.esp\nGrandchild.esp\nMiddle.esp\n",
    );
    let report = publish(&config).await;
    assert_publication(&config, &["skyrim.esm", "middle.esp"], 100.0);
    assert!(
        !published_settings(&config)
            .iter()
            .any(|(editor, _, _)| editor.starts_with("fProfileCycle"))
    );
    let summaries: Vec<_> = report
        .notices
        .iter()
        .filter(|notice| notice.contains("cyclic plugin dependencies"))
        .collect();
    assert_eq!(summaries.len(), 1, "{:?}", report.notices);
    assert!(summaries[0].contains("MO2 profile Default"));
    assert_eq!(
        report.skipped_plugins,
        [
            "CycleA.esp",
            "CycleB.esp",
            "CycleC.esp",
            "Child.esp",
            "Grandchild.esp"
        ]
    );
}

/// A 256-full-plugin MO2 profile keeps its first 254 and publishes usable neighboring records.
#[tokio::test]
async fn final_full_slot_overflow_preserves_profile_publication() {
    let (_dir, config, instance) = fixture();
    let mut entries = String::new();
    let mut order = String::from("Skyrim.esm\n");
    for index in 0..255 {
        let name = format!("Full{index:03}.esp");
        let editor = if index == 0 {
            "fJumpHeightMin".to_owned()
        } else {
            format!("fProfileFull{index:03}")
        };
        plugin_setting(
            &instance.join("mods/Low").join(&name),
            "Skyrim.esm",
            &editor,
            73.0,
        );
        entries.push_str(&format!("*{name}\n"));
        order.push_str(&format!("{name}\n"));
    }
    plugin_setting(
        &instance.join("mods/Low/Dependent.esp"),
        "Full254.esp",
        "fProfileOverflowDependent",
        19.0,
    );
    let path = instance.join("mods/Low/Dependent.esp");
    let mut bytes = fs::read(&path).unwrap();
    bytes[8..12].copy_from_slice(&0x200u32.to_le_bytes());
    fs::write(path, bytes).unwrap();
    entries.push_str("*Dependent.esp\n");
    order.push_str("Dependent.esp\n");
    write(&instance, "profiles/Default/plugins.txt", entries);
    write(&instance, "profiles/Default/loadorder.txt", order);
    let report = publish(&config).await;
    let expected: Vec<_> = std::iter::once("skyrim.esm".to_owned())
        .chain((0..253).map(|index| format!("full{index:03}.esp")))
        .collect();
    assert_publication(
        &config,
        &expected.iter().map(String::as_str).collect::<Vec<_>>(),
        73.0,
    );
    let settings = published_settings(&config);
    assert!(
        settings
            .iter()
            .any(|(editor, _, priority)| editor == "fProfileFull252" && *priority == 253)
    );
    assert!(!settings.iter().any(|(editor, _, _)| {
        [
            "fProfileFull253",
            "fProfileFull254",
            "fProfileOverflowDependent",
        ]
        .contains(&editor.as_str())
    }));
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

/// Verifies resume discards stale staged inputs and outputs after the MO2 profile changes.
#[tokio::test]
async fn resume_rebuilds_input_and_vfs_after_profile_change() {
    let (_dir, mut config, instance) = fixture();
    let original_hash = configuration_hash(&config).unwrap();
    let stop = Cancellation::new();
    stop.cancel();
    let interrupted = run(config.clone(), stop).await.unwrap_err();
    assert!(interrupted.cancelled);
    let staging = interrupted.staging.unwrap();
    write(
        &staging,
        ".mo2-input/scripts/stale.pex",
        "invalid stale script",
    );
    write(&staging, "vfs/scripts/stale.pex", "invalid stale script");
    write(&staging, "scripts/stale.luau", "stale converted script");
    write(&staging, "textures/stale.ktx2", "stale converted texture");
    write(&instance, "profiles/Default/modlist.txt", "-High\n+Low\n");
    assert_eq!(original_hash, configuration_hash(&config).unwrap());
    config.resume_staging = Some(staging.clone());
    let report = run(config.clone(), Cancellation::new()).await.unwrap();
    assert!(report.complete && report.skipped == 0, "{report:?}");
    assert!(!staging.exists());
    assert!(!config.output_dir.join("scripts/stale.luau").exists());
    assert!(!config.output_dir.join("textures/stale.ktx2").exists());
}

/// Verifies source overlap and conflicting plugin selection are rejected and MO2 configs round-trip.
#[tokio::test]
async fn refuses_source_overlap_and_conflicting_plugin_selection() {
    let (_dir, config, instance) = fixture();
    for destination in [
        instance.join("mods/cache"),
        instance.clone(),
        instance.parent().unwrap().to_path_buf(),
    ] {
        let mut unsafe_config = config.clone();
        unsafe_config.cache_dir = Some(destination);
        let error = run(unsafe_config, Cancellation::new()).await.unwrap_err();
        assert!(format!("{:#}", error.error).contains("overlaps"));
    }
    let mut conflict = config.clone();
    conflict.plugins_file = Some(instance.join("profiles/Default/plugins.txt"));
    assert!(
        format!(
            "{:#}",
            run(conflict, Cancellation::new()).await.unwrap_err().error
        )
        .contains("cannot be combined")
    );
    // Existing serialized configs still load; MO2 selections round-trip for the UI.
    let mut json = serde_json::to_value(&config).unwrap();
    json.as_object_mut().unwrap().remove("mo2");
    assert!(
        serde_json::from_value::<PipelineConfig>(json)
            .unwrap()
            .mo2
            .is_none()
    );
    let roundtrip: PipelineConfig =
        serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
    assert_eq!(roundtrip.mo2.unwrap().profile, "Default");
}
