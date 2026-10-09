//! Repair preview and apply against generated assets, with no installed game data.
use converter::mesh::MeshConverter;
use converter::{AssetPipeline, PipelineConfig, cache::ConversionManifest, repair::repair_failed};
use std::{collections::BTreeMap, fs, path::Path};

async fn run_fixture(config: &PipelineConfig) -> converter::pipeline::PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config.clone(), tx).await.unwrap();
    drain.await.unwrap();
    report
}

fn published_bytes(output: &Path) -> BTreeMap<String, Vec<u8>> {
    let manifest_bytes = fs::read(output.join("conversion-manifest.json")).unwrap();
    let manifest: ConversionManifest = serde_json::from_slice(&manifest_bytes).unwrap();
    let mut files: BTreeMap<_, _> = manifest
        .entries
        .values()
        .map(|entry| {
            (
                entry.output.clone(),
                fs::read(output.join(&entry.output)).unwrap(),
            )
        })
        .collect();
    files.insert("conversion-manifest.json".into(), manifest_bytes);
    files.insert(
        "integration-report.json".into(),
        fs::read(output.join("integration-report.json")).unwrap(),
    );
    files
}

fn assert_mesh_textures_resolve(output: &Path) {
    let mesh = output.join("meshes/generated.glb");
    let dependencies = MeshConverter::glb_texture_dependencies(&mesh).unwrap();
    assert!(!dependencies.is_empty());
    for dependency in dependencies {
        assert!(
            mesh.parent().unwrap().join(&dependency.uri).is_file(),
            "missing {}",
            dependency.uri
        );
    }
}

#[tokio::test]
async fn dds_only_actual_failure_repairs_retained_mesh_aliases() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    dummy_content::layout::prepare_directory(&data, false).unwrap();
    dummy_content::layout::generate(
        &data,
        dummy_content::layout::DEFAULT_SEED,
        dummy_content::layout::Formats::all(),
    )
    .unwrap();
    let dds = data.join(dummy_content::layout::GENERATED_DIFFUSE_PATH);
    let good_dds = fs::read(&dds).unwrap();
    fs::write(&dds, b"invalid DDS for repair regression").unwrap();
    let output = root.path().join("modern");
    let config = PipelineConfig::new(&data, &output);
    assert!(!run_fixture(&config).await.complete);
    let manifest_path = output.join("conversion-manifest.json");
    let manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest.failures.len(), 1);
    assert!(
        manifest
            .failures
            .contains_key(dummy_content::layout::GENERATED_DIFFUSE_PATH)
    );
    assert!(
        manifest
            .entries
            .values()
            .any(|entry| entry.output == "meshes/generated.glb")
    );
    let alias = "textures/generated_color.opensky-srgb.opensky-wrap0.ktx2";
    assert!(!output.join(alias).exists());
    let before = published_bytes(&output);
    let failed_preview = repair_failed(&config, false).unwrap();
    assert!(!failed_preview.published);
    assert_eq!(failed_preview.failures.len(), 1);
    assert_eq!(published_bytes(&output), before);
    assert!(repair_failed(&config, true).is_err());
    assert_eq!(published_bytes(&output), before);

    fs::write(&dds, good_dds).unwrap();
    let preview = repair_failed(&config, false).unwrap();
    assert_eq!(preview.converted, 1);
    assert!(preview.failures.is_empty());
    assert_mesh_textures_resolve(&preview.directory.join("assets"));
    assert_eq!(published_bytes(&output), before);
    assert!(!output.join(alias).exists());
    let applied = repair_failed(&config, true).unwrap();
    assert!(applied.published);
    assert_eq!(applied.converted, 1);
    assert_mesh_textures_resolve(&output);
    assert_eq!(
        fs::read(output.join("meshes/generated.glb")).unwrap(),
        before["meshes/generated.glb"]
    );
    let manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert!(manifest.complete);
    assert!(manifest.failures.is_empty());
    assert_eq!(manifest.entries[&format!("alias:{alias}")].output, alias);
    assert_eq!(
        fs::read(output.join(alias)).unwrap(),
        fs::read(output.join("textures/generated_color.ktx2")).unwrap()
    );
    let check =
        converter::check::check_output(&output, converter::check::CheckMode::Full, |_, _| {})
            .unwrap();
    assert!(check.problems.is_empty(), "{:?}", check.problems);
}

