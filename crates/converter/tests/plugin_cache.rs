//! Plugin-only changes rebuild world data while preserving verified asset reuse.
use converter::{
    AssetPipeline, PipelineConfig,
    cache::{ConversionManifest, configuration_hash_for_schema, hash_bytes},
};
use dummy_content::layout;
use rusqlite::Connection;
use std::fs;

/// Compare with the pre-PR configuration contract for every supported schema.
#[test]
fn plugin_repairs_preserve_historical_asset_configuration_hashes() {
    let config = PipelineConfig::new("synthetic-data", "synthetic-output");
    for schema in 12..=16 {
        let mut historical = serde_json::json!({
            "schema": schema,
            "texture_etc1s_quality": config.texture_fallback_quality,
            "texture_uastc_level": config.texture_uastc_level,
            "script_abi_version": config.script_abi_version,
        });
        if schema >= 16 {
            historical["texture_zstd_level"] = serde_json::json!(config.texture_zstd_level);
        }
        assert_eq!(
            configuration_hash_for_schema(&config, schema).unwrap(),
            hash_bytes(&serde_json::to_vec(&historical).unwrap())
        );
    }
}

/// Convert a generated pack with a drain so progress never blocks the worker.
async fn convert(config: PipelineConfig) -> converter::PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    result
}

#[tokio::test]
async fn plugin_changes_rebuild_the_database_without_reconverting_unchanged_assets() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    let output = dir.path().join("modern");
    let config = PipelineConfig::new(&data, &output);
    let first = convert(config.clone()).await;
    assert!(first.converted > 0);
    assert_eq!(first.cache_hits, 0);
    let mesh = fs::read(output.join("meshes/generated.glb")).unwrap();
    let texture = fs::read(output.join("textures/generated_color.ktx2")).unwrap();

    // Change only a game setting in the source plugin; the world database is
    // rebuilt each run and must observe it even though asset hashes are unchanged.
    let mut plugin = fs::read(data.join("Skyrim.esm")).unwrap();
    let name = b"fJumpHeightMin\0";
    let mut payload = [b"EDID".as_slice(), &(name.len() as u16).to_le_bytes(), name].concat();
    payload.extend(b"DATA\x04\x00");
    payload.extend(123.0f32.to_le_bytes());
    plugin.extend(b"GMST");
    plugin.extend((payload.len() as u32).to_le_bytes());
    plugin.extend(0u32.to_le_bytes());
    plugin.extend(0x900u32.to_le_bytes());
    plugin.extend([0; 8]);
    plugin.extend(payload);
    fs::write(data.join("Skyrim.esm"), plugin).unwrap();

    let second = convert(config).await;
    assert_eq!(second.converted, 0);
    assert_eq!(second.cache_hits, first.converted);
    assert_eq!(second.skipped, 0);
    assert_eq!(fs::read(output.join("meshes/generated.glb")).unwrap(), mesh);
    assert_eq!(
        fs::read(output.join("textures/generated_color.ktx2")).unwrap(),
        texture
    );
    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get::<_, f64>(0)
        )
        .unwrap(),
        123.0
    );
    assert!(
        ConversionManifest::load(&output.join("conversion-manifest.json"))
            .unwrap()
            .complete
    );
}
