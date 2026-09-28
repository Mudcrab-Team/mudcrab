use crate::{
    archive::{ArchiveExtractor, ExtractionProgress},
    asset_path::{AssetKind, canonical_asset_path, resolve_asset_uri},
    cache::{
        CacheEntry, ConversionManifest, configuration_hash, configuration_hash_for_schema,
        hash_file,
    },
    config::PipelineConfig,
    esm::{EsmParser, cell_cache::write_cell_cache, exporter::validate_database, read_plugins_txt},
    integration::{IntegrationReport, finalize_world_database},
    mesh::MeshConverter,
    progress::{ProgressEvent, ProgressStage},
    script::ScriptConverter,
    texture::{TextureConverter, TextureEncoding, TextureSemantic},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::mpsc::{Sender, unbounded_channel},
    task::spawn_blocking,
};
use walkdir::WalkDir;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PipelineReport {
    pub complete: bool,
    pub converted: u64,
    pub cache_hits: u64,
    pub skipped: u64,
    pub warnings: Vec<String>,
    pub artifacts: Vec<PathBuf>,
    pub inputs_by_kind: BTreeMap<String, u64>,
    pub elapsed_ms: u128,
    pub integration: Option<IntegrationReport>,
}

/// A run's stop button. The command line sets it when the user interrupts (Ctrl+C); the pipeline
/// checks it between units of work, finishes the asset in flight, and leaves the staging folder
/// behind so `--resume-staging` can pick the run up where it stopped.
#[derive(Clone, Default)]
pub struct Cancellation {
    flag: Arc<AtomicBool>,
}

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

/// A run that stopped short of publishing. `staging` is the folder left behind, when one was kept,
/// and `cancelled` says whether the user interrupted the run rather than it failing.
#[derive(Debug)]
pub struct PipelineFailure {
    pub error: color_eyre::Report,
    pub staging: Option<PathBuf>,
    pub cancelled: bool,
}

impl fmt::Display for PipelineFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:#}", self.error)
    }
}

impl std::error::Error for PipelineFailure {}

impl From<color_eyre::Report> for PipelineFailure {
    fn from(error: color_eyre::Report) -> Self {
        Self {
            error,
            staging: None,
            cancelled: false,
        }
    }
}

/// Renames the staged tree over the output, unless the run was interrupted while it was packing
/// up. The check here is the last one before the output changes, so an interrupt that lands after
/// the final stage stops the run instead of publishing over a good output — the staging folder is
/// kept either way, so the run resumes from where it stopped.
fn publish_if_not_interrupted(
    staging: &Path,
    output: &Path,
    cancellation: &Cancellation,
) -> Result<(), PipelineFailure> {
    if cancellation.is_cancelled() {
        return Err(failure(
            color_eyre::eyre::eyre!("conversion interrupted"),
            staging,
            cancellation,
        ));
    }
    publish_directory(staging, output).map_err(|error| failure(error, staging, cancellation))
}

/// A failure with the staging folder attached when one was kept, so the caller can tell the user
/// how to resume.
fn failure(
    error: color_eyre::Report,
    staging: &Path,
    cancellation: &Cancellation,
) -> PipelineFailure {
    PipelineFailure {
        error,
        staging: staging.exists().then(|| staging.to_path_buf()),
        cancelled: cancellation.is_cancelled(),
    }
}

pub struct AssetPipeline;

impl AssetPipeline {
    pub async fn run_async(
        config: PipelineConfig,
        progress_tx: Sender<ProgressEvent>,
    ) -> Result<PipelineReport, PipelineFailure> {
        Self::run_async_with_cancel(config, progress_tx, Cancellation::new()).await
    }

    pub async fn run_async_with_cancel(
        config: PipelineConfig,
        progress_tx: Sender<ProgressEvent>,
        cancellation: Cancellation,
    ) -> Result<PipelineReport, PipelineFailure> {
        config.validate()?;
        let started = Instant::now();
        send(
            &progress_tx,
            ProgressStage::Discovering,
            0,
            0,
            None,
            "Discovering Skyrim assets",
        )
        .await;
        let previous_manifest = if config.invalidate_cache {
            ConversionManifest::default()
        } else {
            ConversionManifest::load(&config.output_dir.join("conversion-manifest.json"))?
        };
        let expected_configuration = configuration_hash(&config)?;
        let configuration_is_compatible = previous_manifest.configuration_hash
            == expected_configuration
            || (matches!(previous_manifest.schema_version, 12 | 13)
                && previous_manifest.configuration_hash
                    == configuration_hash_for_schema(&config, previous_manifest.schema_version)?);
        let previous_manifest = if configuration_is_compatible {
            previous_manifest
        } else {
            ConversionManifest::default()
        };
        let staging = config
            .resume_staging
            .clone()
            .unwrap_or_else(|| staging_path(&config.output_dir));
        fs::create_dir_all(staging.join("vfs"))
            .wrap_err_with(|| format!("failed to create {}", staging.join("vfs").display()))?;
        // A run that stops keeps its staging folder, whether it failed or was interrupted: the
        // folder is everything the run has done so far, and the caller reports the command that
        // resumes from it. Only publishing removes it, by renaming it over the output.
        let run_result = Self::run_into(
            &config,
            &staging,
            &previous_manifest,
            &progress_tx,
            &cancellation,
        )
        .await;
        let mut report = match run_result {
            Ok(report) => report,
            Err(error) => return Err(failure(error, &staging, &cancellation)),
        };
        send(
            &progress_tx,
            ProgressStage::Publishing,
            1,
            1,
            None,
            "Publishing converted assets",
        )
        .await;
        // The run's own checks are behind it, but an interrupt that landed while it was packing up
        // must still stop it before the staging folder is renamed over the output. The folder is
        // kept, so the run resumes from where it stopped.
        publish_if_not_interrupted(&staging, &config.output_dir, &cancellation)?;
        report.elapsed_ms = started.elapsed().as_millis();
        if report.complete {
            send(
                &progress_tx,
                ProgressStage::Complete,
                1,
                1,
                None,
                "Asset conversion complete",
            )
            .await;
        }
        Ok(report)
    }

