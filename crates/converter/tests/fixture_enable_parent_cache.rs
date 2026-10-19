//! A database-only producer migration preserves assets only with their original proof.

use converter::{
    AssetPipeline, PipelineConfig,
    cache::{
        CONVERTER_SCHEMA_VERSION, ConversionManifest, configuration_hash_for_schema, hash_file,
    },
};
use dummy_content::layout;
use std::{fs, path::Path};

/// Publish synthetic assets through the real pipeline and require complete output.
async fn convert(config: PipelineConfig) -> converter::PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    report
}

/// Read the producer and per-asset hashes actually published by a conversion.
fn manifest(output: &Path) -> ConversionManifest {
    serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap()).unwrap()
}

/// Producer 27 changed the GLB collision data: producer-24 and producer-26 meshes and all old
/// textures rebuild, while proven scripts are kept. Configuration and script source
/// changes still force regeneration instead of being hidden by migration.
#[tokio::test]
async fn earlier_producer_meshes_regenerate_and_scripts_reuse_only_with_valid_proof() {
    for (producer, changed) in [24, 26].into_iter().flat_map(|producer| {
        [
            "none",
            "configuration",
            "source",
            "texture-source",
            "output",
        ]
        .map(|changed| (producer, changed))
    }) {
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
        let mut config = PipelineConfig::new(&data, &output);
        convert(config.clone()).await;
        let mut original = manifest(&output);
        let mesh = original
            .entries
            .values()
            .find(|entry| entry.output.ends_with(".glb"))
            .unwrap();
        let mesh_path = output.join(&mesh.output);
        let original_mesh = fs::read(&mesh_path).unwrap();
        original.schema_version = producer;
        original.asset_configuration_hashes.clear();
        original.configuration_hash = configuration_hash_for_schema(&config, producer).unwrap();
        original
            .save(&output.join("conversion-manifest.json"))
            .unwrap();
        match changed {
            "configuration" => config.texture_zstd_level = 0,
            "source" => fs::write(
                data.join("scripts/generated.pex"),
                dummy_content::pex::minimal("ChangedSource").unwrap(),
            )
            .unwrap(),
            "texture-source" => {
                let path = data.join("textures/generated_color.dds");
                let mut bytes = fs::read(&path).unwrap();
                *bytes.last_mut().unwrap() ^= 0x5A;
                fs::write(path, bytes).unwrap();
            }
            "output" => fs::write(&mesh_path, b"damaged mesh").unwrap(),
            "none" => {}
            _ => unreachable!(),
        }
        let regenerated = original
            .entries
            .values()
            .filter(|entry| entry.output.ends_with(".ktx2") || entry.output.ends_with(".glb"))
            .count() as u64;
        let report = convert(config).await;
        let current = manifest(&output);
        assert_eq!(current.schema_version, CONVERTER_SCHEMA_VERSION);
        assert_eq!(current.retained_mesh_schema_version, None);
        assert_eq!(fs::read(&mesh_path).unwrap(), original_mesh);
        for entry in current.entries.values() {
            assert_eq!(
                hash_file(&output.join(&entry.output)).unwrap(),
                entry.output_hash
            );
        }
        match changed {
            "none" => {
                assert_eq!(report.converted, regenerated, "producer {producer}");
                assert_eq!(
                    report.cache_hits,
                    original.entries.len() as u64 - regenerated
                );
                assert_eq!(current.entries, original.entries);
            }
            "configuration" => {
                assert_eq!(report.cache_hits, 0);
                assert_eq!(report.converted, original.entries.len() as u64);
            }
            "source" | "texture-source" | "output" => {
                // Only the script source change adds a conversion; mesh and texture
                // changes land in outputs that regenerate anyway.
                let converted = regenerated + u64::from(changed == "source");
                assert_eq!(report.converted, converted, "{changed} {producer}");
                assert_eq!(report.cache_hits, original.entries.len() as u64 - converted);
            }
            _ => unreachable!(),
        }
    }
}
