//! Packed normal cache upgrades retain usable neighbouring assets after a bad model.
use converter::{
    AssetPipeline, PipelineConfig,
    cache::{CONVERTER_SCHEMA_VERSION, ConversionManifest, hash_bytes, hash_file},
    texture::{TextureConverter, TextureEncoding},
};
use dummy_content::layout;
use std::{fs, path::Path};

/// Publish through the ordinary pipeline, draining bounded progress messages.
async fn publish(config: &PipelineConfig) -> converter::pipeline::PipelineReport {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config.clone(), sender).await;
    drain.await.unwrap();
    assert!(
        result.is_ok(),
        "publication must retain valid neighbours: {result:?}"
    );
    result.unwrap()
}

/// Read the actual saved output proof, without its load-time compatibility filtering.
fn manifest(output: &Path) -> ConversionManifest {
    serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap()).unwrap()
}

/// Recreate the previous DDS recipe independently, before the lossless normal contract.
fn previous_texture_proof(config: &PipelineConfig) -> String {
    let recipe = serde_json::json!({
        "texture_producer": CONVERTER_SCHEMA_VERSION,
        "texture_etc1s_quality": config.texture_fallback_quality,
        "texture_uastc_level": config.texture_uastc_level,
        "texture_zstd_level": config.texture_zstd_level,
        "texture_encoder": {"mode":"cpu"},
    });
    hash_bytes(&serde_json::to_vec(&recipe).unwrap())
}

#[tokio::test]
async fn warm_packed_normal_rebuilds_old_bc7_and_skips_bad_model() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,pex,nif,esm").unwrap(),
    )
    .unwrap();
    let mut dds = ddsfile::Dds::new_d3d(ddsfile::NewD3dParams {
        width: 4,
        height: 4,
        depth: None,
        format: ddsfile::D3DFormat::A8R8G8B8,
        mipmap_levels: Some(3),
        caps2: None,
    })
    .unwrap();
    dds.data.clear();
    for side in [4, 2, 1] {
        for index in 0..side * side {
            let [r, g, b, a] = if index % 2 == 0 {
                [224, 48, 196, 255]
            } else {
                [32, 224, 160, 255]
            };
            dds.data.extend_from_slice(&[b, g, r, a]);
        }
    }
    let mut source = Vec::new();
    dds.write(&mut source).unwrap();
    fs::write(data.join(layout::GENERATED_NORMAL_PATH), &source).unwrap();
    let output = root.path().join("modern");
    let mut config = PipelineConfig::new(&data, &output);
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    assert!(publish(&config).await.complete);
    let mut saved = manifest(&output);
    let neighbour = saved
        .entries
        .values()
        .find(|entry| entry.output.ends_with(".glb"))
        .unwrap()
        .output
        .clone();
    let neighbour_hash = hash_file(&output.join(&neighbour)).unwrap();
    let old = TextureConverter::convert(&source, TextureEncoding::DataLinear).unwrap();
    assert_eq!(
        ktx2::Reader::new(&old).unwrap().header().format,
        Some(ktx2::Format::BC7_UNORM_BLOCK)
    );
    let mut old_outputs = Vec::new();
    for (key, entry) in &mut saved.entries {
        if key.starts_with(layout::GENERATED_NORMAL_PATH) {
            fs::write(output.join(&entry.output), &old).unwrap();
            entry.output_size = old.len() as u64;
            entry.output_hash = hash_bytes(&old);
            old_outputs.push(entry.output.clone());
        }
    }
    assert!(
        !old_outputs.is_empty(),
        "fixture must carry a normal texture entry"
    );
    saved
        .asset_configuration_hashes
        .insert("dds".into(), previous_texture_proof(&config));
    saved
        .save(&output.join("conversion-manifest.json"))
        .unwrap();
    // A new optional bad model must not abort publication of the upgraded normal or healthy mesh.
    fs::write(data.join("meshes/bad_optional.nif"), b"broken optional NIF").unwrap();
    let report = publish(&config).await;
    assert!(
        !report.complete,
        "skipped model must remain visible as incomplete data"
    );
    let saved = manifest(&output);
    assert!(saved.failures.contains_key("meshes/bad_optional.nif"));
    assert_eq!(hash_file(&output.join(neighbour)).unwrap(), neighbour_hash);
    for name in old_outputs {
        let bytes = fs::read(output.join(name)).unwrap();
        assert_eq!(
            ktx2::Reader::new(&bytes).unwrap().header().format,
            Some(ktx2::Format::R8G8B8A8_UNORM),
            "old BC7 normal must rebuild, not warm-reuse"
        );
    }
}
