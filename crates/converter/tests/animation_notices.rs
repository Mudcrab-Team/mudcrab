use converter::{
    AssetPipeline, PipelineConfig, PipelineReport,
    cache::{ConversionManifest, hash_bytes},
};
use dummy_content::{
    layout,
    nif::{FloatController, StaticShape, effect_shape_with_controllers},
};
use std::{fs, path::Path};

fn generated_data(root: &Path, malformed: bool) {
    layout::prepare_directory(root, false).unwrap();
    layout::generate(
        root,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,esm").unwrap(),
    )
    .unwrap();
    let shape = StaticShape {
        name: "GeneratedAnimatedShape",
        positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: &[[0.0, 0.0, 1.0]; 3],
        uvs: &[[0.0, 0.0]; 3],
        indices: &[[0, 1, 2]],
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    };
    let good = [[0.0, 0.0, 0.0, 0.0], [1.0, 1.0, 0.0, 0.0]];
    let bad = [[0.0, f32::NAN, 0.0, 0.0], [1.0, 1.0, 0.0, 0.0]];
    let controller = |keys| FloatController {
        flags: 0x48,
        frequency: 1.0,
        phase: 0.0,
        start_time: 0.0,
        stop_time: 1.0,
        variable: 8,
        key_type: 1,
        keys,
    };
    let controllers = if malformed {
        vec![controller(&good), controller(&bad)]
    } else {
        vec![controller(&good)]
    };
    let path = root.join(layout::GENERATED_MODEL_PATH);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        effect_shape_with_controllers(&shape, &controllers).unwrap(),
    )
    .unwrap();
}

fn config(data: &Path, output: &Path) -> PipelineConfig {
    let mut config = PipelineConfig::new(data, output);
    config.no_lod = true;
    config
}

async fn run(config: PipelineConfig) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    report
}

fn document(bytes: &[u8]) -> serde_json::Value {
    let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    serde_json::from_slice(&bytes[20..20 + length]).unwrap()
}

