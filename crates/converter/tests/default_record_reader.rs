//! Ordinary API and command-line conversion use the in-house reader unless overridden.
use converter::{
    AssetPipeline, PipelineConfig, config::RecordReader, esm::exporter::validate_database,
};
use dummy_content::layout;
use std::{fs, path::Path, process::Command};

/// Generate a complete independent synthetic world and its native assets.
fn generate(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
}

/// Check actual reader provenance and useful published output rather than only the enum default.
fn assert_reader(output: &Path, expected: &str) {
    let identity: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("record-reader.json")).unwrap()).unwrap();
    assert_eq!(identity["mode"], expected);
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    validate_database(&connection).unwrap();
    let names: Vec<String> = connection
        .prepare("SELECT name FROM plugins ORDER BY priority")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(names, ["Skyrim.esm"]);
    let references: i64 = connection
        .query_row("SELECT count(*) FROM \"references\"", [], |row| row.get(0))
        .unwrap();
    assert!(
        references > 0,
        "the selected reader must publish the fixture world"
    );
    assert!(output.join("meshes/generated.glb").is_file());
    assert_eq!(
        output.join("inhouse-reader-diagnostics.json").is_file(),
        expected == "inhouse"
    );
}

/// The constructor used by the launcher publishes an in-house pack without an explicit reader.
#[tokio::test]
async fn ordinary_pipeline_publishes_inhouse_by_default() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("pack");
    generate(&data);
    let mut config = PipelineConfig::new(&data, &output);
    assert_eq!(config.record_reader, RecordReader::Inhouse);
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, sender).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{report:?}");
    assert_eq!(report.load_order, ["Skyrim.esm"]);
    assert_reader(&output, "inhouse");
}

/// No reader flag selects in-house; an explicit legacy flag still runs the legacy frontend.
#[test]
fn cli_default_and_explicit_legacy_publish_the_requested_frontend() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    generate(&data);
    for (name, override_reader) in [("inhouse", false), ("legacy", true)] {
        let output = directory.path().join(name);
        let mut command = Command::new(env!("CARGO_BIN_EXE_converter"));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command
            .arg(&data)
            .arg(&output)
            .args(["--no-lod", "--cpu-jobs", "2", "--io-jobs", "1"]);
        if override_reader {
            command.args(["--record-reader", "legacy"]);
        }
        let run = command.output().unwrap();
        assert!(
            run.status.success(),
            "{name}: stdout={} stderr={}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_reader(&output, name);
    }
}
