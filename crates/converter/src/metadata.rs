//! Rebuild world metadata from package-matched plugins without reconverting assets.
//!
//! Retained models, textures and scripts are verified against the source package's
//! manifest, not refreshed from current Data. Data supplies checksum-matched plugins,
//! LOD settings and terrain diffuse inputs. Use normal conversion after asset changes.

use crate::{
    AssetPipeline, PipelineConfig, PipelineReport, ProgressEvent, ProgressStage,
    archive::ArchiveExtractor,
    asset_path::{AssetKind, canonical_asset_path, resolve_asset_uri},
    cache::{
        CONVERTER_SCHEMA_VERSION, ConversionManifest, can_reuse_meshes_for_producer,
        configuration_hash, hash_bytes, hash_file, retained_configuration_matches_for_reader,
    },
    esm::{EsmParser, cell_cache::write_cell_cache, exporter::validate_database},
    integration::finalize_world_database,
    lod::albedo::terrain_diffuse_paths,
    mesh::{MeshConverter, nif_source_hash, nif_unrepaired_source_hash, prune_glb_texture_bytes},
    pipeline::{
        LodWorkerOwnership, archive_load_order_priority, compile_lod_chunks,
        conversion_is_complete, discover, installed_archive_owners, non_mo2_archive_is_active,
        overlay_loose_assets, publish_new_directory, report_projection_omissions,
        sort_archives_by_load_order, staging_path, validate_artifacts,
        write_streaming_cost_catalog,
    },
    records::addons::RuntimeSchemas,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use shared::asset_lock::AssetLock;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tokio::sync::mpsc::Sender;
use walkdir::WalkDir;

#[derive(Serialize)]
struct MetadataProvenance {
    record_reader: serde_json::Value,
    retained_asset_record_reader: serde_json::Value,
    /// Exact original add-on documents for the retained asset producer, across generations.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    retained_asset_schema_json: Vec<Vec<u8>>,
    source_converter_schema: u32,
    source_retained_mesh_schema: u32,
    source_manifest_hash: String,
    source_configuration_hash: String,
    retained_asset_configuration_hash: String,
    retained_asset_hashes: BTreeMap<String, String>,
    replay_verified_pruned_assets: BTreeSet<String>,
}

/// Frozen source and original-asset schema facts validated before staging is created.
struct SourceMetadata<'a> {
    manifest_bytes: &'a [u8],
    manifest: &'a mut ConversionManifest,
    retained_reader: &'a serde_json::Value,
    retained_schemas: &'a RuntimeSchemas,
}