fn manifest(output: &Path) -> ConversionManifest {
    serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn asset_only_material_animation_keeps_payload_without_dataset_capability() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("output");
    generated_data(&data, false);
    fs::remove_file(data.join("Skyrim.esm")).unwrap();
    for cached in [false, true] {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let result = AssetPipeline::run_async(config(&data, &output), tx).await;
        drain.await.unwrap();
        assert!(
            result.is_ok(),
            "asset-only animation must publish: {result:?}"
        );
        let report = result.unwrap();
        assert!(report.complete, "{:?}", report.warnings);
        assert!(report.load_order.is_empty());
        assert!(!output.join("skyrim_world.db").exists());
        let manifest = manifest(&output);
        assert!(manifest.complete);
        assert!(manifest.extensions.is_empty());
        let json = document(&fs::read(output.join("meshes/generated.glb")).unwrap());
        assert_eq!(
            json["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"]["channels"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        if cached {
            assert_eq!(report.converted, 0);
            assert!(report.cache_hits > 0);
        }
    }
}

#[tokio::test]
async fn malformed_optional_animation_keeps_complete_capability_on_fresh_and_cached_runs() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("output");
    generated_data(&data, true);
    let first = run(config(&data, &output)).await;
    assert!(first.complete, "{:?}", first.warnings);
    assert!(first.warnings.is_empty());
    assert_eq!(first.skipped, 0);
    assert_eq!(
        first.animation_skipped_channels.get("non-finite key data"),
        Some(&1)
    );
    assert!(!first.animation_notices.is_empty());
    let before = fs::read(output.join("meshes/generated.glb")).unwrap();
    let json = document(&before);
    assert_eq!(
        json["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"]["channels"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let second = run(config(&data, &output)).await;
    assert!(second.complete, "{:?}", second.warnings);
    assert_eq!(second.converted, 0);
    assert!(second.cache_hits > 0);
    assert_eq!(second.animation_notices, first.animation_notices);
    assert_eq!(
        second.animation_skipped_channels,
        first.animation_skipped_channels
    );
    assert_eq!(
        fs::read(output.join("meshes/generated.glb")).unwrap(),
        before
    );
    let manifest = manifest(&output);
    assert!(manifest.complete);
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    let identity = shared::extensions::core_build_identity(&connection).unwrap();
    assert!(identity.is_some());
    let data = shared::extensions::DataExtensions::from_manifest_json(
        &serde_json::to_string(&manifest).unwrap(),
        identity,
    )
    .unwrap();
    assert!(
        data.status(shared::extensions::MATERIAL, 1..=1)
            .is_present()
    );
    assert_eq!(
        manifest.entries[layout::GENERATED_MODEL_PATH]
            .animation_diagnostics
            .as_ref()
            .unwrap()
            .skipped_channels,
        first.animation_skipped_channels
    );
}

#[tokio::test]
async fn old_no_lod_mesh_proof_requires_reconversion_and_gains_animation() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("output");
    generated_data(&data, false);
    assert!(run(config(&data, &output)).await.complete);
    let path = output.join("meshes/generated.glb");
    let bytes = fs::read(&path).unwrap();
    let mut json = document(&bytes);
    for material in json["materials"].as_array_mut().unwrap() {
        material["extensions"]
            .as_object_mut()
            .unwrap()
            .remove("OPEN_SKYRIM_material_animation");
    }
    json["extensionsUsed"]
        .as_array_mut()
        .unwrap()
        .retain(|name| name != "OPEN_SKYRIM_material_animation");
    let mut encoded = serde_json::to_vec(&json).unwrap();
    while !encoded.len().is_multiple_of(4) {
        encoded.push(b' ');
    }
    let old_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let tail = &bytes[20 + old_length..];
    let old = [
        b"glTF".as_slice(),
        &2u32.to_le_bytes(),
        &((20 + encoded.len() + tail.len()) as u32).to_le_bytes(),
        &(encoded.len() as u32).to_le_bytes(),
        b"JSON",
        &encoded,
        tail,
    ]
    .concat();
    fs::write(&path, &old).unwrap();
    let mut old_manifest = manifest(&output);
    old_manifest
        .asset_configuration_hashes
        .insert("nif".into(), "previous-authored-proof".into());
    let entry = old_manifest
        .entries
        .get_mut(layout::GENERATED_MODEL_PATH)
        .unwrap();
    entry.output_size = old.len() as u64;
    entry.output_hash = hash_bytes(&old);
    entry.animation_diagnostics = None;
    fs::write(
        output.join("conversion-manifest.json"),
        serde_json::to_vec(&old_manifest).unwrap(),
    )
    .unwrap();
    let rebuilt = directory.path().join("metadata-only");
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let error = AssetPipeline::rebuild_metadata_async(config(&data, &rebuilt), &output, tx)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("run normal conversion"),
        "{error:#}"
    );
    assert!(!rebuilt.exists());
    assert_eq!(fs::read(&path).unwrap(), old);
    let report = run(config(&data, &output)).await;
    assert!(report.complete, "{:?}", report.warnings);
    assert!(report.converted >= 1);
    let current = fs::read(&path).unwrap();
    assert_ne!(current, old);
    assert_eq!(document(&current)["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"]["channels"].as_array().unwrap().len(),1);
}

/// Snapshot directories as well as every file: an empty repair workspace is a side effect too.
fn pack_tree(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let bytes = entry
                .file_type()
                .is_file()
                .then(|| fs::read(entry.path()).unwrap());
            (
                entry.path().strip_prefix(root).unwrap().to_path_buf(),
                bytes,
            )
        })
        .collect()
}

fn core_identity(output: &Path) -> Option<String> {
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    shared::extensions::core_build_identity(&connection).unwrap()
}

