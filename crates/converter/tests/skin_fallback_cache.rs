//! Warm resolver-state cache proofs and targeted repair eligibility, using synthetic assets only.

use converter::{
    AssetPipeline, PipelineConfig,
    cache::{ConversionManifest, configuration_hash, hash_bytes, hash_file},
    mesh::{MeshConverter, SkeletonResolution, find_skeleton},
    repair::repair_failed,
};
use dummy_content::{
    layout,
    nif::{LegacySkin, SkinInstanceKind, StaticShape, legacy_skinned_shapes, static_shape},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const BODY_KEY: &str = "meshes/actors/test/body.nif";
const BODY_OUTPUT: &str = "meshes/actors/test/body.glb";
const GOOD: [[f32; 4]; 4] = [[0.75, 0.25, 0.0, 0.0]; 4];

/// A two-joint shape whose geometry and weights can be checked independently.
fn skin(name: &str) -> LegacySkin<'_> {
    LegacySkin {
        name,
        instance_kind: SkinInstanceKind::Plain,
        bone_transforms: [
            ([0.0; 3], [0.0, 0.0, 0.0, 1.0], 1.0),
            ([0.0, 0.0, 2.0], [0.0, 0.0, 0.0, 1.0], 1.0),
        ],
        palette: &[1, 0],
        weights: &GOOD,
        with_geometry: true,
        joint_override: None,
    }
}

/// Keep both shapes healthy so resolver changes alone determine whether skins survive.
fn body_fixture() -> Vec<u8> {
    legacy_skinned_shapes(&[skin("Affected"), skin("Healthy")]).unwrap()
}

/// Build a mixed input and detach only the first shape's skin reference.
fn mixed_fixture() -> Vec<u8> {
    let mut bytes = body_fixture();
    let shape = blocks(&bytes)
        .into_iter()
        .find(|(kind, _)| kind == "BSTriShape")
        .unwrap()
        .1;
    // NiAVObject (72 bytes), bounding sphere (16 bytes), then skin reference.
    bytes[shape + 88..shape + 92].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes
}

/// Build a valid remote skeleton containing Bone0 but not Bone1.
fn partial_skeleton() -> Vec<u8> {
    let mut bytes = static_shape(&StaticShape {
        name: "Bone0",
        positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: &[[0.0, 0.0, 1.0]; 3],
        uvs: &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        indices: &[[0, 1, 2]],
        diffuse: "textures/generated_color.dds",
        normal_texture: "textures/generated_normal.dds",
    })
    .unwrap();
    let root = blocks(&bytes)[0].1;
    bytes[root..root + 4].copy_from_slice(&0u32.to_le_bytes());
    bytes
}

/// Generate all dependencies, including the two texture names used by the skin writer.
fn prepare_data(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,pex,nif,esm").unwrap(),
    )
    .unwrap();
    fs::copy(
        data.join(layout::GENERATED_DIFFUSE_PATH),
        data.join("textures/generated_skin.dds"),
    )
    .unwrap();
    fs::copy(
        data.join(layout::GENERATED_NORMAL_PATH),
        data.join("textures/generated_skin_n.dds"),
    )
    .unwrap();
    fs::create_dir_all(data.join(BODY_KEY).parent().unwrap()).unwrap();
}

/// Keep conversion and repair configuration identical and avoid GPU LOD work.
fn config(data: &Path, output: &Path) -> PipelineConfig {
    let mut config = PipelineConfig::new(data, output);
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    config
}

/// Drain progress while the ordinary public pipeline publishes into the requested output.
async fn publish(config: &PipelineConfig) -> converter::pipeline::PipelineReport {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config.clone(), sender).await;
    drain.await.unwrap();
    assert!(result.is_ok(), "publication failed: {result:?}");
    result.unwrap()
}

/// Corrupt a declared skin's bone count while preserving its block and all neighboring bytes.
fn unparsable_skin() -> Vec<u8> {
    let mut bytes = body_fixture();
    let skin = blocks(&bytes)
        .into_iter()
        .find(|(kind, _)| kind == "NiSkinInstance")
        .unwrap()
        .1;
    bytes[skin + 12..skin + 16].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes
}

/// Build an independent raw node-only NIF for the audit's above-limit measurement.
fn audit_scene_chain(depth: u32) -> Vec<u8> {
    let mut blocks = Vec::new();
    for index in 0..depth {
        let mut words = vec![u32::MAX, u32::MAX, u32::MAX, 0, 0, 0, 0];
        words.extend([
            1.0_f32.to_bits(),
            0,
            0,
            0,
            1.0_f32.to_bits(),
            0,
            0,
            0,
            1.0_f32.to_bits(),
        ]);
        words.extend([1.0_f32.to_bits(), u32::MAX]);
        if index + 1 < depth {
            words.extend([1, index + 1]);
        } else {
            words.push(0);
        }
        words.push(0); // effects
        blocks.push(
            words
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        );
    }
    let mut bytes = b"Gamebryo File Format, Version 20.2.0.7\n".to_vec();
    bytes.extend(0x1402_0007_u32.to_le_bytes());
    bytes.push(1);
    for value in [12, depth, 100] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend([0, 0, 0]); // author/process/export script
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(6_u32.to_le_bytes());
    bytes.extend(b"NiNode");
    for _ in 0..depth {
        bytes.extend(0_u16.to_le_bytes());
    }
    for block in &blocks {
        bytes.extend((block.len() as u32).to_le_bytes());
    }
    bytes.extend([0; 12]); // string count, max string length, group count
    for block in blocks {
        bytes.extend(block);
    }
    bytes
}

