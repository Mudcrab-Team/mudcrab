//! Proposed V92: only accepted current-run mesh artifacts may affect the world audit.
//! Historical prune records describe omissions; they do not prove mesh provenance.

use converter::{
    AssetPipeline, PipelineConfig, PipelineReport,
    cache::{
        CONVERTER_SCHEMA_VERSION, ConversionManifest, StagedOutput, StagingJournal,
        configuration_hash, configuration_hash_for_schema,
    },
};
use std::{collections::BTreeSet, fs, path::Path};
use tokio::sync::mpsc;

const MESH: &str = "meshes/generated.glb";
const SOURCE: &str = "meshes/generated.nif";
// The NIF fixture clamps both axes; lighting publishes the wrap0 alias.
const MISSING_TEXTURE: &str = "textures/absent_n.opensky-wrap0.ktx2";

fn generate_data(data: &Path) {
    dummy_content::layout::prepare_directory(data, false).unwrap();
    dummy_content::layout::generate(
        data,
        dummy_content::layout::DEFAULT_SEED,
        dummy_content::layout::Formats::all(),
    )
    .unwrap();
    // Override the archived mesh with a source whose missing normal is pruned.
    let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    fs::write(
        data.join(SOURCE),
        dummy_content::nif::static_shape(&dummy_content::nif::StaticShape {
            name: "RemovedSource",
            positions: &positions,
            normals: &[[0.0, 0.0, 1.0]; 3],
            uvs: &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            indices: &[[0, 1, 2]],
            diffuse: "textures/generated_color.dds",
            normal_texture: "textures/absent_n.dds",
        })
        .unwrap(),
    )
    .unwrap();
}

async fn run(config: PipelineConfig) -> PipelineReport {
    let (tx, mut rx) = mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    report
}

fn assert_removed_mesh_is_unavailable(report: &PipelineReport, output: &Path, schema: u32) {
    let integration = report.integration.as_ref().unwrap();
    eprintln!(
        "schema={schema} nif_inputs={} bounds_updated={} unavailable_model_source_count={} prunes={}",
        report.inputs_by_kind["nif"],
        integration.bounds_updated,
        integration.unavailable_model_source_count,
        report.pruned_texture_references,
    );
    assert!(report.complete);
    assert_eq!(report.inputs_by_kind["nif"], 0);
    assert_eq!(
        integration.unavailable_model_source_count, 1,
        "stale GLB hid the unavailable source"
    );
    assert_eq!(integration.bounds_updated, 0, "stale GLB supplied bounds");
    assert_eq!(report.pruned_texture_references, 0);
    assert!(!report.artifacts.iter().any(|path| path == Path::new(MESH)));
    assert!(!output.join(MESH).exists());
    let manifest = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
    assert_eq!(manifest.schema_version, CONVERTER_SCHEMA_VERSION);
    assert!(!manifest.entries.contains_key(SOURCE));
    assert!(manifest.pruned_texture_references.is_empty());
    let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    let bounded: u64 = db
        .query_row(
            "SELECT count(*) FROM statics WHERE bounds_valid=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bounded, 0, "published database retained stale mesh bounds");
    let published: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("integration-report.json")).unwrap()).unwrap();
    assert_eq!(published["bounds_updated"], 0);
    assert_eq!(published["unavailable_model_source_count"], 1);
}

#[tokio::test]
async fn v92_historical_prune_record_cannot_certify_a_removed_source() {
    for schema in [16, 19, 20, CONVERTER_SCHEMA_VERSION] {
        for with_prune_record in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let data = temp.path().join("Data");
            let output = temp.path().join("output");
            let staging = temp.path().join("output.staging-resume");
            generate_data(&data);
            fs::create_dir_all(staging.join("meshes")).unwrap();
            converter::mesh::MeshConverter::convert_nif_to_glb(
                &data.join(SOURCE),
                &staging.join(MESH),
            )
            .unwrap();
            fs::remove_file(data.join(SOURCE)).unwrap();
            fs::remove_file(data.join("Skyrim - Meshes.bsa")).unwrap();
            fs::create_dir_all(output.join("meshes")).unwrap();
            fs::copy(staging.join(MESH), output.join(MESH)).unwrap();
            let mut config = PipelineConfig::new(&data, &output);
            let mut manifest = ConversionManifest {
                schema_version: schema,
                complete: true,
                configuration_hash: configuration_hash_for_schema(&config, schema).unwrap(),
                ..Default::default()
            };
            if with_prune_record {
                manifest.pruned_texture_references.insert(
                    MESH.to_owned(),
                    BTreeSet::from([MISSING_TEXTURE.to_owned()]),
                );
            }
            manifest
                .save(&output.join("conversion-manifest.json"))
                .unwrap();
            config.resume_staging = Some(staging);
            let report = run(config).await;
            assert_removed_mesh_is_unavailable(&report, &output, schema);
        }
    }
}

#[tokio::test]
async fn v92_removed_source_invalidates_previously_verified_pruned_mesh() {
    for schema in [16, 19, 20, CONVERTER_SCHEMA_VERSION] {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Data");
        let output = temp.path().join("output");
        let staging = temp.path().join("output.staging-resume");
        generate_data(&data);
        let mut config = PipelineConfig::new(&data, &output);
        let first = run(config.clone()).await;
        assert!(first.complete);
        assert_eq!(first.integration.as_ref().unwrap().bounds_updated, 1);
        let mut manifest =
            ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
        assert_eq!(
            manifest.pruned_texture_references[MESH],
            BTreeSet::from([MISSING_TEXTURE.to_owned()])
        );
        fs::create_dir_all(staging.join("meshes")).unwrap();
        fs::copy(output.join(MESH), staging.join(MESH)).unwrap();
        let entry = &manifest.entries[SOURCE];
        StagingJournal::open(&staging)
            .unwrap()
            .record(
                SOURCE,
                &StagedOutput {
                    schema_version: schema,
                    configuration_hash: configuration_hash_for_schema(&config, schema).unwrap(),
                    source_hash: entry.source_hash.clone(),
                    output_size: entry.output_size,
                    output_hash: entry.output_hash.clone(),
                },
            )
            .unwrap();
        manifest.schema_version = schema;
        manifest.configuration_hash = if schema == CONVERTER_SCHEMA_VERSION {
            configuration_hash(&config).unwrap()
        } else {
            configuration_hash_for_schema(&config, schema).unwrap()
        };
        manifest
            .save(&output.join("conversion-manifest.json"))
            .unwrap();
        fs::remove_file(data.join(SOURCE)).unwrap();
        fs::remove_file(data.join("Skyrim - Meshes.bsa")).unwrap();
        config.resume_staging = Some(staging);
        let report = run(config).await;
        assert_removed_mesh_is_unavailable(&report, &output, schema);
    }
}