#[tokio::test]
async fn repair_refuses_stale_or_missing_retained_nif_proof_before_staging() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("output");
    generated_data(&data, false);
    let failed_script = data.join("scripts/retry.pex");
    fs::create_dir_all(failed_script.parent().unwrap()).unwrap();
    fs::write(&failed_script, b"invalid script before repair").unwrap();
    let settings = config(&data, &output);
    let first = run(settings.clone()).await;
    assert!(!first.complete);
    let original = manifest(&output);
    assert_eq!(original.record_data_complete, Some(true));
    assert_eq!(original.failures.len(), 1);
    assert!(original.failures.contains_key("scripts/retry.pex"));
    let mesh = output.join("meshes/generated.glb");
    let retained_mesh = fs::read(&mesh).unwrap();
    let identity = core_identity(&output);
    fs::write(
        &failed_script,
        dummy_content::pex::minimal("Retry").unwrap(),
    )
    .unwrap();
    let manifest_path = output.join("conversion-manifest.json");
    for proof in [Some("previous-authored-proof"), None] {
        let mut stale = original.clone();
        if let Some(proof) = proof {
            stale
                .asset_configuration_hashes
                .insert("nif".into(), proof.into());
        } else {
            stale.asset_configuration_hashes.remove("nif");
        }
        // Schema, broad settings, source and retained output bytes remain valid.
        fs::write(&manifest_path, serde_json::to_vec(&stale).unwrap()).unwrap();
        let before = pack_tree(&output);
        for apply in [false, true] {
            let error = converter::repair::repair_failed(&settings, apply).unwrap_err();
            assert!(
                error.to_string().contains("retained NIF proof"),
                "{error:#}"
            );
            assert!(
                error.to_string().contains("run normal conversion"),
                "{error:#}"
            );
            assert_eq!(pack_tree(&output), before, "proof {proof:?}, apply {apply}");
        }
    }
    // Current proof permits the same repair, preserving successful mesh bytes and identity.
    fs::write(&manifest_path, serde_json::to_vec(&original).unwrap()).unwrap();
    let report = converter::repair::repair_failed(&settings, true).unwrap();
    assert!(report.published);
    assert_eq!(report.converted, 1);
    assert!(report.failures.is_empty());
    assert!(manifest(&output).complete);
    assert!(output.join("scripts/retry.luau").is_file());
    assert_eq!(fs::read(mesh).unwrap(), retained_mesh);
    assert_eq!(core_identity(&output), identity);
    assert_eq!(
        manifest(&output).asset_configuration_hashes,
        original.asset_configuration_hashes
    );
}

#[tokio::test]
async fn repair_without_retained_mesh_needs_no_nif_proof_and_keeps_strict_checks() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("output");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("esm,pex").unwrap(),
    )
    .unwrap();
    let failed_script = data.join("scripts/retry.pex");
    fs::write(&failed_script, b"invalid script before repair").unwrap();
    let settings = config(&data, &output);
    assert!(!run(settings.clone()).await.complete);
    let manifest_path = output.join("conversion-manifest.json");
    let mut original = manifest(&output);
    assert!(
        original
            .entries
            .values()
            .all(|entry| !entry.output.ends_with(".glb"))
    );
    original.asset_configuration_hashes.remove("nif");
    fs::write(&manifest_path, serde_json::to_vec(&original).unwrap()).unwrap();
    fs::write(
        &failed_script,
        dummy_content::pex::minimal("Retry").unwrap(),
    )
    .unwrap();
    let before = pack_tree(&output);
    let mut changed = settings.clone();
    changed.script_abi_version += 1;
    let error = converter::repair::repair_failed(&changed, true).unwrap_err();
    assert!(
        error.to_string().contains("repair configuration differs"),
        "{error:#}"
    );
    assert_eq!(pack_tree(&output), before);
    let retained_script = output.join("scripts/generated.luau");
    let bytes = fs::read(&retained_script).unwrap();
    fs::write(&retained_script, b"corrupt retained script").unwrap();
    let corrupt = pack_tree(&output);
    assert!(converter::repair::repair_failed(&settings, true).is_err());
    assert_eq!(pack_tree(&output), corrupt);
    fs::write(&retained_script, &bytes).unwrap();
    let identity = core_identity(&output);
    let report = converter::repair::repair_failed(&settings, true).unwrap();
    assert!(report.published);
    assert_eq!(report.converted, 1);
    assert!(manifest(&output).complete);
    assert_eq!(fs::read(retained_script).unwrap(), bytes);
    assert_eq!(core_identity(&output), identity);
    assert!(
        !manifest(&output)
            .asset_configuration_hashes
            .contains_key("nif")
    );
}