/// The audit keeps measurements for deep scenes and malformed skins without exporting them.
#[test]
fn nif_audit_retains_refused_scene_and_skin_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    fs::create_dir(&input).unwrap();
    fs::write(input.join("broken.nif"), unparsable_skin()).unwrap();
    fs::write(input.join("healthy.nif"), body_fixture()).unwrap();
    fs::write(input.join("deep.nif"), audit_scene_chain(257)).unwrap();
    let report_path = root.path().join("audit.json");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_nif-audit"))
        .arg(&input)
        .arg(&report_path)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["summary"]["total_files"], 3);
    assert_eq!(report["summary"]["refused_files"], 2);
    assert_eq!(report["summary"]["max_scene_depth"], 257);
    let broken = &report["files"][0];
    assert_eq!(broken["path"], "broken.nif");
    assert_eq!(broken["fallback_blocks"]["NiSkinInstance"], 1);
    assert_eq!(
        broken["fallback_offsets"]["NiSkinInstance"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(broken["max_scene_depth"].as_u64().unwrap() > 0);
    assert!(
        broken["conversion_refusals"][0]
            .as_str()
            .unwrap()
            .contains("NiSkinInstance")
    );
    assert!(broken["parse_error"].is_null());
    assert_eq!(report["summary"]["conversion_attempts"], 0);
    let deep = &report["files"][1];
    assert_eq!(deep["path"], "deep.nif");
    assert_eq!(deep["max_scene_depth"], 257);
    assert!(
        deep["conversion_refusals"][0]
            .as_str()
            .unwrap()
            .contains("257 scene nodes")
    );
    assert!(
        report["files"][2]["conversion_refusals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// Pin the actual pre-refusal proof independently of the current contract list.
fn pre_refusal_mesh_proof() -> String {
    hash_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "mesh_producer": 28,
            "mesh_contracts": [
                "legacy-skin-export", "msn-vertex-normals", "facegen-tint-slot",
                "lighting-type-data", "msn-specular-default", "hidden-niavobject-excluded",
                "nif-root-transform-ignored", "skin-static-fallback"
            ]
        }))
        .unwrap(),
    )
}

/// A malformed skin fails only its model; valid assets and plugin records still publish.
#[tokio::test]
async fn malformed_skin_refusal_publishes_valid_neighbors_and_records_failure() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    prepare_data(&data);
    fs::write(data.join(BODY_KEY), unparsable_skin()).unwrap();
    // Resolver failure is a neutral answer, never permission to publish static geometry.
    assert_eq!(
        find_skeleton(&data.join(BODY_KEY)),
        SkeletonResolution::SelfContained
    );
    let direct = root.path().join("refused-direct.glb");
    assert!(MeshConverter::convert_nif_to_glb(data.join(BODY_KEY), direct.clone()).is_err());
    assert!(!direct.exists());
    let output = root.path().join("modern");
    assert_refused_model(&publish(&config(&data, &output)).await, &output);
}

/// Check the saved failure, diagnostic, omitted bytes and neighboring records after publication.
fn assert_refused_model(report: &converter::pipeline::PipelineReport, output: &Path) {
    assert!(!report.complete, "malformed skin was reused: {report:?}");
    assert_eq!(report.skipped, 1, "{report:?}");
    let saved = manifest(output);
    assert_eq!(saved.failures.len(), 1, "{:?}", saved.failures);
    let failure = &saved.failures[BODY_KEY];
    assert!(failure.contains("NiSkinInstance"), "{failure}");
    assert!(failure.contains("offset"), "{failure}");
    assert!(failure.len() < 2048, "asset diagnostic must stay bounded");
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|warning| *warning == failure)
            .count(),
        1
    );
    assert!(!saved.entries.contains_key(BODY_KEY));
    assert!(!output.join(BODY_OUTPUT).exists());
    assert!(output.join("meshes/generated.glb").is_file());
    assert!(saved.entries.contains_key("meshes/generated.nif"));
    let database = rusqlite::Connection::open_with_flags(
        output.join("skyrim_world.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let records: u64 = database
        .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
        .unwrap();
    assert!(records > 0, "usable plugin records did not survive");
}

/// Seed tool inputs with a valid neighboring GLB and an old static success for a malformed NIF.
fn tool_inputs(root: &Path) -> (PathBuf, PathBuf) {
    let data = root.join("Data");
    prepare_data(&data);
    fs::write(data.join(BODY_KEY), unparsable_skin()).unwrap();
    let assets = root.join("old-assets");
    fs::create_dir_all(assets.join(BODY_OUTPUT).parent().unwrap()).unwrap();
    MeshConverter::convert_nif_to_glb(
        data.join("meshes/generated.nif"),
        assets.join("meshes/generated.glb"),
    )
    .unwrap();
    fs::copy(
        assets.join("meshes/generated.glb"),
        assets.join(BODY_OUTPUT),
    )
    .unwrap();
    (data.join("meshes"), assets)
}

/// A batch annotation tool must process good neighbors and record a failed model after refusal.
#[test]
fn collision_annotation_tool_refusal_preserves_neighbors_and_reports_failure() {
    let root = tempfile::tempdir().unwrap();
    let (sources, assets) = tool_inputs(root.path());
    let output = root.path().join("annotated");
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_collision-annotate"))
        .arg(&sources)
        .arg(assets.join("meshes"))
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        !run.status.success(),
        "incomplete annotation needs a failing exit status"
    );
    let report = output.join("collision-annotation-report.json");
    assert!(
        report.is_file(),
        "a bad model aborted the batch before its failure report"
    );
    let report: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(report["complete"], false);
    assert_eq!(report["annotated"], 1);
    let failures = report["failures"].as_object().unwrap();
    assert_eq!(failures.len(), 1);
    let message = failures["actors/test/body.glb"].as_str().unwrap();
    assert!(message.contains("NiSkinInstance"), "{message}");
    assert!(message.len() < 4096);
    assert!(output.join("generated.glb").is_file());
    assert!(!output.join("actors/test/body.glb").exists());
    assert!(
        assets.join(BODY_OUTPUT).is_file(),
        "input pack must stay intact"
    );
}