    async fn run_into(
        config: &PipelineConfig,
        staging: &Path,
        previous: &ConversionManifest,
        progress_tx: &Sender<ProgressEvent>,
        cancellation: &Cancellation,
    ) -> Result<PipelineReport> {
        let mut report = PipelineReport::default();
        let mut manifest = ConversionManifest {
            schema_version: crate::cache::CONVERTER_SCHEMA_VERSION,
            complete: false,
            configuration_hash: configuration_hash(config)?,
            inputs_by_kind: Default::default(),
            failures: Default::default(),
            archives: Default::default(),
            entries: Default::default(),
        };
        let files = discover(&config.data_dir)?;
        let plugins = plugin_paths(config, &files)?;
        let archives: Vec<_> = files
            .iter()
            .filter(|path| extension(path, &["bsa", "ba2"]))
            .cloned()
            .collect();
        let mut enabled_archives: Vec<_> = archives
            .into_iter()
            .filter(|archive| !extension(archive, &["ba2"]) || config.enable_ba2)
            .collect();
        sort_archives_by_load_order(&mut enabled_archives, &plugins);
        if !enabled_archives.is_empty() {
            send(
                progress_tx,
                ProgressStage::Extracting,
                0,
                enabled_archives.len() as u64,
                None,
                "Extracting Skyrim archives",
            )
            .await;
        }

        let vfs_dir = staging.join("vfs");
        fs::create_dir_all(&vfs_dir)?;

        // Bytes already extracted, so each archive reports progress against the whole run instead
        // of restarting at zero.
        let mut extracted_bytes = 0u64;

        for (index, archive) in enabled_archives.iter().enumerate() {
            interrupt(cancellation)?;
            send(
                progress_tx,
                ProgressStage::Extracting,
                index as u64,
                enabled_archives.len() as u64,
                Some(archive.clone()),
                "Extracting archive",
            )
            .await;
            let archive_for_worker = archive.clone();
            let vfs_for_worker = vfs_dir.clone();
            let previous_cache_root = config.output_dir.join(".ingestion-cache");
            let cache_root = staging.join(".ingestion-cache");
            let archive_key = archive
                .strip_prefix(&config.data_dir)
                .unwrap_or(archive)
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase();
            let previous_entry = previous.archives.get(&archive_key).cloned();
            let verify_cache = config.verify_cache;

            // The extractor reports file by file; the event keeps the archive count the launcher
            // reads and carries the smoother per-file fraction for the status line.
            let archive_totals = Arc::new(Mutex::new((0u64, 0u64)));
            let totals_for_worker = Arc::clone(&archive_totals);
            let progress_for_worker = progress_tx.clone();
            let progress_archive = archive.clone();
            let archives_total = enabled_archives.len() as u64;
            let archive_index = index as u64;
            let progress = move |extracted: ExtractionProgress| {
                *totals_for_worker
                    .lock()
                    .expect("archive totals mutex poisoned") =
                    (extracted.total_files, extracted.total_bytes);
                let archive_fraction = if extracted.total_bytes > 0 {
                    extracted.completed_bytes as f32 / extracted.total_bytes as f32
                } else if extracted.total_files > 0 {
                    extracted.completed_files as f32 / extracted.total_files as f32
                } else {
                    1.0
                };
                let _ = progress_for_worker.blocking_send(
                    ProgressEvent::new(
                        ProgressStage::Extracting,
                        archive_index,
                        archives_total,
                        Some(progress_archive.clone()),
                        "Extracting archive",
                    )
                    .with_bytes(
                        extracted_bytes + extracted.completed_bytes,
                        extracted_bytes + extracted.total_bytes,
                    )
                    .with_stage_fraction(
                        (archive_index as f32 + archive_fraction) / archives_total as f32,
                    ),
                );
            };

            // A stop is checked before every entry, so it does not wait for a multi-gigabyte
            // archive to finish.
            let stop_for_worker = cancellation.clone();
            #[cfg(test)]
            let output_for_hook = config.output_dir.clone();
            let stop = move || {
                #[cfg(test)]
                stop_hook::tick(&output_for_hook, &stop_for_worker);
                stop_for_worker.is_cancelled()
            };

            let result = spawn_blocking(move || {
                ArchiveExtractor::extract_cached(
                    &archive_for_worker,
                    &vfs_for_worker,
                    &previous_cache_root,
                    &cache_root,
                    previous_entry.as_ref(),
                    verify_cache,
                    Some(&progress),
                    Some(&stop),
                )
            })
            .await
            .wrap_err("archive worker panicked")?;
            // An archive abandoned by a stop ends the run as an interrupt, the same as a stop
            // between archives, rather than being counted as a skipped archive. Its cache entry
            // was never recorded, so a resume extracts it again from the start.
            if result.is_err() {
                interrupt(cancellation)?;
            }

            send(
                progress_tx,
                ProgressStage::Extracting,
                (index + 1) as u64,
                enabled_archives.len() as u64,
                Some(archive.clone()),
                "Extracted archive",
            )
            .await;

            match result {
                Ok(outcome) => {
                    let (_, bytes) = *archive_totals
                        .lock()
                        .expect("archive totals mutex poisoned");
                    extracted_bytes += bytes;
                    if outcome.cache_hit {
                        report.cache_hits += outcome.files.len() as u64;
                    } else {
                        report.converted += outcome.files.len() as u64;
                    }
                    manifest.archives.insert(archive_key, outcome.cache_entry);
                }
                Err(error) if !config.fail_fast => {
                    report.skipped += 1;
                    let message = format!("{}: {error:#}", archive.display());
                    manifest.failures.insert(
                        archive.to_string_lossy().replace('\\', "/"),
                        message.clone(),
                    );
                    report.warnings.push(message);
                }
                Err(error) => return Err(error),
            }
        }

        overlay_loose_assets(&config.data_dir, &staging.join("vfs"), &files)?;
        interrupt(cancellation)?;

        if !plugins.is_empty() {
            send(
                progress_tx,
                ProgressStage::Database,
                0,
                plugins.len() as u64,
                None,
                "Building skyrim_world.db",
            )
            .await;
            let db_path = staging.join("skyrim_world.db");
            EsmParser::convert_plugins(&plugins, &db_path)?;
            validate_database(&Connection::open(&db_path)?)?;
            let merged = EsmParser::merge_plugins(&plugins)?;
            write_cell_cache(&merged, &staging.join("cell_cache.rkyv"))?;
            report.artifacts.extend([
                PathBuf::from("skyrim_world.db"),
                PathBuf::from("cell_cache.rkyv"),
            ]);
        }

        let vfs_files = discover(&staging.join("vfs"))?;
        {
            let mut batch = ConversionBatch {
                config,
                staging,
                previous,
                manifest: &mut manifest,
                report: &mut report,
                progress_tx,
                cancellation,
            };
            batch
                .convert_kind(&vfs_files, "nif", ProgressStage::Meshes, None)
                .await?;
        }
        let texture_semantics = collect_texture_semantics(staging)?;
        {
            let mut batch = ConversionBatch {
                config,
                staging,
                previous,
                manifest: &mut manifest,
                report: &mut report,
                progress_tx,
                cancellation,
            };
            batch
                .convert_kind(
                    &vfs_files,
                    "dds",
                    ProgressStage::Textures,
                    Some(&texture_semantics),
                )
                .await?;
            let aliases = publish_srgb_texture_aliases(staging)?;
            batch.report.artifacts.extend(aliases);
            let pruned = MeshConverter::prune_dangling_texture_uris(staging)?;
            let pruned_uris: u64 = pruned
                .iter()
                .map(|file| file.removed_uris.len() as u64)
                .sum();
            let mut pruned_completed = 0;
            for file in &pruned {
                for uri in &file.removed_uris {
                    pruned_completed += 1;
                    // The warning goes out as a notice on the progress channel rather than to
                    // stderr, so it cannot splice into the status line the CLI is redrawing.
                    send_notice(
                        progress_tx,
                        ProgressStage::Textures,
                        Some(PathBuf::from(&file.glb)),
                        &format!(
                            "warning: pruned dangling texture {uri} referenced by {} (no converted artifact)",
                            file.glb
                        ),
                    )
                    .await;
                    let key = resolve_asset_uri(staging, &staging.join(&file.glb), uri)
                        .ok()
                        .and_then(|resolved| {
                            resolved.strip_prefix(staging).ok().map(Path::to_path_buf)
                        })
                        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_else(|| uri.clone());
                    batch
                        .record_skip(
                            ProgressStage::Textures,
                            pruned_completed,
                            pruned_uris,
                            key,
                            PathBuf::from(&file.glb),
                            color_eyre::eyre::eyre!(
                                "texture {uri} has no converted artifact; reference pruned"
                            ),
                        )
                        .await;
                }
            }
            batch
                .convert_kind(&vfs_files, "pex", ProgressStage::Scripts, None)
                .await?;
        }
        interrupt(cancellation)?;
        if let Some(integration) = finalize_world_database(staging)? {
            if !integration.passed {
                report.warnings.push(format!(
                    "asset integration failed: {} missing models, {} invalid models, {} missing textures, terrain/cache cells {}/{}",
                    integration.missing_model_count,
                    integration.invalid_model_count,
                    integration.missing_texture_count,
                    integration.terrain_cells,
                    integration.cache_cells,
                ));
            }
            report.integration = Some(integration);
            report
                .artifacts
                .push(PathBuf::from("integration-report.json"));
        }
        let runtime_path = staging.join("scripts/papyrus_runtime.luau");
        if let Some(parent) = runtime_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &runtime_path,
            include_str!("../../shared/src/papyrus_runtime.luau"),
        )?;
        report
            .artifacts
            .push(PathBuf::from("scripts/papyrus_runtime.luau"));
        send(
            progress_tx,
            ProgressStage::Validating,
            0,
            report.artifacts.len() as u64,
            None,
            "Validating generated artifacts",
        )
        .await;
        validate_artifacts(staging, &report.artifacts, &texture_semantics)?;
        send(
            progress_tx,
            ProgressStage::Validating,
            report.artifacts.len() as u64,
            report.artifacts.len() as u64,
            None,
            "Generated artifacts are valid",
        )
        .await;
        interrupt(cancellation)?;
        manifest.complete = report.skipped == 0 && report.warnings.is_empty();
        report.complete = manifest.complete;
        report.inputs_by_kind = manifest.inputs_by_kind.clone();
        manifest.save(&staging.join("conversion-manifest.json"))?;
        report
            .artifacts
            .push(PathBuf::from("conversion-manifest.json"));
        Ok(report)
    }
}

