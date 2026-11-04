//! Publication regressions for reader-independent archive and asset reuse.

use super::*;
use crate::config::RecordReader;
use dummy_content::{Entry, ba2, dds, esm, inhouse_perks as native, layout, rng::Rng};
use serde_json::Value;

const ARCHIVE_NAME: &str = "skyrim - review.ba2";
const SECOND_ARCHIVE_NAME: &str = "skyrim - secondreview.ba2";
const ARCHIVE_PAYLOAD: &[u8] = b"review archive payload";
const ROLE_TEXTURE_SOURCE: &str = "textures/review_role.dds";
const ROLE_TEXTURE_OUTPUT: &str = "textures/review_role.ktx2";
/// The last producer whose texture containers stored level data largest-first.
const PRODUCER_27: u32 = 27;

/// Keeps the generated input and published output alive throughout each regression.
struct Fixture {
    _directory: tempfile::TempDir,
    data: PathBuf,
    output: PathBuf,
}

impl Fixture {
    /// Generates one archive, optionally with a plugin and five native asset inputs.
    fn new(with_assets: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let output = directory.path().join("modern");
        fs::create_dir_all(&data).unwrap();
        if with_assets {
            layout::generate(
                &data,
                layout::DEFAULT_SEED,
                layout::Formats::parse("nif,pex,esm").unwrap(),
            )
            .unwrap();
            write_dds(&data, layout::GENERATED_DIFFUSE_PATH, dds::Format::Bc1Unorm);
            write_dds(&data, layout::GENERATED_NORMAL_PATH, dds::Format::Bc5Unorm);
        }
        write_archive(&data, ARCHIVE_NAME, ARCHIVE_PAYLOAD);
        Self {
            _directory: directory,
            data,
            output,
        }
    }

    /// Disables LOD and bounds conversion concurrency for every fixture run.
    fn config(&self, reader: RecordReader) -> PipelineConfig {
        let mut config = PipelineConfig::new(&self.data, &self.output);
        config.record_reader = reader;
        config.cpu_jobs = 2;
        config.no_lod = true;
        config
    }

    /// Resolves a disjoint new destination for explicit metadata publication.
    fn sibling_output(&self, name: &str) -> PathBuf {
        self.output.parent().unwrap().join(name)
    }
}

/// Writes a tiny native DDS whose payload and mip count do not require UASTC encoding.
fn write_dds(data: &Path, relative: &str, format: dds::Format) {
    let path = data.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        dds::generate(
            &dds::Spec::new(format, 8, 8).with_mip_levels(4),
            &mut Rng::new(0x0BCA_C4E0),
        )
        .unwrap(),
    )
    .unwrap();
}

/// Stores an opaque entry whose extraction counts are independent of asset conversion.
/// The entry is an opaque `.lod` input: `ConverterInputs` selects it and the
/// extractor copies it unparsed, so each archive still yields one converted file.
fn write_archive(data: &Path, name: &str, payload: &[u8]) {
    fs::write(
        data.join(name),
        ba2::general(
            &[Entry::new("lodsettings/review.lod", payload)],
            ba2::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
}

/// Runs the complete conversion, validation and pack-publication path.
async fn publish(config: PipelineConfig) -> PipelineReport {
    let report = publish_report(config).await;
    assert!(report.complete, "{:#?}", report.warnings);
    report
}

/// Publish usable records while allowing a fixture's deliberate recovery warnings.
async fn publish_report(config: PipelineConfig) -> PipelineReport {
    assert!(config.no_lod);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config, sender).await;
    drain.await.unwrap();
    let report = result.unwrap();
    assert_eq!(report.skipped, 0);
    report
}

/// Runs explicit metadata retention through its new-directory publication boundary.
async fn publish_metadata(config: PipelineConfig, source: &Path) -> PipelineReport {
    assert!(config.no_lod);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result = AssetPipeline::rebuild_metadata_async(config, source, sender).await;
    drain.await.unwrap();
    let report = result.unwrap();
    assert!(report.complete, "{:#?}", report.warnings);
    report
}

/// Reads raw published provenance without applying normal cache-eligibility filtering.
fn manifest(output: &Path) -> ConversionManifest {
    serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap()).unwrap()
}

/// Reads a published JSON artifact without assuming its optional-field layout.
fn json_artifact(output: &Path, name: &str) -> Value {
    serde_json::from_slice(&fs::read(output.join(name)).unwrap()).unwrap()
}