#[tokio::test]
async fn manager_only_empty_nif_publishes_static_glb_with_non_gating_notice() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("output");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("esm").unwrap(),
    )
    .unwrap();
    let input = data.join("meshes/manager_only.nif");
    fs::create_dir_all(input.parent().unwrap()).unwrap();
    fs::write(
        &input,
        dummy_content::nif::manager_only_transform_sequence().unwrap(),
    )
    .unwrap();
    // Exercise the public single-file converter as well as pipeline notice propagation.
    let direct = directory.path().join("direct.glb");
    converter::mesh::MeshConverter::convert_nif_to_glb(&input, &direct).unwrap();
    let single = document(&fs::read(direct).unwrap());
    assert!(single.get("buffers").is_none());
    assert!(single.get("animations").is_none());
    assert!(
        single["meshes"]
            .as_array()
            .is_none_or(|meshes| meshes.is_empty())
    );
    assert_eq!(single["scenes"][0]["nodes"], serde_json::json!([]));
    assert!(
        single["nodes"]
            .as_array()
            .is_none_or(|nodes| nodes.is_empty())
    );
    let expected = "transform animation omitted: the GLB has no buffer for animation data";
    assert_eq!(
        single["asset"]["extras"]["openSkyrim"]["animationWarnings"],
        serde_json::json!([expected])
    );
    let first = run(config(&data, &output)).await;
    assert!(first.complete, "{:?}", first.warnings);
    assert_eq!(first.skipped, 0);
    assert!(first.warnings.is_empty());
    assert!(
        first
            .animation_notices
            .iter()
            .any(|notice| notice.contains(expected)),
        "{:?}",
        first.animation_notices
    );
    let packed = fs::read(output.join("meshes/manager_only.glb")).unwrap();
    assert_eq!(document(&packed), single);
    let saved = manifest(&output);
    assert!(saved.complete && saved.failures.is_empty());
    assert_eq!(
        saved.entries["meshes/manager_only.nif"]
            .animation_diagnostics
            .as_ref()
            .unwrap()
            .notices,
        [expected]
    );
    let second = run(config(&data, &output)).await;
    assert!(second.complete, "{:?}", second.warnings);
    assert_eq!(second.converted, 0);
    assert!(second.cache_hits > 0);
    assert_eq!(second.animation_notices, first.animation_notices);
    assert_eq!(
        fs::read(output.join("meshes/manager_only.glb")).unwrap(),
        packed
    );
}

