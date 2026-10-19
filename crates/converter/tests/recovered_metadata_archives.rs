//! Metadata rebuilds must not reactivate companions excluded by plugin recovery.

use converter::{AssetPipeline, PipelineConfig, PipelineReport, cache::hash_file};
use dummy_content::{Entry, bsa, layout, pex};
use rusqlite::Connection;
use std::{fs, path::Path};

/// Encode a complete native header independently of archive ownership selection.
fn plugin(masters: &[&str]) -> Vec<u8> {
    let mut payload = Vec::new();
    for master in masters {
        for (tag, bytes) in [
            (b"MAST", format!("{master}\0").into_bytes()),
            (b"DATA", vec![0; 8]),
        ] {
            payload.extend_from_slice(tag);
            payload.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
            payload.extend_from_slice(&bytes);
        }
    }
    [
        b"TES4".as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &[0; 16],
        &payload,
    ]
    .concat()
}

/// Generate complete, manifest-backed model, texture and script families for metadata reuse.
fn source_inputs(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,esm").unwrap(),
    )
    .unwrap();
    let script = pex::minimal("SurvivingArchive").unwrap();
    fs::write(
        data.join("Skyrim.bsa"),
        bsa::v105(
            &[Entry::new("scripts/survivor.pex", &script)],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
}

/// Use matching producer settings for the original package and isolated metadata output.
fn config(data: &Path, output: &Path) -> PipelineConfig {
    let mut config = PipelineConfig::new(data, output);
    config.no_lod = true;
    config.cpu_jobs = 2;
    config
}

/// Publish through the production conversion path and drain its progress channel.
async fn convert(data: &Path, output: &Path) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config(data, output), tx).await;
    drain.await.unwrap();
    let report = result.unwrap();
    assert!(report.complete, "{report:?}");
    assert_eq!(report.skipped, 0, "{report:?}");
    report
}

/// Rebuild from the package's recorded plugin order, without a fresh player or MO2 selection.
async fn rebuild(data: &Path, source: &Path, output: &Path) -> color_eyre::Result<PipelineReport> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::rebuild_metadata_async(config(data, output), source, tx).await;
    drain.await.unwrap();
    result
}

/// Inspect the published namespace rather than deriving it from the fixture's source filenames.
fn plugin_names(output: &Path) -> Vec<String> {
    let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
    let mut statement = connection
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The normal pipeline already excluded this malformed companion; metadata must not read it.
#[tokio::test]
async fn metadata_rebuild_does_not_read_recovered_away_specific_owner_archive() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    source_inputs(&data);
    fs::write(data.join("Skyrim - Broken.esp"), plugin(&["Missing.esm"])).unwrap();
    let ignored_archive = data.join("Skyrim - Broken.bsa");
    fs::write(&ignored_archive, b"ignored malformed companion archive").unwrap();

    let report = convert(&data, &source).await;
    assert!(
        report.notices.iter().any(|notice| {
            notice.contains("skipping Skyrim - Broken.esp:") && notice.contains("Missing.esm")
        }),
        "{report:?}"
    );
    assert_eq!(plugin_names(&source), ["Skyrim.esm"]);
    let source_manifest = fs::read(source.join("conversion-manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&source_manifest).unwrap();
    let archives: Vec<_> = manifest["archives"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(archives, ["skyrim.bsa"]);
    assert!(source.join("meshes/generated.glb").is_file());
    assert!(source.join("textures/generated_color.ktx2").is_file());
    let source_database_hash = hash_file(&source.join("skyrim_world.db")).unwrap();
    let retained_hash = hash_file(&source.join("scripts/survivor.luau")).unwrap();
    let ignored_hash = hash_file(&ignored_archive).unwrap();

    let result = rebuild(&data, &source, &output).await;
    let rebuilt = result.unwrap_or_else(|error| {
        panic!("metadata reactivated a recovered-away installed archive: {error:?}")
    });
    assert!(rebuilt.complete, "{rebuilt:?}");
    assert_eq!(plugin_names(&output), ["Skyrim.esm"]);
    assert_eq!(
        hash_file(&output.join("scripts/survivor.luau")).unwrap(),
        retained_hash
    );
    assert_eq!(
        fs::read(source.join("conversion-manifest.json")).unwrap(),
        source_manifest
    );
    assert_eq!(
        hash_file(&source.join("skyrim_world.db")).unwrap(),
        source_database_hash
    );
    assert_eq!(hash_file(&ignored_archive).unwrap(), ignored_hash);
}

/// The ownership guard must still read admitted archives and preserve strict extraction errors.
#[tokio::test]
async fn metadata_rebuild_still_rejects_malformed_surviving_owner_archive() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    source_inputs(&data);
    convert(&data, &source).await;
    let source_manifest = fs::read(source.join("conversion-manifest.json")).unwrap();
    let retained_hash = hash_file(&source.join("scripts/survivor.luau")).unwrap();
    fs::write(
        data.join("Skyrim.bsa"),
        b"malformed active companion archive",
    )
    .unwrap();

    let error = rebuild(&data, &source, &output).await.unwrap_err();
    let diagnostic = format!("{error:?}");
    assert!(
        diagnostic.contains("unsupported archive magic"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("Skyrim.bsa"), "{diagnostic}");
    assert!(!output.exists());
    assert_eq!(
        fs::read(source.join("conversion-manifest.json")).unwrap(),
        source_manifest
    );
    assert_eq!(
        hash_file(&source.join("scripts/survivor.luau")).unwrap(),
        retained_hash
    );
}