impl AssetPipeline {
    /// Publishes only to a new, disjoint directory. The source package and its
    /// retained GLB/KTX2/Luau bytes are never modified or relabeled in place.
    /// MO2 source selection is unsupported by this package-matched Data route.
    /// Reader data-loss warnings keep the rebuilt report and manifest incomplete;
    /// successful compatibility notices do not affect their shared completeness rule.
    pub async fn rebuild_metadata_async(
        mut config: PipelineConfig,
        source_assets: &Path,
        progress_tx: Sender<ProgressEvent>,
    ) -> Result<PipelineReport> {
        ensure!(
            config.mo2.is_none(),
            "metadata rebuild does not support MO2 selection; use normal conversion"
        );
        config.validate()?;
        config.freeze_schema_addons()?;
        ensure!(
            config.resume_staging.is_none()
                && !config.invalidate_cache
                && config.verify_cache
                && config.plugins_file.is_none(),
            "metadata rebuild requires recorded plugin order and cannot resume, invalidate, or bypass asset verification"
        );
        let started = Instant::now();
        let source = fs::canonicalize(source_assets)?;
        config.data_dir = fs::canonicalize(&config.data_dir)?;
        config.output_dir = new_output_path(&config.output_dir, &source, &config.data_dir)?;
        // Both leases stay with the rebuild until publication; the terrain worker holds clones.
        let source_lock = Arc::new(AssetLock::acquire_shared(&source)?);
        let output_lock = Arc::new(AssetLock::acquire_exclusive(&config.output_dir)?);
        let source_manifest = fs::read(source.join("conversion-manifest.json"))?;
        // Read the original version; normal cache loading deliberately invalidates
        // incompatible assets during migration, which this explicit route verifies instead.
        let mut manifest: ConversionManifest = serde_json::from_slice(&source_manifest)?;
        let source_reader = source_reader_identity(&source)?;
        let retained_reader = retained_reader_identity(&source, &source_reader)?;
        let source_schemas = source_reader_schemas(&source, &source_reader)?;
        ensure!(
            source_schemas.identities() == manifest.schema_addons,
            "source schema add-on manifest differs from its database provenance"
        );
        let retained_schemas = retained_reader_schemas(&source, &retained_reader, &source_schemas)?;
        let source_settings = settings_for_source_reader(&config, &source_reader, source_schemas)?;
        let retained_schema_documents = retained_schemas.addon_bytes().collect::<Vec<_>>();
        let retained_settings = settings_for_source_reader(
            &config,
            &retained_reader,
            RuntimeSchemas::from_addon_bytes(&retained_schema_documents)
                .map_err(color_eyre::eyre::Error::msg)?,
        )?;
        // Metadata rebuild copies retained KTX2 bytes verbatim and then stamps the current
        // schema. Only current-producer textures carry the current layout, so an earlier
        // producer's texture bytes cannot be relabeled here; they need a normal conversion.
        ensure!(
            manifest.schema_version == CONVERTER_SCHEMA_VERSION,
            "metadata rebuild requires converter schema {CONVERTER_SCHEMA_VERSION} textures, but this source is schema {}; \
             its KTX2 textures predate the current texture layout. Run normal conversion to regenerate them",
            manifest.schema_version
        );
        ensure!(
            manifest.complete && manifest.failures.is_empty(),
            "metadata rebuild requires complete converter schema {CONVERTER_SCHEMA_VERSION} assets with no failures"
        );
        ensure!(
            manifest.retained_mesh_schema_version.is_some()
                || !source.join("metadata-rebuild.json").exists(),
            "missing retained mesh provenance in metadata-only source; rebuild from verified original assets"
        );
        let mesh_schema = manifest
            .retained_mesh_schema_version
            .unwrap_or(manifest.schema_version);
        // Only the explicit mesh pairs in `can_reuse_meshes_for_producer` survive a
        // producer change, so producers 24 and 26 cannot reach the current schema here.
        ensure!(
            (mesh_schema == CONVERTER_SCHEMA_VERSION
                || can_reuse_meshes_for_producer(mesh_schema, CONVERTER_SCHEMA_VERSION))
                && mesh_schema <= manifest.schema_version,
            "unsupported retained mesh cache contract"
        );
        ensure!(
            manifest.retained_mesh_schema_version.is_none()
                || manifest.retained_asset_configuration_hash.is_some(),
            "missing retained producer configuration; rebuild from verified original assets"
        );
        ensure!(
            retained_configuration_matches_for_reader(
                &source_settings,
                manifest.schema_version,
                &manifest.configuration_hash,
                &source_reader,
            )?,
            "retained asset configuration does not match rebuild settings"
        );
        let retained_configuration = manifest
            .retained_asset_configuration_hash
            .as_deref()
            .unwrap_or(&manifest.configuration_hash);
        ensure!(
            retained_configuration_matches_for_reader(
                &retained_settings,
                mesh_schema,
                retained_configuration,
                &retained_reader,
            )?,
            "retained producer configuration does not match rebuild settings"
        );
        let plugins = matched_plugins(&source, &config.data_dir)?;
        let staging = staging_path(&config.output_dir);
        fs::create_dir(&staging)?;
        let result = rebuild_into(
            &config,
            &source,
            &staging,
            &plugins,
            SourceMetadata {
                manifest_bytes: &source_manifest,
                manifest: &mut manifest,
                retained_reader: &retained_reader,
                retained_schemas: &retained_schemas,
            },
            &progress_tx,
            Arc::new(LodWorkerOwnership {
                _runtime: vec![source_lock.clone(), output_lock.clone()],
                _repair: None,
            }),
        )
        .await;
        let mut report = match result {
            Ok(report) => report,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        // The rebuilt manifest is final here; the catalog must describe exactly those bytes.
        // Like the normal route, a catalog failure fails the rebuild before publication.
        if let Err(error) = write_streaming_cost_catalog(&staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        report.artifacts.push(PathBuf::from(
            shared::streaming_costs::STREAMING_COST_FILE_NAME,
        ));
        let publication_started = Instant::now();
        if let Err(error) = publish_new_directory(&staging, &config.output_dir, &output_lock) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        report.publication_elapsed_ms = publication_started.elapsed().as_millis();
        report.elapsed_ms = started.elapsed().as_millis();
        Ok(report)
    }
}

/// Original packages predate reader stamps and were produced by the legacy frontend.
fn source_reader_identity(source: &Path) -> Result<serde_json::Value> {
    let path = source.join("record-reader.json");
    if path.is_file() {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    } else {
        // Absence is the historical unversioned contract, never the current projection.
        Ok(serde_json::json!({"mode": "legacy"}))
    }
}

/// Retained bytes keep their producer identity across repeated metadata rebuilds.
fn retained_reader_identity(
    source: &Path,
    current: &serde_json::Value,
) -> Result<serde_json::Value> {
    let path = source.join("metadata-rebuild.json");
    if path.is_file() {
        let provenance: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        if let Some(reader) = provenance.get("retained_asset_record_reader") {
            return Ok(reader.clone());
        }
        ensure!(
            current == &serde_json::json!({"mode": "legacy"}),
            "missing retained asset reader provenance"
        );
    }
    Ok(current.clone())
}

/// Reconstruct current source schemas from the pack rather than requested output add-ons.
fn source_reader_schemas(source: &Path, identity: &serde_json::Value) -> Result<RuntimeSchemas> {
    let reader: crate::config::RecordReader = serde_json::from_value(identity["mode"].clone())?;
    if reader == crate::config::RecordReader::Legacy {
        return RuntimeSchemas::from_addon_bytes(&[]).map_err(color_eyre::eyre::Error::msg);
    }
    let conn = Connection::open_with_flags(
        source.join("skyrim_world.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    crate::records::history::stored_schemas(&conn)
}

/// Original asset schemas remain separate from later rebuilt metadata schema snapshots.
fn retained_reader_schemas(
    source: &Path,
    identity: &serde_json::Value,
    current: &RuntimeSchemas,
) -> Result<RuntimeSchemas> {
    let path = source.join("metadata-rebuild.json");
    if !path.is_file() {
        let documents = current.addon_bytes().collect::<Vec<_>>();
        return RuntimeSchemas::from_addon_bytes(&documents).map_err(color_eyre::eyre::Error::msg);
    }
    let provenance: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let documents: Vec<Vec<u8>> = if let Some(documents) =
        provenance.get("retained_asset_schema_json")
    {
        serde_json::from_value(documents.clone())?
    } else {
        ensure!(
            identity
                .get("schema_addons")
                .and_then(serde_json::Value::as_array)
                .is_none_or(Vec::is_empty),
            "missing retained asset schema add-on documents; use normal conversion to regenerate assets"
        );
        Vec::new()
    };
    let references = documents.iter().map(Vec::as_slice).collect::<Vec<_>>();
    RuntimeSchemas::from_addon_bytes(&references).map_err(color_eyre::eyre::Error::msg)
}

/// Validate asset settings under their recorded reader without relabeling retained bytes.
fn settings_for_source_reader(
    config: &PipelineConfig,
    identity: &serde_json::Value,
    schemas: RuntimeSchemas,
) -> Result<PipelineConfig> {
    let reader: crate::config::RecordReader = serde_json::from_value(identity["mode"].clone())?;
    // A pack's string-table language changes its database text, never its asset bytes, so a
    // pack read in any language proves its assets; the proof is checked in that language.
    let language = identity["language"]
        .as_str()
        .unwrap_or(crate::config::DEFAULT_LANGUAGE)
        .to_owned();
    ensure!(
        identity
            == &crate::esm::inhouse::reader_identity_with_schemas_in_language(
                reader, &schemas, &language
            )
            || (reader == crate::config::RecordReader::Inhouse
                && [
                    crate::cache::inhouse_adapter10_reader_identity(&schemas),
                    crate::cache::inhouse_adapter11_reader_identity(&schemas),
                ]
                .iter()
                .any(|prior| prior.as_ref() == Some(identity)))
            || (reader == crate::config::RecordReader::Legacy
                && (identity == &serde_json::json!({"mode": "legacy"})
                    || identity
                        == &serde_json::json!({"mode": "legacy", "projection_version": 1})
                    || identity
                        == &serde_json::json!({"mode": "legacy", "projection_version": 2})
                    || identity
                        == &serde_json::json!({"mode": "legacy", "projection_version": 3}))
                && schemas.identities().is_empty()),
        "unsupported source reader producer identity; use normal conversion to regenerate assets before metadata-only migration"
    );
    let mut settings = config.clone();
    settings.record_reader = reader;
    settings.language = language;
    settings.schema_addons.clear();
    settings.runtime_schemas = Some(Arc::new(schemas));
    Ok(settings)
}

/// Resolve a new metadata output disjoint from source assets and game inputs.
fn new_output_path(output: &Path, source: &Path, data: &Path) -> Result<PathBuf> {
    let name = output
        .file_name()
        .ok_or_else(|| color_eyre::eyre::eyre!("output has no directory name"))?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let output = fs::canonicalize(parent)?.join(name);
    ensure!(
        !output.starts_with(source)
            && !source.starts_with(&output)
            && !output.starts_with(data)
            && !data.starts_with(&output),
        "metadata output must be disjoint from source assets and Skyrim Data"
    );
    ensure!(
        output
            .symlink_metadata()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "metadata rebuild requires a new output directory: {}",
        output.display()
    );
    Ok(output)
}

fn matched_plugins(source: &Path, data: &Path) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let connection = Connection::open_with_flags(
        source.join("skyrim_world.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let rows: Vec<(String, i64, String)> = connection
        .prepare("SELECT name, priority, lower(hex(checksum)) FROM plugins ORDER BY priority, id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    ensure!(!rows.is_empty(), "source database has no plugin provenance");
    let mut seen = BTreeSet::new();
    let mut plugins = Vec::with_capacity(rows.len());
    let mut hashes = Vec::with_capacity(rows.len());
    for (index, (name, priority, expected)) in rows.into_iter().enumerate() {
        ensure!(
            priority == index as i64,
            "source plugin priorities must be unique and contiguous"
        );
        ensure!(
            Path::new(&name).components().count() == 1
                && matches!(
                    Path::new(&name).components().next(),
                    Some(Component::Normal(_))
                )
                && !name.contains(['\\', ':']),
            "invalid plugin name {name}"
        );
        ensure!(
            seen.insert(shared::plugin_selection::normalize_plugin_name(&name)),
            "duplicate plugin name {name}"
        );
        let path = data.join(&name);
        ensure!(
            hash_file(&path)? == expected,
            "plugin checksum mismatch: {name}"
        );
        plugins.push(path);
        hashes.push(expected);
    }
    Ok((plugins, hashes))
}

/// Rebuild requested metadata while publishing the already verified original asset provenance.
async fn rebuild_into(
    config: &PipelineConfig,
    source: &Path,
    staging: &Path,
    matched: &(Vec<PathBuf>, Vec<String>),
    metadata: SourceMetadata<'_>,
    progress: &Sender<ProgressEvent>,
    ownership: Arc<LodWorkerOwnership>,
) -> Result<PipelineReport> {
    let SourceMetadata {
        manifest_bytes: source_manifest,
        manifest,
        retained_reader,
        retained_schemas,
    } = metadata;
    let (plugins, plugin_hashes) = matched;
    progress_event(
        progress,
        ProgressStage::Validating,
        "Verifying and copying retained assets",
    )
    .await;
    let (retained, replay_verified_pruned_assets) =
        copy_verified_assets(source, staging, manifest)?;
    let provenance = MetadataProvenance {
        record_reader: crate::esm::inhouse::configured_reader_identity(config)?,
        retained_asset_record_reader: retained_reader.clone(),
        retained_asset_schema_json: retained_schemas.addon_bytes().map(<[u8]>::to_vec).collect(),
        source_converter_schema: manifest.schema_version,
        source_retained_mesh_schema: manifest
            .retained_mesh_schema_version
            .unwrap_or(manifest.schema_version),
        source_manifest_hash: hash_bytes(source_manifest),
        source_configuration_hash: manifest.configuration_hash.clone(),
        retained_asset_configuration_hash: manifest
            .retained_asset_configuration_hash
            .clone()
            .unwrap_or_else(|| manifest.configuration_hash.clone()),
        retained_asset_hashes: retained.clone(),
        replay_verified_pruned_assets,
    };
    let mut report = PipelineReport {
        load_order: plugins
            .iter()
            .map(|path| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect(),
        cache_hits: retained.len() as u64,
        pruned_texture_references: manifest
            .pruned_texture_references
            .values()
            .map(|references| references.len() as u64)
            .sum(),
        artifacts: retained.keys().map(PathBuf::from).collect(),
        ..Default::default()
    };
    if provenance.record_reader != provenance.retained_asset_record_reader {
        let notice = "Metadata reader changed; retained models and textures keep their verified source producer bytes, including earlier texture-role encodings. Use normal conversion to regenerate those assets.";
        eprintln!("note: {notice}");
        report.notices.push(notice.to_owned());
    }
    progress_event(
        progress,
        ProgressStage::Extracting,
        "Resolving current LOD settings",
    )
    .await;
    let files = discover(&config.data_dir)?;
    // Keep the checksum-matched source package's plugin order authoritative. Current installed
    // identities prevent a surviving prefix from reactivating a more specific omitted owner.
    let archive_owners = installed_archive_owners(&config.data_dir, &files, plugins);
    let mut archives: Vec<_> = files
        .iter()
        .filter(|path| {
            path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("bsa")
                    || (config.enable_ba2 && ext.eq_ignore_ascii_case("ba2"))
            })
        })
        .cloned()
        .collect();
    archives.retain(|archive| {
        if !non_mo2_archive_is_active(archive, &archive_owners) {
            report.lod_warnings.push(format!(
                "LOD extraction omitted archive {}: installed owner is absent from source-package plugin order",
                archive.display()
            ));
            false
        } else if archive_load_order_priority(archive, plugins).is_some() {
            true
        } else {
            report.lod_warnings.push(format!(
                "LOD extraction omitted archive {}: no matching source-package plugin",
                archive.display()
            ));
            false
        }
    });
    sort_archives_by_load_order(&mut archives, plugins);
    let vfs = staging.join("vfs");
    fs::create_dir(&vfs)?;
    for archive in &archives {
        ArchiveExtractor::extract_lod_settings(archive, &vfs)?;
    }
    let loose: Vec<_> = files
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lod"))
        })
        .cloned()
        .collect();
    overlay_loose_assets(&config.data_dir, &vfs, &loose)?;

    progress_event(
        progress,
        ProgressStage::Database,
        "Rebuilding authoritative world metadata",
    )
    .await;
    let database = staging.join("skyrim_world.db");
    // Both readers resolve localized names; decode only the selected plugin tables.
    let strings = crate::esm::inhouse::localized_string_paths(plugins, &config.language);
    for archive in &archives {
        if let Err(error) = ArchiveExtractor::extract_paths(archive, &vfs, &strings) {
            eprintln!(
                "warning: {}: unable to extract localized names: {error}",
                archive.display()
            );
        }
    }
    let conversion = EsmParser::convert_plugins_with_reader_and_schemas(
        plugins,
        &database,
        config.record_reader,
        &vfs,
        &config.language,
        Some(config.schemas()?.as_ref()),
    )?;
    // This fresh read owns record completeness independently of the retained assets.
    manifest.record_data_complete = Some(conversion.warnings.is_empty());
    report.warnings.extend(conversion.warnings);
    report.notices.extend(conversion.notices);
    let phases = conversion.phases;
    let merged = conversion.records;
    validate_database(&Connection::open(&database)?)?;
    if config.record_reader == crate::config::RecordReader::Inhouse {
        crate::esm::inhouse::write_database_profile(staging, &database, &phases)?;
        crate::esm::inhouse::write_terrain_caches(&merged, &database, staging)?;
    } else {
        write_cell_cache(&merged, &staging.join("cell_cache.rkyv"))?;
    }
    crate::esm::inhouse::write_configured_reader_identity(staging, config)?;
    report.artifacts.push(PathBuf::from("record-reader.json"));
    if config.record_reader == crate::config::RecordReader::Inhouse {
        report
            .artifacts
            .push(PathBuf::from(crate::esm::inhouse::DATABASE_PROFILE_FILE));
        report
            .artifacts
            .push(PathBuf::from("inhouse-reader-diagnostics.json"));
        report
            .artifacts
            .push(PathBuf::from("cell_cache_preserved.rkyv"));
    }
    // Merged terrain (#215) keeps only the records its stage reads. The LAND each cell keeps is
    // checked here, before the texture, mesh and LOD stages, so a bad load order fails in minutes;
    // the positions are the ones the cell cache above was ordered by.
    let source_offsets = if config.merged_terrain {
        crate::terrain_pipeline::cell_cache_source_offsets(config.record_reader, &database)?
    } else {
        HashMap::new()
    };
    let merged_records = if config.merged_terrain {
        crate::esm::cell_cache::merged_terrain_lands(&merged, &source_offsets)?;
        Some(crate::terrain_pipeline::terrain_records(merged))
    } else {
        drop(merged);
        None
    };
    let diffuse_paths = terrain_diffuse_paths(&Connection::open(&database)?)?;
    for archive in &archives {
        ArchiveExtractor::extract_paths(archive, &vfs, &diffuse_paths)?;
    }
    let loose_diffuse: Vec<_> = files
        .iter()
        .filter_map(|path| {
            let relative = path.strip_prefix(&config.data_dir).ok()?;
            let canonical =
                canonical_asset_path(&relative.to_string_lossy(), AssetKind::Texture, "dds")
                    .ok()?;
            (path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dds"))
                && diffuse_paths.contains(Path::new(&canonical)))
            .then(|| path.clone())
        })
        .collect();
    overlay_loose_assets(&config.data_dir, &vfs, &loose_diffuse)?;
    for relative in &diffuse_paths {
        if let Some(entry) = manifest
            .entries
            .get(&relative.to_string_lossy().to_string())
            && vfs.join(relative).is_file()
        {
            let raw_hash = hash_file(&vfs.join(relative))?;
            ensure!(
                entry
                    .source_hash
                    .split_once(":texture-encoding:")
                    .is_some_and(|(recorded, _)| recorded == raw_hash),
                "retained terrain texture source differs from current inputs: {}",
                relative.display()
            );
        }
    }
    report.artifacts.extend([
        PathBuf::from("skyrim_world.db"),
        PathBuf::from("cell_cache.rkyv"),
    ]);
    let lod_started = Instant::now();
    compile_lod_chunks(
        config,
        staging,
        source,
        plugins,
        plugin_hashes,
        progress,
        &mut report,
        Some(ownership),
    )
    .await?;
    report.lod_elapsed_ms = lod_started.elapsed().as_millis();
    // Merged terrain (#215) is rebuilt from the records like the database and the LOD chunks
    // are, so a rebuild with the option on produces it instead of ignoring the option.
    if config.merged_terrain {
        crate::terrain_pipeline::run_merged_terrain_stage(
            staging,
            merged_records,
            source_offsets,
            plugins,
            &crate::terrain_pipeline::TerrainOptions::from_config(config),
            progress,
            &mut report,
        )
        .await?;
    }
    let integration = finalize_world_database(staging)?
        .ok_or_else(|| color_eyre::eyre::eyre!("missing rebuilt world database"))?;
    ensure!(
        integration.passed,
        "rebuilt asset integration failed: {:?}",
        integration.issues
    );
    // The proof set from the fresh read above does not see projection omissions.
    if report_projection_omissions(&mut report, &integration.projection_omissions) {
        manifest.record_data_complete = Some(false);
    }
    report.integration = Some(integration);
    report
        .artifacts
        .push(PathBuf::from("integration-report.json"));
    // Albedo is embedded in new LOD payloads. Retained textures still match
    // the source manifest; validate copied model dependencies separately. The merged terrain
    // payloads (#215) are generated here too and are validated the same way.
    let payloads = report
        .artifacts
        .iter()
        .filter(|path| path.starts_with("lod") || path.starts_with("terrain"))
        .cloned()
        .collect::<Vec<_>>();
    validate_artifacts(staging, &payloads, &BTreeMap::new(), config.cpu_jobs)?;
    for path in retained.keys().filter(|path| path.ends_with(".glb")) {
        for dependency in MeshConverter::glb_texture_dependencies(&staging.join(path))? {
            ensure!(
                resolve_asset_uri(staging, &staging.join(path), &dependency.uri)?.is_file(),
                "missing retained texture {} for {path}",
                dependency.uri
            );
        }
    }
    ensure!(
        hash_file(&source.join("conversion-manifest.json"))? == provenance.source_manifest_hash,
        "source manifest changed during metadata rebuild"
    );
    manifest.schema_version = CONVERTER_SCHEMA_VERSION;
    manifest.retained_mesh_schema_version = Some(provenance.source_retained_mesh_schema);
    manifest.retained_asset_configuration_hash =
        Some(provenance.retained_asset_configuration_hash.clone());
    manifest.configuration_hash = configuration_hash(config)?;
    manifest.schema_addons = config.schemas()?.identities().to_vec();
    // No archive extraction cache is copied by this route.
    manifest.archives.clear();
    report.complete = conversion_is_complete(&report);
    manifest.complete = report.complete;
    manifest.save(&staging.join("conversion-manifest.json"))?;
    fs::write(
        staging.join("metadata-source-conversion-manifest.json"),
        source_manifest,
    )?;
    fs::write(
        staging.join("metadata-rebuild.json"),
        serde_json::to_vec_pretty(&provenance)?,
    )?;
    report.artifacts.extend([
        PathBuf::from("conversion-manifest.json"),
        PathBuf::from("metadata-rebuild.json"),
        PathBuf::from("metadata-source-conversion-manifest.json"),
    ]);
    progress_event(
        progress,
        ProgressStage::Publishing,
        "Publishing isolated metadata rebuild",
    )
    .await;
    Ok(report)
}

/// Copies verified runtime bytes, reproducing historical pruning for stale output hashes.
/// Repair presence bits come from the complete source manifest rather than current Data.
fn copy_verified_assets(
    source: &Path,
    staging: &Path,
    manifest: &mut ConversionManifest,
) -> Result<(BTreeMap<String, String>, BTreeSet<String>)> {
    // Reuse requires a complete package with no failed assets. Its successful DDS entries
    // therefore preserve the producer's texture-source presence even without a raw VFS.
    let source_textures = manifest
        .entries
        .keys()
        .filter(|key| key.starts_with("textures/") && key.ends_with(".dds"))
        .cloned()
        .collect();
    let mut expected = BTreeMap::new();
    for entry in manifest.entries.values() {
        let kind = if entry.output.starts_with("meshes/") {
            AssetKind::Mesh
        } else if entry.output.starts_with("textures/") {
            AssetKind::Texture
        } else if entry.output.starts_with("scripts/") {
            AssetKind::Script
        } else {
            bail!("unsupported retained asset {}", entry.output)
        };
        let extension = match kind {
            AssetKind::Mesh => "glb",
            AssetKind::Texture => "ktx2",
            AssetKind::Script => "luau",
            _ => unreachable!(),
        };
        ensure!(
            canonical_asset_path(&entry.output, kind, extension)? == entry.output,
            "noncanonical retained output {}",
            entry.output
        );
        ensure!(
            expected
                .insert(
                    entry.output.clone(),
                    (entry.output_hash.clone(), entry.output_size)
                )
                .is_none(),
            "duplicate retained output {}",
            entry.output
        );
    }
    let runtime = "scripts/papyrus_runtime.luau";
    expected.insert(
        runtime.into(),
        (
            hash_bytes(include_bytes!("../../shared/src/papyrus_runtime.luau")),
            include_bytes!("../../shared/src/papyrus_runtime.luau").len() as u64,
        ),
    );
    let mut retained = BTreeMap::new();
    let mut replay = BTreeSet::new();
    for folder in ["meshes", "textures", "scripts"] {
        let folder = source.join(folder);
        match folder.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        // A plugin-only pack may have no mesh/texture directory. The final expected-key
        // check still rejects every missing recorded asset, including the runtime helper.
        for entry in WalkDir::new(&folder).follow_links(false) {
            let entry = entry?;
            ensure!(
                !entry.file_type().is_symlink(),
                "retained asset symlinks are not supported: {}",
                entry.path().display()
            );
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(source)?
                .to_string_lossy()
                .replace('\\', "/");
            let key = if relative.starts_with("textures/") {
                crate::asset_path::runtime_texture_source(&relative).ok_or_else(|| {
                    color_eyre::eyre::eyre!("unsupported retained texture: {relative}")
                })?
            } else {
                relative.clone()
            };
            let (hash, size) = expected.get(&key).ok_or_else(|| {
                color_eyre::eyre::eyre!("retained asset has no manifest provenance: {relative}")
            })?;
            let destination = staging.join(&relative);
            fs::create_dir_all(destination.parent().unwrap())?;
            let actual_size = fs::copy(entry.path(), &destination)
                .wrap_err_with(|| format!("failed to copy retained asset {relative}"))?;
            let actual_hash = hash_file(&destination)?;
            if actual_size != *size || actual_hash != *hash {
                ensure!(
                    relative.ends_with(".glb")
                        && manifest.pruned_texture_references.contains_key(&relative),
                    "retained asset checksum mismatch: {relative}"
                );
                verify_legacy_prune(
                    source,
                    staging,
                    manifest,
                    &relative,
                    &actual_hash,
                    actual_size,
                    &source_textures,
                )?;
                replay.insert(relative.clone());
            }
            retained.insert(relative, actual_hash);
        }
    }
    for key in expected.keys() {
        ensure!(
            retained.contains_key(key),
            "source package is missing manifest asset {key}"
        );
    }
    Ok((retained, replay))
}

/// Verifies exact source and pre-prune checksums, then reproduces only historical pruning.
/// Accepts the original unsuffixed hash or the shared scoped hash for SOURCE's texture snapshot;
/// never applies repair or refreshes a retained material from current Data.
fn verify_legacy_prune(
    source: &Path,
    staging: &Path,
    manifest: &mut ConversionManifest,
    glb: &str,
    retained_hash: &str,
    retained_size: u64,
    source_textures: &BTreeSet<String>,
) -> Result<()> {
    let source_key = canonical_asset_path(glb, AssetKind::Mesh, "nif")?;
    let entry = manifest
        .entries
        .get_mut(&source_key)
        .ok_or_else(|| color_eyre::eyre::eyre!("missing pruned source provenance: {glb}"))?;
    let nif = source.join("vfs").join(&source_key);
    ensure!(
        nif.is_file(),
        "legacy pruned GLB replay requires raw source NIF {} for {glb}; this runtime package omits the source. Run a normal conversion into a new package, then reuse that package instead",
        nif.display()
    );
    ensure!(
        nif_source_hash(&nif, &source_key, source_textures)? == entry.source_hash
            || nif_unrepaired_source_hash(&nif)? == entry.source_hash,
        "pruned GLB source checksum mismatch: {glb}"
    );
    let target = staging.join(glb);
    // Older manifests hashed GLBs before pruning. Prove the exact original
    // checksum and deterministic postprocess; arbitrary mutations still fail.
    MeshConverter::convert_nif_to_glb(&nif, &target)?;
    ensure!(
        hash_file(&target)? == entry.output_hash
            && fs::metadata(&target)?.len() == entry.output_size,
        "cannot reproduce original pruned GLB checksum: {glb}"
    );
    let original = fs::read(&target)?;
    let source_document = source.join(glb);
    let (pruned, removed_uris) = prune_glb_texture_bytes(source, &source_document, &original)?
        .ok_or_else(|| color_eyre::eyre::eyre!("recorded prune did not reproduce: {glb}"))?;
    let references: BTreeSet<_> = removed_uris
        .iter()
        .map(|uri| {
            let path = resolve_asset_uri(source, &source_document, uri)?;
            Ok(path
                .strip_prefix(source)?
                .to_string_lossy()
                .replace('\\', "/"))
        })
        .collect::<Result<_>>()?;
    ensure!(
        manifest.pruned_texture_references.get(glb) == Some(&references),
        "recorded texture prune differs: {glb}"
    );
    ensure!(
        hash_bytes(&pruned) == retained_hash && pruned.len() as u64 == retained_size,
        "retained asset checksum mismatch after verified prune: {glb}"
    );
    fs::write(&target, &pruned)?;
    entry.output_hash = retained_hash.into();
    entry.output_size = retained_size;
    Ok(())
}

async fn progress_event(progress: &Sender<ProgressEvent>, stage: ProgressStage, message: &str) {
    let _ = progress
        .send(ProgressEvent::new(stage, 0, 1, None, message))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheEntry;

    #[test]
    fn retained_sampler_aliases_require_exact_source_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let staging = directory.path().join("staging");
        for folder in ["meshes", "textures", "scripts"] {
            fs::create_dir_all(source.join(folder)).unwrap();
        }
        fs::write(
            source.join("scripts/papyrus_runtime.luau"),
            include_bytes!("../../shared/src/papyrus_runtime.luau"),
        )
        .unwrap();
        fs::write(source.join("textures/shared.ktx2"), b"verified texture").unwrap();
        let mut manifest = ConversionManifest::default();
        manifest.entries.insert(
            "textures/shared.dds".into(),
            CacheEntry {
                source_hash: "source".into(),
                output: "textures/shared.ktx2".into(),
                output_size: 16,
                output_hash: hash_bytes(b"verified texture"),
            },
        );
        let mut aliases = Vec::new();
        for mode in 0..3 {
            for transfer in ["", ".opensky-srgb"] {
                let alias = format!("textures/shared{transfer}.opensky-wrap{mode}.ktx2");
                fs::write(source.join(&alias), b"verified texture").unwrap();
                aliases.push(alias);
            }
        }
        let (retained, _) = copy_verified_assets(&source, &staging, &mut manifest).unwrap();
        for alias in &aliases {
            assert_eq!(retained[alias], hash_bytes(b"verified texture"));
            assert_eq!(fs::read(staging.join(alias)).unwrap(), b"verified texture");
        }
        fs::write(source.join(&aliases[0]), b"tampered texture").unwrap();
        assert!(
            copy_verified_assets(&source, &directory.path().join("tampered"), &mut manifest)
                .is_err()
        );
    }
}

#[cfg(test)]
mod metadata_schema_controls {
    use super::*;
    use crate::{cache::asset_configuration_hash_for_schema, config::RecordReader};

    /// Unsupported MO2 metadata is rejected before configuration reads or output work.
    #[tokio::test]
    async fn mo2_metadata_rebuild_is_rejected_before_source_and_output_work() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let sentinel = source.join("sentinel");
        fs::write(&sentinel, b"untouched source package").unwrap();
        let data = directory.path().join("absent-Data");
        let output = directory.path().join("output");
        let instance = directory.path().join("absent-instance");
        let mut config = PipelineConfig::new(&data, &output);
        config.mo2 = Some(mo2::Selection {
            instance_path: instance.clone(),
            profile: "Absent".into(),
        });
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let error = AssetPipeline::rebuild_metadata_async(config, &source, tx)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("metadata rebuild does not support MO2 selection"),
            "wrong source boundary: {error}"
        );
        assert_eq!(fs::read(&sentinel).unwrap(), b"untouched source package");
        assert_eq!(fs::read_dir(&source).unwrap().count(), 1);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        assert!(!data.exists());
        assert!(!output.exists());
        assert!(!instance.exists());
        assert!(rx.try_recv().is_err());
    }