#[tokio::test]
async fn dds_repair_refreshes_existing_aliases_and_isolates_retained_mesh_pruning() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    dummy_content::layout::prepare_directory(&data, false).unwrap();
    dummy_content::layout::generate(
        &data,
        dummy_content::layout::DEFAULT_SEED,
        dummy_content::layout::Formats::all(),
    )
    .unwrap();
    let output = root.path().join("modern");
    let config = PipelineConfig::new(&data, &output);
    assert!(run_fixture(&config).await.complete);
    let alias = "textures/generated_color.opensky-srgb.opensky-wrap0.ktx2";
    let old_texture = fs::read(output.join(alias)).unwrap();
    let replacement = root.path().join("replacement");
    dummy_content::layout::prepare_directory(&replacement, false).unwrap();
    dummy_content::layout::generate(
        &replacement,
        dummy_content::layout::DEFAULT_SEED + 1,
        dummy_content::layout::Formats::all(),
    )
    .unwrap();
    fs::copy(
        replacement.join(dummy_content::layout::GENERATED_DIFFUSE_PATH),
        data.join(dummy_content::layout::GENERATED_DIFFUSE_PATH),
    )
    .unwrap();

    // A retained mesh also has an absent-source dependency that pruning rewrites.
    // Keep the replacement the same length so GLB chunk lengths remain valid.
    let mesh = output.join("meshes/generated.glb");
    let mut bytes = fs::read(&mesh).unwrap();
    let needle = b"generated_normal";
    let position = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    bytes[position..position + needle.len()].copy_from_slice(b"unavailable_norm");
    fs::write(&mesh, bytes).unwrap();
    let manifest_path = output.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    for entry in manifest
        .entries
        .values_mut()
        .filter(|entry| entry.output == "meshes/generated.glb")
    {
        entry.output_hash = converter::cache::hash_file(&mesh).unwrap();
        entry.output_size = fs::metadata(&mesh).unwrap().len();
    }
    manifest.failures.insert(
        dummy_content::layout::GENERATED_DIFFUSE_PATH.into(),
        "retry texture with changed source".into(),
    );
    manifest.complete = false;
    manifest.save(&manifest_path).unwrap();
    let before = published_bytes(&output);
    let preview = repair_failed(&config, false).unwrap();
    assert_eq!(preview.converted, 1);
    assert!(preview.failures.is_empty());
    let staged = preview.directory.join("assets");
    assert_mesh_textures_resolve(&staged);
    let new_texture = fs::read(staged.join("textures/generated_color.ktx2")).unwrap();
    assert_ne!(new_texture, old_texture);
    assert_eq!(fs::read(staged.join(alias)).unwrap(), new_texture);
    assert_ne!(
        fs::read(staged.join("meshes/generated.glb")).unwrap(),
        before["meshes/generated.glb"]
    );
    assert_eq!(published_bytes(&output), before);
    let staged_manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(staged.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert!(
        staged_manifest.pruned_texture_references["meshes/generated.glb"]
            .iter()
            .any(|uri| uri.contains("unavailable_norm"))
    );

    let applied = repair_failed(&config, true).unwrap();
    assert!(applied.published);
    assert_eq!(applied.converted, 1);
    assert_mesh_textures_resolve(&output);
    assert_eq!(fs::read(output.join(alias)).unwrap(), new_texture);
    assert_eq!(
        fs::read(output.join("textures/generated_color.ktx2")).unwrap(),
        new_texture
    );
    assert_ne!(fs::read(&mesh).unwrap(), before["meshes/generated.glb"]);
    let manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert!(manifest.complete);
    assert!(manifest.failures.is_empty());
    assert_eq!(
        manifest.entries[&format!("alias:{alias}")].output_hash,
        converter::cache::hash_file(&output.join(alias)).unwrap()
    );
    let check =
        converter::check::check_output(&output, converter::check::CheckMode::Full, |_, _| {})
            .unwrap();
    assert!(check.problems.is_empty(), "{:?}", check.problems);
}

