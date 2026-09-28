//! A conversion stores each extracted archive entry once: in the staging workspace, a `vfs`
//! file and its content-addressed blob in `.ingestion-cache` are one file, not two copies of its
//! bytes. Neither ships in the published runtime pack.

use converter::PipelineConfig;
use std::{fs, io::Write};

#[tokio::test]
async fn staged_vfs_entries_are_one_file_with_their_cache_blobs() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    dummy_content::layout::prepare_directory(&data, false).unwrap();
    // Archives only. A loose copy of an extracted path replaces the `vfs` entry with a copy of
    // its own bytes (`overlay_loose_assets`), which is the override path, not the storage one.
    dummy_content::layout::generate(
        &data,
        dummy_content::layout::DEFAULT_SEED,
        dummy_content::layout::Formats {
            dds: false,
            pex: false,
            nif: false,
            bsa: true,
            ba2: true,
            esm: true,
        },
    )
    .unwrap();

    let output = directory.path().join("modern");
    // Resumed staging survives publication, so the workspace links stay inspectable.
    let staging = directory.path().join("modern.staging-linked");
    std::fs::create_dir_all(&staging).unwrap();
    let mut config = PipelineConfig::new(&data, &output);
    config.resume_staging = Some(staging.clone());
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = converter::AssetPipeline::run_async(config, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "the fixture conversion did not complete");

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    // On a filesystem without hard links the converter copies by design (`link_or_copy`), so only
    // equal bytes can be asked of the two names there.
    let links = hard_links_supported(&output);
    let mut entries = 0;
    for archive in manifest["archives"].as_object().unwrap().values() {
        for file in archive["files"].as_array().unwrap() {
            let path = file["path"].as_str().unwrap();
            let hash = file["hash"].as_str().unwrap();
            let vfs = staging.join("vfs").join(path);
            let blob = staging
                .join(".ingestion-cache/sha256")
                .join(&hash[..2])
                .join(hash);
            if !links {
                assert_eq!(fs::read(&vfs).unwrap(), fs::read(&blob).unwrap());
                entries += 1;
                continue;
            }
            // A byte appended through the `vfs` name has to appear in the blob: a copy, which is
            // what the converter stored before, would not see it.
            fs::OpenOptions::new()
                .append(true)
                .open(&vfs)
                .unwrap()
                .write_all(b"+")
                .unwrap();
            assert!(
                fs::read(&blob).unwrap().last() == Some(&b'+'),
                "{} and {} are not one file",
                vfs.display(),
                blob.display()
            );
            entries += 1;
        }
    }
    assert!(entries > 0, "no archive entry was extracted");
    assert!(
        !output.join("vfs").exists(),
        "the runtime pack must not ship the staging workspace"
    );
    assert!(
        !output.join(".ingestion-cache").exists(),
        "the runtime pack must not ship the ingestion cache"
    );
}

/// Whether the filesystem holding `directory` can hard-link.
fn hard_links_supported(directory: &std::path::Path) -> bool {
    let probe = directory.join(".link-probe");
    let link = directory.join(".link-probe-link");
    fs::write(&probe, b"probe").unwrap();
    let supported = fs::hard_link(&probe, &link).is_ok();
    let _ = fs::remove_file(&link);
    fs::remove_file(&probe).unwrap();
    supported
}