    /// The metadata route must ship a sidecar for its rebuilt manifest. A stale sidecar linked
    /// from the source pack is unlinked, never written through, and the incomplete-reader note
    /// still reaches the catalog.
    #[test]
    fn metadata_rebuild_catalog_describes_the_final_manifest_and_leaves_the_source_alone() {
        use shared::streaming_costs::{STREAMING_COST_FILE_NAME, StreamingCostCatalog};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let staging = temp.path().join("staging");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(source.join(STREAMING_COST_FILE_NAME), b"source catalog").unwrap();
        fs::hard_link(
            source.join(STREAMING_COST_FILE_NAME),
            staging.join(STREAMING_COST_FILE_NAME),
        )
        .unwrap();
        let manifest = ConversionManifest {
            complete: false,
            ..Default::default()
        };
        manifest
            .save(&staging.join("conversion-manifest.json"))
            .unwrap();
        let manifest_bytes = fs::read(staging.join("conversion-manifest.json")).unwrap();

        write_streaming_cost_catalog(&staging).unwrap();

        let catalog = StreamingCostCatalog::load(
            &staging.join(STREAMING_COST_FILE_NAME),
            &hash_bytes(&manifest_bytes),
        )
        .unwrap();
        assert!(
            catalog
                .notes
                .iter()
                .any(|note| note.contains("manifest is incomplete")),
            "incomplete rebuild must keep its unknown-resource note"
        );
        assert_eq!(
            fs::read(source.join(STREAMING_COST_FILE_NAME)).unwrap(),
            b"source catalog",
            "the source pack's catalog must not change through a shared link"
        );
        assert!(
            StreamingCostCatalog::load(
                &source.join(STREAMING_COST_FILE_NAME),
                &hash_bytes(&manifest_bytes),
            )
            .is_err()
        );
    }

