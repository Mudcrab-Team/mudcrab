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
        CONVERTER_SCHEMA_VERSION, ConversionManifest, configuration_hash, hash_bytes, hash_file,
        retained_configuration_matches,
    },
    esm::{EsmParser, cell_cache::write_cell_cache, exporter::validate_database},
    integration::finalize_world_database,
    lod::albedo::terrain_diffuse_paths,
    mesh::{MeshConverter, nif_source_hash, nif_unrepaired_source_hash, prune_glb_texture_bytes},
    pipeline::{
        archive_load_order_priority, compile_lod_chunks, discover, overlay_loose_assets,
        publish_new_directory, sort_archives_by_load_order, staging_path, validate_artifacts,
    },
};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use shared::asset_lock::AssetLock;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    time::Instant,
};
use tokio::sync::mpsc::Sender;
use walkdir::WalkDir;

#[derive(Serialize)]
struct MetadataProvenance {
    record_reader: serde_json::Value,
    retained_asset_record_reader: serde_json::Value,
    source_converter_schema: u32,
    source_retained_mesh_schema: u32,
    source_manifest_hash: String,
    source_configuration_hash: String,
    retained_asset_configuration_hash: String,
    retained_asset_hashes: BTreeMap<String, String>,
    replay_verified_pruned_assets: BTreeSet<String>,
}

