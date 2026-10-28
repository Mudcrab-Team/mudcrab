//! Producer-27 texture containers regenerate under producer 28 with smallest-first level data.
//! Its meshes also regenerate because a producer-27 proof predates the named mesh contracts,
//! while scripts stay cache hits on their own proofs.

use converter::{
    AssetPipeline, PipelineConfig, PipelineReport,
    cache::{
        CONVERTER_SCHEMA_VERSION, ConversionManifest, asset_configuration_hash_for_schema,
        hash_bytes,
    },
};
use dummy_content::layout;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// The last producer whose texture containers stored level data largest-first.
const PRODUCER_27: u32 = 27;

async fn run(config: PipelineConfig) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{report:?}");
    report
}

fn manifest(output: &Path) -> ConversionManifest {
    serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap()).unwrap()
}

/// Hashes every source file so the test can prove the inputs were never written.
fn tree_hashes(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut hashes = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                hashes.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    hash_bytes(&fs::read(&path).unwrap()),
                );
            }
        }
    }
    hashes
}

/// Level index entries as `(byte_offset, byte_length, uncompressed_byte_length)`, base level first.
fn level_entries(bytes: &[u8]) -> Vec<(usize, usize, usize)> {
    let count = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
    (0..count)
        .map(|level| {
            let start = ktx2::Header::LENGTH + level * ktx2::LevelIndex::LENGTH;
            let field = |at: usize| {
                u64::from_le_bytes(bytes[start + at..start + at + 8].try_into().unwrap()) as usize
            };
            (field(0), field(8), field(16))
        })
        .collect()
}

/// Rewrites a current texture into the producer-27 container: base level first, each level at
/// a 16-byte boundary. Its level data and index entries describe the same pixels as before.
fn producer_27_texture_layout(bytes: &[u8]) -> Vec<u8> {
    let entries = level_entries(bytes);
    let first = entries.iter().map(|entry| entry.0).min().unwrap();
    let mut out = bytes[..first].to_vec();
    let mut moved = Vec::with_capacity(entries.len());
    for &(offset, length, uncompressed) in &entries {
        while !out.len().is_multiple_of(16) {
            out.push(0);
        }
        moved.push((out.len(), length, uncompressed));
        out.extend_from_slice(&bytes[offset..offset + length]);
    }
    for (level, (offset, length, uncompressed)) in moved.into_iter().enumerate() {
        let start = ktx2::Header::LENGTH + level * ktx2::LevelIndex::LENGTH;
        out[start..start + 8].copy_from_slice(&(offset as u64).to_le_bytes());
        out[start + 8..start + 16].copy_from_slice(&(length as u64).to_le_bytes());
        out[start + 16..start + 24].copy_from_slice(&(uncompressed as u64).to_le_bytes());
    }
    out
}

#[tokio::test]
async fn producer_27_texture_containers_regenerate_under_producer_28() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("modern");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let source_before = tree_hashes(&data);
    let mut config = PipelineConfig::new(&data, &output);
    config.no_lod = true;
    run(config.clone()).await;

    let current = manifest(&output);
    assert_eq!(current.schema_version, CONVERTER_SCHEMA_VERSION);
    let textures: Vec<String> = current
        .entries
        .iter()
        .filter(|(_, entry)| entry.output.ends_with(".ktx2"))
        .map(|(key, _)| key.clone())
        .collect();
    let untouched: Vec<String> = current
        .entries
        .iter()
        .filter(|(_, entry)| entry.output.ends_with(".glb") || entry.output.ends_with(".luau"))
        .map(|(key, _)| key.clone())
        .collect();
    assert!(!textures.is_empty() && !untouched.is_empty());

    let current_bytes: BTreeMap<String, Vec<u8>> = textures
        .iter()
        .map(|key| {
            let output_name = &current.entries[key].output;
            (key.clone(), fs::read(output.join(output_name)).unwrap())
        })
        .collect();
    assert!(
        current_bytes
            .values()
            .any(|bytes| level_entries(bytes).len() > 1),
        "the regression needs at least one mip chain"
    );
    for bytes in current_bytes.values() {
        let offsets: Vec<usize> = level_entries(bytes).iter().map(|entry| entry.0).collect();
        assert!(
            offsets.windows(2).all(|pair| pair[0] > pair[1]),
            "producer 28 stores the base level last: {offsets:?}"
        );
    }

    // Publish the pack as producer 27 left it: old texture bytes and 27 settings proofs. Its mesh
    // bytes match a current export only because this fixture rewrites the producer in place.
    let mut legacy = current.clone();
    legacy.schema_version = PRODUCER_27;
    let mut stale_bytes = BTreeMap::new();
    for key in &textures {
        let entry = legacy.entries.get_mut(key).unwrap();
        let stale = producer_27_texture_layout(&current_bytes[key]);
        fs::write(output.join(&entry.output), &stale).unwrap();
        entry.output_size = stale.len() as u64;
        entry.output_hash = hash_bytes(&stale);
        stale_bytes.insert(key.clone(), stale);
    }
    assert!(
        textures
            .iter()
            .any(|key| stale_bytes[key] != current_bytes[key]),
        "the old layout must differ from the current layout"
    );
    for kind in ["dds", "nif", "pex"] {
        legacy.asset_configuration_hashes.insert(
            kind.into(),
            asset_configuration_hash_for_schema(&config, PRODUCER_27, kind).unwrap(),
        );
    }
    legacy
        .save(&output.join("conversion-manifest.json"))
        .unwrap();
    let untouched_before: BTreeMap<&String, Vec<u8>> = untouched
        .iter()
        .map(|key| {
            let name = &current.entries[key].output;
            (key, fs::read(output.join(name)).unwrap())
        })
        .collect();

    let rerun = run(config.clone()).await;

    // Textures regenerate into the producer-28 layout, and meshes regenerate because the
    // producer-27 proofs cannot name the current mesh contract.
    let reused_scripts = untouched
        .iter()
        .filter(|key| current.entries[*key].output.ends_with(".luau"))
        .count();
    assert_eq!(rerun.cache_hits as usize, reused_scripts, "{rerun:?}");
    assert_eq!(
        rerun.converted as usize,
        textures.len() + untouched.len() - reused_scripts,
        "{rerun:?}"
    );
    for key in &textures {
        let name = &current.entries[key].output;
        let regenerated = fs::read(output.join(name)).unwrap();
        assert_eq!(regenerated, current_bytes[key], "{key}");
        assert_ne!(regenerated, stale_bytes[key], "{key}");
    }
    // Rebuilt meshes and reused scripts must still match the bytes the pack held.
    for key in &untouched {
        let name = &current.entries[key].output;
        assert_eq!(
            fs::read(output.join(name)).unwrap(),
            untouched_before[key],
            "{key}"
        );
    }
    let published = manifest(&output);
    assert_eq!(published.schema_version, CONVERTER_SCHEMA_VERSION);
    assert_eq!(published.retained_mesh_schema_version, None);
    assert_eq!(run(config).await.converted, 0);
    assert_eq!(
        tree_hashes(&data),
        source_before,
        "source assets must never change"
    );
}