    const ORIGINAL: &[u8] = br#"{"version":1,"name":"original_source","records":[{"signature":"ZMEX","source":"synthetic metadata fixture","fields":[{"signature":"SCAL","name":"value","kind":"u32"}]}]}"#;
    const REQUESTED: &[u8] = br#"{"version":1,"name":"requested_output","records":[{"signature":"ZMEX","source":"synthetic metadata fixture","fields":[{"signature":"SCAL","name":"value","kind":"i32"}]}]}"#;

    /// A pack read in another language proves its assets in that language: the language
    /// changes database text, never asset bytes.
    #[test]
    fn source_settings_accept_a_pack_read_in_another_language() {
        let schemas = || RuntimeSchemas::from_addon_bytes(&[]).unwrap();
        let identity = crate::esm::inhouse::reader_identity_with_schemas_in_language(
            RecordReader::Inhouse,
            &schemas(),
            "german",
        );
        let mut config = PipelineConfig::new("Data", "output");
        config.record_reader = RecordReader::Inhouse;
        let settings = settings_for_source_reader(&config, &identity, schemas()).unwrap();
        assert_eq!(settings.language, "german");
        assert_eq!(
            crate::esm::inhouse::configured_reader_identity(&settings).unwrap(),
            identity
        );
        let mut altered = identity.clone();
        altered["string_table_encoding"] = serde_json::json!("windows-1252");
        assert!(settings_for_source_reader(&config, &altered, schemas()).is_err());
    }