struct ConversionBatch<'a> {
    config: &'a PipelineConfig,
    staging: &'a Path,
    previous: &'a ConversionManifest,
    manifest: &'a mut ConversionManifest,
    report: &'a mut PipelineReport,
    progress_tx: &'a Sender<ProgressEvent>,
    cancellation: &'a Cancellation,
}

impl ConversionBatch<'_> {
    async fn convert_kind(
        &mut self,
        files: &[PathBuf],
        source_ext: &str,
        stage: ProgressStage,
        texture_semantics: Option<&BTreeMap<String, BTreeSet<TextureSemantic>>>,
    ) -> Result<()> {
        let selected_paths: Vec<_> = files
            .iter()
            .filter(|path| extension(path, &[source_ext]))
            .cloned()
            .collect();

        let (target_ext, asset_kind) = match source_ext {
            "dds" => ("ktx2", AssetKind::Texture),
            "nif" => ("glb", AssetKind::Mesh),
            "pex" => ("luau", AssetKind::Script),
            _ => unreachable!(),
        };
        let staging_vfs = self.staging.join("vfs");
        let mut target_sources = BTreeMap::<String, PathBuf>::new();
        let mut selected = Vec::with_capacity(selected_paths.len());
        // The source sizes are one `stat` each and give the status line something to weigh the
        // stage by: a mesh and a texture are the same "item" but not the same work.
        let mut source_sizes = BTreeMap::<String, u64>::new();
        let mut bytes_total = 0u64;
        for source in selected_paths {
            let relative = source.strip_prefix(&staging_vfs)?.to_owned();
            let target_key =
                canonical_asset_path(&relative.to_string_lossy(), asset_kind, target_ext)?;
            if let Some(previous) = target_sources.insert(target_key.clone(), relative.clone()) {
                bail!(
                    "normalized output collision for {target_key}: {} and {}",
                    previous.display(),
                    relative.display()
                );
            }
            let source_key =
                canonical_asset_path(&relative.to_string_lossy(), asset_kind, source_ext)?;
            let encoding = if source_ext == "dds" {
                let known_semantics = texture_semantics
                    .and_then(|semantics| semantics.get(&target_key))
                    .cloned()
                    .unwrap_or_default();
                Some(TextureEncoding::from_semantics(&known_semantics)?)
            } else {
                None
            };
            let source_bytes = fs::metadata(&source).map_or(0, |metadata| metadata.len());
            bytes_total += source_bytes;
            source_sizes.insert(source_key.clone(), source_bytes);
            selected.push((
                source,
                relative,
                PathBuf::from(target_key),
                source_key,
                encoding,
            ));
        }

        self.manifest
            .inputs_by_kind
            .insert(source_ext.to_owned(), selected.len() as u64);

        if selected.is_empty() {
            return Ok(());
        }
        interrupt(self.cancellation)?;

        let total_files = selected.len() as u64;
        let progress_tx = self.progress_tx.clone();
        let (outcome_tx, mut outcome_rx) = unbounded_channel();

        let staging_root = self.staging.to_path_buf();
        let output_dir = self.config.output_dir.clone();
        let source_kind = source_ext.to_owned();
        let etc1s_quality = self.config.texture_etc1s_quality;
        let uastc_level = self.config.texture_uastc_level;
        let cpu_jobs = self.config.cpu_jobs;
        let previous_entries = self.previous.entries.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let run_cancelled = self.cancellation.clone();

        let rayon_handle = spawn_blocking(move || -> Result<()> {
            use rayon::prelude::*;

            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(cpu_jobs)
                .build()
                .wrap_err("failed to create asset conversion worker pool")?;
            pool.install(|| {
                selected.into_par_iter().enumerate().for_each(
                    |(index, (source, relative, target_rel, key, encoding))| {
                        // An interrupted run stops before starting another asset; the one already
                        // in flight finishes, so no half-written output is left behind.
                        if worker_cancelled.load(Ordering::Relaxed) || run_cancelled.is_cancelled()
                        {
                            return;
                        }
                        let target = staging_root.join(&target_rel);

                        let mut hash = match hash_file(&source) {
                            Ok(h) => h,
                            Err(err) => {
                                let _ = outcome_tx.send((
                                    index,
                                    key,
                                    String::new(),
                                    target_rel,
                                    relative.clone(),
                                    Err(err),
                                    target,
                                ));
                                return;
                            }
                        };

                        if let Some(encoding) = encoding {
                            hash.push_str(&format!(":texture-encoding:{encoding:?}"));
                        }

                        if source_kind == "nif" {
                            for dependency in MeshConverter::dependency_paths(&source) {
                                match hash_file(&dependency) {
                                    Ok(dep_hash) => {
                                        hash.push(':');
                                        hash.push_str(&dep_hash);
                                    }
                                    Err(err) => {
                                        let _ = outcome_tx.send((
                                            index,
                                            key,
                                            hash,
                                            target_rel,
                                            relative.clone(),
                                            Err(err),
                                            target,
                                        ));
                                        return;
                                    }
                                }
                            }
                        }

                        // Check cache
                        if let Some(entry) =
                            previous_entries.get(&key).filter(|e| e.source_hash == hash)
                        {
                            let old = output_dir.join(&entry.output);
                            if old.is_file()
                                && fs::metadata(&old).is_ok_and(|m| m.len() == entry.output_size)
                                && hash_file(&old).is_ok_and(|h| h == entry.output_hash)
                            {
                                if let Some(parent) = target.parent() {
                                    let _ = fs::create_dir_all(parent);
                                }
                                if fs::copy(&old, &target).is_ok() {
                                    let _ = outcome_tx.send((
                                        index,
                                        key,
                                        hash,
                                        target_rel,
                                        relative.clone(),
                                        Ok(true), // is_cache_hit = true
                                        target,
                                    ));
                                    return;
                                };
                            }
                        }

                        let existing_is_valid = target.is_file()
                            && fs::metadata(&target).is_ok_and(|metadata| metadata.len() > 0)
                            && match source_kind.as_str() {
                                "dds" => fs::read(&target).is_ok_and(|bytes| {
                                    crate::texture::inspect_ktx2(
                                        &bytes,
                                        encoding.expect("DDS conversion requires an encoding"),
                                    )
                                    .is_ok()
                                }),
                                "nif" | "pex" => true,
                                _ => false,
                            };

                        let result = if existing_is_valid {
                            Ok(())
                        } else {
                            match source_kind.as_str() {
                                "dds" => TextureConverter::convert_dds_to_ktx2_with_options(
                                    &source,
                                    &target,
                                    encoding.expect("DDS conversion requires an encoding"),
                                    etc1s_quality,
                                    uastc_level,
                                )
                                .map(|_| ()),
                                "nif" => MeshConverter::convert_nif_to_glb(&source, &target),
                                "pex" => ScriptConverter::convert_pex_to_luau(&source, &target),
                                _ => unreachable!(),
                            }
                        };

                        let result = result
                            .map(|_| false)
                            .wrap_err_with(|| format!("failed to convert {}", relative.display()));
                        let _ = outcome_tx.send((
                            index,
                            key,
                            hash,
                            target_rel,
                            relative.to_path_buf(),
                            result,
                            target,
                        ));
                    },
                );
            });
            Ok(())
        });

        let mut completed = 0u64;
        let mut first_error = None;
        let fail_fast = self.config.fail_fast;
        let mut bytes_completed = 0u64;
        while let Some((_, key, hash, target_rel, relative, conversion, target)) =
            outcome_rx.recv().await
        {
            completed += 1;
            bytes_completed += source_sizes.get(&key).copied().unwrap_or(0);
            if self.cancellation.is_cancelled() {
                break;
            }

            match conversion {
                Ok(is_cache_hit) => {
                    if fail_fast && first_error.is_some() {
                        continue;
                    }
                    if !is_cache_hit {
                        let size = match fs::metadata(&target) {
                            Ok(metadata) => metadata.len(),
                            Err(error) => {
                                if fail_fast {
                                    return Err(error).wrap_err_with(|| {
                                        format!("failed to convert {}", relative.display())
                                    });
                                }
                                self.record_skip(
                                    stage,
                                    completed,
                                    total_files,
                                    key,
                                    relative,
                                    error.into(),
                                )
                                .await;
                                continue;
                            }
                        };
                        let output_hash = match hash_file(&target) {
                            Ok(output_hash) => output_hash,
                            Err(error) => {
                                if fail_fast {
                                    return Err(error).wrap_err_with(|| {
                                        format!("failed to convert {}", relative.display())
                                    });
                                }
                                self.record_skip(
                                    stage,
                                    completed,
                                    total_files,
                                    key,
                                    relative,
                                    error,
                                )
                                .await;
                                continue;
                            }
                        };
                        send_with_bytes(
                            &progress_tx,
                            stage,
                            completed,
                            total_files,
                            Some(relative.clone()),
                            "Converted asset",
                            (bytes_completed, bytes_total),
                        )
                        .await;
                        self.manifest.entries.insert(
                            key,
                            CacheEntry {
                                source_hash: hash,
                                output: target_rel
                                    .to_string_lossy()
                                    .into_owned()
                                    .replace('\\', "/"),
                                output_size: size,
                                output_hash,
                            },
                        );
                        self.report.converted += 1;
                    } else {
                        send_with_bytes(
                            &progress_tx,
                            stage,
                            completed,
                            total_files,
                            Some(relative.clone()),
                            "Converted asset",
                            (bytes_completed, bytes_total),
                        )
                        .await;
                        if let Some(entry) = self.previous.entries.get(&key) {
                            self.manifest.entries.insert(key, entry.clone());
                        }
                        self.report.cache_hits += 1;
                    }
                    self.report.artifacts.push(target_rel);
                }
                Err(error) => {
                    if fail_fast {
                        if first_error.is_none() {
                            cancelled.store(true, Ordering::Relaxed);
                            send(
                                &progress_tx,
                                stage,
                                completed,
                                total_files,
                                Some(relative),
                                "Asset conversion failed",
                            )
                            .await;
                            first_error = Some(error);
                        }
                    } else {
                        self.record_skip(stage, completed, total_files, key, relative, error)
                            .await;
                    }
                }
            }
        }

        rayon_handle
            .await
            .wrap_err("rayon batch worker panicked")??;
        interrupt(self.cancellation)?;
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    async fn record_skip(
        &mut self,
        stage: ProgressStage,
        completed: u64,
        total: u64,
        key: String,
        relative: PathBuf,
        error: color_eyre::eyre::Error,
    ) {
        send(
            self.progress_tx,
            stage,
            completed,
            total,
            Some(relative.clone()),
            "Asset skipped",
        )
        .await;
        let message = format!("{}: {error:#}", relative.display());
        self.manifest.failures.insert(key, message.clone());
        self.report.warnings.push(message);
        self.report.skipped += 1;
    }
}