/// Closure conversion must validate current NIF input before reusing an old static GLB.
#[test]
fn asset_closure_tool_refusal_rejects_old_success_and_preserves_neighbor() {
    let root = tempfile::tempdir().unwrap();
    let (sources, assets) = tool_inputs(root.path());
    let neighbor = fs::read(assets.join("meshes/generated.glb")).unwrap();
    let database = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    database.execute_batch("CREATE TABLE statics(id INTEGER PRIMARY KEY, model_path TEXT); CREATE TABLE \"references\"(base_form_id INTEGER); INSERT INTO statics VALUES(1,'generated.nif'),(2,'actors/test/body.nif'); INSERT INTO \"references\" VALUES(1),(2);").unwrap();
    drop(database);
    let report = root.path().join("closure-report.json");
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_asset-closure-convert"))
        .arg(&sources)
        .arg(&assets)
        .arg(&report)
        .output()
        .unwrap();
    assert!(
        !run.status.success(),
        "closure reused a refused NIF's old GLB"
    );
    let report: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(report["failures"], 1);
    assert_eq!(report["reused"], 1);
    let failed = report["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["status"] == "failed")
        .unwrap();
    assert_eq!(failed["model_path"], "actors/test/body.nif");
    let message = failed["error"].as_str().unwrap();
    assert!(message.contains("NiSkinInstance"), "{message}");
    assert!(message.len() < 4096);
    assert!(!assets.join(BODY_OUTPUT).exists());
    assert_eq!(
        fs::read(assets.join("meshes/generated.glb")).unwrap(),
        neighbor
    );
}

/// A published old static success for the same malformed source must be rebuilt and refused.
#[tokio::test]
async fn warm_cache_refuses_pre_refusal_static_success() {
    assert_old_skin_success_is_refused(0).await;
}

/// A resumed journal must also reject the old same-producer mesh settings proof.
#[tokio::test]
async fn resumed_cache_refuses_pre_refusal_static_success() {
    assert_old_skin_success_is_refused(1).await;
}

/// A staging copy without a journal must not recover old proof from the published manifest.
#[tokio::test]
async fn resumed_manifest_cache_refuses_pre_refusal_static_success() {
    assert_old_skin_success_is_refused(2).await;
}

/// Seed genuine valid output bytes with an independently pinned old proof and unchanged bad input.
async fn assert_old_skin_success_is_refused(mode: u8) {
    use converter::cache::{StagedOutput, StagingJournal};

    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    prepare_data(&data);
    fs::write(data.join(BODY_KEY), body_fixture()).unwrap();
    let output = root.path().join("modern");
    let mut config = config(&data, &output);
    assert!(publish(&config).await.complete);
    let mut saved = manifest(&output);
    let texture_proof = saved.asset_configuration_hashes["dds"].clone();
    let script_proof = saved.asset_configuration_hashes["pex"].clone();
    let neighbor = fs::read(output.join("meshes/generated.glb")).unwrap();
    fs::write(data.join(BODY_KEY), unparsable_skin()).unwrap();
    // A valid static GLB is what the old parser's Unhandled skin could publish.
    fs::write(output.join(BODY_OUTPUT), &neighbor).unwrap();
    let entry = saved.entries.get_mut(BODY_KEY).unwrap();
    entry.source_hash = hash_file(&data.join(BODY_KEY)).unwrap();
    entry.output_hash = hash_bytes(&neighbor);
    entry.output_size = neighbor.len() as u64;
    let record = StagedOutput {
        schema_version: saved.schema_version,
        configuration_hash: pre_refusal_mesh_proof(),
        source_hash: entry.source_hash.clone(),
        output_size: entry.output_size,
        output_hash: entry.output_hash.clone(),
    };
    saved
        .asset_configuration_hashes
        .insert("nif".into(), pre_refusal_mesh_proof());
    saved
        .save(&output.join("conversion-manifest.json"))
        .unwrap();
    if mode != 0 {
        let staging = root.path().join("modern.staging-old-skin");
        fs::create_dir_all(staging.join(BODY_OUTPUT).parent().unwrap()).unwrap();
        fs::write(staging.join(BODY_OUTPUT), &neighbor).unwrap();
        if mode == 1 {
            StagingJournal::open(&staging)
                .unwrap()
                .record(BODY_KEY, &record)
                .unwrap();
            // Exercise the journal alone, independently of manifest reuse.
            fs::rename(&output, root.path().join("retained-old-pack")).unwrap();
        }
        config.resume_staging = Some(staging);
    }
    let report = publish(&config).await;
    assert_refused_model(&report, &output);
    assert_eq!(
        fs::read(output.join("meshes/generated.glb")).unwrap(),
        neighbor
    );
    let current = manifest(&output);
    assert_eq!(current.asset_configuration_hashes["dds"], texture_proof);
    assert_eq!(current.asset_configuration_hashes["pex"], script_proof);
}

/// Read the actual saved proof, including failures and per-kind settings.
fn manifest(output: &Path) -> ConversionManifest {
    serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap()).unwrap()
}