    /// Known prior adapters retain the exact original schema and reject unproven variants.
    #[test]
    fn adapter10_source_settings_require_original_saved_schema_and_identity() {
        for adapter in [10, 11] {
            let original = RuntimeSchemas::from_addon_bytes(&[ORIGINAL]).unwrap();
            let mut identity =
                crate::esm::inhouse::reader_identity_with_schemas(RecordReader::Inhouse, &original);
            identity["adapter_version"] = serde_json::json!(adapter);
            let mut config = PipelineConfig::new("Data", "output");
            config.record_reader = RecordReader::Inhouse;
            config
                .schema_addons
                .push("missing-current-addon.json".into());
            config.runtime_schemas = Some(Arc::new(
                RuntimeSchemas::from_addon_bytes(&[REQUESTED]).unwrap(),
            ));
            let recovered = settings_for_source_reader(&config, &identity, original);
            assert!(
                recovered.is_ok(),
                "adapter-{adapter} producer identity must be retained: {recovered:?}"
            );
            let recovered = recovered.unwrap();
            assert!(recovered.schema_addons.is_empty());
            assert_eq!(
                recovered.schemas().unwrap().addon_bytes().next().unwrap(),
                ORIGINAL
            );
            assert_eq!(identity["adapter_version"], adapter);
            assert_eq!(crate::esm::inhouse::ADAPTER_VERSION, 12);
            assert!(
                settings_for_source_reader(
                    &config,
                    &identity,
                    RuntimeSchemas::from_addon_bytes(&[REQUESTED]).unwrap()
                )
                .is_err()
            );
            for (key, value) in [
                ("adapter_version", serde_json::json!(8)),
                ("schema_sha256", serde_json::json!("different-schema")),
                ("schema_addons", serde_json::json!([])),
                ("language", serde_json::json!("french")),
                ("extra", serde_json::json!(true)),
            ] {
                let mut altered = identity.clone();
                altered[key] = value;
                assert!(
                    settings_for_source_reader(
                        &config,
                        &altered,
                        RuntimeSchemas::from_addon_bytes(&[ORIGINAL]).unwrap()
                    )
                    .is_err(),
                    "accepted altered {key}"
                );
            }
        }
    }