fn collect_texture_semantics(
    staging: &Path,
) -> Result<BTreeMap<String, BTreeSet<TextureSemantic>>> {
    let mut semantics = BTreeMap::<String, BTreeSet<TextureSemantic>>::new();
    for entry in WalkDir::new(staging)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
    {
        let glb = entry.path();
        if !extension(glb, &["glb"]) {
            continue;
        }
        for dependency in MeshConverter::glb_texture_dependencies(glb)? {
            let resolved = resolve_asset_uri(staging, glb, &dependency.uri)?;
            let relative = resolved.strip_prefix(staging)?;
            let key = canonical_asset_path(&relative.to_string_lossy(), AssetKind::Texture, "ktx2")
                .and_then(|key| source_texture_key(&key))
                .wrap_err_with(|| {
                    format!(
                        "invalid texture dependency {:?} resolved from {}",
                        dependency.uri,
                        glb.display()
                    )
                })?;
            semantics
                .entry(key)
                .or_default()
                .insert(dependency.semantic);
        }
    }

    let database = staging.join("skyrim_world.db");
    if database.is_file() {
        let connection = Connection::open(&database)?;
        let columns = [
            ("diffuse_path", TextureSemantic::BaseColor),
            ("normal_path", TextureSemantic::Normal),
            ("glow_path", TextureSemantic::Emissive),
            ("height_path", TextureSemantic::Height),
            ("environment_path", TextureSemantic::EnvironmentCube),
            ("mask_path", TextureSemantic::EnvironmentMask),
            ("specular_path", TextureSemantic::SpecularGlossiness),
            ("detail_path", TextureSemantic::Detail),
        ];
        for (column, semantic) in columns {
            let query = format!(
                "SELECT {column} FROM texture_sets WHERE {column} IS NOT NULL AND {column} <> ''"
            );
            let mut statement = connection.prepare(&query)?;
            let paths = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for path in paths {
                insert_texture_semantic(&mut semantics, &path, semantic).wrap_err_with(|| {
                    format!("invalid texture_sets.{column} reference {path:?}")
                })?;
            }
        }
        let mut statement = connection.prepare(
            "SELECT flow_normal_path FROM waters \
             WHERE flow_normal_path IS NOT NULL AND flow_normal_path <> ''",
        )?;
        let paths = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for path in paths {
            insert_texture_semantic(&mut semantics, &path, TextureSemantic::Normal)
                .wrap_err_with(|| format!("invalid waters.flow_normal_path reference {path:?}"))?;
        }
    }

    for (path, texture_semantics) in &semantics {
        TextureEncoding::from_semantics(texture_semantics)
            .wrap_err_with(|| format!("incompatible texture uses for {path}"))?;
    }
    Ok(semantics)
}