/// Snapshot every published file, rather than only the manifest's tracked artifacts.
fn pack_bytes(output: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(output)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry.path().strip_prefix(output).unwrap().to_path_buf(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

/// Read the writer's header to locate blocks without searching for coincidental byte patterns.
fn blocks(bytes: &[u8]) -> Vec<(String, usize)> {
    /// Advance over one little-endian header integer.
    fn u32_at(bytes: &[u8], cursor: &mut usize) -> u32 {
        let value = u32::from_le_bytes(bytes[*cursor..*cursor + 4].try_into().unwrap());
        *cursor += 4;
        value
    }
    /// Advance over one short header integer.
    fn u16_at(bytes: &[u8], cursor: &mut usize) -> u16 {
        let value = u16::from_le_bytes(bytes[*cursor..*cursor + 2].try_into().unwrap());
        *cursor += 2;
        value
    }
    /// Skip a length-prefixed exporter string.
    fn skip8(bytes: &[u8], cursor: &mut usize) {
        *cursor += 1 + bytes[*cursor] as usize;
    }
    /// Decode a length-prefixed type or name string.
    fn string32(bytes: &[u8], cursor: &mut usize) -> String {
        let length = u32_at(bytes, cursor) as usize;
        let value = String::from_utf8(bytes[*cursor..*cursor + length].to_vec()).unwrap();
        *cursor += length;
        value
    }
    let mut cursor = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    u32_at(bytes, &mut cursor);
    cursor += 1;
    u32_at(bytes, &mut cursor);
    let count = u32_at(bytes, &mut cursor);
    let version = u32_at(bytes, &mut cursor);
    if version > 130 {
        u32_at(bytes, &mut cursor);
    }
    skip8(bytes, &mut cursor);
    if version < 131 {
        skip8(bytes, &mut cursor);
    }
    skip8(bytes, &mut cursor);
    if version >= 103 {
        skip8(bytes, &mut cursor);
    }
    let type_count = u16_at(bytes, &mut cursor);
    let types: Vec<_> = (0..type_count)
        .map(|_| string32(bytes, &mut cursor))
        .collect();
    let indices: Vec<_> = (0..count).map(|_| u16_at(bytes, &mut cursor)).collect();
    let sizes: Vec<_> = (0..count).map(|_| u32_at(bytes, &mut cursor)).collect();
    let strings = u32_at(bytes, &mut cursor);
    u32_at(bytes, &mut cursor);
    for _ in 0..strings {
        string32(bytes, &mut cursor);
    }
    let groups = u32_at(bytes, &mut cursor);
    for _ in 0..groups {
        u32_at(bytes, &mut cursor);
    }
    indices
        .into_iter()
        .zip(sizes)
        .map(|(kind, size)| {
            let start = cursor;
            cursor += size as usize;
            (types[kind as usize].clone(), start)
        })
        .collect()
}

struct Glb {
    json: Value,
    bin: Vec<u8>,
}

/// Read JSON and binary chunks from an actual public-API GLB output.
fn read_glb(path: &Path) -> Glb {
    let bytes = fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"glTF");
    let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json = serde_json::from_slice(&bytes[20..20 + length]).unwrap();
    let bin_start = 28 + length;
    Glb {
        json,
        bin: bytes[bin_start..].to_vec(),
    }
}

/// Select a mesh by its authored name rather than relying on export order.
fn mesh_node<'a>(glb: &'a Glb, name: &str) -> (usize, &'a Value) {
    let nodes = glb.json["nodes"].as_array().unwrap();
    nodes
        .iter()
        .enumerate()
        .find(|(_, node)| {
            node["mesh"].as_u64().is_some_and(|mesh| {
                node["name"].as_str() == Some(name)
                    || glb.json["meshes"][mesh as usize]["name"].as_str() == Some(name)
            })
        })
        .unwrap_or_else(|| panic!("missing geometry for {name}: {}", glb.json))
}