    /// Missing old stamps remain unversioned; later metadata cannot infer a newer asset reader.
    #[test]
    fn legacy_source_and_retained_reader_contracts_remain_distinct() {
        let temp = tempfile::tempdir().unwrap();
        let original = serde_json::json!({"mode": "legacy"});
        let current = crate::esm::inhouse::reader_identity(RecordReader::Legacy);
        assert_ne!(current, original);
        assert_eq!(source_reader_identity(temp.path()).unwrap(), original);
        assert_eq!(
            retained_reader_identity(temp.path(), &original).unwrap(),
            original
        );

        let provenance_path = temp.path().join("metadata-rebuild.json");
        fs::write(&provenance_path, b"{}").unwrap();
        assert_eq!(
            retained_reader_identity(temp.path(), &original).unwrap(),
            original
        );
        assert!(retained_reader_identity(temp.path(), &current).is_err());

        for metadata_reader in [
            crate::esm::inhouse::reader_identity(RecordReader::Inhouse),
            current,
        ] {
            fs::write(
                temp.path().join("record-reader.json"),
                serde_json::to_vec(&metadata_reader).unwrap(),
            )
            .unwrap();
            fs::write(
                &provenance_path,
                serde_json::to_vec(&serde_json::json!({
                    "record_reader": metadata_reader,
                    "retained_asset_record_reader": original,
                }))
                .unwrap(),
            )
            .unwrap();
            let stamped = source_reader_identity(temp.path()).unwrap();
            assert_eq!(stamped, metadata_reader);
            assert_eq!(
                retained_reader_identity(temp.path(), &stamped).unwrap(),
                original
            );
        }
    }