fn insert_texture_semantic(
    semantics: &mut BTreeMap<String, BTreeSet<TextureSemantic>>,
    path: &str,
    semantic: TextureSemantic,
) -> Result<()> {
    let key = canonical_asset_path(path, AssetKind::Texture, "ktx2")?;
    semantics.entry(key).or_default().insert(semantic);
    Ok(())
}

fn source_texture_key(runtime_key: &str) -> Result<String> {
    if let Some(stem) = runtime_key.strip_suffix(".opensky-srgb.ktx2") {
        return Ok(format!("{stem}.ktx2"));
    }
    Ok(runtime_key.to_owned())
}

fn publish_srgb_texture_aliases(staging: &Path) -> Result<Vec<PathBuf>> {
    let mut aliases = BTreeSet::new();
    for entry in WalkDir::new(staging)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file() && extension(entry.path(), &["glb"]))
    {
        let glb = entry.path();
        for dependency in MeshConverter::glb_texture_dependencies(glb)? {
            let destination = resolve_asset_uri(staging, glb, &dependency.uri)?;
            let relative = destination.strip_prefix(staging)?.to_owned();
            let runtime_key =
                canonical_asset_path(&relative.to_string_lossy(), AssetKind::Texture, "ktx2")?;
            if runtime_key.ends_with(".opensky-srgb.ktx2") {
                aliases.insert(PathBuf::from(runtime_key));
            }
        }
    }
    let mut published = Vec::new();
    for alias in aliases {
        let source = staging.join(source_texture_key(&alias.to_string_lossy())?);
        if !source.is_file() {
            continue;
        }
        let destination = staging.join(&alias);
        if destination.is_file() {
            fs::remove_file(&destination)?;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::hard_link(&source, &destination)
            .or_else(|_| fs::copy(&source, &destination).map(|_| ()))?;
        published.push(alias);
    }
    Ok(published)
}

fn discover(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<_> = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .collect();
    files.sort_by_key(|path| path.to_string_lossy().to_ascii_lowercase());
    Ok(files)
}