/// Changes only the full metadata identity, preserving any separately recorded asset keys.
fn drift_metadata_identity(output: &Path) {
    let path = output.join("conversion-manifest.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["configuration_hash"] = Value::String("different-full-reader-schema-identity".into());
    fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

/// Captures every manifest-backed asset so unchanged bytes can be checked after publication.
fn asset_bytes(output: &Path) -> BTreeMap<String, Vec<u8>> {
    manifest(output)
        .entries
        .values()
        .map(|entry| {
            (
                entry.output.clone(),
                fs::read(output.join(&entry.output)).unwrap(),
            )
        })
        .collect()
}

/// Checks retained asset bytes without depending on hard-link support or file timestamps.
fn assert_asset_bytes(output: &Path, expected: &BTreeMap<String, Vec<u8>>) {
    for (relative, bytes) in expected {
        assert_eq!(
            &fs::read(output.join(relative)).unwrap(),
            bytes,
            "{relative}"
        );
    }
}

/// Resolves the content-addressed persistent blob recorded for an archive's first entry.
fn archive_blob(config: &PipelineConfig, archive: &str) -> PathBuf {
    let entry = &manifest(&config.output_dir).archives[archive].files[0];
    config
        .ingestion_cache_dir()
        .join(".ingestion-cache/sha256")
        .join(&entry.hash[..2])
        .join(&entry.hash)
}

/// Adds a recoverably damaged optional bound field beside a complete synthetic static.
fn append_broken_stat_neighbors(data: &Path) {
    let mut bytes = fs::read(data.join("Skyrim.esm")).unwrap();
    for (id, name, bounds) in [
        (0x6000, "ReviewDamagedBounds", vec![0xA7; 11]),
        (0x6001, "ReviewUsableNeighbor", vec![0; 12]),
    ] {
        let payload = [
            native::subrecord(b"EDID", &native::text(name)),
            native::subrecord(b"OBND", &bounds),
            native::subrecord(b"MODL", &native::text(layout::GENERATED_MODEL_PATH)),
        ]
        .concat();
        bytes.extend(native::record(b"STAT", id, 0, &payload));
    }
    layout::write_plugin(data, &bytes).unwrap();
}

/// Reader switches preserve unchanged extraction and asset candidates while rebuilding world data.
#[tokio::test]
async fn reader_switch_reuses_archives_and_assets_but_rebuilds_database_and_cells() {
    let fixture = Fixture::new(true);
    append_broken_stat_neighbors(&fixture.data);
    let legacy = fixture.config(RecordReader::Legacy);
    let first = publish(legacy.clone()).await;
    assert_eq!(first.converted, 6);
    assert_eq!(first.cache_hits, 0);
    let expected = asset_bytes(&fixture.output);
    assert_eq!(expected.len(), 5);
    let original_archive = manifest(&fixture.output).archives[ARCHIVE_NAME].clone();
    let warm = publish(legacy).await;
    assert_eq!(warm.converted, 0);
    assert_eq!(warm.cache_hits, 6);

    let database = Connection::open(fixture.output.join("skyrim_world.db")).unwrap();
    database
        .execute_batch("CREATE TABLE rv_b_previous_database(value INTEGER); INSERT INTO rv_b_previous_database VALUES(73);")
        .unwrap();
    drop(database);
    fs::write(
        fixture.output.join("cell_cache.rkyv"),
        b"old cell cache sentinel",
    )
    .unwrap();

    let inhouse = fixture.config(RecordReader::Inhouse);
    let switched = publish_report(inhouse).await;
    assert!(!switched.complete);
    assert_eq!(switched.warnings.len(), 1, "{:?}", switched.warnings);
    assert!(switched.warnings[0].contains("skyrim.esm"));
    assert!(switched.warnings[0].contains("OBND"));
    assert!(!manifest(&fixture.output).complete);
    assert_eq!(
        switched.converted, 0,
        "reader identity must not re-encode unchanged assets or re-extract archives"
    );
    assert_eq!(switched.cache_hits, 6);
    assert_eq!(
        manifest(&fixture.output).archives[ARCHIVE_NAME],
        original_archive
    );
    assert_asset_bytes(&fixture.output, &expected);
    assert_eq!(
        json_artifact(&fixture.output, "record-reader.json"),
        crate::esm::inhouse::reader_identity(RecordReader::Inhouse)
    );
    let database = Connection::open(fixture.output.join("skyrim_world.db")).unwrap();
    let stale_tables: u32 = database
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='rv_b_previous_database'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale_tables, 0, "the database must be freshly rebuilt");
    let cells: u32 = database
        .query_row("SELECT count(*) FROM land", [], |row| row.get(0))
        .unwrap();
    assert_eq!(cells, 9);
    let published: Vec<u8> = database
        .query_row("SELECT data FROM records WHERE form_id=24576", [], |row| {
            row.get(0)
        })
        .unwrap();
    let recovered =
        rkyv::from_bytes::<crate::esm::types::ArchivedRecordData, rkyv::rancor::Error>(&published)
            .unwrap();
    assert!(
        recovered
            .subrecords
            .iter()
            .any(|field| field.tag == *b"MODL")
    );
    assert!(
        !recovered
            .subrecords
            .iter()
            .any(|field| field.tag == *b"OBND"),
        "malformed authored bounds must be omitted from canonical publication"
    );
    let neighbor: String = database
        .query_row("SELECT editor_id FROM statics WHERE id=24577", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(neighbor, "ReviewUsableNeighbor");
    let diagnostics = json_artifact(&fixture.output, "inhouse-reader-diagnostics.json");
    assert_eq!(diagnostics["decoder"]["skyrim.esm"]["skipped_fields"], 1);
    assert!(
        diagnostics["decoder"]["skyrim.esm"]["first_by_category"]["field"]
            .as_str()
            .unwrap()
            .contains("OBND")
    );
    crate::esm::cell_cache::validate_cell_cache(&fixture.output.join("cell_cache.rkyv")).unwrap();
    assert!(fixture.output.join("cell_cache_preserved.rkyv").is_file());
}

/// Known full legacy proofs retain DDS/PEX, but cannot prove the named NIF output contract.
#[tokio::test]
async fn known_legacy_full_hashes_reuse_dds_and_pex_but_regenerate_nif_without_kind_keys() {
    for contract in [
        "historical",
        "projection-1",
        "projection-2",
        "projection-3",
        "projection-3-nonenglish",
        "current",
        "unproven",
    ] {
        let fixture = Fixture::new(true);
        append_broken_stat_neighbors(&fixture.data);
        let mut config = fixture.config(RecordReader::Legacy);
        if contract == "projection-3-nonenglish" {
            config.language = "french".to_owned();
        }
        publish(config.clone()).await;
        let expected = asset_bytes(&fixture.output);
        let mut original = manifest(&fixture.output);
        let archive_proof = original.archives.clone();
        original.asset_configuration_hashes.clear();
        original.configuration_hash = match contract {
            "historical" => {
                configuration_hash_for_schema(&config, CONVERTER_SCHEMA_VERSION).unwrap()
            }
            "projection-1" => crate::cache::legacy_projection_v1_configuration_hash(
                &config,
                CONVERTER_SCHEMA_VERSION,
            )
            .unwrap(),
            "projection-2" => crate::cache::legacy_projection_v2_configuration_hash(
                &config,
                CONVERTER_SCHEMA_VERSION,
            )
            .unwrap(),
            "projection-3" | "projection-3-nonenglish" => {
                crate::cache::legacy_projection_v3_configuration_hash(
                    &config,
                    CONVERTER_SCHEMA_VERSION,
                )
                .unwrap()
            }
            "current" => configuration_hash(&config).unwrap(),
            _ => "unproven-settings".into(),
        };
        original
            .save(&fixture.output.join("conversion-manifest.json"))
            .unwrap();
        let source_reader = match contract {
            "historical" => Some(serde_json::json!({"mode": "legacy"})),
            "projection-1" => Some(serde_json::json!({"mode": "legacy", "projection_version": 1})),
            "projection-2" => Some(serde_json::json!({"mode": "legacy", "projection_version": 2})),
            "projection-3" | "projection-3-nonenglish" => {
                Some(serde_json::json!({"mode": "legacy", "projection_version": 3}))
            }
            _ => None,
        };
        if let Some(source_reader) = source_reader {
            fs::write(
                fixture.output.join("record-reader.json"),
                serde_json::to_vec(&source_reader).unwrap(),
            )
            .unwrap();
        }
        let report = publish(config.clone()).await;
        let rejected = matches!(contract, "unproven" | "projection-3-nonenglish");
        assert_eq!(report.converted, if rejected { 5 } else { 1 });
        assert_eq!(report.cache_hits, if rejected { 1 } else { 5 });
        assert_asset_bytes(&fixture.output, &expected);
        let published = manifest(&fixture.output);
        assert_eq!(published.archives, archive_proof);
        assert_eq!(
            published.configuration_hash,
            configuration_hash(&config).unwrap()
        );
        assert_eq!(
            json_artifact(&fixture.output, "record-reader.json"),
            crate::esm::inhouse::reader_identity_in_language(
                RecordReader::Legacy,
                &config.language
            )
        );
        let database = Connection::open(fixture.output.join("skyrim_world.db")).unwrap();
        let neighbor: String = database
            .query_row("SELECT editor_id FROM statics WHERE id=24577", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(neighbor, "ReviewUsableNeighbor");
    }
}

/// An earlier adapter rebuilds metadata while verified asset and archive proofs remain usable.
#[tokio::test]
async fn metadata_identity_drift_preserves_verified_assets_and_archives() {
    let fixture = Fixture::new(true);
    let config = fixture.config(RecordReader::Inhouse);
    assert_eq!(publish(config.clone()).await.converted, 6);
    let expected = asset_bytes(&fixture.output);
    let original_archives = manifest(&fixture.output).archives;
    drift_metadata_identity(&fixture.output);
    let mut prior_reader = json_artifact(&fixture.output, "record-reader.json");
    prior_reader["adapter_version"] = serde_json::json!(11);
    fs::write(
        fixture.output.join("record-reader.json"),
        serde_json::to_vec_pretty(&prior_reader).unwrap(),
    )
    .unwrap();
    let database = Connection::open(fixture.output.join("skyrim_world.db")).unwrap();
    database
        .execute_batch("CREATE TABLE rv_b_previous_adapter(value INTEGER); INSERT INTO rv_b_previous_adapter VALUES(11);")
        .unwrap();
    drop(database);
    let stale_cache = b"adapter-11 stale terrain cache";
    for name in ["cell_cache.rkyv", "cell_cache_preserved.rkyv"] {
        fs::write(fixture.output.join(name), stale_cache).unwrap();
    }

    let rebuilt = publish(config).await;
    assert_eq!(
        rebuilt.converted, 0,
        "a metadata-only identity change must retain asset and archive candidates"
    );
    assert_eq!(rebuilt.cache_hits, 6);
    assert_eq!(manifest(&fixture.output).archives, original_archives);
    assert_asset_bytes(&fixture.output, &expected);
    assert_eq!(
        json_artifact(&fixture.output, "record-reader.json"),
        crate::esm::inhouse::reader_identity(RecordReader::Inhouse)
    );
    let database = Connection::open(fixture.output.join("skyrim_world.db")).unwrap();
    let stale_tables: u32 = database
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='rv_b_previous_adapter'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale_tables, 0, "the database must be freshly rebuilt");
    for name in ["cell_cache.rkyv", "cell_cache_preserved.rkyv"] {
        assert_ne!(fs::read(fixture.output.join(name)).unwrap(), stale_cache);
    }
}

/// Asset encoder settings do not alter the bytes an archive extractor must restore.
#[tokio::test]
async fn changed_texture_settings_keep_unchanged_archive_extraction_hits() {
    let fixture = Fixture::new(false);
    let mut config = fixture.config(RecordReader::Legacy);
    assert_eq!(publish(config.clone()).await.converted, 1);
    config.texture_zstd_level = 0;

    let changed = publish(config.clone()).await;
    assert_eq!(
        changed.converted, 0,
        "encoder settings do not affect archive extraction"
    );
    assert_eq!(changed.cache_hits, 1);
    let warm = publish(config).await;
    assert_eq!(warm.converted, 0);
    assert_eq!(warm.cache_hits, 1);
}

/// Retained archive candidates still require the source hash, including after a reader change.
#[tokio::test]
async fn changed_archive_source_misses_while_unchanged_archive_survives_reader_switch() {
    let fixture = Fixture::new(false);
    write_archive(
        &fixture.data,
        SECOND_ARCHIVE_NAME,
        b"second unaffected archive",
    );
    assert_eq!(
        publish(fixture.config(RecordReader::Legacy))
            .await
            .converted,
        2
    );
    let before = manifest(&fixture.output).archives;
    let replacement = b"mutant archive payload";
    write_archive(&fixture.data, ARCHIVE_NAME, replacement);
    let config = fixture.config(RecordReader::Inhouse);

    let changed = publish(config.clone()).await;
    assert_eq!(
        changed.converted, 1,
        "only the changed archive is extracted again"
    );
    assert_eq!(changed.cache_hits, 1);
    let after = manifest(&fixture.output).archives;
    assert_ne!(
        after[ARCHIVE_NAME].source_hash,
        before[ARCHIVE_NAME].source_hash
    );
    assert_eq!(after[SECOND_ARCHIVE_NAME], before[SECOND_ARCHIVE_NAME]);
    assert_eq!(
        after[ARCHIVE_NAME].source_hash,
        hash_file(&fixture.data.join(ARCHIVE_NAME)).unwrap()
    );
    assert_eq!(
        fs::read(archive_blob(&config, ARCHIVE_NAME)).unwrap(),
        replacement
    );
}

/// Same-size corrupted blobs miss integrity verification while unaffected retained archives hit.
#[tokio::test]
async fn corrupted_archive_blob_misses_while_unchanged_archive_survives_config_change() {
    let fixture = Fixture::new(false);
    write_archive(
        &fixture.data,
        SECOND_ARCHIVE_NAME,
        b"second unaffected archive",
    );
    let mut config = fixture.config(RecordReader::Legacy);
    assert_eq!(publish(config.clone()).await.converted, 2);
    let corrupt = vec![b'!'; ARCHIVE_PAYLOAD.len()];
    fs::write(archive_blob(&config, ARCHIVE_NAME), &corrupt).unwrap();
    config.texture_zstd_level = 0;
    assert!(config.verify_cache);

    let rebuilt = publish(config.clone()).await;
    // Sealed packs are the authoritative archive copy. The damaged per-file blob
    // fails hash verification and is never trusted, so the archive restores from
    // its verified pack instead of being extracted again.
    assert_eq!(
        rebuilt.converted, 0,
        "a corrupted derived blob must not force an extraction"
    );
    assert_eq!(rebuilt.cache_hits, 2, "both archives remain reusable");
    let entry = &manifest(&config.output_dir).archives[ARCHIVE_NAME].files[0];
    assert_eq!(
        hash_file(&archive_blob(&config, ARCHIVE_NAME)).unwrap(),
        entry.hash,
        "the damaged blob is repaired from the verified pack"
    );
}

/// Explicit invalidation continues to force extraction even when the source is unchanged.
#[tokio::test]
async fn explicit_invalidation_discards_archive_candidates() {
    let fixture = Fixture::new(false);
    let mut config = fixture.config(RecordReader::Inhouse);
    assert_eq!(publish(config.clone()).await.converted, 1);
    assert_eq!(publish(config.clone()).await.cache_hits, 1);
    config.invalidate_cache = true;

    let invalidated = publish(config).await;
    assert_eq!(invalidated.converted, 1);
    assert_eq!(invalidated.cache_hits, 0);
}

/// Publishes a plugin-only texture role edit without changing any texture or model input bytes.
fn write_role_plugin(data: &Path, slot: usize) {
    let mut bytes = esm::plugin(&esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    let mut slots = [None; 8];
    slots[slot] = Some(ROLE_TEXTURE_SOURCE);
    bytes.extend_from_slice(&esm::texture_set(0x5000, slots).unwrap());
    layout::write_plugin(data, &bytes).unwrap();
}

/// A changed resolved texture role rebuilds its DDS while independent outputs remain cache hits.
#[tokio::test]
async fn texture_role_change_after_metadata_drift_rebuilds_only_affected_dds() {
    let fixture = Fixture::new(true);
    write_dds(&fixture.data, ROLE_TEXTURE_SOURCE, dds::Format::Bc1Unorm);
    write_role_plugin(&fixture.data, 3);
    let config = fixture.config(RecordReader::Inhouse);
    assert_eq!(publish(config.clone()).await.converted, 7);
    let before = manifest(&fixture.output);
    let mut expected = asset_bytes(&fixture.output);
    let old_texture = expected.remove(ROLE_TEXTURE_OUTPUT).unwrap();
    let dds_hash = hash_file(&fixture.data.join(ROLE_TEXTURE_SOURCE)).unwrap();
    write_role_plugin(&fixture.data, 4);
    drift_metadata_identity(&fixture.output);

    let changed = publish(config).await;
    assert_eq!(
        changed.converted, 1,
        "only the DDS whose encoding changed must rebuild"
    );
    assert_eq!(changed.cache_hits, 6);
    assert_asset_bytes(&fixture.output, &expected);
    assert_ne!(
        fs::read(fixture.output.join(ROLE_TEXTURE_OUTPUT)).unwrap(),
        old_texture
    );
    assert_eq!(
        hash_file(&fixture.data.join(ROLE_TEXTURE_SOURCE)).unwrap(),
        dds_hash
    );
    let after = manifest(&fixture.output);
    assert_ne!(
        after.entries[ROLE_TEXTURE_SOURCE].source_hash,
        before.entries[ROLE_TEXTURE_SOURCE].source_hash
    );
    assert_eq!(after.archives, before.archives);
    let database = Connection::open(fixture.output.join("skyrim_world.db")).unwrap();
    let height: String = database
        .query_row(
            "SELECT height_path FROM texture_sets WHERE id=20480",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(height, ROLE_TEXTURE_SOURCE);
}

/// Repeated metadata publication preserves original reader, settings, producer and asset bytes.
#[tokio::test]
async fn repeated_metadata_migrations_preserve_recorded_asset_producer() {
    let fixture = Fixture::new(true);
    let legacy = fixture.config(RecordReader::Legacy);
    publish(legacy.clone()).await;
    let original = manifest(&fixture.output);
    let expected = asset_bytes(&fixture.output);
    let original_reader = json_artifact(&fixture.output, "record-reader.json");
    let first_output = fixture.sibling_output("metadata-inhouse");
    let second_output = fixture.sibling_output("metadata-legacy-again");
    let mut first_config = fixture.config(RecordReader::Inhouse);
    first_config.output_dir = first_output.clone();
    publish_metadata(first_config, &fixture.output).await;
    let mut second_config = legacy;
    second_config.output_dir = second_output.clone();
    publish_metadata(second_config, &first_output).await;

    for output in [&first_output, &second_output] {
        let retained = manifest(output);
        assert_eq!(
            retained.retained_mesh_schema_version,
            Some(original.schema_version)
        );
        assert_eq!(
            retained.retained_asset_configuration_hash.as_deref(),
            Some(original.configuration_hash.as_str())
        );
        assert_eq!(
            retained.asset_configuration_hashes, original.asset_configuration_hashes,
            "metadata publication must retain the original per-kind settings"
        );
        assert!(retained.archives.is_empty());
        assert_asset_bytes(output, &expected);
        let provenance = json_artifact(output, "metadata-rebuild.json");
        assert_eq!(provenance["retained_asset_record_reader"], original_reader);
        assert_eq!(
            provenance["source_retained_mesh_schema"],
            original.schema_version
        );
        assert_eq!(
            provenance["retained_asset_configuration_hash"],
            original.configuration_hash
        );
    }
    assert_eq!(
        json_artifact(&first_output, "record-reader.json")["mode"],
        "inhouse"
    );
    assert_eq!(
        json_artifact(&second_output, "record-reader.json")["mode"],
        "legacy"
    );
}

/// Historical reader proofs and a fresh NIF kind proof survive repeated metadata publications.
#[tokio::test]
async fn historical_legacy_reader_identity_with_current_nif_proof_survives_repeated_metadata_migrations()
 {
    for projection_version in [None, Some(1), Some(2), Some(3)] {
        let fixture = Fixture::new(true);
        let legacy = fixture.config(RecordReader::Legacy);
        publish(legacy.clone()).await;
        let expected = asset_bytes(&fixture.output);
        let mut original = manifest(&fixture.output);
        original.configuration_hash = match projection_version {
            None => configuration_hash_for_schema(&legacy, CONVERTER_SCHEMA_VERSION).unwrap(),
            Some(1) => crate::cache::legacy_projection_v1_configuration_hash(
                &legacy,
                CONVERTER_SCHEMA_VERSION,
            )
            .unwrap(),
            Some(2) => crate::cache::legacy_projection_v2_configuration_hash(
                &legacy,
                CONVERTER_SCHEMA_VERSION,
            )
            .unwrap(),
            Some(3) => crate::cache::legacy_projection_v3_configuration_hash(
                &legacy,
                CONVERTER_SCHEMA_VERSION,
            )
            .unwrap(),
            _ => unreachable!(),
        };
        // Retain the fresh mesh contract proof; historical combined proofs still cover DDS/PEX.
        original
            .asset_configuration_hashes
            .retain(|kind, _| kind == "nif");
        assert_eq!(
            original
                .asset_configuration_hashes
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["nif"]
        );
        original
            .save(&fixture.output.join("conversion-manifest.json"))
            .unwrap();
        let original_reader = match projection_version {
            Some(version) => serde_json::json!({"mode": "legacy", "projection_version": version}),
            None => serde_json::json!({"mode": "legacy"}),
        };
        fs::write(
            fixture.output.join("record-reader.json"),
            serde_json::to_vec(&original_reader).unwrap(),
        )
        .unwrap();
        let source_hash = hash_file(&fixture.output.join("conversion-manifest.json")).unwrap();

        let first_output = fixture.sibling_output("old-legacy-to-inhouse");
        let second_output = fixture.sibling_output("old-legacy-to-versioned-legacy");
        let mut first_config = fixture.config(RecordReader::Inhouse);
        first_config.output_dir = first_output.clone();
        publish_metadata(first_config, &fixture.output).await;
        let mut second_config = legacy;
        second_config.output_dir = second_output.clone();
        publish_metadata(second_config.clone(), &first_output).await;
        for output in [&first_output, &second_output] {
            let retained = manifest(output);
            assert_eq!(
                retained.retained_mesh_schema_version,
                Some(CONVERTER_SCHEMA_VERSION)
            );
            assert_eq!(
                retained.retained_asset_configuration_hash.as_deref(),
                Some(original.configuration_hash.as_str())
            );
            assert_eq!(
                retained.asset_configuration_hashes, original.asset_configuration_hashes,
                "metadata retention must preserve only the original fresh NIF proof"
            );
            let provenance = json_artifact(output, "metadata-rebuild.json");
            assert_eq!(provenance["retained_asset_record_reader"], original_reader);
            assert_eq!(
                provenance["retained_asset_configuration_hash"],
                original.configuration_hash
            );
            assert_asset_bytes(output, &expected);
        }
        assert_eq!(
            hash_file(&fixture.output.join("conversion-manifest.json")).unwrap(),
            source_hash
        );
        assert_eq!(
            json_artifact(&second_output, "record-reader.json"),
            crate::esm::inhouse::reader_identity(RecordReader::Legacy)
        );
        assert_ne!(
            manifest(&second_output).configuration_hash,
            original.configuration_hash
        );
        let report = publish(second_config).await;
        assert_eq!(
            report.converted, 1,
            "metadata retention did not copy the archive cache"
        );
        assert_eq!(
            report.cache_hits, 5,
            "original current-producer asset settings remain proven"
        );
        assert_asset_bytes(&second_output, &expected);
    }
}

/// Explicit historical byte retention does not upgrade old meshes or textures into normal hits.
#[tokio::test]
async fn pre_producer_28_metadata_sources_are_rejected_before_relabeling() {
    let fixture = Fixture::new(true);
    let legacy = fixture.config(RecordReader::Legacy);
    publish(legacy.clone()).await;
    for schema in [16, PRODUCER_27] {
        let mut original = manifest(&fixture.output);
        original.schema_version = schema;
        original.asset_configuration_hashes.clear();
        original.configuration_hash = configuration_hash_for_schema(&legacy, schema).unwrap();
        fs::write(
            fixture.output.join("record-reader.json"),
            br#"{"mode":"legacy"}"#,
        )
        .unwrap();
        original
            .save(&fixture.output.join("conversion-manifest.json"))
            .unwrap();
        let rejected_output = fixture.sibling_output(&format!("rejected-schema-{schema}"));
        let mut config = fixture.config(RecordReader::Inhouse);
        config.output_dir = rejected_output.clone();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
        let error = AssetPipeline::rebuild_metadata_async(config, &fixture.output, sender)
            .await
            .unwrap_err()
            .to_string();
        drain.await.unwrap();
        assert!(
            error.contains("predate the current texture layout")
                && error.contains("normal conversion"),
            "schema {schema}: {error}"
        );
        assert!(
            !rejected_output.exists(),
            "schema {schema} published output"
        );
    }
}

/// A current pack whose GLBs came from producer 27 cannot prove the named mesh contracts, so a
/// metadata rebuild rejects the retained meshes and requires the normal route to regenerate them.
#[tokio::test]
async fn producer_27_retained_meshes_are_rejected_by_metadata_rebuild() {
    let fixture = Fixture::new(true);
    let config = fixture.config(RecordReader::Legacy);
    publish(config.clone()).await;
    let mut source = manifest(&fixture.output);
    source.retained_mesh_schema_version = Some(PRODUCER_27);
    source.retained_asset_configuration_hash =
        Some(crate::cache::current_configuration_hash_for_schema(&config, PRODUCER_27).unwrap());
    source.asset_configuration_hashes.insert(
        "nif".into(),
        asset_configuration_hash_for_schema(&config, PRODUCER_27, "nif").unwrap(),
    );
    source
        .save(&fixture.output.join("conversion-manifest.json"))
        .unwrap();
    let rebuilt_output = fixture.sibling_output("producer-27-retained-meshes");
    let mut rebuild_config = config.clone();
    rebuild_config.output_dir = rebuilt_output.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let error = AssetPipeline::rebuild_metadata_async(rebuild_config, &fixture.output, sender)
        .await
        .unwrap_err()
        .to_string();
    drain.await.unwrap();
    assert!(
        error.contains("unsupported retained mesh cache contract"),
        "{error}"
    );
    assert!(!rebuilt_output.exists());
}
/// An unknown retained script producer cannot gain eligibility from current metadata.
/// Expected baseline red: the two script outputs are reused instead of rebuilt.
#[tokio::test]
async fn unknown_retained_script_producer_does_not_gain_current_metadata_eligibility() {
    let fixture = Fixture::new(true);
    let config = fixture.config(RecordReader::Legacy);
    let first = publish(config.clone()).await;
    let mut recorded = manifest(&fixture.output);
    let expected = asset_bytes(&fixture.output);
    let original_archives = recorded.archives.clone();
    let expected_asset_conversions = recorded.entries.len() as u64;
    let archive_files: u64 = recorded
        .archives
        .values()
        .map(|entry| entry.files.len() as u64)
        .sum();
    assert_eq!(first.converted, expected_asset_conversions + archive_files);
    assert!(
        recorded
            .entries
            .values()
            .any(|entry| entry.output.ends_with(".luau"))
    );
    assert!(
        25 <= recorded.schema_version,
        "the test isolates original producer policy"
    );

    // Producer 25 is outside the existing known script set 12..24 and 26 (current 27) but
    // below the metadata stamp, so rejecting producer>metadata cannot hide a
    // mistaken decision based only on the metadata producer. Preserve any new
    // kind map; its script ABI proof deliberately still matches.
    recorded.retained_mesh_schema_version = Some(25);
    recorded.retained_asset_configuration_hash =
        Some(configuration_hash_for_schema(&config, 25).unwrap());
    recorded
        .save(&fixture.output.join("conversion-manifest.json"))
        .unwrap();

    let rebuilt = publish(config.clone()).await;
    assert_eq!(
        rebuilt.converted, expected_asset_conversions,
        "every asset, including old scripts, must rebuild"
    );
    assert_eq!(
        rebuilt.cache_hits, archive_files,
        "only independently verified archive extraction reuses"
    );
    assert_asset_bytes(&fixture.output, &expected);
    let current = manifest(&fixture.output);
    assert_eq!(current.archives, original_archives);
    assert_eq!(current.schema_version, CONVERTER_SCHEMA_VERSION);
    assert_eq!(current.retained_mesh_schema_version, None);
    assert_eq!(current.retained_asset_configuration_hash, None);
    assert_eq!(
        current.configuration_hash,
        configuration_hash(&config).unwrap()
    );
}

/// Missing retained producer provenance cannot be replaced by matching kind keys.
/// Expected baseline red: unrelated archive extraction is also discarded.
#[tokio::test]
async fn unproven_metadata_retention_rebuilds_assets_and_preserves_archive_proof() {
    let fixture = Fixture::new(true);
    let config = fixture.config(RecordReader::Legacy);
    publish(config.clone()).await;
    let recorded = manifest(&fixture.output);
    let expected = asset_bytes(&fixture.output);
    let expected_asset_conversions = recorded.entries.len() as u64;
    let archive_files: u64 = recorded
        .archives
        .values()
        .map(|entry| entry.files.len() as u64)
        .sum();
    assert_eq!(recorded.retained_mesh_schema_version, None);
    assert_eq!(recorded.retained_asset_configuration_hash, None);

    // Normal conversion tests the marker's existence. The map, when present,
    // still declares current settings but cannot establish original producers.
    fs::write(fixture.output.join("metadata-rebuild.json"), b"{}").unwrap();
    let rebuilt = publish(config.clone()).await;
    assert_eq!(rebuilt.converted, expected_asset_conversions);
    assert_eq!(rebuilt.cache_hits, archive_files);
    assert_asset_bytes(&fixture.output, &expected);
    let current = manifest(&fixture.output);
    assert_eq!(current.archives, recorded.archives);
    assert_eq!(current.retained_mesh_schema_version, None);
    assert_eq!(current.retained_asset_configuration_hash, None);
    assert!(!fixture.output.join("metadata-rebuild.json").exists());
    assert_eq!(
        current.configuration_hash,
        configuration_hash(&config).unwrap()
    );
}

/// Unknown output producers retain archive candidates through ordinary publication.
/// Expected baseline red: loader clears otherwise reusable archive ingestion.
#[tokio::test]
async fn unknown_output_producer_rebuilds_assets_without_reextracting_unchanged_archives() {
    let fixture = Fixture::new(true);
    let config = fixture.config(RecordReader::Legacy);
    publish(config.clone()).await;
    let mut recorded = manifest(&fixture.output);
    let expected = asset_bytes(&fixture.output);
    let expected_asset_conversions = recorded.entries.len() as u64;
    let archive_files: u64 = recorded
        .archives
        .values()
        .map(|entry| entry.files.len() as u64)
        .sum();
    recorded.schema_version = CONVERTER_SCHEMA_VERSION + 1;
    recorded.configuration_hash =
        configuration_hash_for_schema(&config, recorded.schema_version).unwrap();
    recorded
        .save(&fixture.output.join("conversion-manifest.json"))
        .unwrap();

    let rebuilt = publish(config).await;
    assert_eq!(rebuilt.converted, expected_asset_conversions);
    assert_eq!(rebuilt.cache_hits, archive_files);
    assert_asset_bytes(&fixture.output, &expected);
    let current = manifest(&fixture.output);
    assert_eq!(current.archives, recorded.archives);
    assert_eq!(current.schema_version, CONVERTER_SCHEMA_VERSION);
}
