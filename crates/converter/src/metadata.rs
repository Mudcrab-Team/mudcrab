//! Rebuild world metadata from package-matched plugins and verified retained assets.
//!
//! Retained models, textures and scripts are verified against the source package's
//! manifest, not refreshed from current Data; scripts with a stale ABI or proof
//! regenerate from checksum-matched Data, with local failures reported as incomplete
//! publication. Data supplies checksum-matched plugins, LOD settings, optional PC
//! controls and terrain diffuse inputs. Use normal conversion after asset changes.

use crate::{
    AssetPipeline, PipelineConfig, PipelineReport, ProgressEvent, ProgressStage,
    archive::ArchiveExtractor,
    asset_path::{AssetKind, canonical_asset_path, resolve_asset_uri},
    cache::{
        BODY_COMPOSE_CONTEXT, BODY_COMPOSE_RECIPE, CONVERTER_SCHEMA_VERSION, CacheEntry,
        ConversionManifest, DEFAULT_GAIT_RECIPE, body_compose_inputs,
        can_reuse_meshes_for_producer, configuration_hash, default_gait_inputs, hash_bytes,
        hash_file, recover_script_abi,
    },
    esm::{
        EsmParser,
        cell_cache::{write_cell_cache, write_preserved_cell_cache},
        exporter::validate_database,
        grass_cache::write_grass_cache,
    },
    integration::finalize_world_database,
    lod::albedo::terrain_diffuse_paths,
    mesh::{MeshConverter, NifSnapshot, prune_glb_texture_bytes, volcanic_normal_cache_suffix},
    pipeline::{
        LodWorkerOwnership, archive_load_order_priority, compile_lod_chunks,
        conversion_is_complete, convert_staged_scripts, discover, installed_archive_owners,
        non_mo2_archive_is_active, overlay_loose_assets, publish_new_directory,
        report_projection_omissions, sort_archives_by_load_order, staging_path, validate_artifacts,
        write_streaming_cost_catalog,
    },
    records::addons::RuntimeSchemas,
    reuse::{Kind, Plan},
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
    /// Grass cells newly baked during this metadata rebuild.
    grass_baked_cells: usize,
    /// Unchanged cells reused from the source package's valid grass cache.
    grass_reused_cells: usize,
    /// What the rebuild did with each asset kind, and the settings proofs behind it.
    asset_kinds: BTreeMap<&'static str, KindProvenance>,
    /// Script ABI of the source pack's combined settings proof, and the one it was rebuilt to.
    source_script_abi: u32,
    script_abi: u32,
    /// Earlier base-schema digest of the source database, accepted by the schema lineage.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_schema_lineage: Option<SchemaLineage>,
}

/// One asset kind's outcome in a metadata rebuild.
#[derive(Serialize)]
struct KindProvenance {
    /// `reused` or `rebuilt`.
    action: &'static str,
    /// The settings proof the source manifest recorded, when it recorded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_proof: Option<String>,
    /// The settings proof of the published bytes.
    proof: String,
}

/// An earlier base schema the source database was decoded with.
#[derive(Serialize)]
struct SchemaLineage {
    sha256: String,
    produced_by: String,
    difference: String,
}

/// Frozen source and original-asset schema facts validated before staging is created.
struct SourceMetadata<'a> {
    manifest_bytes: &'a [u8],
    manifest: &'a mut ConversionManifest,
    retained_reader: &'a serde_json::Value,
    retained_schemas: &'a RuntimeSchemas,
    plan: &'a Plan,
    source_script_abi: u32,
    source_lineage: Option<&'a str>,
}