fn validate_artifacts(
    staging: &Path,
    artifacts: &[PathBuf],
    texture_semantics: &BTreeMap<String, BTreeSet<TextureSemantic>>,
) -> Result<()> {
    let lua = mlua::Lua::new();
    for relative in artifacts {
        let path = staging.join(relative);
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("ktx2") => {
                let bytes = fs::read(&path)?;
                let key = source_texture_key(&canonical_asset_path(
                    &relative.to_string_lossy(),
                    AssetKind::Texture,
                    "ktx2",
                )?)?;
                let known_semantics = texture_semantics.get(&key).cloned().unwrap_or_default();
                let encoding = TextureEncoding::from_semantics(&known_semantics)?;
                let metadata = crate::texture::inspect_ktx2(&bytes, encoding)
                    .wrap_err_with(|| format!("invalid KTX2 {}", path.display()))?;
                ensure!(
                    metadata.encoded_bytes == fs::metadata(&path)?.len()
                        && !metadata.sha256.is_empty()
                        && metadata.expanded_rgba_bytes > 0,
                    "KTX2 metadata validation failed for {}",
                    path.display()
                );
            }
            Some("glb") => {
                let bytes = fs::read(&path)?;
                if bytes.len() < 12 || &bytes[..4] != b"glTF" {
                    bail!("invalid GLB artifact {}", path.display());
                }
            }
            Some("luau") => {
                let source = fs::read_to_string(&path)?;
                lua.load(&source)
                    .set_name(path.to_string_lossy())
                    .into_function()
                    .map_err(|error| {
                        color_eyre::eyre::eyre!("invalid Luau artifact {}: {error}", path.display())
                    })?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn overlay_loose_assets(data: &Path, vfs: &Path, files: &[PathBuf]) -> Result<()> {
    let mut seen = BTreeMap::<String, PathBuf>::new();
    for source in files
        .iter()
        .filter(|path| extension(path, &["dds", "nif", "pex"]))
    {
        let relative = source.strip_prefix(data)?;
        let (kind, extension) = if extension(source, &["dds"]) {
            (AssetKind::Texture, "dds")
        } else if extension(source, &["nif"]) {
            (AssetKind::Mesh, "nif")
        } else {
            (AssetKind::Script, "pex")
        };
        let canonical = canonical_asset_path(&relative.to_string_lossy(), kind, extension)?;
        if let Some(previous) = seen.insert(canonical.clone(), source.to_owned()) {
            bail!(
                "loose assets contain normalized path collision for {canonical}: {} and {}",
                previous.display(),
                source.display()
            );
        }
        let destination = vfs.join(canonical);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn plugin_paths(config: &PipelineConfig, files: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if let Some(path) = &config.plugins_file {
        return read_plugins_txt(path, &config.data_dir);
    }
    let mut plugins: Vec<_> = files
        .iter()
        .filter(|path| extension(path, &["esm", "esp", "esl"]))
        .cloned()
        .collect();
    plugins.sort_by_key(|path| {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let rank = match name.as_str() {
            "skyrim.esm" => 0,
            "update.esm" => 1,
            "dawnguard.esm" => 2,
            "hearthfires.esm" => 3,
            "dragonborn.esm" => 4,
            _ => 10,
        };
        (rank, name)
    });
    Ok(plugins)
}

fn sort_archives_by_load_order(archives: &mut [PathBuf], plugins: &[PathBuf]) {
    let plugin_stems = plugins
        .iter()
        .filter_map(|path| path.file_stem())
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        .collect::<Vec<_>>();
    archives.sort_by_key(|archive| {
        let stem = archive
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let priority = plugin_stems
            .iter()
            .enumerate()
            .filter(|(_, plugin)| {
                stem == plugin.as_str()
                    || stem
                        .strip_prefix(plugin.as_str())
                        .and_then(|suffix| suffix.chars().next())
                        .is_some_and(|separator| matches!(separator, ' ' | '-' | '_'))
            })
            .map(|(index, _)| index)
            .next()
            .unwrap_or(usize::MAX);
        (priority, stem)
    });
}

fn extension(path: &Path, expected: &[&str]) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            expected
                .iter()
                .any(|expected| value.eq_ignore_ascii_case(expected))
        })
}

/// Strips the leading asset kind folder (e.g., "textures", "meshes", "scripts")
/// from a relative path in a case-insensitive manner.
///
/// This avoids creating double-nested output directory structures when processing
/// assets extracted from BSA archives or loose mod folders with mixed-case naming
/// (such as `Textures\actors\dragon.dds` or `Meshes\armor\iron.nif`).
fn staging_path(output: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // The whole file name, not `with_extension`: a dotted output name (`Skyrim.Converted`) must
    // give `Skyrim.Converted.staging-...`, the prefix resuming and `find_resumable_staging` expect.
    let name = output.file_name().unwrap_or_default().to_string_lossy();
    output.with_file_name(format!("{name}.staging-{}-{stamp}", std::process::id()))
}

fn publish_directory(staging: &Path, output: &Path) -> Result<()> {
    let backup = output.with_extension(format!("backup-{}", std::process::id()));
    if backup.exists() {
        bail!("refusing to overwrite stale backup {}", backup.display());
    }
    if output.exists() {
        fs::rename(output, &backup).wrap_err("failed to preserve previous asset output")?;
    }
    if let Err(error) = fs::rename(staging, output) {
        if backup.exists() {
            let _ = fs::rename(&backup, output);
        }
        return Err(error).wrap_err("failed to publish converted assets");
    }
    if backup.exists() {
        fs::remove_dir_all(backup)?;
    }
    Ok(())
}

async fn send(
    tx: &Sender<ProgressEvent>,
    stage: ProgressStage,
    completed: u64,
    total: u64,
    current_file: Option<PathBuf>,
    message: &str,
) {
    let _ = tx
        .send(ProgressEvent::new(
            stage,
            completed,
            total,
            current_file,
            message,
        ))
        .await;
}

/// Sends a one-off line for the person watching: the CLI ends any open status line before it, so a
/// warning never splices into a redrawn line.
async fn send_notice(
    tx: &Sender<ProgressEvent>,
    stage: ProgressStage,
    current_file: Option<PathBuf>,
    message: &str,
) {
    let _ = tx
        .send(ProgressEvent::notice(stage, current_file, message))
        .await;
}

/// Sends a progress event that also carries byte counts, which the status line weighs a stage by.
#[allow(clippy::too_many_arguments)]
async fn send_with_bytes(
    tx: &Sender<ProgressEvent>,
    stage: ProgressStage,
    completed: u64,
    total: u64,
    current_file: Option<PathBuf>,
    message: &str,
    bytes: (u64, u64),
) {
    let _ = tx
        .send(
            ProgressEvent::new(stage, completed, total, current_file, message)
                .with_bytes(bytes.0, bytes.1),
        )
        .await;
}

/// Stops the run at the next safe point when the user interrupted it. The caller keeps the staging
/// folder, so the run can be resumed where it stopped.
fn interrupt(cancellation: &Cancellation) -> Result<()> {
    ensure!(!cancellation.is_cancelled(), "conversion interrupted");
    Ok(())
}

/// Test-only: presses Stop from inside the extractor after a given number of per-entry stop
/// checks of one output's run, so a test stops part way through an archive without racing it.
#[cfg(test)]
mod stop_hook {
    use super::Cancellation;
    use std::{
        path::{Path, PathBuf},
        sync::Mutex,
    };

    static CANCEL_AFTER: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());

    pub(super) fn cancel_after(output: &Path, checks: usize) {
        CANCEL_AFTER
            .lock()
            .unwrap()
            .push((output.to_path_buf(), checks));
    }

    pub(super) fn tick(output: &Path, cancellation: &Cancellation) {
        let mut hooks = CANCEL_AFTER.lock().unwrap();
        if let Some(index) = hooks.iter().position(|(path, _)| path == output) {
            if hooks[index].1 == 0 {
                hooks.remove(index);
                cancellation.cancel();
            } else {
                hooks[index].1 -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dotted_output_name_keeps_its_whole_name_in_the_staging_folder() {
        let staging = staging_path(Path::new("converted/Skyrim.Converted"));
        let name = staging.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("Skyrim.Converted.staging-"), "{name}");
        assert_eq!(staging.parent(), Some(Path::new("converted")));
        // And the folder it names is the one a resume finds.
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("Skyrim.Converted");
        let staging = staging_path(&output);
        fs::create_dir_all(&staging).unwrap();
        assert_eq!(crate::find_resumable_staging(&output), Some((staging, 0)));
    }
    use tokio::sync::mpsc;

    #[test]
    fn maps_srgb_runtime_aliases_back_to_their_converted_source() {
        assert_eq!(
            source_texture_key("textures/effects/fire.opensky-srgb.ktx2").unwrap(),
            "textures/effects/fire.ktx2"
        );
        assert_eq!(
            source_texture_key("textures/effects/fire.ktx2").unwrap(),
            "textures/effects/fire.ktx2"
        );
    }

    #[test]
    fn publishes_a_distinct_asset_path_for_srgb_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let staging = directory.path();
        fs::create_dir_all(staging.join("meshes")).unwrap();
        fs::create_dir_all(staging.join("textures/effects")).unwrap();
        fs::write(staging.join("textures/effects/fire.ktx2"), b"texture").unwrap();
        let mut json = serde_json::to_vec(&serde_json::json!({
            "asset": { "version": "2.0" },
            "images": [{ "uri": "../textures/effects/fire.opensky-srgb.ktx2" }],
            "textures": [{ "source": 0 }],
            "materials": [{
                "pbrMetallicRoughness": { "baseColorTexture": { "index": 0 } }
            }]
        }))
        .unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&u32::try_from(20 + json.len()).unwrap().to_le_bytes());
        glb.extend_from_slice(&u32::try_from(json.len()).unwrap().to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        fs::write(staging.join("meshes/fire.glb"), glb).unwrap();

        let aliases = publish_srgb_texture_aliases(staging).unwrap();

        assert_eq!(
            aliases,
            vec![PathBuf::from("textures/effects/fire.opensky-srgb.ktx2")]
        );
        assert_eq!(fs::read(staging.join(&aliases[0])).unwrap(), b"texture");
    }

    #[test]
    fn orders_archives_by_plugin_load_order() {
        let plugins = vec![
            PathBuf::from("Skyrim.esm"),
            PathBuf::from("Update.esm"),
            PathBuf::from("Example.esp"),
        ];
        let mut archives = vec![
            PathBuf::from("Example - Textures.bsa"),
            PathBuf::from("Skyrim - Textures.bsa"),
            PathBuf::from("Update.bsa"),
            PathBuf::from("Skyrim - Meshes.bsa"),
        ];
        sort_archives_by_load_order(&mut archives, &plugins);
        assert_eq!(
            archives,
            vec![
                PathBuf::from("Skyrim - Meshes.bsa"),
                PathBuf::from("Skyrim - Textures.bsa"),
                PathBuf::from("Update.bsa"),
                PathBuf::from("Example - Textures.bsa"),
            ]
        );
    }

    #[tokio::test]
    async fn converts_and_reuses_assets_end_to_end() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Data");
        let output = temp.path().join("modern");
        fs::create_dir_all(data.join("scripts")).unwrap();
        fs::write(
            data.join("scripts/one.pex"),
            dummy_content::pex::minimal("One").unwrap(),
        )
        .unwrap();
        fs::write(
            data.join("scripts/two.pex"),
            dummy_content::pex::minimal("Two").unwrap(),
        )
        .unwrap();
        let mut config = PipelineConfig::new(&data, &output);
        config.cpu_jobs = 2;
        let (tx, mut rx) = mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let first = AssetPipeline::run_async(config.clone(), tx).await.unwrap();
        drain.await.unwrap();
        assert_eq!(first.converted, 2);
        assert!(output.join("scripts/one.luau").is_file());
        assert!(output.join("scripts/papyrus_runtime.luau").is_file());

        let (tx, mut rx) = mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let second = AssetPipeline::run_async(config, tx).await.unwrap();
        drain.await.unwrap();
        assert_eq!(second.cache_hits, 2);
        assert_eq!(second.skipped, 0);
        assert!(
            ConversionManifest::load(&output.join("conversion-manifest.json"))
                .unwrap()
                .complete
        );
    }

    #[tokio::test]
    async fn reuses_and_invalidates_archive_ingestion_cache_end_to_end() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Data");
        let output = temp.path().join("modern");
        fs::create_dir_all(&data).unwrap();
        fs::write(
            data.join("assets.ba2"),
            dummy_content::ba2::general(
                &[dummy_content::Entry::new(
                    "docs/readme.txt",
                    b"cached asset",
                )],
                dummy_content::ba2::Compression::None,
            )
            .unwrap(),
        )
        .unwrap();
        let config = PipelineConfig::new(&data, &output);

        let first = run_without_progress(config.clone()).await;
        assert_eq!(first.converted, 1);
        assert_eq!(first.cache_hits, 0);
        assert_eq!(
            fs::read(output.join("vfs/docs/readme.txt")).unwrap(),
            b"cached asset"
        );

        let second = run_without_progress(config.clone()).await;
        assert_eq!(second.converted, 0);
        assert_eq!(second.cache_hits, 1);

        let mut invalidated = config;
        invalidated.invalidate_cache = true;
        let third = run_without_progress(invalidated).await;
        assert_eq!(third.converted, 1);
        assert_eq!(third.cache_hits, 0);
    }

    #[tokio::test]
    async fn a_failed_batch_stops_and_keeps_staging_for_a_resume() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Data");
        let output = temp.path().join("modern");
        fs::create_dir_all(data.join("textures")).unwrap();
        fs::write(data.join("textures/bad.dds"), b"not a DDS").unwrap();
        fs::write(data.join("textures/also-bad.dds"), b"also not a DDS").unwrap();

        let (tx, mut rx) = mpsc::channel(64);
        let collect = tokio::spawn(async move {
            let mut events = Vec::new();
            while let Some(event) = rx.recv().await {
                events.push(event);
            }
            events
        });
        let mut config = PipelineConfig::new(&data, &output);
        config.fail_fast = true;
        let failure = AssetPipeline::run_async(config, tx).await.unwrap_err();
        let events = collect.await.unwrap();

        assert!(failure.to_string().contains("failed to convert"));
        assert!(!failure.cancelled);
        assert!(!output.exists());
        let staging = failure
            .staging
            .expect("a failed run keeps the work it has done");
        assert!(staging.is_dir());
        let last = events.last().unwrap();
        assert_eq!(last.stage, ProgressStage::Textures);
        assert_eq!(last.message, "Asset conversion failed");
        assert!(last.current_file.as_ref().is_some_and(|path| {
            path == Path::new("textures/bad.dds") || path == Path::new("textures/also-bad.dds")
        }));
    }

    #[tokio::test]
    async fn an_interrupted_run_keeps_staging_and_resumes_from_it() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Data");
        let output = temp.path().join("modern");
        fs::create_dir_all(data.join("scripts")).unwrap();
        fs::write(
            data.join("scripts/one.pex"),
            dummy_content::pex::minimal("One").unwrap(),
        )
        .unwrap();

        let cancellation = Cancellation::new();
        cancellation.cancel();
        let (tx, mut rx) = mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let failure = AssetPipeline::run_async_with_cancel(
            PipelineConfig::new(&data, &output),
            tx,
            cancellation,
        )
        .await
        .unwrap_err();
        drain.await.unwrap();

        assert!(failure.cancelled);
        assert!(failure.to_string().contains("interrupted"));
        assert!(!output.exists(), "an interrupted run does not publish");
        let staging = failure
            .staging
            .expect("an interrupted run keeps its staging folder");
        assert!(staging.is_dir());

        // The kept folder is a working resume point: the run finishes from it.
        let mut config = PipelineConfig::new(&data, &output);
        config.resume_staging = Some(staging);
        let report = run_without_progress(config).await;
        assert!(report.complete);
        assert!(output.join("scripts/one.luau").is_file());
    }

    #[tokio::test]
    async fn skips_failed_assets_and_records_redo_list() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Data");
        let output = temp.path().join("modern");
        fs::create_dir_all(data.join("textures")).unwrap();
        fs::write(data.join("textures/bad.dds"), b"not a DDS").unwrap();
        fs::write(data.join("textures/also-bad.dds"), b"also not a DDS").unwrap();

        let (tx, mut rx) = mpsc::channel(64);
        let collect = tokio::spawn(async move {
            let mut events = Vec::new();
            while let Some(event) = rx.recv().await {
                events.push(event);
            }
            events
        });
        let report = AssetPipeline::run_async(PipelineConfig::new(&data, &output), tx)
            .await
            .unwrap();
        let events = collect.await.unwrap();

        assert_eq!(report.skipped, 2);
        assert_eq!(report.warnings.len(), 2);
        assert!(!report.complete);
        let manifest = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
        assert!(!manifest.complete);
        assert_eq!(manifest.failures.len(), 2);
        assert!(manifest.failures.contains_key("textures/bad.dds"));
        assert!(manifest.failures.contains_key("textures/also-bad.dds"));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.message == "Asset skipped")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn an_interrupt_mid_batch_stops_at_the_next_asset_and_resumes() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Data");
        let output = temp.path().join("modern");
        fs::create_dir_all(data.join("scripts")).unwrap();
        let assets = 300;
        for index in 0..assets {
            let script = dummy_content::pex::minimal("Script").unwrap();
            fs::write(data.join(format!("scripts/script{index:03}.pex")), script).unwrap();
        }

        let cancellation = Cancellation::new();
        let interrupt = cancellation.clone();
        let (tx, mut rx) = mpsc::channel::<ProgressEvent>(64);
        let watcher = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if event.message == "Converted asset" {
                    interrupt.cancel();
                }
            }
        });
        let mut config = PipelineConfig::new(&data, &output);
        config.cpu_jobs = 1;
        let failure = AssetPipeline::run_async_with_cancel(config, tx, cancellation)
            .await
            .unwrap_err();
        watcher.await.unwrap();

        assert!(failure.cancelled);
        let staging = failure
            .staging
            .clone()
            .expect("an interrupted run keeps staging");
        assert!(staging.is_dir());
        assert!(!output.exists(), "an interrupted run does not publish");
        // The manifest is written once, at the end of a run, so nothing in flight — or after it —
        // is recorded as converted; the resume re-checks the staged files instead.
        assert!(!staging.join("conversion-manifest.json").exists());
        let staged: Vec<_> = fs::read_dir(staging.join("scripts"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "luau")
            })
            .collect();
        assert!(
            staged.len() < assets,
            "the run converted all {assets} assets despite the interrupt"
        );
        for file in &staged {
            assert!(
                !fs::read(file).unwrap().is_empty(),
                "{} was left half written",
                file.display()
            );
        }

        // The kept folder resumes the run to completion.
        let mut config = PipelineConfig::new(&data, &output);
        config.resume_staging = Some(staging);
        let report = run_without_progress(config).await;
        assert!(report.complete);
        assert_eq!(report.skipped, 0);
        let converted = fs::read_dir(output.join("scripts"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "luau")
            })
            .count();
        assert!(converted >= assets, "resume left assets behind");
    }

    /// A stop pressed while one large archive is being extracted takes effect inside it, not
    /// when the whole archive is done, and ends the run as an interrupt rather than as a skipped
    /// archive. Nothing of the archive is cached, so the resume extracts it again in full.
    #[tokio::test]
    async fn an_interrupt_inside_an_archive_stops_its_extraction_and_resumes() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Data");
        let output = temp.path().join("modern");
        fs::create_dir_all(&data).unwrap();
        let entries = 2048;
        let names: Vec<String> = (0..entries)
            .map(|index| format!("docs/file{index:04}.txt"))
            .collect();
        let contents: Vec<Vec<u8>> = (0..entries)
            .map(|index| format!("entry {index}").into_bytes())
            .collect();
        let archive_entries: Vec<_> = names
            .iter()
            .zip(&contents)
            .map(|(name, data)| dummy_content::Entry::new(name, data))
            .collect();
        fs::write(
            data.join("assets.ba2"),
            dummy_content::ba2::general(&archive_entries, dummy_content::ba2::Compression::None)
                .unwrap(),
        )
        .unwrap();

        // Stop from inside the extractor after its first entries: only the entries already in
        // flight on other threads can still finish, far fewer than the archive holds.
        stop_hook::cancel_after(&output, 16);
        let cancellation = Cancellation::new();
        let (tx, mut rx) = mpsc::channel::<ProgressEvent>(64);
        let watcher = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let failure = AssetPipeline::run_async_with_cancel(
            PipelineConfig::new(&data, &output),
            tx,
            cancellation,
        )
        .await
        .unwrap_err();
        watcher.await.unwrap();

        assert!(failure.cancelled, "a stop is reported as an interrupt");
        assert!(failure.to_string().contains("interrupted"));
        assert!(!output.exists(), "an interrupted run does not publish");
        let staging = failure
            .staging
            .clone()
            .expect("an interrupted run keeps staging");
        let written: Vec<_> = fs::read_dir(staging.join("vfs/docs"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            written.len() < entries,
            "the whole archive was extracted despite the stop"
        );
        assert!(
            written.iter().all(|name| !name.ends_with(".partial")),
            "a stop left a half-written file behind"
        );
        // The archive's cache entry is only recorded once it is complete.
        assert!(!staging.join(".ingestion-cache/sha256").exists());
        assert!(!staging.join("conversion-manifest.json").exists());

        let mut config = PipelineConfig::new(&data, &output);
        config.resume_staging = Some(staging);
        let report = run_without_progress(config).await;
        assert!(report.complete);
        assert_eq!(report.skipped, 0);
        assert_eq!(
            fs::read_dir(output.join("vfs/docs")).unwrap().count(),
            entries
        );
        assert_eq!(
            fs::read(output.join("vfs/docs/file2047.txt")).unwrap(),
            b"entry 2047"
        );
        let manifest = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
        assert_eq!(manifest.archives["assets.ba2"].files.len(), entries);
    }

    /// The window between the last stage and the publish rename cannot be hit from outside in a
    /// test without racing the run, so the gate itself is driven directly: a run that was
    /// interrupted while it was packing up must not publish, and must keep its staging folder.
    #[test]
    fn an_interrupt_before_the_publish_keeps_staging_and_leaves_the_output_alone() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("modern");

        let staging = temp.path().join("modern.staging-1-1");
        fs::create_dir_all(staging.join("scripts")).unwrap();
        fs::write(staging.join("scripts/one.luau"), b"return 1").unwrap();
        let cancellation = Cancellation::new();
        cancellation.cancel();
        let failure = publish_if_not_interrupted(&staging, &output, &cancellation).unwrap_err();
        assert!(failure.cancelled);
        assert!(failure.to_string().contains("interrupted"));
        assert!(!output.exists(), "an interrupted run must not publish");
        assert_eq!(failure.staging.as_deref(), Some(staging.as_path()));
        assert!(staging.is_dir(), "the staging folder is kept for a resume");

        // Without the interrupt the same gate publishes, so the assertion above is about the
        // cancellation and not about publishing being broken.
        let staging = temp.path().join("modern.staging-2-2");
        fs::create_dir_all(staging.join("scripts")).unwrap();
        fs::write(staging.join("scripts/one.luau"), b"return 1").unwrap();
        publish_if_not_interrupted(&staging, &output, &Cancellation::new()).unwrap();
        assert_eq!(
            fs::read(output.join("scripts/one.luau")).unwrap(),
            b"return 1"
        );
    }

    async fn run_without_progress(config: PipelineConfig) -> PipelineReport {
        let (tx, mut rx) = mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let report = AssetPipeline::run_async(config, tx).await.unwrap();
        drain.await.unwrap();
        report
    }
}