    /// Only explicitly known legacy identities can authorize retained metadata settings.
    #[test]
    fn legacy_settings_accept_known_contracts_and_reject_unknown_projection_versions() {
        let config = PipelineConfig::new("Data", "output");
        for identity in [
            serde_json::json!({"mode": "legacy"}),
            serde_json::json!({"mode": "legacy", "projection_version": 1}),
            serde_json::json!({"mode": "legacy", "projection_version": 2}),
            serde_json::json!({"mode": "legacy", "projection_version": 3}),
            crate::esm::inhouse::reader_identity(RecordReader::Legacy),
        ] {
            let settings = settings_for_source_reader(
                &config,
                &identity,
                RuntimeSchemas::from_addon_bytes(&[]).unwrap(),
            )
            .unwrap();
            assert_eq!(settings.record_reader, RecordReader::Legacy);
        }
        for identity in [
            serde_json::json!({"mode": "legacy", "projection_version": 0}),
            serde_json::json!({"mode": "legacy", "projection_version": 5}),
            serde_json::json!({"mode": "legacy", "unproven": true}),
        ] {
            assert!(
                settings_for_source_reader(
                    &config,
                    &identity,
                    RuntimeSchemas::from_addon_bytes(&[]).unwrap(),
                )
                .is_err()
            );
        }
    }