/// Decode float accessors, respecting buffer-view stride and accessor offset.
fn floats<const N: usize>(glb: &Glb, index: u64) -> Vec<[f32; N]> {
    let accessor = &glb.json["accessors"][index as usize];
    assert_eq!(accessor["componentType"], 5126);
    let view = &glb.json["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let start = view["byteOffset"].as_u64().unwrap_or(0) as usize
        + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
    let stride = view["byteStride"].as_u64().unwrap_or((N * 4) as u64) as usize;
    (0..accessor["count"].as_u64().unwrap() as usize)
        .map(|row| {
            std::array::from_fn(|axis| {
                let offset = start + row * stride + axis * 4;
                f32::from_le_bytes(glb.bin[offset..offset + 4].try_into().unwrap())
            })
        })
        .collect()
}

/// Decode a triangle accessor with either GLB index width.
fn indices(glb: &Glb, index: u64) -> Vec<u32> {
    let accessor = &glb.json["accessors"][index as usize];
    let view = &glb.json["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let start = view["byteOffset"].as_u64().unwrap_or(0) as usize
        + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
    let width = match accessor["componentType"].as_u64().unwrap() {
        5123 => 2,
        5125 => 4,
        other => panic!("unexpected index type {other}"),
    };
    let stride = view["byteStride"].as_u64().unwrap_or(width as u64) as usize;
    (0..accessor["count"].as_u64().unwrap() as usize)
        .map(|row| {
            let offset = start + row * stride;
            if width == 2 {
                u32::from(u16::from_le_bytes(
                    glb.bin[offset..offset + 2].try_into().unwrap(),
                ))
            } else {
                u32::from_le_bytes(glb.bin[offset..offset + 4].try_into().unwrap())
            }
        })
        .collect()
}

/// Check both complete shapes and actual skin bindings, independent of cache hit counts.
fn assert_body(path: &Path, skinned: bool) {
    let glb = read_glb(path);
    assert_eq!(
        glb.json["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["mesh"].is_u64())
            .count(),
        2
    );
    for name in ["Affected", "Healthy"] {
        let (_, node) = mesh_node(&glb, name);
        let primitive =
            &glb.json["meshes"][node["mesh"].as_u64().unwrap() as usize]["primitives"][0];
        let positions = floats::<3>(&glb, primitive["attributes"]["POSITION"].as_u64().unwrap());
        assert_eq!(positions.len(), 4, "{name} lost vertices");
        for (actual, expected) in positions.iter().zip([
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [-0.5, 0.866_025_4, 0.0],
            [-0.5, -0.866_025_4, 0.0],
        ]) {
            for (actual, expected) in actual.iter().zip(expected) {
                assert!((actual - expected).abs() < 1e-5, "{name}: {positions:?}");
            }
        }
        assert_eq!(
            indices(&glb, primitive["indices"].as_u64().unwrap()),
            [0, 1, 2, 0, 2, 3]
        );
        if skinned {
            let skin = node["skin"]
                .as_u64()
                .unwrap_or_else(|| panic!("{name} lost its skin"));
            let binding = &glb.json["skins"][skin as usize];
            assert_eq!(binding["joints"].as_array().unwrap().len(), 2);
            for joint in binding["joints"].as_array().unwrap() {
                assert!(
                    joint.as_u64().unwrap() < glb.json["nodes"].as_array().unwrap().len() as u64
                );
            }
            assert!(primitive["attributes"]["JOINTS_0"].is_u64());
            let weights = floats::<4>(&glb, primitive["attributes"]["WEIGHTS_0"].as_u64().unwrap());
            assert_eq!(weights.len(), 4);
            for mut row in weights {
                row.sort_by(f32::total_cmp);
                for (actual, expected) in row.into_iter().zip([0.0, 0.0, 0.25, 0.75]) {
                    assert!((actual - expected).abs() < 1e-5);
                }
            }
        } else {
            assert!(
                node.get("skin").is_none(),
                "{name} must be rigid when unresolved"
            );
            assert!(primitive["attributes"].get("JOINTS_0").is_none());
            assert!(primitive["attributes"].get("WEIGHTS_0").is_none());
        }
    }
}

/// An unchanged body must switch both ways in one warm output when a remote partial rig appears.
#[tokio::test]
async fn warm_cache_tracks_self_contained_unresolved_and_back() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    prepare_data(&data);
    let body = data.join(BODY_KEY);
    let original = body_fixture();
    fs::write(&body, &original).unwrap();
    let source_hash = hash_file(&body).unwrap();
    let output = root.path().join("modern");
    let config = config(&data, &output);
    assert_eq!(find_skeleton(&body), SkeletonResolution::SelfContained);
    assert!(publish(&config).await.complete);
    assert_body(&output.join(BODY_OUTPUT), true);
    let self_contained = fs::read(output.join(BODY_OUTPUT)).unwrap();
    let initial = manifest(&output);
    assert_eq!(initial.entries[BODY_KEY].source_hash, source_hash);
    let neighbor = fs::read(output.join("meshes/generated.glb")).unwrap();
    assert_eq!(
        publish(&config).await.converted,
        0,
        "control must actually be warm"
    );

    let skeleton = data.join("meshes/actors/other/skeleton.nif");
    fs::create_dir_all(skeleton.parent().unwrap()).unwrap();
    let partial = partial_skeleton();
    fs::write(&skeleton, &partial).unwrap();
    let resolution = find_skeleton(&body);
    assert!(
        matches!(resolution, SkeletonResolution::Unresolved { best_coverage } if (best_coverage - 0.5).abs() < 1e-6),
        "{resolution:?}"
    );
    let unresolved = publish(&config).await;
    assert!(unresolved.complete, "{unresolved:?}");
    assert!(
        unresolved.converted >= 1,
        "body must be rebuilt: {unresolved:?}"
    );
    assert_body(&output.join(BODY_OUTPUT), false);
    let rigid = fs::read(output.join(BODY_OUTPUT)).unwrap();
    assert_ne!(rigid, self_contained);
    assert_eq!(
        manifest(&output).entries[BODY_KEY].source_hash,
        format!("{source_hash}:skin-resolution:unresolved")
    );
    assert_eq!(
        publish(&config).await.converted,
        0,
        "unresolved result must also be reusable"
    );

    // Retain the skeleton outside Data instead of deleting fixture evidence.
    let retained = root.path().join("retained-partial-skeleton.nif");
    fs::rename(&skeleton, &retained).unwrap();
    assert_eq!(fs::read(&retained).unwrap(), partial);
    assert_eq!(find_skeleton(&body), SkeletonResolution::SelfContained);
    let restored = publish(&config).await;
    assert!(restored.complete, "{restored:?}");
    assert!(
        restored.converted >= 1,
        "rigid cache entry must be replaced: {restored:?}"
    );
    assert_body(&output.join(BODY_OUTPUT), true);
    assert_eq!(fs::read(output.join(BODY_OUTPUT)).unwrap(), self_contained);
    assert_eq!(manifest(&output).entries[BODY_KEY].source_hash, source_hash);
    assert_eq!(fs::read(&body).unwrap(), original);
    assert_eq!(
        fs::read(output.join("meshes/generated.glb")).unwrap(),
        neighbor
    );
    assert_eq!(publish(&config).await.converted, 0);

    // A complete selected rig keeps the established content dependency proof.
    let mut covering = legacy_skinned_shapes(&[skin("CoveringRig")]).unwrap();
    let root_block = blocks(&covering)[0].1;
    // A skeleton needs a named root; the mesh writer deliberately uses NULL_REF there.
    covering[root_block..root_block + 4].copy_from_slice(&0u32.to_le_bytes());
    fs::write(&skeleton, covering).unwrap();
    assert_eq!(
        find_skeleton(&body),
        SkeletonResolution::Skeleton(skeleton.clone())
    );
    assert!(publish(&config).await.complete);
    assert_body(&output.join(BODY_OUTPUT), true);
    assert_eq!(
        manifest(&output).entries[BODY_KEY].source_hash,
        format!("{source_hash}:{}", hash_file(&skeleton).unwrap())
    );
    assert_eq!(publish(&config).await.converted, 0);
}

/// Repair must reject both main proofs and the unpublished fallback proof before staging.
#[tokio::test]
async fn repair_rejects_old_mesh_contract_even_with_matching_schema_and_configuration() {
    for contracts in [
        &["legacy-skin-export"][..],
        &[
            "legacy-skin-export",
            "msn-vertex-normals",
            "facegen-tint-slot",
        ][..],
        &["legacy-skin-export", "skin-static-fallback"][..],
        &[
            "legacy-skin-export",
            "msn-vertex-normals",
            "facegen-tint-slot",
            "lighting-type-data",
            "msn-specular-default",
            "hidden-niavobject-excluded",
            "nif-root-transform-ignored",
        ][..],
    ] {
        assert_repair_rejects_stale_contract(contracts).await;
    }
}

/// An independently pinned stale settings proof cannot authorize mutation of a retained pack.
async fn assert_repair_rejects_stale_contract(contracts: &[&str]) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    prepare_data(&data);
    fs::write(data.join("meshes/mixed.nif"), mixed_fixture()).unwrap();
    fs::write(data.join(BODY_KEY), b"broken NIF awaiting repair").unwrap();
    let output = root.path().join("modern");
    let config = config(&data, &output);
    assert!(!publish(&config).await.complete);
    let mut saved = manifest(&output);
    assert_eq!(saved.failures.len(), 1, "{:?}", saved.failures);
    assert!(saved.failures.contains_key(BODY_KEY));
    assert_eq!(saved.schema_version, 28);
    assert_eq!(
        saved.configuration_hash,
        configuration_hash(&config).unwrap()
    );
    fs::write(data.join(BODY_KEY), body_fixture()).unwrap();

    // Model the old mixed output: its healthy skinned sibling exists, its rigid sibling does not.
    // Use public conversion of the surviving shape, without depending on the new mixed exporter.
    let survivor = root.path().join("historical-survivor.nif");
    let old_output = root.path().join("historical/meshes/mixed.glb");
    fs::create_dir_all(old_output.parent().unwrap()).unwrap();
    fs::write(
        &survivor,
        legacy_skinned_shapes(&[skin("Healthy")]).unwrap(),
    )
    .unwrap();
    MeshConverter::convert_nif_to_glb_with_skeleton(&survivor, &old_output, None).unwrap();
    let old = read_glb(&old_output);
    assert_eq!(old.json["meshes"].as_array().unwrap().len(), 1);
    assert!(mesh_node(&old, "Healthy").1["skin"].is_u64());
    let old_bytes = fs::read(&old_output).unwrap();
    fs::write(output.join("meshes/mixed.glb"), &old_bytes).unwrap();
    for dependency in
        MeshConverter::glb_texture_dependencies(&output.join("meshes/mixed.glb")).unwrap()
    {
        assert!(
            output.join("meshes").join(&dependency.uri).is_file(),
            "historical fixture has a missing texture: {}",
            dependency.uri
        );
    }
    let entry = saved.entries.get_mut("meshes/mixed.nif").unwrap();
    entry.output_hash = hash_bytes(&old_bytes);
    entry.output_size = old_bytes.len() as u64;
    saved.asset_configuration_hashes.insert(
        "nif".into(),
        hash_bytes(
            &serde_json::to_vec(
                &serde_json::json!({"mesh_producer": 28, "mesh_contracts": contracts}),
            )
            .unwrap(),
        ),
    );
    saved
        .save(&output.join("conversion-manifest.json"))
        .unwrap();
    assert_eq!(
        saved.configuration_hash,
        configuration_hash(&config).unwrap()
    );
    let checked =
        converter::check::check_output(&output, converter::check::CheckMode::Full, |_, _| {})
            .unwrap();
    assert!(
        checked.problems.iter().all(|problem| matches!(
            problem,
            converter::check::CheckProblem::Incomplete
                | converter::check::CheckProblem::RecordedFailures { count: 1 }
        )),
        "{:?}",
        checked.problems
    );
    let before = pack_bytes(&output);
    let directory_count = fs::read_dir(&output).unwrap().count();
    for apply in [false, true] {
        let result = repair_failed(&config, apply);
        assert!(
            result.is_err(),
            "stale mesh contract must reject repair (apply={apply}): {result:?}"
        );
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("mesh output contract"), "{error}");
        assert_eq!(
            pack_bytes(&output),
            before,
            "repair changed the source pack"
        );
        assert_eq!(fs::read_dir(&output).unwrap().count(), directory_count);
        assert!(!fs::read_dir(&output).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".repair-")
        }));
        assert!(!output.with_file_name("modern.repair-journal.json").exists());
    }
}

