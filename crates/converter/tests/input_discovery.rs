//! Discovery failures must stop conversion before publication.
#![cfg(unix)]

use converter::{
    AssetPipeline, PipelineConfig, PipelineFailure, PipelineReport, ProgressEvent, ProgressStage,
};
use std::{
    fs,
    future::Future,
    os::unix::fs::PermissionsExt,
    path::Path,
    task::{Context, Waker},
};
use tokio::sync::mpsc;

async fn collect(mut rx: mpsc::Receiver<ProgressEvent>) -> Vec<ProgressEvent> {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    events
}

async fn run(
    config: PipelineConfig,
) -> (Result<PipelineReport, PipelineFailure>, Vec<ProgressEvent>) {
    let (tx, rx) = mpsc::channel(64);
    let progress = tokio::spawn(collect(rx));
    let result = AssetPipeline::run_async(config, tx).await;
    (result, progress.await.unwrap())
}

fn assert_discovery_failure(
    result: Result<PipelineReport, PipelineFailure>,
    events: &[ProgressEvent],
    root: &Path,
    unreadable: &Path,
) {
    let failure = result.unwrap_err();
    let message = failure.to_string();
    assert!(message.contains("failed to discover inputs"), "{message}");
    assert!(message.contains(&root.display().to_string()), "{message}");
    assert!(
        message.contains(&unreadable.display().to_string()),
        "{message}"
    );
    assert!(failure.error.chain().any(|cause| {
        cause
            .downcast_ref::<walkdir::Error>()
            .and_then(walkdir::Error::io_error)
            .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
    }));
    assert!(failure.staging.unwrap().is_dir());
    assert!(!failure.cancelled);
    assert!(events.iter().all(|event| !matches!(
        event.stage,
        ProgressStage::Publishing | ProgressStage::Complete
    )));
}

#[tokio::test]
async fn unreadable_data_subtree_stops_conversion_and_preserves_published_output() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("modern");
    fs::create_dir(&data).unwrap();
    let mut config = PipelineConfig::new(&data, &output);
    config.cpu_jobs = 2;
    assert!(run(config.clone()).await.0.unwrap().complete);
    let manifest = fs::read(output.join("conversion-manifest.json")).unwrap();

    let unreadable = data.join("scripts/blocked");
    fs::create_dir_all(&unreadable).unwrap();
    fs::write(unreadable.join("hidden.pex"), b"hidden input").unwrap();
    let permissions = fs::metadata(&unreadable).unwrap().permissions();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o111)).unwrap();
    if fs::read_dir(&unreadable).is_ok() {
        // Privileged runners can bypass Unix permissions.
        fs::set_permissions(&unreadable, permissions).unwrap();
        return;
    }
    let (result, events) = run(config).await;
    fs::set_permissions(&unreadable, permissions).unwrap();

    assert_discovery_failure(result, &events, &data, &unreadable);
    assert_eq!(
        fs::read(output.join("conversion-manifest.json")).unwrap(),
        manifest
    );
}

#[tokio::test]
async fn unreadable_staged_vfs_subtree_stops_conversion_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("modern");
    let staging = directory.path().join("modern.staging-test");
    fs::create_dir(&data).unwrap();
    let mut config = PipelineConfig::new(&data, &output);
    config.cpu_jobs = 2;
    assert!(run(config.clone()).await.0.unwrap().complete);
    let manifest = fs::read(output.join("conversion-manifest.json")).unwrap();

    // A header-only plugin reaches the Database progress event after the loose overlay.
    let mut plugin = [0; 24];
    plugin[..4].copy_from_slice(b"TES4");
    fs::write(data.join("Skyrim.esm"), plugin).unwrap();
    fs::create_dir(data.join("textures")).unwrap();
    fs::write(data.join("textures/hidden.dds"), b"hidden input").unwrap();
    fs::create_dir(&staging).unwrap();
    config.resume_staging = Some(staging.clone());

    // The Discovering event fills this channel. The Database send then suspends
    // the pipeline, so permissions change after overlay and before VFS discovery.
    let (tx, rx) = mpsc::channel(1);
    let mut pipeline = Box::pin(AssetPipeline::run_async(config, tx));
    assert!(
        pipeline
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    let vfs = staging.join("vfs");
    let unreadable = vfs.join("textures");
    assert!(unreadable.join("hidden.dds").is_file());
    let permissions = fs::metadata(&unreadable).unwrap().permissions();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o111)).unwrap();
    if fs::read_dir(&unreadable).is_ok() {
        fs::set_permissions(&unreadable, permissions).unwrap();
        return;
    }
    let progress = tokio::spawn(collect(rx));
    let result = pipeline.as_mut().await;
    // Drop the completed future's sender before waiting for the channel to close.
    drop(pipeline);
    let events = progress.await.unwrap();
    fs::set_permissions(&unreadable, permissions).unwrap();
    assert_discovery_failure(result, &events, &vfs, &unreadable);
    assert!(staging.join("skyrim_world.db").is_file());
    assert_eq!(
        fs::read(output.join("conversion-manifest.json")).unwrap(),
        manifest
    );
}
