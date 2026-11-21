//! A plugin that fails its discovery hash keeps the reader's decoded digest in a real run.
//! Admission reads the header, then the source is renamed away so no file proof is recorded.
//! The conversion hook restores it before decoding, so a later readable change must hit the
//! mixed-generation refusal of the post-export recheck instead of being ignored.
use super::*;

fn small_plugin(id: u32) -> Vec<u8> {
    let header = [
        1.7f32.to_le_bytes().to_vec(),
        1u32.to_le_bytes().to_vec(),
        0x1000u32.to_le_bytes().to_vec(),
    ]
    .concat();
    let sub = |tag: &[u8; 4], bytes: &[u8]| {
        [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
    };
    let record = |tag: &[u8; 4], id: u32, payload: Vec<u8>| {
        [
            tag.as_slice(),
            &(payload.len() as u32).to_le_bytes(),
            &0u32.to_le_bytes(),
            &id.to_le_bytes(),
            &[0; 8],
            &payload,
        ]
        .concat()
    };
    let mut fields = sub(b"HEDR", &header);
    fields.extend(sub(b"MAST", b"Skyrim.esm\0"));
    fields.extend(sub(b"DATA", &[0; 8]));
    let mut bytes = record(b"TES4", 0, fields);
    bytes.extend(record(b"KYWD", id, sub(b"EDID", b"DecodedDigestKeyword\0")));
    bytes
}

struct ClearHook;
impl Drop for ClearHook {
    fn drop(&mut self) {
        crate::esm::AFTER_PLUGIN_ADMISSION.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Remove the source after admission, then restore it just before the reader opens it.
fn arm_recovery(plugin: &Path, retained: &Path) -> ClearHook {
    let missing = plugin.to_path_buf();
    let backup = retained.to_path_buf();
    crate::esm::AFTER_PLUGIN_ADMISSION.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(move || {
            fs::rename(&missing, &backup).unwrap();
            let (from, to) = (backup.clone(), missing.clone());
            // The conversion calls this again after hashing and before decoding.
            crate::esm::AFTER_PLUGIN_ADMISSION.with(|slot| {
                assert!(slot.borrow().is_none());
                *slot.borrow_mut() = Some(Box::new(move || fs::rename(&from, &to).unwrap()));
            });
        }));
    });
    ClearHook
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("modern");
    dummy_content::layout::generate(
        &data,
        dummy_content::layout::DEFAULT_SEED,
        dummy_content::layout::Formats::parse("dds,nif").unwrap(),
    )
    .unwrap();
    fs::write(
        data.join("Skyrim.esm"),
        dummy_content::esm::plugin(&dummy_content::esm::Plugin {
            author: "Decoded digest wiring",
            worldspace: "DecodedDigestWorld",
            cells: &[dummy_content::esm::Cell {
                grid_x: 0,
                grid_y: 0,
            }],
            model_path: dummy_content::layout::GENERATED_MODEL_PATH,
            diffuse: dummy_content::layout::GENERATED_DIFFUSE_PATH,
            normal_texture: dummy_content::layout::GENERATED_NORMAL_PATH,
        })
        .unwrap(),
    )
    .unwrap();
    let plugin = data.join("MutableMod.esp");
    fs::write(&plugin, small_plugin(0x0100_0822)).unwrap();
    let retained = directory.path().join("MutableMod.retained");
    (directory, data, output, plugin, retained)
}

fn config(data: &Path, output: &Path) -> PipelineConfig {
    let mut config = PipelineConfig::new(data, output);
    config.lod_texture_encoder = TextureEncoder::Cpu;
    config.cpu_jobs = 2;
    config.terrain_default_texture = crate::config::TerrainDefaultTexture::White;
    config
        .lod_origins
        .insert("DecodedDigestWorld".into(), [0, 0]);
    config
}

/// Apply one source change when LOD compilation starts: after the database proof, before recheck.
async fn run(
    config: PipelineConfig,
    change: Option<(PathBuf, Vec<u8>)>,
) -> (Result<PipelineReport, PipelineFailure>, bool) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ProgressEvent>(64);
    let watcher = tokio::spawn(async move {
        let mut applied = false;
        while let Some(event) = rx.recv().await {
            if !applied
                && let Some((path, bytes)) = &change
                && event.stage == ProgressStage::LodChunks
                && event.message.contains("Compiling terrain LOD chunks")
            {
                fs::write(path, bytes).unwrap();
                applied = true;
            }
        }
        applied
    });
    let result = AssetPipeline::run_async(config, tx).await;
    let applied = watcher.await.unwrap();
    (result, applied)
}

#[tokio::test(flavor = "current_thread")]
async fn recovered_plugin_digest_survives_the_real_run_and_rejects_a_later_change() {
    let (_directory, data, output, plugin, retained) = fixture();
    let changed = small_plugin(0x0100_0823);
    let hook = arm_recovery(&plugin, &retained);
    let (result, applied) = run(
        config(&data, &output),
        Some((plugin.clone(), changed.clone())),
    )
    .await;
    drop(hook);
    assert!(applied, "LOD compilation start was never observed");
    assert_eq!(fs::read(&plugin).unwrap(), changed);
    let error = result.expect_err("the recovered digest must catch a later readable change");
    assert!(
        format!("{error:#}").contains("mixed input generation"),
        "{error:#}"
    );
}