/// Current-contract repair uses the same unresolved proof and retains both rigid shapes.
#[tokio::test]
async fn repair_current_contract_unresolved_mesh_preserves_geometry_and_warm_proof() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    prepare_data(&data);
    // Establish texture roles before the broken body becomes usable; this test
    // isolates mesh source proofs from repair's separate texture-role refresh.
    fs::write(data.join("meshes/healthy.nif"), body_fixture()).unwrap();
    let skeleton = data.join("meshes/actors/other/skeleton.nif");
    fs::create_dir_all(skeleton.parent().unwrap()).unwrap();
    fs::write(&skeleton, partial_skeleton()).unwrap();
    let body = data.join(BODY_KEY);
    fs::write(&body, b"broken NIF awaiting repair").unwrap();
    let output = root.path().join("modern");
    let config = config(&data, &output);
    assert!(!publish(&config).await.complete);
    let initial = manifest(&output);
    assert_eq!(initial.failures.len(), 1, "{:?}", initial.failures);
    assert!(initial.failures.contains_key(BODY_KEY));
    let neighbor = fs::read(output.join("meshes/generated.glb")).unwrap();
    fs::write(&body, body_fixture()).unwrap();
    let resolution = find_skeleton(&body);
    assert!(
        matches!(resolution, SkeletonResolution::Unresolved { best_coverage } if (best_coverage - 0.5).abs() < 1e-6),
        "{resolution:?}"
    );
    let before = pack_bytes(&output);
    let preview = repair_failed(&config, false).unwrap();
    assert!(!preview.published);
    assert_eq!(preview.converted, 1);
    assert!(preview.failures.is_empty(), "{:?}", preview.failures);
    assert_body(&preview.directory.join("assets").join(BODY_OUTPUT), false);
    let preview_manifest = manifest(&preview.directory.join("assets"));
    let expected_hash = format!("{}:skin-resolution:unresolved", hash_file(&body).unwrap());
    assert_eq!(
        preview_manifest.entries[BODY_KEY].source_hash,
        expected_hash
    );
    // Preview artifacts are retained; compare the original published paths only.
    for (relative, bytes) in before {
        assert_eq!(fs::read(output.join(relative)).unwrap(), bytes);
    }
    let applied = repair_failed(&config, true).unwrap();
    assert!(applied.published);
    assert_eq!(applied.converted, 1);
    assert!(applied.failures.is_empty(), "{:?}", applied.failures);
    assert_body(&output.join(BODY_OUTPUT), false);
    let repaired = manifest(&output);
    assert!(repaired.complete);
    assert!(repaired.failures.is_empty());
    assert_eq!(
        repaired.asset_configuration_hashes,
        initial.asset_configuration_hashes
    );
    assert_eq!(repaired.entries[BODY_KEY].source_hash, expected_hash);
    assert_eq!(
        fs::read(output.join("meshes/generated.glb")).unwrap(),
        neighbor
    );
    let repaired_bytes = fs::read(output.join(BODY_OUTPUT)).unwrap();
    let warm = publish(&config).await;
    assert!(warm.complete, "{warm:?}");
    let after_warm = manifest(&output);
    let changed: Vec<_> = after_warm
        .entries
        .iter()
        .filter(|(key, entry)| repaired.entries.get(*key) != Some(*entry))
        .map(|(key, entry)| (key, repaired.entries.get(key), entry))
        .collect();
    assert_eq!(
        warm.converted, 0,
        "repair and ordinary conversion must agree; changed={changed:?}: {warm:?}"
    );
    assert_eq!(fs::read(output.join(BODY_OUTPUT)).unwrap(), repaired_bytes);
    assert_eq!(
        manifest(&output).entries[BODY_KEY].source_hash,
        expected_hash
    );
}