#[tokio::test]
async fn preview_preserves_pack_and_apply_finalizes_repaired_mesh_bounds() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    dummy_content::layout::prepare_directory(&data, false).unwrap();
    dummy_content::layout::generate(
        &data,
        dummy_content::layout::DEFAULT_SEED,
        dummy_content::layout::Formats::all(),
    )
    .unwrap();
    let output = root.path().join("modern");
    let config = PipelineConfig::new(&data, &output);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config.clone(), tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete);

    // Model the published state of a failed mesh conversion. Its database still
    // exists but has no bounds; repair must finalize those after restoring GLB.
    let database = output.join("skyrim_world.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute("UPDATE statics SET bounds_valid=0", [])
        .unwrap();
    drop(connection);
    let manifest_path = output.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let mesh_key = manifest
        .entries
        .iter()
        .find(|(_, entry)| entry.output == "meshes/generated.glb")
        .map(|(key, _)| key.clone())
        .unwrap();
    let texture_key = manifest
        .entries
        .iter()
        .find(|(key, entry)| key.ends_with(".dds") && entry.output.ends_with(".ktx2"))
        .map(|(key, _)| key.clone())
        .unwrap();
    let texture = manifest.entries.remove(&texture_key).unwrap();
    fs::remove_file(output.join(&texture.output)).unwrap();
    manifest
        .failures
        .insert(texture_key.clone(), "fixture texture failure".into());
    manifest.entries.remove(&mesh_key);
    manifest
        .failures
        .insert(mesh_key, "fixture mesh failure".into());
    manifest.complete = false;
    for entry in manifest
        .entries
        .values_mut()
        .filter(|entry| entry.output == "skyrim_world.db")
    {
        entry.output_hash = converter::cache::hash_file(&database).unwrap();
        entry.output_size = fs::metadata(&database).unwrap().len();
    }
    manifest.save(&manifest_path).unwrap();
    fs::remove_file(output.join("meshes/generated.glb")).unwrap();
    let before_manifest = fs::read(&manifest_path).unwrap();
    let before_database = fs::read(&database).unwrap();
    let before_integration = fs::read(output.join("integration-report.json")).unwrap();

    let preview = repair_failed(&config, false).unwrap();
    assert!(!preview.published);
    assert_eq!(
        preview.directory.parent(),
        Some(fs::canonicalize(&output).unwrap().as_path())
    );
    assert!(
        !preview
            .directory
            .join("assets")
            .join(preview.directory.file_name().unwrap())
            .exists()
    );
    assert!(preview.failures.is_empty());
    assert_eq!(fs::read(&manifest_path).unwrap(), before_manifest);
    assert_eq!(fs::read(&database).unwrap(), before_database);
    assert_eq!(
        fs::read(output.join("integration-report.json")).unwrap(),
        before_integration
    );
    assert!(!output.join("meshes/generated.glb").exists());
    let staged = preview.directory.join("assets");
    let connection = rusqlite::Connection::open(staged.join("skyrim_world.db")).unwrap();
    let bounded: i64 = connection
        .query_row(
            "SELECT count(*) FROM statics WHERE bounds_valid=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(bounded > 0);
    drop(connection);
    // Old previews are neither pack inputs nor semantic-discovery inputs.
    fs::write(staged.join("meshes/invalid-preview.glb"), "not a GLB").unwrap();

    let applied = repair_failed(&config, true).unwrap();
    assert!(applied.published);
    assert!(
        !applied
            .directory
            .join("assets")
            .join(preview.directory.file_name().unwrap())
            .exists()
    );
    assert!(applied.failures.is_empty());
    let manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert!(manifest.complete);
    assert_eq!(
        manifest.entries[&texture_key].source_hash,
        texture.source_hash
    );
    assert!(
        !manifest.entries[&texture_key]
            .source_hash
            .contains(":gpu-uastc-")
    );
    assert!(output.join("meshes/generated.glb").is_file());
    let integration: converter::integration::IntegrationReport =
        serde_json::from_slice(&fs::read(output.join("integration-report.json")).unwrap()).unwrap();
    assert!(integration.passed);
    assert!(integration.bounds_updated > 0);
    let check =
        converter::check::check_output(&output, converter::check::CheckMode::Full, |_, _| {})
            .unwrap();
    assert!(check.problems.is_empty(), "{:?}", check.problems);

    // Conversion must recover a repair journal BEFORE parsing the previous
    // manifest, not merely before replacing the output at the end of the run.
    let backup = root.path().join("interrupted-repair-backup");
    fs::create_dir(&backup).unwrap();
    fs::copy(&manifest_path, backup.join("conversion-manifest.json")).unwrap();
    let journal = output.with_file_name("modern.repair-journal.json");
    fs::write(
        &journal,
        serde_json::to_vec(&serde_json::json!({
            "backup": backup,
            "paths": [["conversion-manifest.json", true]]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(&manifest_path, "interrupted invalid manifest").unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let converted = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(converted.complete);
    assert!(converted.cache_hits > 0);
    assert!(!journal.exists());
}

#[test]
fn repair_refuses_readers_before_journal_recovery() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    let output = root.path().join("modern");
    fs::create_dir(&data).unwrap();
    fs::create_dir(&output).unwrap();
    let manifest = output.join("conversion-manifest.json");
    fs::write(&manifest, "untouched").unwrap();
    let journal = output.with_file_name("modern.repair-journal.json");
    fs::write(&journal, "invalid journal must not be read").unwrap();
    let reader = shared::asset_lock::AssetLock::acquire_shared(&output).unwrap();
    for apply in [false, true] {
        let error = repair_failed(&PipelineConfig::new(&data, &output), apply).unwrap_err();
        assert!(
            error.to_string().contains("asset directory in use"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&manifest).unwrap(), "untouched");
        assert_eq!(
            fs::read_to_string(&journal).unwrap(),
            "invalid journal must not be read"
        );
        assert_eq!(fs::read_dir(&output).unwrap().count(), 1);
    }
    drop(reader);
}

#[tokio::test]
async fn conversion_and_repair_share_ownership_before_manifest_reads() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir(&data).unwrap();
    let output = root.path().join("modern");
    let config = PipelineConfig::new(&data, &output);
    let owner = converter::repair::OutputOwnership::acquire(&output).unwrap();
    fs::create_dir(&output).unwrap();
    let manifest = output.join("conversion-manifest.json");
    fs::write(&manifest, "invalid manifest must not be read").unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let error = AssetPipeline::run_async(config.clone(), tx)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("another conversion or repair"),
        "{error}"
    );
    let error = repair_failed(&config, false).unwrap_err();
    assert!(
        error.to_string().contains("another conversion or repair"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(&manifest).unwrap(),
        "invalid manifest must not be read"
    );
    drop(owner);
    converter::repair::OutputOwnership::acquire(&output).unwrap();
}

#[tokio::test]
async fn conversion_keeps_ownership_until_publication_finishes() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir(&data).unwrap();
    let output = root.path().join("modern");
    let config = PipelineConfig::new(&data, &output);
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let run = tokio::spawn(AssetPipeline::run_async(config, tx));
    let mut saw_publication = false;
    while let Some(event) = rx.recv().await {
        if event.stage == converter::progress::ProgressStage::Publishing {
            saw_publication = true;
            assert!(converter::repair::OutputOwnership::acquire(&output).is_err());
        }
    }
    run.await.unwrap().unwrap();
    assert!(saw_publication);
    converter::repair::OutputOwnership::acquire(&output).unwrap();
}
