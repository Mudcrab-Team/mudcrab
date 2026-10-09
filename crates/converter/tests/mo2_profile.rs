//! MO2 selection through the real converter, including cache and resume boundaries.
use converter::{AssetPipeline, PipelineConfig, cache::configuration_hash, pipeline::Cancellation};
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
            sub(b"EDID", b"fJumpHeightMin\0"),
            sub(b"DATA", &value.to_le_bytes()),
        ]
        .concat(),
    ));
    fs::write(path, bytes).unwrap();
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

/// Verifies missing-master errors identify the MO2 profile and the plugin requiring that master.
#[tokio::test]
async fn missing_master_has_profile_and_plugin_context() {
    let (_dir, config, instance) = fixture();
    plugin(&instance.join("overwrite/PATCH.esp"), "Missing.esm", 1.0);
    let error = run(config, Cancellation::new()).await.unwrap_err();
    let text = format!("{:#}", error.error);
    assert!(text.contains("MO2 profile Default"), "{text}");
    assert!(
        text.contains("patch.esp") && text.contains("Missing.esm"),
        "{text}"
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