/// Pin the mesh proof before raising the shared scene/collision traversal budget.
fn pre_depth_expansion_mesh_proof() -> String {
    hash_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "mesh_producer": 28,
            "mesh_contracts": [
                "legacy-skin-export", "msn-vertex-normals", "facegen-tint-slot",
                "lighting-type-data", "msn-specular-default", "hidden-niavobject-excluded",
                "nif-root-transform-ignored", "skin-static-fallback", "nif-scene-and-skin-parse"
            ]
        }))
        .unwrap(),
    )
}

/// A warm pack carrying the old collision traversal proof must rebuild its meshes only.
#[tokio::test]
async fn warm_cache_rebuilds_pre_depth_expansion_mesh_proof() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    prepare_data(&data);
    let output = root.path().join("modern");
    let config = config(&data, &output);
    assert!(publish(&config).await.complete);
    let mut saved = manifest(&output);
    let texture_proof = saved.asset_configuration_hashes["dds"].clone();
    let script_proof = saved.asset_configuration_hashes["pex"].clone();
    let old_proof = pre_depth_expansion_mesh_proof();
    assert_eq!(
        old_proof,
        "b62ec72de284a86f9352426adeda91c0c400cf922ffd931c6be9d3c040031036"
    );
    saved
        .asset_configuration_hashes
        .insert("nif".into(), old_proof.clone());
    fs::write(
        output.join("conversion-manifest.json"),
        serde_json::to_vec_pretty(&saved).unwrap(),
    )
    .unwrap();
    let report = publish(&config).await;
    assert!(report.complete, "{report:?}");
    assert!(
        report.converted >= 1,
        "old traversal proof was reused: {report:?}"
    );
    let current = manifest(&output);
    assert_ne!(current.asset_configuration_hashes["nif"], old_proof);
    assert_eq!(current.asset_configuration_hashes["dds"], texture_proof);
    assert_eq!(current.asset_configuration_hashes["pex"], script_proof);
}