impl AssetPipeline {
    /// Publishes only to a new, disjoint directory. The source package and its
    /// retained GLB/KTX2/Luau bytes are never modified or relabeled in place.
    pub async fn rebuild_metadata_async(
        mut config: PipelineConfig,
        source_assets: &Path,
        progress_tx: Sender<ProgressEvent>,
    ) -> Result<PipelineReport> {
        config.validate()?;
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
        let _source_lock = AssetLock::acquire_shared(&source)?;
        let output_lock = AssetLock::acquire_exclusive(&config.output_dir)?;
        let source_manifest = fs::read(source.join("conversion-manifest.json"))?;
        // Read the original version; normal cache loading deliberately invalidates
        // incompatible assets during migration, which this explicit route verifies instead.
        let mut manifest: ConversionManifest = serde_json::from_slice(&source_manifest)?;
        let source_reader = source_reader_identity(&source)?;
        let retained_reader = retained_reader_identity(&source, &source_reader)?;
        let source_settings = settings_for_source_reader(&config, &source_reader)?;
        let retained_settings = settings_for_source_reader(&config, &retained_reader)?;
        ensure!(
            (matches!(manifest.schema_version, 15 | 16 | 19..=24)
                || manifest.schema_version == CONVERTER_SCHEMA_VERSION)
                && manifest.complete
                && manifest.failures.is_empty(),
            "metadata rebuild requires complete converter schema 15, 16, 19 through 24 or {} assets",
            CONVERTER_SCHEMA_VERSION
        );
        ensure!(
            manifest.retained_mesh_schema_version.is_some()
                || !source.join("metadata-rebuild.json").exists(),
            "missing retained mesh provenance in metadata-only source; rebuild from verified original assets"
        );
        let mesh_schema = manifest
            .retained_mesh_schema_version
            .unwrap_or(manifest.schema_version);
        ensure!(
            (matches!(mesh_schema, 15 | 16 | 19..=24) || mesh_schema == CONVERTER_SCHEMA_VERSION)
                && mesh_schema <= manifest.schema_version,
            "unsupported retained mesh cache contract"
        );
        ensure!(
            manifest.retained_mesh_schema_version.is_none()
                || manifest.retained_asset_configuration_hash.is_some(),
            "missing retained producer configuration; rebuild from verified original assets"
        );
        ensure!(
            retained_configuration_matches(
                &source_settings,
                manifest.schema_version,
                &manifest.configuration_hash
            )?,
            "retained asset configuration does not match rebuild settings"
        );
        let retained_configuration = manifest
            .retained_asset_configuration_hash
            .as_deref()
            .unwrap_or(&manifest.configuration_hash);
        ensure!(
            retained_configuration_matches(
                &retained_settings,
                mesh_schema,
                retained_configuration
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
            &source_manifest,
            &mut manifest,
            &progress_tx,
        )
        .await;
        let mut report = match result {
            Ok(report) => report,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
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
        Ok(crate::esm::inhouse::reader_identity(
            crate::config::RecordReader::Legacy,
        ))
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
            current["mode"] == "legacy",
            "missing retained asset reader provenance"
        );
    }
    Ok(current.clone())
}

/// Validate asset settings under their recorded reader without relabeling retained bytes.
fn settings_for_source_reader(
    config: &PipelineConfig,
    identity: &serde_json::Value,
) -> Result<PipelineConfig> {
    let reader: crate::config::RecordReader = serde_json::from_value(identity["mode"].clone())?;
    ensure!(
        identity == &crate::esm::inhouse::reader_identity(reader),
        "unsupported source reader producer identity; use normal conversion to regenerate assets before metadata-only migration"
    );
    let mut settings = config.clone();
    settings.record_reader = reader;
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
            seen.insert(name.to_ascii_lowercase()),
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

async fn rebuild_into(
    config: &PipelineConfig,
    source: &Path,
    staging: &Path,
    matched: &(Vec<PathBuf>, Vec<String>),
    source_manifest: &[u8],
    manifest: &mut ConversionManifest,
    progress: &Sender<ProgressEvent>,
) -> Result<PipelineReport> {
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
        record_reader: crate::esm::inhouse::reader_identity(config.record_reader),
        retained_asset_record_reader: retained_reader_identity(
            source,
            &source_reader_identity(source)?,
        )?,
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
        if archive_load_order_priority(archive, plugins).is_some() {
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
    if config.record_reader == crate::config::RecordReader::Inhouse {
        let strings = crate::esm::inhouse::localized_string_paths(plugins);
        for archive in &archives {
            // Strings are metadata inputs; decode only the selected plugin tables.
            if let Err(error) = ArchiveExtractor::extract_paths(archive, &vfs, &strings) {
                eprintln!(
                    "warning: {}: unable to extract localized names: {error}",
                    archive.display()
                );
            }
        }
    }
    let merged =
        EsmParser::convert_plugins_with_reader(plugins, &database, config.record_reader, &vfs)?;
    validate_database(&Connection::open(&database)?)?;
    if config.record_reader == crate::config::RecordReader::Inhouse {
        crate::esm::inhouse::write_terrain_caches(&merged, &database, staging)?;
    } else {
        write_cell_cache(&merged, &staging.join("cell_cache.rkyv"))?;
    }
    crate::esm::inhouse::write_reader_identity(staging, config.record_reader)?;
    report.artifacts.push(PathBuf::from("record-reader.json"));
    if config.record_reader == crate::config::RecordReader::Inhouse {
        report
            .artifacts
            .push(PathBuf::from("inhouse-reader-diagnostics.json"));
        report
            .artifacts
            .push(PathBuf::from("cell_cache_preserved.rkyv"));
    }
    drop(merged);
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
        plugins,
        plugin_hashes,
        progress,
        &mut report,
    )
    .await?;
    report.lod_elapsed_ms = lod_started.elapsed().as_millis();
    let integration = finalize_world_database(staging)?
        .ok_or_else(|| color_eyre::eyre::eyre!("missing rebuilt world database"))?;
    ensure!(
        integration.passed,
        "rebuilt asset integration failed: {:?}",
        integration.issues
    );
    report.integration = Some(integration);
    report
        .artifacts
        .push(PathBuf::from("integration-report.json"));
    // Albedo is embedded in new LOD payloads. Retained textures still match
    // the source manifest; validate copied model dependencies separately.
    let payloads = report
        .artifacts
        .iter()
        .filter(|path| path.starts_with("lod"))
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
    // No archive extraction cache is copied by this route.
    manifest.archives.clear();
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
    report.complete = true;
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
        for entry in WalkDir::new(source.join(folder)).follow_links(false) {
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