    /// Source settings use saved documents even when requested paths and layouts differ.
    #[test]
    fn source_settings_bind_saved_schema_without_changing_asset_settings() {
        let original = RuntimeSchemas::from_addon_bytes(&[ORIGINAL]).unwrap();
        let identity =
            crate::esm::inhouse::reader_identity_with_schemas(RecordReader::Inhouse, &original);
        let mut config = PipelineConfig::new("Data", "output");
        config.record_reader = RecordReader::Inhouse;
        config
            .schema_addons
            .push("absent_requested_file.json".into());
        config.runtime_schemas = Some(Arc::new(
            RuntimeSchemas::from_addon_bytes(&[REQUESTED]).unwrap(),
        ));
        let source = settings_for_source_reader(&config, &identity, original).unwrap();
        assert!(source.schema_addons.is_empty());
        assert_eq!(
            source.schemas().unwrap().identities()[0].name,
            "original_source"
        );
        assert_ne!(
            configuration_hash(&source).unwrap(),
            configuration_hash(&config).unwrap()
        );
        for kind in ["nif", "dds", "pex"] {
            assert_eq!(
                asset_configuration_hash_for_schema(&source, CONVERTER_SCHEMA_VERSION, kind)
                    .unwrap(),
                asset_configuration_hash_for_schema(&config, CONVERTER_SCHEMA_VERSION, kind)
                    .unwrap()
            );
        }
        let legacy = settings_for_source_reader(
            &config,
            &crate::esm::inhouse::reader_identity(RecordReader::Legacy),
            RuntimeSchemas::from_addon_bytes(&[]).unwrap(),
        )
        .unwrap();
        assert!(legacy.schemas().unwrap().identities().is_empty());
    }

    /// Repeated rebuilds recover the original asset producer rather than current metadata add-ons.
    #[test]
    fn retained_asset_documents_stay_bound_to_original_reader() {
        let temp = tempfile::tempdir().unwrap();
        let original = RuntimeSchemas::from_addon_bytes(&[ORIGINAL]).unwrap();
        let current = RuntimeSchemas::from_addon_bytes(&[REQUESTED]).unwrap();
        let identity =
            crate::esm::inhouse::reader_identity_with_schemas(RecordReader::Inhouse, &original);
        let path = temp.path().join("metadata-rebuild.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "retained_asset_record_reader": identity,
                "retained_asset_schema_json": [ORIGINAL],
            }))
            .unwrap(),
        )
        .unwrap();
        let recovered = retained_reader_schemas(temp.path(), &identity, &current).unwrap();
        assert_eq!(recovered.identities(), original.identities());
        assert_eq!(recovered.addon_bytes().next().unwrap(), ORIGINAL);
        settings_for_source_reader(&PipelineConfig::new("Data", "output"), &identity, recovered)
            .unwrap();

        fs::write(&path, b"{}").unwrap();
        assert!(retained_reader_schemas(temp.path(), &identity, &current).is_err());
        let mut earlier = identity;
        earlier["adapter_version"] = serde_json::json!(8);
        assert!(
            settings_for_source_reader(&PipelineConfig::new("Data", "output"), &earlier, original,)
                .is_err()
        );
    }

    /// Empty asset kinds can be absent while every recorded asset remains mandatory.
    #[test]
    fn plugin_only_packages_skip_absent_asset_folders_and_require_recorded_assets() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("scripts")).unwrap();
        fs::write(
            source.join("scripts/papyrus_runtime.luau"),
            include_bytes!("../../shared/src/papyrus_runtime.luau"),
        )
        .unwrap();
        let mut manifest = ConversionManifest::default();
        let (retained, _) =
            copy_verified_assets(&source, &temp.path().join("staging"), &mut manifest).unwrap();
        assert_eq!(retained.len(), 1);
        manifest.entries.insert(
            "meshes/missing.nif".into(),
            crate::cache::CacheEntry {
                source_hash: "synthetic_source".into(),
                output: "meshes/missing.glb".into(),
                output_size: 7,
                output_hash: hash_bytes(b"missing"),
            },
        );
        let error =
            copy_verified_assets(&source, &temp.path().join("missing"), &mut manifest).unwrap_err();
        assert!(format!("{error}").contains("missing manifest asset meshes/missing.glb"));
    }
}