/// Preserve block boundaries while making the first shape's shader reference out of range.
fn invalid_material_reference(mut bytes: Vec<u8>) -> Vec<u8> {
    let shape = blocks(&bytes)
        .into_iter()
        .find(|(kind, _)| kind == "BSTriShape")
        .unwrap()
        .1;
    // NiAVObject (72), bounds (16), skin reference (4), then shader reference.
    bytes[shape + 92..shape + 96].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
    bytes
}

/// A second material error must preserve a scene/skin refusal's already measured diagnostics.
#[test]
fn nif_audit_retains_policy_diagnostics_when_material_inspection_fails() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    fs::create_dir(&input).unwrap();
    let combined = input.join("combined.nif");
    fs::write(&combined, invalid_material_reference(unparsable_skin())).unwrap();
    fs::write(input.join("healthy.nif"), body_fixture()).unwrap();
    let diagnostics = MeshConverter::inspect_nif(&combined)
        .expect("a material failure must retain the skin and scene measurements");
    assert_eq!(diagnostics.fallback_blocks["NiSkinInstance"], 1);
    assert!(diagnostics.max_scene_depth > 0);
    let material_reason = diagnostics
        .conversion_refusals
        .iter()
        .find(|reason| reason.starts_with("material inspection unavailable: "))
        .unwrap();
    assert!(material_reason.chars().count() <= 512);
    assert_eq!(diagnostics.material_shape_count, 0);
    assert!(MeshConverter::validate_nif_for_conversion(&combined).is_err());
    let output = root.path().join("refused.glb");
    assert!(MeshConverter::convert_nif_to_glb(&combined, &output).is_err());
    assert!(!output.exists());
    assert!(MeshConverter::extract_collision(&combined).is_err());
    let report_path = root.path().join("audit.json");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_nif-audit"))
        .arg(&input)
        .arg(&report_path)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["summary"]["total_files"], 2);
    assert_eq!(report["summary"]["parsed_files"], 2);
    assert_eq!(report["summary"]["refused_files"], 1);
    assert_eq!(report["summary"]["material_failures"], 0);
    let file = &report["files"][0];
    assert_eq!(file["path"], "combined.nif");
    assert_eq!(file["fallback_blocks"]["NiSkinInstance"], 1);
    assert_eq!(file["max_scene_depth"], diagnostics.max_scene_depth);
    assert!(file["parse_error"].is_null());
    assert_eq!(file["conversion_refusals"].as_array().unwrap().len(), 2);
    // A material-only failure keeps the existing inspection error contract.
    let material_only = root.path().join("material-only.nif");
    fs::write(&material_only, invalid_material_reference(body_fixture())).unwrap();
    let error = MeshConverter::inspect_nif(&material_only).unwrap_err();
    assert!(format!("{error:#}").contains("invalid NIF material"));
}