/// Exercise palette identity through the actual public exporter, not a hand-built GLB.
#[test]
fn duplicate_transform_names_use_palette_pointer_in_published_glb() {
    use dummy_content::nif::static_shape_with_duplicate_transform_targets;

    let shape = StaticShape {
        name: "DuplicateTarget",
        positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: &[[0.0, 0.0, 1.0]; 3],
        uvs: &[[0.0, 0.0]; 3],
        indices: &[[0, 1, 2]],
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    };
    for palette in [
        Some(&[4u32][..]),
        Some(&[4u32, 4][..]),
        None,
        Some(&[1u32, 4][..]),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("duplicates.nif");
        let output = directory.path().join("duplicates.glb");
        fs::write(
            &input,
            static_shape_with_duplicate_transform_targets(&shape, palette).unwrap(),
        )
        .unwrap();
        converter::mesh::MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
        let bytes = fs::read(output).unwrap();
        let json = document(&bytes);
        let nodes = json["nodes"].as_array().unwrap();
        let duplicates: Vec<_> = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node["name"] == shape.name)
            .collect();
        assert_eq!(duplicates.len(), 2, "{json:#}");
        assert!(
            duplicates
                .iter()
                .all(|(_, node)| node.get("mesh").is_some())
        );
        let first = duplicates
            .iter()
            .find(|(_, node)| node["translation"][0] == 10.0)
            .unwrap()
            .0;
        let second = duplicates
            .iter()
            .find(|(_, node)| node["translation"][0] == 20.0)
            .unwrap()
            .0;
        assert_ne!(first, second);
        assert_eq!(
            nodes[first]["translation"],
            serde_json::json!([10.0, 0.0, 0.0])
        );
        assert_eq!(
            nodes[second]["translation"],
            serde_json::json!([20.0, 0.0, 0.0])
        );
        if palette.is_some_and(|targets| targets.iter().all(|&target| target == 4)) {
            let channels = json["animations"][0]["channels"].as_array().unwrap();
            assert_eq!(channels.len(), 1);
            assert_eq!(channels[0]["target"]["node"], second);
            assert_eq!(channels[0]["target"]["path"], "translation");
            assert_eq!(
                json["animations"][0]["samplers"][0]["interpolation"],
                "LINEAR"
            );
            let accessor = json["animations"][0]["samplers"][0]["output"]
                .as_u64()
                .unwrap() as usize;
            assert_eq!(json["accessors"][accessor]["type"], "VEC3");
            assert_eq!(json["accessors"][accessor]["count"], 2);
            let warnings = &json["asset"]["extras"]["openSkyrim"]["animationWarnings"];
            assert!(
                warnings.is_null() || warnings.as_array().unwrap().is_empty(),
                "{warnings}"
            );
        } else {
            assert!(
                json.get("animations")
                    .is_none_or(|clips| clips.as_array().unwrap().is_empty())
            );
            let warnings = json["asset"]["extras"]["openSkyrim"]["animationWarnings"]
                .as_array()
                .unwrap();
            assert!(
                warnings.iter().any(|warning| {
                    let warning = warning.as_str().unwrap();
                    warning.contains("ambiguous") && warning.contains(shape.name)
                }),
                "{warnings:?}"
            );
        }
    }
}

#[test]
fn billboard_metadata_survives_nif_to_glb_export() {
    use dummy_content::nif::static_shape_with_billboard;

    let shape = StaticShape {
        name: "BillboardShape",
        positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: &[[0.0, 0.0, 1.0]; 3],
        uvs: &[[0.0, 0.0]; 3],
        indices: &[[0, 1, 2]],
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    };
    for (mode, expected) in [
        (0, serde_json::json!("alwaysFaceCamera")),
        (1, serde_json::json!("rotateAboutUp")),
        (9, serde_json::json!("rotateAboutUp2")),
        (42, serde_json::json!(42)),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("billboard.nif");
        let output = directory.path().join("billboard.glb");
        fs::write(&input, static_shape_with_billboard(&shape, mode).unwrap()).unwrap();
        converter::mesh::MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
        let json = document(&fs::read(output).unwrap());
        let nodes = json["nodes"].as_array().unwrap();
        let billboards: Vec<_> = nodes
            .iter()
            .filter(|node| node["extras"]["openSkyrim"].get("billboard").is_some())
            .collect();
        assert_eq!(billboards.len(), 1, "{json:#}");
        assert_eq!(billboards[0]["extras"]["openSkyrim"]["billboard"], expected);
        let child = billboards[0]["children"][0].as_u64().unwrap() as usize;
        assert_eq!(nodes[child]["name"], shape.name);
        assert!(nodes[child].get("mesh").is_some());
    }
}
