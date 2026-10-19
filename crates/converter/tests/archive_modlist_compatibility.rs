//! Synthetic archive compatibility and recovery through real pack publication.
use converter::{AssetPipeline, PipelineConfig, cache::ConversionManifest, config::RecordReader};
use dummy_content::{Entry, bsa, layout};
use rusqlite::Connection;
use std::{fs, path::Path};

/// Generate native synthetic assets and independent neighboring static records.
fn generate(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let plugin_path = data.join("Skyrim.esm");
    let mut plugin = fs::read(&plugin_path).unwrap();
    for (id, name) in [
        (0x900u32, "ArchiveNeighborBefore"),
        (0x902, "ArchiveNeighborAfter"),
    ] {
        let mut payload = Vec::new();
        for (tag, value) in [(b"EDID", name), (b"MODL", layout::GENERATED_MODEL_PATH)] {
            payload.extend_from_slice(tag);
            payload.extend_from_slice(&((value.len() + 1) as u16).to_le_bytes());
            payload.extend_from_slice(value.as_bytes());
            payload.push(0);
        }
        plugin.extend_from_slice(b"STAT");
        plugin.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        plugin.extend_from_slice(&0u32.to_le_bytes());
        plugin.extend_from_slice(&id.to_le_bytes());
        plugin.extend_from_slice(&[0; 4]);
        plugin.extend_from_slice(&44u16.to_le_bytes());
        plugin.extend_from_slice(&[0; 2]);
        plugin.extend_from_slice(&payload);
    }
    fs::write(plugin_path, plugin).unwrap();
}

/// Publish with the selected native reader while permitting unusable archives to be skipped.
async fn publish(data: &Path, output: &Path) -> (converter::PipelineReport, PipelineConfig) {
    let mut config = PipelineConfig::new(data, output);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.fail_fast = false;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config.clone(), tx).await.unwrap();
    drain.await.unwrap();
    assert!(output.join("conversion-manifest.json").is_file());
    (report, config)
}

/// Compare decompressed VFS source bytes retained in the public ingestion cache.
fn assert_retained(
    output: &Path,
    config: &PipelineConfig,
    archive: &str,
    path: &str,
    expected: &[u8],
) {
    let manifest = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
    let entries = manifest
        .archives
        .get(archive)
        .unwrap_or_else(|| panic!("missing archive {archive}"));
    let entry = entries
        .files
        .iter()
        .find(|entry| entry.path == path)
        .unwrap_or_else(|| panic!("missing decoded asset {path} from {archive}"));
    let blob = config
        .ingestion_cache_dir()
        .join(".ingestion-cache/sha256")
        .join(&entry.hash[..2])
        .join(&entry.hash);
    assert_eq!(
        fs::read(blob).unwrap(),
        expected,
        "decoded payload for {path}"
    );
}

/// The advisory file category bits must not reject structurally valid legacy or SSE archives.
#[tokio::test]
async fn advisory_file_flags_preserve_v104_and_v105_decoded_assets() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    let output = temp.path().join("published");
    generate(&data);
    let mesh = fs::read(data.join(layout::GENERATED_MODEL_PATH)).unwrap();
    let legacy_note = b"legacy archive asymmetric payload 104";
    let sse_note = b"SSE archive distinct payload 105, longer than legacy";
    for (version, flags, note) in [
        (104, 0xfecb_01ffu32, legacy_note.as_slice()),
        (105, 0x4000_0000, sse_note.as_slice()),
    ] {
        let mesh_path = format!("meshes/archive{version}.nif");
        let note_path = format!("docs/archive{version}.txt");
        let entries = [Entry::new(&mesh_path, &mesh), Entry::new(&note_path, note)];
        let mut archive = if version == 104 {
            bsa::v104(&entries, bsa::Compression::Zlib).unwrap()
        } else {
            bsa::v105(&entries, bsa::Compression::Lz4).unwrap()
        };
        // This independent header mutation changes only the advisory file categories.
        archive[32..36].copy_from_slice(&flags.to_le_bytes());
        fs::write(data.join(format!("assets{version}.bsa")), archive).unwrap();
    }
    let (report, config) = publish(&data, &output).await;
    for (version, note) in [(104, legacy_note.as_slice()), (105, sse_note.as_slice())] {
        let archive = format!("assets{version}.bsa");
        assert_retained(
            &output,
            &config,
            &archive,
            &format!("meshes/archive{version}.nif"),
            &mesh,
        );
        assert_retained(
            &output,
            &config,
            &archive,
            &format!("docs/archive{version}.txt"),
            note,
        );
    }
    assert!(report.complete, "{:?}", report.warnings);
}

/// A truly truncated archive must not prevent publication of a valid archive and plugin neighbors.
#[tokio::test]
async fn malformed_archive_keeps_valid_archive_and_native_plugin_neighbors() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    let output = temp.path().join("published");
    generate(&data);
    let good = b"usable archive neighbor payload";
    fs::write(
        data.join("good.bsa"),
        bsa::v105(&[Entry::new("docs/good.txt", good)], bsa::Compression::Lz4).unwrap(),
    )
    .unwrap();
    let mut broken = bsa::v104(
        &[Entry::new("docs/broken.txt", b"unrecoverable")],
        bsa::Compression::None,
    )
    .unwrap();
    // Preserve the header counts but remove every folder/file table and payload byte.
    broken.truncate(36);
    fs::write(data.join("broken.bsa"), broken).unwrap();
    let (report, config) = publish(&data, &output).await;
    assert_retained(&output, &config, "good.bsa", "docs/good.txt", good);
    let manifest = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
    assert!(!manifest.archives.contains_key("broken.bsa"));
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.to_ascii_lowercase().contains("broken.bsa")),
        "missing malformed archive diagnostic: {:?}",
        report.warnings
    );
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    for (id, name) in [
        (0x900u32, "ArchiveNeighborBefore"),
        (0x902, "ArchiveNeighborAfter"),
    ] {
        let retained: (String, String) = db
            .query_row(
                "SELECT editor_id, model_path FROM statics WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            retained,
            (name.to_owned(), layout::GENERATED_MODEL_PATH.to_owned())
        );
        let count: u32 = db
            .query_row(
                "SELECT count(*) FROM records WHERE form_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    // A skipped archive may mark the published pack partial; usable content must still survive.
}
