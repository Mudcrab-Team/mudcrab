//! Repair preview and apply against generated assets, with no installed game data.
use converter::{AssetPipeline, PipelineConfig, cache::ConversionManifest, repair::repair_failed};
use std::fs;

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

    let preview = repair_failed(&config, false).unwrap();
    assert!(!preview.published);
    assert!(preview.failures.is_empty());
    assert_eq!(fs::read(&manifest_path).unwrap(), before_manifest);
    assert_eq!(fs::read(&database).unwrap(), before_database);
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

    let applied = repair_failed(&config, true).unwrap();
    assert!(applied.published);
    assert!(applied.failures.is_empty());
    let manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert!(manifest.complete);
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