impl AssetPipeline {
    /// Publishes only to a new, disjoint directory. The source package and its
    /// retained GLB/KTX2/Luau bytes are never modified or relabeled in place.
    /// MO2 source selection is unsupported by this package-matched Data route.
    /// Reader and grass data-loss warnings keep the rebuilt report and manifest incomplete;
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
        // A saved configuration cannot make rebuilt scripts use an older ABI, and it cannot
        // name an ABI this converter does not emit: the stamped proof would not describe
        // the script bytes.
        ensure!(
            config.script_abi_version <= crate::script::SCRIPT_ABI_VERSION,
            "script ABI {} is newer than the ABI {} this converter emits; use a converter that emits it",
            config.script_abi_version,
            crate::script::SCRIPT_ABI_VERSION
        );
        config.script_abi_version = crate::script::SCRIPT_ABI_VERSION;
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
        let source_lineage = crate::records::schema_lineage::lineage_digest(&source_reader);
        let source_schemas =
            source_reader_schemas(&source, &source_reader, source_lineage.as_deref())?;
        ensure!(
            source_schemas.identities() == manifest.schema_addons,
            "source schema add-on manifest differs from its database provenance"
        );
        let retained_schemas = retained_reader_schemas(&source, &retained_reader, &source_schemas)?;
        let source_settings = settings_for_source_reader_in_lineage(
            &config,
            &source_reader,
            source_schemas,
            source_lineage.as_deref(),
        )?;
        let retained_lineage = crate::records::schema_lineage::lineage_digest(&retained_reader);
        let retained_schema_documents = retained_schemas.addon_bytes().collect::<Vec<_>>();
        let retained_settings = settings_for_source_reader_in_lineage(
            &config,
            &retained_reader,
            RuntimeSchemas::from_addon_bytes(&retained_schema_documents)
                .map_err(color_eyre::eyre::Error::msg)?,
            retained_lineage.as_deref(),
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
        // The combined settings proof also binds the script ABI. Recover the ABI it records
        // instead of requiring the current one: every other setting must still match exactly,
        // and scripts the current ABI cannot prove are converted again below.
        let source_script_abi = recover_script_abi(
            &source_settings,
            manifest.schema_version,
            &manifest.configuration_hash,
            &source_reader,
            source_lineage.as_deref(),
        )?
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("retained asset configuration does not match rebuild settings")
        })?;
        let retained_configuration = manifest
            .retained_asset_configuration_hash
            .as_deref()
            .unwrap_or(&manifest.configuration_hash);
        ensure!(
            recover_script_abi(
                &retained_settings,
                mesh_schema,
                retained_configuration,
                &retained_reader,
                retained_lineage.as_deref(),
            )?
            .is_some(),
            "retained producer configuration does not match rebuild settings"
        );
        // Each asset kind is decided on its own proof. A changed mesh or texture proof still
        // refuses the rebuild; a stale script ABI rebuilds only the scripts.
        let plan = Plan::decide(
            &manifest.asset_configuration_hashes,
            |kind| {
                let (schema, key) = match kind {
                    Kind::Nif => (mesh_schema, "nif"),
                    Kind::Dds => (CONVERTER_SCHEMA_VERSION, "dds"),
                    Kind::Pex => (CONVERTER_SCHEMA_VERSION, "pex"),
                };
                crate::cache::asset_configuration_hash_for_schema(&config, schema, key)
            },
            source_script_abi == config.script_abi_version,
        )?;
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
                plan: &plan,
                source_script_abi,
                source_lineage: source_lineage.as_deref(),
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
fn source_reader_schemas(
    source: &Path,
    identity: &serde_json::Value,
    lineage: Option<&str>,
) -> Result<RuntimeSchemas> {
    let reader: crate::config::RecordReader = serde_json::from_value(identity["mode"].clone())?;
    if reader == crate::config::RecordReader::Legacy {
        return RuntimeSchemas::from_addon_bytes(&[]).map_err(color_eyre::eyre::Error::msg);
    }
    let conn = Connection::open_with_flags(
        source.join("skyrim_world.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    crate::records::history::stored_schemas_with_lineage(&conn, lineage)
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
#[cfg(test)]
fn settings_for_source_reader(
    config: &PipelineConfig,
    identity: &serde_json::Value,
    schemas: RuntimeSchemas,
) -> Result<PipelineConfig> {
    settings_for_source_reader_in_lineage(config, identity, schemas, None)
}

/// As above, also accepting the earlier base-schema digest `lineage` in the recorded identity.
fn settings_for_source_reader_in_lineage(
    config: &PipelineConfig,
    identity: &serde_json::Value,
    schemas: RuntimeSchemas,
    lineage: Option<&str>,
) -> Result<PipelineConfig> {
    let reader: crate::config::RecordReader = serde_json::from_value(identity["mode"].clone())?;
    // A pack's string-table language changes its database text, never its asset bytes, so a
    // pack read in any language proves its assets; the proof is checked in that language.
    let language = identity["language"]
        .as_str()
        .unwrap_or(crate::config::DEFAULT_LANGUAGE)
        .to_owned();
    // An earlier base schema the lineage trusts differs from the compiled one only in its
    // recorded digest. Every other identity field is still compared exactly.
    let with_lineage = |mut candidate: serde_json::Value| {
        if let Some(earlier) = lineage {
            candidate["schema_sha256"] = serde_json::json!(earlier);
        }
        candidate
    };
    ensure!(
        identity
            == &crate::esm::inhouse::reader_identity_with_schemas_in_language(
                reader, &schemas, &language
            )
            || (lineage.is_some()
                && reader == crate::config::RecordReader::Inhouse
                && [
                    Some(
                        crate::esm::inhouse::reader_identity_with_schemas_in_language(
                            reader, &schemas, &language
                        )
                    ),
                    crate::cache::inhouse_adapter10_reader_identity(&schemas),
                    crate::cache::inhouse_adapter11_reader_identity(&schemas),
                    crate::cache::inhouse_adapter12_reader_identity(&schemas, &language),
                    crate::cache::inhouse_adapter14_reader_identity(&schemas, &language),
                ]
                .into_iter()
                .flatten()
                .any(|candidate| &with_lineage(candidate) == identity))
            || (reader == crate::config::RecordReader::Inhouse
                && [
                    crate::cache::inhouse_adapter10_reader_identity(&schemas),
                    crate::cache::inhouse_adapter11_reader_identity(&schemas),
                    crate::cache::inhouse_adapter12_reader_identity(&schemas, &language),
                    crate::cache::inhouse_adapter13_reader_identity(&schemas, &language),
                    crate::cache::inhouse_adapter14_reader_identity(&schemas, &language),
                ]
                .iter()
                .any(|prior| prior.as_ref() == Some(identity)))
            || (reader == crate::config::RecordReader::Legacy
                && schemas.identities().is_empty()
                && [
                    crate::cache::legacy_projection4_reader_identity(&language),
                    crate::cache::legacy_projection5_reader_identity(&language),
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
    // The lock file and the staging folder would land inside another pack, and a registered
    // pack is immutable. The converter does not read the pack registry, so refuse any output
    // folder that sits inside a directory holding a conversion manifest.
    if let Some(pack) = output
        .ancestors()
        .skip(1)
        .find(|ancestor| ancestor.join("conversion-manifest.json").exists())
    {
        bail!(
            "metadata output {} lies inside the asset pack {}; choose a folder outside every pack",
            output.display(),
            pack.display()
        );
    }
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

/// Enforce retained source identity for every ordinary LTEX diffuse.
fn verify_retained_terrain_source(
    vfs: &Path,
    relative: &Path,
    manifest: &ConversionManifest,
) -> Result<()> {
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
    Ok(())
}

/// Discard partial or stale default bytes so publication can select its existing fallback.
/// Failure to remove a staged file is a publication failure, not damaged input recovery.
fn discard_default_source(
    vfs: &Path,
    relative: &Path,
    error: &color_eyre::Report,
    report: &mut PipelineReport,
) -> Result<()> {
    match fs::remove_file(vfs.join(relative)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    report.notices.push(format!(
        "unusable default land source {} omitted: {error:#}",
        relative.display()
    ));
    Ok(())
}

/// Stage one default-only path without letting a broken override abort metadata publication.
/// Ordinary terrain paths are excluded by the caller and retain strict overlay/hash checks.
fn stage_optional_default_source(
    config: &PipelineConfig,
    vfs: &Path,
    relative: &Path,
    archives: &[PathBuf],
    files: &[PathBuf],
    report: &mut PipelineReport,
) -> Result<()> {
    let paths = BTreeSet::from([relative.to_owned()]);
    for archive in archives {
        if let Err(error) = ArchiveExtractor::extract_runtime_paths(archive, vfs, &paths) {
            discard_default_source(vfs, relative, &error, report)?;
        }
    }
    let loose: Vec<_> = files
        .iter()
        .filter(|path| {
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dds"))
            {
                return false;
            }
            path.strip_prefix(&config.data_dir)
                .ok()
                .and_then(|path| {
                    canonical_asset_path(&path.to_string_lossy(), AssetKind::Texture, "dds").ok()
                })
                .is_some_and(|canonical| Path::new(&canonical) == relative)
        })
        .cloned()
        .collect();
    if let Err(error) = overlay_loose_assets(&config.data_dir, vfs, &loose) {
        // A failed winning loose override must not expose a lower-priority archive copy.
        discard_default_source(vfs, relative, &error, report)?;
    }
    // Generated default images are never retained: publication consumes current DDS bytes.
    // Only ordinary LTEX inputs must match the retained package's texture proofs.
    Ok(())
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
        plan,
        source_script_abi,
        source_lineage,
    } = metadata;
    let (plugins, plugin_hashes) = matched;
    progress_event(
        progress,
        ProgressStage::Validating,
        "Verifying and copying retained assets",
    )
    .await;
    let mut reuse_notices = Vec::new();
    // Stale scripts are never copied. Their source hashes are what the rebuilt scripts are
    // checked against, so keep them aside before the retained copy drops them.
    let rebuild_scripts = plan.rebuilds(Kind::Pex);
    let old_scripts = if rebuild_scripts {
        take_script_entries(manifest)
    } else {
        BTreeMap::new()
    };
    let mesh_schema = manifest
        .retained_mesh_schema_version
        .unwrap_or(manifest.schema_version);
    let mut asset_kinds = BTreeMap::new();
    for (kind, action) in plan.outcomes() {
        let schema = if kind == Kind::Nif {
            mesh_schema
        } else {
            CONVERTER_SCHEMA_VERSION
        };
        asset_kinds.insert(
            kind.key(),
            KindProvenance {
                action: action.as_str(),
                source_proof: manifest.asset_configuration_hashes.get(kind.key()).cloned(),
                proof: crate::cache::asset_configuration_hash_for_schema(
                    config,
                    schema,
                    kind.key(),
                )?,
            },
        );
    }
    let (retained, replay_verified_pruned_assets) = copy_verified_assets_with(
        source,
        staging,
        manifest,
        &mut reuse_notices,
        !rebuild_scripts,
    )?;
    let mut provenance = MetadataProvenance {
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
        grass_baked_cells: 0,
        grass_reused_cells: 0,
        asset_kinds,
        source_script_abi,
        script_abi: config.script_abi_version,
        source_schema_lineage: source_lineage
            .and_then(crate::records::schema_lineage::entry_for)
            .map(|entry| SchemaLineage {
                sha256: entry.sha256.clone(),
                produced_by: entry.produced_by.clone(),
                difference: entry.difference.clone(),
            }),
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
        notices: reuse_notices,
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
    // Reingest only the optional controls; verified meshes/textures/scripts remain retained.
    let control_scratch = tempfile::tempdir_in(staging.parent().unwrap_or(staging))?;
    let mut control_sources = BTreeMap::new();
    for (index, archive) in archives.iter().enumerate() {
        let root = control_scratch.path().join(index.to_string());
        let provider = archive
            .strip_prefix(&config.data_dir)
            .unwrap_or(archive)
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        let warnings =
            match crate::controls::extract_archive(&mut control_sources, archive, &provider, &root)
            {
                Ok(warnings) => warnings,
                Err(warning) => vec![warning],
            };
        report.warnings.extend(warnings);
    }
    for file in &files {
        if let Ok(relative) = file.strip_prefix(&config.data_dir) {
            crate::controls::record_loose(&mut control_sources, relative, file);
        }
    }
    crate::controls::publish(staging, control_sources, &mut report)?;
    let loose: Vec<_> = files
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lod"))
        })
        .cloned()
        .collect();
    overlay_loose_assets(&config.data_dir, &vfs, &loose)?;
    if rebuild_scripts {
        progress_event(
            progress,
            ProgressStage::Scripts,
            "Converting scripts again for the current script ABI",
        )
        .await;
        rebuild_scripts_from_data(
            config,
            staging,
            &files,
            &archives,
            &old_scripts,
            manifest,
            &mut report,
            progress,
        )
        .await?;
    }

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
        if let Err(error) = ArchiveExtractor::extract_runtime_paths(archive, &vfs, &strings) {
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
    report.intended_omissions.extend(conversion.omissions);
    let phases = conversion.phases;
    let merged = conversion.records;
    validate_database(&Connection::open(&database)?)?;
    if config.record_reader == crate::config::RecordReader::Inhouse {
        crate::esm::inhouse::write_database_profile(staging, &database, &phases)?;
        crate::esm::inhouse::write_terrain_caches(&merged, &database, staging)?;
    } else {
        write_cell_cache(&merged, &staging.join("cell_cache.rkyv"))?;
        write_preserved_cell_cache(&merged, &staging.join("cell_cache_preserved.rkyv"))?;
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
    }
    report
        .artifacts
        .push(PathBuf::from("cell_cache_preserved.rkyv"));
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
    // Read the freshly rebuilt cache, not the retained source pack's coverage.
    // A path also named by an LTEX remains required, even if it is a configured default.
    let default_paths: BTreeSet<_> = if config.terrain_default_texture
        == crate::config::TerrainDefaultTexture::SkyrimIni
        && crate::terrain_pipeline::default_land_is_needed(staging)?
    {
        crate::terrain_pipeline::default_land_sources(config)?
            .into_iter()
            .filter(|path| !diffuse_paths.contains(path))
            .collect()
    } else {
        BTreeSet::new()
    };
    for archive in &archives {
        ArchiveExtractor::extract_runtime_paths(archive, &vfs, &diffuse_paths)?;
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
        verify_retained_terrain_source(&vfs, relative, manifest)?;
    }
    for relative in &default_paths {
        stage_optional_default_source(config, &vfs, relative, &archives, &files, &mut report)?;
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
    crate::lod::retail_terrain_import::TerrainCheckpoint::remove_from(staging)?;
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
    report_projection_omissions(&mut report, &integration.projection_omissions);
    report.integration = Some(integration);
    let grass = write_grass_cache(
        &Connection::open(&database)?,
        staging,
        &staging.join("cell_cache_preserved.rkyv"),
        Some(&source.join("grass_cache.rkyv")),
        &staging.join("grass_cache.rkyv"),
    )?;
    provenance.grass_baked_cells = grass.baked_cells;
    provenance.grass_reused_cells = grass.reused_cells;
    // Unavailable geometry only limits the optional obstruction mask.
    report.notices.extend(grass.notices());
    let grass_warnings = grass.warnings();
    if !grass_warnings.is_empty() {
        // Retained asset repair cannot restore grass data omitted by this rebuild.
        manifest.record_data_complete = Some(false);
    }
    report.warnings.extend(grass_warnings);
    report.artifacts.push(PathBuf::from("grass_cache.rkyv"));
    report
        .artifacts
        .push(PathBuf::from("integration-report.json"));
    // Validate new LOD, merged terrain and grass payloads. Retained textures
    // match the source manifest; validate copied model dependencies separately.
    let rebuilt_artifacts = report
        .artifacts
        .iter()
        .filter(|path| {
            path.starts_with("lod")
                || path.starts_with("terrain")
                || *path == Path::new("grass_cache.rkyv")
        })
        .cloned()
        .collect::<Vec<_>>();
    validate_artifacts(
        staging,
        &rebuilt_artifacts,
        &BTreeMap::new(),
        config.cpu_jobs,
    )?;
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
    report
        .warnings
        .extend(crate::extensions::refresh_manifest_with_notices(
            manifest, staging,
        )?);
    report.complete = conversion_is_complete(&report);
    manifest.complete = report.complete;
    crate::cache::merge_intended_omissions(&mut report.intended_omissions);
    manifest.intended_omissions = report.intended_omissions.clone();
    crate::cache::record_grass_artifact(manifest, staging)?;
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

/// Counts local omissions while retaining only one bounded example for the report.
#[derive(Default)]
struct UnprovenAssets {
    count: u64,
    first: String,
    cleanup_failures: u64,
    first_cleanup_failure: String,
}

impl UnprovenAssets {
    /// Record one omitted output without growing diagnostics with the source pack.
    fn record(&mut self, path: &str) {
        self.count += 1;
        if self.count == 1 {
            self.first = path.chars().take(256).collect();
        }
    }

    /// Count a failed scratch cleanup and keep only the first bounded example.
    fn record_cleanup_failure(&mut self, path: &str, error: &std::io::Error) {
        self.cleanup_failures += 1;
        if self.cleanup_failures == 1 {
            self.first_cleanup_failure = format!("{path}: {error}").chars().take(256).collect();
        }
    }

    /// Omit an unpublished optional output. A leftover directory is a bounded cleanup notice;
    /// any other surviving entry could be a publishable file, so that case fails closed.
    fn omit_derived(&mut self, path: &str, destination: &Path) -> Result<()> {
        self.record(path);
        if let Err(error) = fs::remove_file(destination)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            match fs::symlink_metadata(destination) {
                Ok(metadata) if metadata.is_dir() => self.record_cleanup_failure(path, &error),
                Err(lookup) if lookup.kind() == std::io::ErrorKind::NotFound => {}
                _ => {
                    return Err(error).wrap_err_with(|| {
                        format!("failed to remove unpublished derived output {path}")
                    });
                }
            }
        }
        Ok(())
    }
}

/// Copies one optional output into scratch, proves its exact bytes, then moves it into an absent
/// destination. The absence check precedes the rename, which would otherwise replace an entry.
fn link_verified_derived(
    source: &Path,
    candidate: &Path,
    destination: &Path,
    (expected_hash, expected_size): (&str, u64),
) -> Result<()> {
    let copied = {
        let mut reader = fs::File::open(source)?;
        let mut writer = fs::File::create(candidate)?;
        std::io::copy(&mut reader, &mut writer)?
    };
    ensure!(
        copied == expected_size && hash_file(candidate)? == expected_hash,
        "derived output {} does not match its proof",
        source.display()
    );
    match fs::symlink_metadata(destination) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => bail!("refusing to replace existing {}", destination.display()),
        Err(error) => return Err(error.into()),
    }
    fs::create_dir_all(destination.parent().unwrap())?;
    fs::rename(candidate, destination)?;
    Ok(())
}

/// Publishes an optional derived output only after verification in private scratch beside staging.
/// Returns `false` for a local omission. Only a failure that could leave bytes at the final path
/// (an unremovable non-directory) is an error, so the whole conversion fails closed.
fn publish_derived_output(
    source: &Path,
    staging: &Path,
    relative: &str,
    expected: (&str, u64),
    unproven: &mut UnprovenAssets,
) -> Result<bool> {
    let destination = staging.join(relative);
    let Some(parent) = staging.parent() else {
        bail!(
            "staging {} has no parent for derived verification",
            staging.display()
        );
    };
    let outcome = match tempfile::Builder::new()
        .prefix(".derived-verify-")
        .tempdir_in(parent)
    {
        Ok(scratch) => {
            let outcome = link_verified_derived(
                source,
                &scratch.path().join("candidate.bin"),
                &destination,
                expected,
            );
            if let Err(error) = scratch.close() {
                unproven.record_cleanup_failure(relative, &error);
            }
            outcome
        }
        Err(error) => Err(error.into()),
    };
    match outcome {
        Ok(()) => Ok(true),
        Err(_) => {
            unproven.omit_derived(relative, &destination)?;
            Ok(false)
        }
    }
}

/// A retained recipe authenticates only the producer's exact output and logical inputs.
/// Cleanup subtree recognition is intentionally broader and cannot establish provenance.
pub(crate) fn supported_derived_proof(path: &str, proof: &crate::cache::DerivedOutput) -> bool {
    let mut inputs = BTreeSet::new();
    if crate::pipeline::authored_actor_idle::supported_proof(path, proof) {
        inputs.extend(proof.inputs.keys().cloned());
    }
    for profile in shared::creature_idle::CREATURE_IDLES {
        if path == profile.clip_scene && proof.recipe == crate::cache::CREATURE_IDLE_RECIPE {
            inputs.extend(profile.inputs().map(str::to_owned));
        }
    }
    for sex in ["male", "female"] {
        if path == format!("meshes/actors/character/composed/default/{sex}/body.glb")
            && matches!(
                proof.recipe.as_str(),
                "body-compose-v1" | BODY_COMPOSE_RECIPE
            )
        {
            inputs.extend(body_compose_inputs(sex));
            if proof.recipe == BODY_COMPOSE_RECIPE {
                inputs.insert(BODY_COMPOSE_CONTEXT.into());
            }
        }
        for stem in ["mt_idle", "mt_walkforward", "mt_runforward"] {
            if path == format!("meshes/actors/character/animations/{sex}/{stem}.glb")
                && proof.recipe == DEFAULT_GAIT_RECIPE
            {
                inputs.extend(default_gait_inputs(sex, stem));
            }
        }
    }
    let sha256 = |hash: &str| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    };
    !inputs.is_empty()
        && proof.inputs.keys().cloned().collect::<BTreeSet<_>>() == inputs
        && proof.inputs.values().all(|hash| sha256(hash))
        && sha256(&proof.output_hash)
        && proof.output_size > 0
}

/// Copies verified runtime bytes, reproducing historical pruning for stale output hashes.
/// Unproven or unreadable optional derived outputs and strays are omitted with a bounded notice;
/// metadata retention never invents a producer proof from observed file bytes.
/// Repair presence bits come from the complete source manifest rather than current Data.
#[cfg(test)]
fn copy_verified_assets(
    source: &Path,
    staging: &Path,
    manifest: &mut ConversionManifest,
    notices: &mut Vec<String>,
) -> Result<(BTreeMap<String, String>, BTreeSet<String>)> {
    copy_verified_assets_with(source, staging, manifest, notices, true)
}

/// As above. With `retain_scripts` false the rebuild converts the scripts again, so no script
/// entry or helper is expected and the source `scripts` folder is not read.
fn copy_verified_assets_with(
    source: &Path,
    staging: &Path,
    manifest: &mut ConversionManifest,
    notices: &mut Vec<String>,
    retain_scripts: bool,
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
        // Grass is regenerated from preserved terrain after model finalization.
        if entry.output == "grass_cache.rkyv" {
            continue;
        }
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
    let mut unproven = UnprovenAssets::default();
    let mut excluded_derived = BTreeSet::new();
    let mut derived = BTreeSet::new();
    for (path, proof) in &manifest.derived_outputs {
        if !supported_derived_proof(path, proof) || expected.contains_key(path) {
            unproven.record(path);
            excluded_derived.insert(path.clone());
            continue;
        }
        expected.insert(path.clone(), (proof.output_hash.clone(), proof.output_size));
        derived.insert(path.clone());
    }
    let runtime = "scripts/papyrus_runtime.luau";
    if retain_scripts {
        expected.insert(
            runtime.into(),
            (
                hash_bytes(include_bytes!("../../shared/src/papyrus_runtime.luau")),
                include_bytes!("../../shared/src/papyrus_runtime.luau").len() as u64,
            ),
        );
    }
    let mut retained = BTreeMap::new();
    let mut replay = BTreeSet::new();
    for folder in ["meshes", "textures", "scripts"] {
        if folder == "scripts" && !retain_scripts {
            continue;
        }
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
            let relative = entry
                .path()
                .strip_prefix(source)?
                .to_string_lossy()
                .replace('\\', "/");
            if entry.file_type().is_symlink()
                && (!expected.contains_key(&relative) || derived.contains(&relative))
            {
                if !excluded_derived.contains(&relative) {
                    unproven.record(&relative);
                    excluded_derived.insert(relative);
                }
                continue;
            }
            ensure!(
                !entry.file_type().is_symlink(),
                "retained asset symlinks are not supported: {}",
                entry.path().display()
            );
            if !entry.file_type().is_file() {
                continue;
            }
            if excluded_derived.contains(&relative) && !expected.contains_key(&relative) {
                continue;
            }
            let key = if relative.starts_with("textures/") {
                crate::asset_path::runtime_texture_source(&relative)
            } else {
                Some(relative.clone())
            };
            let Some((hash, size)) = key.as_ref().and_then(|key| expected.get(key)) else {
                unproven.record(&relative);
                continue;
            };
            if derived.contains(&relative) {
                // Optional bytes reach staging only after their scratch copy matches the proof.
                if !publish_derived_output(
                    entry.path(),
                    staging,
                    &relative,
                    (hash.as_str(), *size),
                    &mut unproven,
                )? {
                    // Do not diagnose this same output again in the missing-file check.
                    excluded_derived.insert(relative);
                    continue;
                }
                retained.insert(relative, hash.clone());
                continue;
            }
            let destination = staging.join(&relative);
            // Ordinary recorded assets retain strict copy, hash and integrity errors.
            let copied: Result<_> = (|| {
                fs::create_dir_all(destination.parent().unwrap())?;
                let actual_size = fs::copy(entry.path(), &destination)
                    .wrap_err_with(|| format!("failed to copy retained asset {relative}"))?;
                Ok((actual_size, hash_file(&destination)?))
            })();
            let (actual_size, actual_hash) = copied?;
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
        if derived.contains(key) && !retained.contains_key(key) {
            if !excluded_derived.contains(key) {
                unproven.record(key);
            }
            continue;
        }
        ensure!(
            retained.contains_key(key),
            "source package is missing manifest asset {key}"
        );
    }
    manifest
        .derived_outputs
        .retain(|path, _| derived.contains(path) && retained.contains_key(path));
    manifest
        .pruned_texture_references
        .retain(|path, _| retained.contains_key(path));
    if unproven.count > 0 {
        let cleanup = if unproven.cleanup_failures > 0 {
            format!(
                " Could not remove {} partial derived copies; first: {}.",
                unproven.cleanup_failures, unproven.first_cleanup_failure
            )
        } else {
            String::new()
        };
        let notice = format!(
            "Omitted {} unproven retained assets (optional derived outputs or stray files); first: {}.{cleanup} Run normal conversion to regenerate derived outputs.",
            unproven.count, unproven.first
        );
        eprintln!("note: {notice}");
        notices.push(notice);
    } else if unproven.cleanup_failures > 0 {
        let notice = format!(
            "Could not remove {} derived verification scratch entries; first: {}.",
            unproven.cleanup_failures, unproven.first_cleanup_failure
        );
        eprintln!("note: {notice}");
        notices.push(notice);
    }
    Ok((retained, replay))
}

/// Removes every script entry from the manifest and returns it by source key.
fn take_script_entries(manifest: &mut ConversionManifest) -> BTreeMap<String, CacheEntry> {
    let keys: Vec<String> = manifest
        .entries
        .iter()
        .filter(|(_, entry)| entry.output.starts_with("scripts/"))
        .map(|(key, _)| key.clone())
        .collect();
    keys.into_iter()
        .filter_map(|key| manifest.entries.remove(&key).map(|entry| (key, entry)))
        .collect()
}

/// Authenticate winning source keys and hashes before decoding. Only a recorded extraction
/// failure can explain an unavailable script; every available script must match its producer.
fn verify_script_sources(
    recorded: &BTreeMap<String, CacheEntry>,
    available: &BTreeMap<String, String>,
    unavailable: &BTreeMap<String, String>,
) -> Result<()> {
    let mut differing = recorded
        .iter()
        .filter(|(key, entry)| {
            available.get(*key).map_or_else(
                || !unavailable.contains_key(*key),
                |hash| hash != &entry.source_hash,
            )
        })
        .map(|(key, _)| key)
        .chain(available.keys().filter(|key| !recorded.contains_key(*key)))
        .chain(
            unavailable
                .keys()
                .filter(|key| !recorded.contains_key(*key)),
        );
    if let Some(first) = differing.next() {
        let count = 1 + differing.count();
        let first: String = first.chars().take(256).collect();
        bail!(
            "script sources in Data differ from the source package ({count} of {} scripts; first: {first}); \
             run normal conversion to regenerate assets",
            recorded.len().max(available.len() + unavailable.len())
        );
    }
    Ok(())
}

/// Identified extraction failures and bounded diagnostics for unusable archive member names.
#[derive(Default)]
struct StagedScriptArchives {
    unavailable: BTreeMap<String, String>,
    skipped_names: u64,
    warnings: Vec<String>,
}

/// Admit archive keys that remain stable through overlay and VFS source authentication.
fn script_archive_member_key(path: &Path) -> Result<String> {
    let key = canonical_asset_path(&path.to_string_lossy(), AssetKind::Script, "pex")?;
    ensure!(
        canonical_asset_path(&key, AssetKind::Script, "pex")? == key,
        "script archive member name has an unstable canonical identity"
    );
    Ok(key)
}

/// Overlay complete archive selections, retrying failed batches one script at a time.
/// Identified failed winners remove lower-priority bytes; unusable names produce local warnings.
fn stage_script_archives(
    config: &PipelineConfig,
    staging: &Path,
    archives: &[PathBuf],
) -> Result<StagedScriptArchives> {
    let vfs = staging.join("vfs");
    let mut staged = StagedScriptArchives::default();
    for (index, archive) in archives.iter().enumerate() {
        let scratch = staging.join(format!(".metadata-script-archive-{index}"));
        fs::create_dir(&scratch)?;
        let mut skipped_names = 0u64;
        let mut first_bad_name = None;
        let extracted = {
            let mut usable_key = |path: &Path| -> Result<Option<String>> {
                match script_archive_member_key(path) {
                    Ok(key) => Ok(Some(key)),
                    Err(error) if config.fail_fast => Err(error).wrap_err_with(|| {
                        format!(
                            "unusable script member {} in {}",
                            path.display(),
                            archive.display()
                        )
                    }),
                    Err(error) => {
                        skipped_names = skipped_names.saturating_add(1);
                        if first_bad_name.is_none() {
                            let name: String = path.to_string_lossy().chars().take(256).collect();
                            let reason: String = format!("{error:#}").chars().take(256).collect();
                            first_bad_name = Some(format!("{name}: {reason}"));
                        }
                        Ok(None)
                    }
                }
            };
            let extracted: Vec<PathBuf> =
                match ArchiveExtractor::extract_script_inputs(archive, &scratch) {
                    Ok(extracted) => extracted.into_iter().map(|file| file.path).collect(),
                    Err(error)
                        if config.fail_fast || error.is::<crate::archive::StagingUnavailable>() =>
                    {
                        return Err(error).wrap_err_with(|| {
                            format!("failed to extract scripts from {}", archive.display())
                        });
                    }
                    Err(_) => {
                        // A fresh directory gives each retry only its own fully written payload.
                        fs::remove_dir_all(&scratch)?;
                        fs::create_dir(&scratch)?;
                        let selected = ArchiveExtractor::list_runtime_paths(archive)?
                            .into_iter()
                            .filter(|path| {
                                path.extension()
                                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pex"))
                            });
                        let mut extracted = Vec::new();
                        for path in selected {
                            let Some(key) = usable_key(&path)? else {
                                continue;
                            };
                            match ArchiveExtractor::extract_runtime_paths(
                                archive,
                                &scratch,
                                &BTreeSet::from([path.clone()]),
                            ) {
                                Ok(_) => extracted.push(path),
                                Err(error) if error.is::<crate::archive::StagingUnavailable>() => {
                                    return Err(error);
                                }
                                Err(error) => {
                                    let destination = vfs.join(&key);
                                    match fs::remove_file(&destination) {
                                        Ok(()) => {}
                                        Err(error)
                                            if error.kind() == std::io::ErrorKind::NotFound => {}
                                        Err(error) => {
                                            return Err(error)
                                                .wrap_err("failed to discard overridden script");
                                        }
                                    }
                                    staged.unavailable.insert(
                                        key.clone(),
                                        format!(
                                            "skipped script {key} from {}: {error:#}",
                                            archive.display()
                                        ),
                                    );
                                }
                            }
                        }
                        extracted
                    }
                };
            let mut admitted = Vec::new();
            for path in extracted {
                if let Some(key) = usable_key(&path)? {
                    admitted.push((path, key));
                }
            }
            admitted
        };
        let files: Vec<_> = extracted
            .iter()
            .map(|(path, _)| scratch.join(path))
            .collect();
        overlay_loose_assets(&scratch, &vfs, &files)?;
        for (_, key) in extracted {
            staged.unavailable.remove(&key);
        }
        fs::remove_dir_all(&scratch)?;
        if let Some(first) = first_bad_name {
            staged.skipped_names = staged.skipped_names.saturating_add(skipped_names);
            staged.warnings.push(format!(
                "skipped {skipped_names} script archive members with unusable names from {} (first: {first})",
                archive.display()
            ));
        }
    }
    Ok(staged)
}

/// Converts the scripts again from the Data scripts for the current script ABI and replaces the
/// runtime helper. Only the manifest's script entries and script proof change.
#[allow(clippy::too_many_arguments)]
async fn rebuild_scripts_from_data(
    config: &PipelineConfig,
    staging: &Path,
    files: &[PathBuf],
    archives: &[PathBuf],
    recorded: &BTreeMap<String, CacheEntry>,
    manifest: &mut ConversionManifest,
    report: &mut PipelineReport,
    progress: &Sender<ProgressEvent>,
) -> Result<()> {
    let vfs = staging.join("vfs");
    let staged = stage_script_archives(config, staging, archives)?;
    let mut unavailable = staged.unavailable;
    report.skipped = report.skipped.saturating_add(staged.skipped_names);
    report.warnings.extend(staged.warnings);
    let loose: Vec<_> = files
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pex"))
        })
        .cloned()
        .collect();
    overlay_loose_assets(&config.data_dir, &vfs, &loose)?;
    let mut available = BTreeMap::new();
    for path in discover(&vfs)?.into_iter().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pex"))
    }) {
        let relative = path.strip_prefix(&vfs)?;
        let key = canonical_asset_path(&relative.to_string_lossy(), AssetKind::Script, "pex")?;
        available.insert(key.clone(), hash_file(&path)?);
        unavailable.remove(&key);
    }
    verify_script_sources(recorded, &available, &unavailable)?;
    for (key, message) in unavailable {
        manifest.failures.insert(key, message.clone());
        report.warnings.push(message);
        report.skipped += 1;
    }
    convert_staged_scripts(config, staging, manifest, report, progress).await?;
    // The helper belongs to the ABI like the scripts do, so it is the current embedded one.
    let runtime = staging.join("scripts/papyrus_runtime.luau");
    fs::create_dir_all(runtime.parent().unwrap())?;
    fs::write(
        &runtime,
        include_bytes!("../../shared/src/papyrus_runtime.luau"),
    )?;
    report
        .artifacts
        .push(PathBuf::from("scripts/papyrus_runtime.luau"));
    manifest.asset_configuration_hashes.insert(
        Kind::Pex.key().into(),
        crate::cache::asset_configuration_hash_for_schema(
            config,
            CONVERTER_SCHEMA_VERSION,
            Kind::Pex.key(),
        )?,
    );
    Ok(())
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
    let suffix = volcanic_normal_cache_suffix(&source_key, source_textures);
    let expected = suffix
        .as_deref()
        .and_then(|suffix| entry.source_hash.strip_suffix(suffix))
        .unwrap_or(&entry.source_hash);
    let target = staging.join(glb);
    // Older manifests hashed GLBs before pruning. Replay the exact source
    // proof before checking the original GLB and deterministic postprocess.
    MeshConverter::convert_nif_with_local_resolution(
        &NifSnapshot::read(&nif)?,
        &target,
        Some(expected),
    )?;
    ensure!(
        hash_file(&target)? == entry.output_hash
            && fs::metadata(&target)?.len() == entry.output_size,
        "cannot reproduce original pruned GLB checksum: {glb}. Run a normal conversion into a new package, then reuse that package instead"
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

    /// Failed optional-copy cleanup is counted once per output while verified neighbors survive.
    #[test]
    fn retained_derived_copy_cleanup_failures_are_bounded_notices() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let staging = directory.path().join("staging");
        fs::create_dir_all(source.join("scripts")).unwrap();
        fs::write(
            source.join("scripts/papyrus_runtime.luau"),
            include_bytes!("../../shared/src/papyrus_runtime.luau"),
        )
        .unwrap();
        let neighbor = "scripts/neighbor.luau";
        let bytes = b"verified neighbor";
        fs::write(source.join(neighbor), bytes).unwrap();
        let mut manifest = ConversionManifest::default();
        manifest.entries.insert(
            "scripts/neighbor.pex".into(),
            CacheEntry {
                source_hash: hash_bytes(b"source"),
                output: neighbor.into(),
                output_size: bytes.len() as u64,
                output_hash: hash_bytes(bytes),
            },
        );
        for sex in ["male", "female"] {
            let relative = format!("meshes/actors/character/composed/default/{sex}/body.glb");
            fs::create_dir_all(source.join(&relative).parent().unwrap()).unwrap();
            fs::write(source.join(&relative), b"proven body").unwrap();
            manifest.derived_outputs.insert(
                relative.clone(),
                crate::cache::DerivedOutput {
                    recipe: "body-compose-v1".into(),
                    inputs: body_compose_inputs(sex)
                        .into_iter()
                        .map(|path| (path, hash_bytes(b"input")))
                        .collect(),
                    output_size: 11,
                    output_hash: hash_bytes(b"proven body"),
                },
            );
            // A nonempty destination directory makes publication and remove_file fail
            // deterministically, without relying on permissions or a timing race.
            let blocked = staging.join(relative);
            fs::create_dir_all(&blocked).unwrap();
            fs::write(blocked.join("keep"), b"unrelated destination bytes").unwrap();
        }
        let mut notices = Vec::new();
        let (retained, _) =
            copy_verified_assets(&source, &staging, &mut manifest, &mut notices).unwrap();
        assert_eq!(
            retained.len(),
            2,
            "only the ordinary neighbor and runtime remain"
        );
        assert_eq!(retained[neighbor], hash_bytes(bytes));
        assert_eq!(fs::read(staging.join(neighbor)).unwrap(), bytes);
        assert!(manifest.derived_outputs.is_empty());
        assert_eq!(notices.len(), 1);
        assert!(
            notices[0].contains("2 unproven retained assets"),
            "{notices:?}"
        );
        assert!(
            notices[0].contains("Could not remove 2 partial derived copies; first:"),
            "{notices:?}"
        );
        // Both diagnostic categories keep one example, rather than listing every failure.
        assert_ne!(
            notices[0].contains("/female/body.glb"),
            notices[0].contains("/male/body.glb"),
            "{notices:?}"
        );
        for sex in ["male", "female"] {
            assert_eq!(
                fs::read(staging.join(format!(
                    "meshes/actors/character/composed/default/{sex}/body.glb/keep"
                )))
                .unwrap(),
                b"unrelated destination bytes"
            );
        }
    }

    /// A mismatched optional copy never occupies its final path while a verified sibling publishes.
    #[test]
    fn rejected_derived_copy_never_reaches_staging_beside_verified_neighbor() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let staging = directory.path().join("staging");
        fs::create_dir_all(source.join("scripts")).unwrap();
        fs::write(
            source.join("scripts/papyrus_runtime.luau"),
            include_bytes!("../../shared/src/papyrus_runtime.luau"),
        )
        .unwrap();
        let proven = b"proven body";
        let mut manifest = ConversionManifest::default();
        for (sex, source_bytes) in [("male", &b"rejected body"[..]), ("female", &proven[..])] {
            let relative = format!("meshes/actors/character/composed/default/{sex}/body.glb");
            fs::create_dir_all(source.join(&relative).parent().unwrap()).unwrap();
            fs::write(source.join(&relative), source_bytes).unwrap();
            manifest.derived_outputs.insert(
                relative,
                crate::cache::DerivedOutput {
                    recipe: "body-compose-v1".into(),
                    inputs: body_compose_inputs(sex)
                        .into_iter()
                        .map(|path| (path, hash_bytes(b"input")))
                        .collect(),
                    output_size: proven.len() as u64,
                    output_hash: hash_bytes(proven),
                },
            );
        }
        let mut notices = Vec::new();
        let (retained, _) =
            copy_verified_assets(&source, &staging, &mut manifest, &mut notices).unwrap();
        let male = "meshes/actors/character/composed/default/male/body.glb";
        let female = "meshes/actors/character/composed/default/female/body.glb";
        assert_eq!(retained[female], hash_bytes(proven));
        assert_eq!(fs::read(staging.join(female)).unwrap(), proven);
        assert!(!retained.contains_key(male));
        assert!(
            !staging.join(male).exists(),
            "rejected bytes reached staging"
        );
        assert_eq!(manifest.derived_outputs.len(), 1);
        assert!(manifest.derived_outputs.contains_key(female));
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].contains("1 unproven retained assets"),
            "{notices:?}"
        );
        assert!(!notices[0].contains("Could not remove"), "{notices:?}");
    }

    /// An unpublished output whose stale occupant cannot be removed fails closed without overwrite.
    #[test]
    fn unremovable_rejected_derived_file_fails_closed_without_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let staging = directory.path().join("staging");
        fs::create_dir_all(source.join("scripts")).unwrap();
        fs::write(
            source.join("scripts/papyrus_runtime.luau"),
            include_bytes!("../../shared/src/papyrus_runtime.luau"),
        )
        .unwrap();
        let relative = "meshes/actors/character/composed/default/male/body.glb";
        let proven = b"proven body";
        fs::create_dir_all(source.join(relative).parent().unwrap()).unwrap();
        fs::write(source.join(relative), b"rejected body").unwrap();
        let mut manifest = ConversionManifest::default();
        manifest.derived_outputs.insert(
            relative.into(),
            crate::cache::DerivedOutput {
                recipe: "body-compose-v1".into(),
                inputs: body_compose_inputs("male")
                    .into_iter()
                    .map(|path| (path, hash_bytes(b"input")))
                    .collect(),
                output_size: proven.len() as u64,
                output_hash: hash_bytes(proven),
            },
        );
        // A read-only parent blocks unlink on Unix. On Windows, keep a shared-write handle
        // without delete sharing: the old overwrite-then-unlink path could leave rejected bytes.
        let destination = staging.join(relative);
        let parent = destination.parent().unwrap().to_path_buf();
        fs::create_dir_all(&parent).unwrap();
        fs::write(&destination, b"stale occupant").unwrap();
        let set_readonly = |path: &Path, readonly: bool| {
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_readonly(readonly);
            fs::set_permissions(path, permissions).unwrap();
        };
        set_readonly(parent.as_path(), true);
        #[cfg(not(windows))]
        set_readonly(destination.as_path(), true);
        // Root can unlink through a read-only parent; skip rather than assert rejection.
        #[cfg(unix)]
        if fs::remove_file(&destination).is_ok() {
            set_readonly(parent.as_path(), false);
            eprintln!("skipping: Unix permissions allow root to remove the fixture");
            return;
        }
        #[cfg(windows)]
        let removal_guard = {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
            fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(&destination)
                .unwrap()
        };
        let result = copy_verified_assets(&source, &staging, &mut manifest, &mut Vec::new());
        #[cfg(windows)]
        drop(removal_guard);
        set_readonly(destination.as_path(), false);
        set_readonly(parent.as_path(), false);
        assert!(result.is_err(), "unremovable occupant must fail closed");
        assert_eq!(fs::read(&destination).unwrap(), b"stale occupant");
    }

    /// Sampler aliases retain the canonical texture's exact bytes and reject tampering.
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
        for sampler in shared::texture_aliases::WRAP_TEXTURE_SUFFIXES {
            for transfer in ["", shared::texture_aliases::SRGB_TEXTURE_SUFFIX] {
                let alias = format!("textures/shared{transfer}{sampler}.ktx2");
                fs::write(source.join(&alias), b"verified texture").unwrap();
                aliases.push(alias);
            }
        }
        let (retained, _) =
            copy_verified_assets(&source, &staging, &mut manifest, &mut Vec::new()).unwrap();
        for alias in &aliases {
            assert_eq!(retained[alias], hash_bytes(b"verified texture"));
            assert_eq!(fs::read(staging.join(alias)).unwrap(), b"verified texture");
        }
        fs::write(source.join(&aliases[0]), b"tampered texture").unwrap();
        assert!(
            copy_verified_assets(
                &source,
                &directory.path().join("tampered"),
                &mut manifest,
                &mut Vec::new()
            )
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
        for adapter in [10, 11, 12, 13, 14] {
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
            assert_eq!(crate::esm::inhouse::ADAPTER_VERSION, 15);
            assert!(
                settings_for_source_reader(
                    &config,
                    &identity,
                    RuntimeSchemas::from_addon_bytes(&[REQUESTED]).unwrap()
                )
                .is_err()
            );
            for (key, value) in [
                // Rule: a metadata-only migration to adapter 15 accepts retained-asset sources
                // of adapters 10 to 14 (the loop above) and nothing else. Adapter 8 and any
                // adapter newer than the producer are unproven and must regenerate assets.
                ("adapter_version", serde_json::json!(8)),
                ("adapter_version", serde_json::json!(16)),
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

    #[test]
    fn adapter12_retains_exact_non_english_source_settings() {
        let schemas = RuntimeSchemas::from_addon_bytes(&[ORIGINAL]).unwrap();
        let identity = crate::cache::inhouse_adapter12_reader_identity(&schemas, "french").unwrap();
        let config = PipelineConfig::new("Data", "output");
        let recovered = settings_for_source_reader(&config, &identity, schemas).unwrap();
        assert_eq!(recovered.language, "french");
        assert_eq!(
            recovered.schemas().unwrap().addon_bytes().next().unwrap(),
            ORIGINAL
        );
        assert_eq!(identity["adapter_version"], 12);
        assert_eq!(identity["string_table_encoding"], "utf-8-or-windows-1252");
    }

    /// Adapter 13 source proofs preserve language and schemas without authorizing altered identities.
    #[test]
    fn adapter13_source_settings_preserve_exact_schema_and_language() {
        for language in ["english", "french"] {
            let schemas = RuntimeSchemas::from_addon_bytes(&[ORIGINAL]).unwrap();
            let mut identity = crate::esm::inhouse::reader_identity_with_schemas_in_language(
                RecordReader::Inhouse,
                &schemas,
                language,
            );
            identity["adapter_version"] = serde_json::json!(13);
            let config = PipelineConfig::new("Data", "output");
            let recovered = settings_for_source_reader(&config, &identity, schemas).unwrap();
            assert_eq!(recovered.language, language);
            assert_eq!(
                recovered.schemas().unwrap().addon_bytes().next().unwrap(),
                ORIGINAL
            );
            assert_ne!(
                crate::esm::inhouse::configured_reader_identity(&recovered).unwrap(),
                identity
            );
            assert!(
                settings_for_source_reader(
                    &config,
                    &identity,
                    RuntimeSchemas::from_addon_bytes(&[REQUESTED]).unwrap(),
                )
                .is_err()
            );
            for (field, value) in [
                ("adapter_version", serde_json::json!(99)),
                ("text_encoding", serde_json::json!("utf-8")),
                ("string_table_encoding", serde_json::json!("unproven")),
                ("extra", serde_json::json!(true)),
            ] {
                let mut altered = identity.clone();
                altered[field] = value;
                assert!(
                    settings_for_source_reader(
                        &config,
                        &altered,
                        RuntimeSchemas::from_addon_bytes(&[ORIGINAL]).unwrap(),
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn adapter13_retains_exact_non_english_source_settings() {
        let schemas = RuntimeSchemas::from_addon_bytes(&[ORIGINAL]).unwrap();
        let identity = crate::cache::inhouse_adapter13_reader_identity(&schemas, "french").unwrap();
        let config = PipelineConfig::new("Data", "output");
        let recovered = settings_for_source_reader(&config, &identity, schemas).unwrap();
        assert_eq!(recovered.language, "french");
        assert_eq!(identity["adapter_version"], 13);
        assert_eq!(identity["string_table_encoding"], "utf-8-or-windows-1252");
    }

    #[test]
    fn adapter14_retains_exact_source_settings_after_occlusion_metadata_update() {
        let schemas = RuntimeSchemas::from_addon_bytes(&[ORIGINAL]).unwrap();
        let identity = crate::cache::inhouse_adapter14_reader_identity(&schemas, "french").unwrap();
        let recovered =
            settings_for_source_reader(&PipelineConfig::new("Data", "output"), &identity, schemas)
                .unwrap();
        assert_eq!(recovered.language, "french");
        assert_eq!(identity["adapter_version"], 14);
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
            crate::cache::legacy_projection4_reader_identity("english").unwrap(),
            crate::cache::legacy_projection5_reader_identity("english").unwrap(),
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
            serde_json::json!({"mode": "legacy", "projection_version": 99}),
            serde_json::json!({"mode": "legacy", "projection_version": 7}),
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

    /// Projection-four/five source assets retain exact language and encoding identities.
    #[test]
    fn legacy_projection_four_source_settings_preserve_language_and_reject_unproven_variants() {
        for (projection, language) in [(4, "english"), (4, "french"), (5, "english"), (5, "french")]
        {
            let config = PipelineConfig::new("Data", "output");
            let mut identity = serde_json::json!({
                "mode": "legacy", "projection_version": projection, "language": language,
                "text_encoding": "windows-1252",
            });
            if language == "french" {
                identity["string_table_encoding"] = serde_json::json!("utf-8-or-windows-1252");
            }
            let settings = settings_for_source_reader(
                &config,
                &identity,
                RuntimeSchemas::from_addon_bytes(&[]).unwrap(),
            )
            .unwrap();
            assert_eq!(settings.record_reader, RecordReader::Legacy);
            assert_eq!(settings.language, language);
            for (field, value) in [
                ("projection_version", serde_json::json!(99)),
                ("text_encoding", serde_json::json!("utf-8")),
                ("extra", serde_json::json!(true)),
            ] {
                let mut altered = identity.clone();
                altered[field] = value;
                assert!(
                    settings_for_source_reader(
                        &config,
                        &altered,
                        RuntimeSchemas::from_addon_bytes(&[]).unwrap(),
                    )
                    .is_err()
                );
            }
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
        let (retained, _) = copy_verified_assets(
            &source,
            &temp.path().join("staging"),
            &mut manifest,
            &mut Vec::new(),
        )
        .unwrap();
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
        let error = copy_verified_assets(
            &source,
            &temp.path().join("missing"),
            &mut manifest,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(format!("{error}").contains("missing manifest asset meshes/missing.glb"));
    }
}

#[cfg(test)]
mod per_kind_tests {
    use super::*;
    use crate::config::RecordReader;

    fn entry(output: &str, source_hash: &str) -> CacheEntry {
        CacheEntry {
            source_hash: source_hash.into(),
            output: output.into(),
            output_size: 1,
            output_hash: "0".repeat(64),
        }
    }

    fn scripts(items: &[(&str, &str)]) -> BTreeMap<String, CacheEntry> {
        items
            .iter()
            .map(|(name, hash)| {
                (
                    format!("scripts/{name}.pex"),
                    entry(&format!("scripts/{name}.luau"), hash),
                )
            })
            .collect()
    }

    /// Available script hashes describe raw winning PEX bytes independently of decoding.
    fn script_hashes(items: &[(&str, &str)]) -> BTreeMap<String, String> {
        items
            .iter()
            .map(|(name, hash)| (format!("scripts/{name}.pex"), (*hash).into()))
            .collect()
    }

    /// Rebuilt scripts are accepted only for exactly the recorded sources.
    #[test]
    fn rebuilt_scripts_must_match_the_recorded_sources() {
        let recorded = scripts(&[("a", "h1"), ("b", "h2")]);
        verify_script_sources(
            &recorded,
            &script_hashes(&[("a", "h1"), ("b", "h2")]),
            &BTreeMap::new(),
        )
        .unwrap();
        for (label, rebuilt) in [
            (
                "changed hash",
                script_hashes(&[("a", "h1"), ("b", "other")]),
            ),
            ("missing script", script_hashes(&[("a", "h1")])),
            (
                "extra script",
                script_hashes(&[("a", "h1"), ("b", "h2"), ("c", "h3")]),
            ),
        ] {
            let error = verify_script_sources(&recorded, &rebuilt, &BTreeMap::new()).unwrap_err();
            assert!(
                error.to_string().contains("script sources in Data differ"),
                "{label}: {error}"
            );
        }
    }

    /// One failed selected archive member permits only that recorded key to be unavailable.
    #[test]
    fn metadata_script_recovery_authenticates_each_available_and_unavailable_key() {
        let recorded = scripts(&[("a", "h1"), ("b", "h2"), ("c", "h3")]);
        let unavailable =
            BTreeMap::from([("scripts/b.pex".into(), "archive member failed".into())]);
        verify_script_sources(
            &recorded,
            &script_hashes(&[("a", "h1"), ("c", "h3")]),
            &unavailable,
        )
        .unwrap();
        for available in [
            script_hashes(&[("a", "h1")]),
            script_hashes(&[("a", "changed"), ("c", "h3")]),
            script_hashes(&[("a", "h1"), ("c", "h3"), ("extra", "h4")]),
        ] {
            assert!(verify_script_sources(&recorded, &available, &unavailable).is_err());
        }
        let extra_failure =
            BTreeMap::from([("scripts/extra.pex".into(), "archive member failed".into())]);
        assert!(
            verify_script_sources(
                &recorded,
                &script_hashes(&[("a", "h1"), ("b", "h2"), ("c", "h3")]),
                &extra_failure,
            )
            .is_err()
        );
    }

    /// Script entries leave the manifest and keep their source hashes; others stay.
    #[test]
    fn script_entries_are_taken_by_source_key() {
        let mut manifest = ConversionManifest::default();
        manifest.entries.extend(scripts(&[("a", "h1")]));
        manifest
            .entries
            .insert("meshes/x.nif".into(), entry("meshes/x.glb", "m"));
        let taken = take_script_entries(&mut manifest);
        assert_eq!(taken.keys().collect::<Vec<_>>(), ["scripts/a.pex"]);
        assert_eq!(taken["scripts/a.pex"].source_hash, "h1");
        assert_eq!(
            manifest.entries.keys().collect::<Vec<_>>(),
            ["meshes/x.nif"]
        );
    }

    /// A lineage digest replaces only the recorded schema digest. Any other difference in the
    /// reader identity still refuses, and without lineage the earlier digest is unknown.
    #[test]
    fn lineage_substitutes_only_the_schema_digest() {
        let schemas = || RuntimeSchemas::from_addon_bytes(&[]).unwrap();
        let config = PipelineConfig::new("Data", "output");
        let earlier = "ee7e85d996711c656abcc427fb76496452c7d0e89c1b383abf64a8acd1392d30";
        let mut identity =
            crate::esm::inhouse::reader_identity_with_schemas(RecordReader::Inhouse, &schemas());
        identity["schema_sha256"] = serde_json::json!(earlier);
        assert!(settings_for_source_reader(&config, &identity, schemas()).is_err());
        settings_for_source_reader_in_lineage(&config, &identity, schemas(), Some(earlier))
            .unwrap();
        let mut other_adapter = identity.clone();
        other_adapter["adapter_version"] = serde_json::json!(1);
        assert!(
            settings_for_source_reader_in_lineage(
                &config,
                &other_adapter,
                schemas(),
                Some(earlier)
            )
            .is_err()
        );
        let mut other_diagnostics = identity;
        other_diagnostics["diagnostics_version"] = serde_json::json!(999);
        assert!(
            settings_for_source_reader_in_lineage(
                &config,
                &other_diagnostics,
                schemas(),
                Some(earlier)
            )
            .is_err()
        );
    }

    /// The combined proof names its script ABI, and nothing else may differ.
    #[test]
    fn script_abi_is_recovered_from_the_combined_proof_only() {
        let mut config = PipelineConfig::new("Data", "output");
        config.record_reader = RecordReader::Inhouse;
        let identity = crate::esm::inhouse::configured_reader_identity(&config).unwrap();
        for abi in 1..=config.script_abi_version {
            let mut producer = config.clone();
            producer.script_abi_version = abi;
            let recorded = crate::cache::configuration_hash(&producer).unwrap();
            assert_eq!(
                recover_script_abi(
                    &config,
                    CONVERTER_SCHEMA_VERSION,
                    &recorded,
                    &identity,
                    None
                )
                .unwrap(),
                Some(abi)
            );
            // Any other changed setting finds no ABI.
            let mut changed = config.clone();
            changed.texture_zstd_level += 1;
            assert_eq!(
                recover_script_abi(
                    &changed,
                    CONVERTER_SCHEMA_VERSION,
                    &recorded,
                    &identity,
                    None
                )
                .unwrap(),
                None
            );
        }
    }
}
