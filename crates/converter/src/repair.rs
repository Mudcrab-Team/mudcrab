//! Targeted repair of a published pack. Never runs plugin/database conversion.
use crate::{
    archive::{ArchiveExtractor, safe_relative_path},
    asset_path::{AssetKind, canonical_asset_path, is_authoring_resource, resolve_asset_uri},
    cache::{
        CONVERTER_SCHEMA_VERSION, CacheEntry, ConversionManifest, configuration_hash, hash_file,
        link_or_copy,
    },
    check::{CheckMode, CheckProblem, check_output},
    config::PipelineConfig,
    mesh::MeshConverter,
    pipeline::{
        collect_texture_semantics, mo2_archive_is_active, plugin_paths,
        publish_srgb_texture_aliases, sort_archives_by_load_order, source_texture_key,
        validate_artifact,
    },
    script::ScriptConverter,
    texture::{TextureConverter, TextureEncoding},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

#[derive(Debug, Serialize)]
pub struct RepairReport {
    pub directory: PathBuf,
    pub converted: usize,
    pub excluded: usize,
    pub published: bool,
    pub failures: BTreeMap<String, String>,
}

#[derive(Clone)]
struct Source {
    path: PathBuf,
    archive: Option<(PathBuf, String)>,
    expected_hash: Option<String>,
}

fn kind(path: &str) -> Option<(AssetKind, &'static str, &'static str)> {
    match Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "nif" => Some((AssetKind::Mesh, "nif", "glb")),
        "dds" => Some((AssetKind::Texture, "dds", "ktx2")),
        "pex" => Some((AssetKind::Script, "pex", "luau")),
        _ => None,
    }
}

/// Without `apply`, only a sibling repair directory is written. Keep it for inspection.
pub fn repair_failed(config: &PipelineConfig, apply: bool) -> Result<RepairReport> {
    config.validate()?;
    ensure!(
        config.resume_staging.is_none() && !config.invalidate_cache,
        "repair cannot resume staging or invalidate the pack"
    );
    let manifest_path = config.output_dir.join("conversion-manifest.json");
    let original_bytes = fs::read(&manifest_path)?;
    let mut manifest: ConversionManifest = serde_json::from_slice(&original_bytes)?;
    ensure!(
        manifest.schema_version == CONVERTER_SCHEMA_VERSION,
        "repair requires the current manifest schema"
    );
    ensure!(
        !manifest.failures.is_empty(),
        "manifest has no failed inputs to repair"
    );
    ensure!(
        manifest.configuration_hash == configuration_hash(config)?,
        "repair configuration differs from the original conversion; use the same Data, MO2 instance/profile and encoding settings"
    );
    check_existing(
        &config.output_dir,
        if apply {
            CheckMode::Full
        } else {
            CheckMode::Quick
        },
    )?;

    let directory = config.output_dir.with_file_name(format!(
        "{}.repair-{}",
        config
            .output_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        std::process::id()
    ));
    fs::create_dir(&directory).wrap_err_with(|| {
        format!(
            "repair directory already exists or cannot be created: {}",
            directory.display()
        )
    })?;
    let staged = directory.join("assets");
    fs::create_dir(&staged)?;
    let mut report = RepairReport {
        directory: directory.clone(),
        converted: 0,
        excluded: 0,
        published: false,
        failures: BTreeMap::new(),
    };

    let (resolved, plugins) = if let Some(selection) = &config.mo2 {
        let resolved = mo2::Instance::open(&selection.instance_path)?
            .resolve(&config.data_dir, &selection.profile)?;
        let plugins = resolved
            .plugins
            .iter()
            .map(|key| resolved.files[key].clone())
            .collect();
        (resolved.files, plugins)
    } else {
        let mut resolved = BTreeMap::new();
        for entry in WalkDir::new(&config.data_dir).follow_links(false) {
            let entry = entry?;
            if entry.file_type().is_file() {
                let key = entry
                    .path()
                    .strip_prefix(&config.data_dir)?
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase();
                ensure!(
                    resolved.insert(key.clone(), entry.into_path()).is_none(),
                    "source collision: {key}"
                );
            }
        }
        let files = resolved.values().cloned().collect::<Vec<_>>();
        let plugins = plugin_paths(config, &files, &mut Vec::new())?;
        (resolved, plugins)
    };
    let mut archives: Vec<_> = resolved
        .iter()
        .filter(|(key, path)| {
            (key.ends_with(".bsa") || (config.enable_ba2 && key.ends_with(".ba2")))
                && (config.mo2.is_none() || mo2_archive_is_active(path, &plugins))
        })
        .map(|(_, path)| path.clone())
        .collect();
    sort_archives_by_load_order(&mut archives, &plugins);
    let archive_keys: BTreeMap<_, _> = resolved
        .iter()
        .map(|(key, path)| (path.clone(), key.clone()))
        .collect();
    let mut targets: BTreeSet<String> = manifest
        .failures
        .keys()
        .filter(|key| kind(key).is_some())
        .cloned()
        .collect();
    let mut sources = BTreeMap::<String, Source>::new();
    let cache = config.ingestion_cache_dir().join(".ingestion-cache");
    let new_cache = directory.join(".ingestion-cache");
    let mut repaired_archives = BTreeSet::new();
    for archive in archives {
        let archive_key = &archive_keys[&archive];
        let failure_key = manifest
            .failures
            .keys()
            .find(|key| {
                key.replace('\\', "/")
                    .eq_ignore_ascii_case(&archive.to_string_lossy().replace('\\', "/"))
            })
            .cloned();
        let fresh = failure_key.is_some();
        if let Some(failure_key) = failure_key {
            eprintln!("Repair archive: {}", archive.display());
            match ArchiveExtractor::extract_cached(
                &archive,
                &directory.join("extracted").join(archive_key),
                &cache,
                &new_cache,
                None,
                true,
                None,
                None,
            ) {
                Ok(outcome) => {
                    for file in &outcome.cache_entry.files {
                        if kind(&file.path).is_some() {
                            targets.insert(file.path.clone());
                        }
                    }
                    manifest
                        .archives
                        .insert(archive_key.clone(), outcome.cache_entry);
                    manifest.failures.remove(&failure_key);
                    repaired_archives.insert(archive_key.clone());
                }
                Err(error) => {
                    manifest.failures.insert(failure_key, format!("{error:#}"));
                    continue;
                }
            }
        }
        let Some(entry) = manifest.archives.get(archive_key) else {
            continue;
        };
        for file in &entry.files {
            if kind(&file.path).is_none() {
                continue;
            }
            let hash = &file.hash;
            ensure!(
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "invalid ingestion hash"
            );
            let blob = (if fresh { &new_cache } else { &cache })
                .join("sha256")
                .join(&hash[..2])
                .join(hash);
            sources.insert(
                file.path.clone(),
                Source {
                    path: blob,
                    archive: Some((archive.clone(), entry.source_hash.clone())),
                    expected_hash: Some(hash.clone()),
                },
            );
        }
    }
    // Loose files override archives, just as in the main conversion pipeline.
    for (relative, path) in &resolved {
        if let Some((asset_kind, ext, _)) = kind(relative) {
            let key = canonical_asset_path(relative, asset_kind, ext)?;
            ensure!(
                !sources
                    .get(&key)
                    .is_some_and(|source| source.archive.is_none()),
                "normalized loose-source collision: {key}"
            );
            sources.insert(
                key,
                Source {
                    path: path.clone(),
                    archive: None,
                    expected_hash: None,
                },
            );
        }
    }
    for extension in ["nif", "dds", "pex"] {
        let count = sources
            .keys()
            .filter(|key| {
                !is_authoring_resource(key)
                    && Path::new(key)
                        .extension()
                        .is_some_and(|ext| ext == extension)
            })
            .count();
        manifest
            .inputs_by_kind
            .insert(extension.into(), count as u64);
    }
    let mut verified_archives = BTreeSet::new();
    let mut changed = BTreeSet::<String>::new();
    eprintln!(
        "Resolved {} runtime sources; inspecting published texture semantics",
        sources.len()
    );
    let mut semantics = collect_texture_semantics(&config.output_dir)?;
    for key in targets
        .clone()
        .into_iter()
        .filter(|key| !key.ends_with(".dds"))
    {
        if is_authoring_resource(&key) {
            manifest.failures.remove(&key);
            manifest.excluded_inputs.insert(
                key,
                "BodySlide/Outfit Studio authoring resource, not runtime data".into(),
            );
            report.excluded += 1;
            continue;
        }
        attempt(
            config,
            &staged,
            &key,
            &sources,
            &mut verified_archives,
            &semantics,
            &mut manifest,
            &mut changed,
            &mut report,
        );
    }
    for (key, values) in collect_texture_semantics(&staged)? {
        semantics.entry(key).or_default().extend(values);
    }
    // Stage existing texture dependencies by link, and repair missing dependencies
    // from their winning DDS sources. Never prune a texture that actually exists in Data.
    let meshes: Vec<_> = changed
        .iter()
        .filter(|path| path.ends_with(".glb"))
        .cloned()
        .collect();
    for mesh in &meshes {
        for dependency in MeshConverter::glb_texture_dependencies(&staged.join(mesh))? {
            let destination = resolve_asset_uri(&staged, &staged.join(mesh), &dependency.uri)?;
            let relative = destination
                .strip_prefix(&staged)?
                .to_string_lossy()
                .replace('\\', "/");
            let base = source_texture_key(&relative)?;
            let old = checked_destination(&config.output_dir, &base)?;
            let encoding = TextureEncoding::from_semantics(
                &semantics.get(&base).cloned().unwrap_or_default(),
            )?;
            let compatible =
                old.is_file() && crate::texture::inspect_ktx2(&fs::read(&old)?, encoding).is_ok();
            if compatible {
                let to = checked_destination(&staged, &base)?;
                if !to.exists() {
                    fs::create_dir_all(to.parent().unwrap())?;
                    link_or_copy(&old, &to)?;
                }
            } else {
                let source = base
                    .strip_suffix(".ktx2")
                    .map(|stem| format!("{stem}.dds"))
                    .unwrap();
                if sources.contains_key(&source) {
                    targets.insert(source);
                } else {
                    ensure!(
                        !old.is_file(),
                        "texture {base} needs a new encoding, but its DDS source is unavailable"
                    );
                }
            }
        }
    }
    for key in targets.into_iter().filter(|key| key.ends_with(".dds")) {
        attempt(
            config,
            &staged,
            &key,
            &sources,
            &mut verified_archives,
            &semantics,
            &mut manifest,
            &mut changed,
            &mut report,
        );
    }
    let aliases = publish_srgb_texture_aliases(&staged)?;
    for alias in aliases {
        let key = alias.to_string_lossy().replace('\\', "/");
        let base = source_texture_key(&key)?;
        // Publish new aliases or refresh an alias whose underlying texture was repaired.
        if changed.contains(&base) || !config.output_dir.join(&key).is_file() {
            changed.insert(key.clone());
            record_output(
                &mut manifest,
                format!("alias:{key}"),
                key.clone(),
                hash_file(&staged.join(&base))?,
                &staged,
            )?;
        }
    }
    let source_textures = sources
        .keys()
        .filter(|key| key.starts_with("textures/") && key.ends_with(".dds"))
        .cloned()
        .collect();
    for file in MeshConverter::prune_dangling_texture_uris_with_sources(&staged, &source_textures)?
    {
        if !file.removed_uris.is_empty() {
            let references = manifest
                .pruned_texture_references
                .entry(file.glb.clone())
                .or_default();
            for uri in file.removed_uris {
                let resolved = resolve_asset_uri(&staged, &staged.join(&file.glb), &uri)?;
                references.insert(
                    resolved
                        .strip_prefix(&staged)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    for mesh in &meshes {
        for dependency in MeshConverter::glb_texture_dependencies(&staged.join(mesh))? {
            let target = resolve_asset_uri(&staged, &staged.join(mesh), &dependency.uri)?;
            // A failed DDS remains in the failure list, not a silent material prune.
            if !target.is_file() {
                let relative = target
                    .strip_prefix(&staged)?
                    .to_string_lossy()
                    .replace('\\', "/");
                let base = source_texture_key(&relative)?;
                let key = format!("{}.dds", base.strip_suffix(".ktx2").unwrap());
                ensure!(
                    manifest.failures.contains_key(&key),
                    "unresolved texture {} in repaired {mesh}",
                    dependency.uri
                );
            }
        }
    }
    let mut lua = None;
    for relative in &changed {
        validate_artifact(&staged, Path::new(relative), &semantics, &mut lua)?;
    }
    for entry in manifest
        .entries
        .values_mut()
        .filter(|entry| changed.contains(&entry.output))
    {
        entry.output_size = fs::metadata(staged.join(&entry.output))?.len();
        entry.output_hash = hash_file(&staged.join(&entry.output))?;
    }
    report.failures = manifest.failures.clone();
    manifest.complete = false;
    manifest.save(&staged.join("conversion-manifest.json"))?;
    fs::write(
        directory.join("repair-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    if apply {
        ensure!(
            manifest.failures.is_empty(),
            "repair still has {} failed inputs; original pack unchanged, see {}",
            manifest.failures.len(),
            directory.display()
        );
        ensure!(
            fs::read(&manifest_path)? == original_bytes,
            "published manifest changed while repair ran"
        );
        ensure!(
            configuration_hash(config)? == manifest.configuration_hash,
            "MO2 configuration changed while repair ran"
        );
        check_existing(&config.output_dir, CheckMode::Full)?;
        // New ingestion blobs are content-addressed; persisting them does not alter
        // any existing blob or runtime artifact.
        for archive_key in repaired_archives {
            for file in &manifest.archives[&archive_key].files {
                let relative = PathBuf::from("sha256")
                    .join(&file.hash[..2])
                    .join(&file.hash);
                let destination = cache.join(&relative);
                if !destination.exists() {
                    fs::create_dir_all(destination.parent().unwrap())?;
                    link_or_copy(&new_cache.join(relative), &destination)?;
                }
            }
        }
        manifest.complete = true;
        manifest.save(&staged.join("conversion-manifest.json"))?;
        changed.insert("conversion-manifest.json".into());
        publish(
            &config.output_dir,
            &staged,
            &directory.join("backup"),
            &changed,
        )?;
        report.published = true;
        fs::write(
            directory.join("repair-report.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn attempt(
    config: &PipelineConfig,
    staged: &Path,
    key: &str,
    sources: &BTreeMap<String, Source>,
    verified: &mut BTreeSet<PathBuf>,
    semantics: &BTreeMap<String, BTreeSet<crate::texture::TextureSemantic>>,
    manifest: &mut ConversionManifest,
    changed: &mut BTreeSet<String>,
    report: &mut RepairReport,
) {
    eprintln!("Repair input: {key}");
    let result = (|| -> Result<()> {
        let source = sources
            .get(key)
            .ok_or_else(|| color_eyre::eyre::eyre!("winning source is missing: {key}"))?;
        if let Some((archive, expected)) = &source.archive
            && !verified.contains(archive)
        {
            ensure!(
                hash_file(archive)? == *expected,
                "archive changed since conversion: {}",
                archive.display()
            );
            verified.insert(archive.clone());
        }
        let mut source_hash = hash_file(&source.path)?;
        if let Some(expected) = &source.expected_hash {
            ensure!(
                source_hash == *expected,
                "ingestion blob hash mismatch for {key}"
            );
        }
        let (asset_kind, ext, target_ext) = kind(key).unwrap();
        let output = canonical_asset_path(key, asset_kind, target_ext)?;
        let target = checked_destination(staged, &output)?;
        if ext == "nif" {
            for dependency in MeshConverter::dependency_paths(&source.path) {
                source_hash.push(':');
                source_hash.push_str(&hash_file(&dependency)?);
            }
            MeshConverter::convert_nif_to_glb(&source.path, &target)?;
            MeshConverter::glb_texture_dependencies(&target)?;
            if MeshConverter::is_geometry_template(&source.path)? {
                manifest.excluded_inputs.insert(key.into(), "Geometryless overlay template; standalone mesh is empty, runtime overlays are not implemented".into());
                report.excluded += 1;
            } else {
                MeshConverter::glb_bounds(&target)?;
            }
        } else if ext == "pex" {
            ScriptConverter::convert_pex_to_luau(&source.path, &target)?;
        } else {
            let encoding = TextureEncoding::from_semantics(
                &semantics.get(&output).cloned().unwrap_or_default(),
            )?;
            source_hash.push_str(&format!(":texture-encoding:{encoding:?}"));
            TextureConverter::convert_dds_to_ktx2_with_options(
                &source.path,
                &target,
                encoding,
                config.texture_fallback_quality,
                config.texture_uastc_level,
                config.texture_zstd_level,
            )?;
        }
        record_output(manifest, key.into(), output.clone(), source_hash, staged)?;
        changed.insert(output);
        manifest.failures.remove(key);
        report.converted += 1;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("Repair failed: {key}: {error:#}");
        manifest.failures.insert(key.into(), format!("{error:#}"));
    }
}

fn record_output(
    manifest: &mut ConversionManifest,
    key: String,
    output: String,
    source_hash: String,
    root: &Path,
) -> Result<()> {
    let path = checked_destination(root, &output)?;
    manifest.entries.insert(
        key,
        CacheEntry {
            source_hash,
            output,
            output_size: fs::metadata(&path)?.len(),
            output_hash: hash_file(&path)?,
        },
    );
    Ok(())
}

fn check_existing(output: &Path, mode: CheckMode) -> Result<()> {
    let check = check_output(output, mode, |_, _| {})?;
    for problem in check.problems {
        ensure!(
            matches!(
                problem,
                CheckProblem::Incomplete | CheckProblem::RecordedFailures { .. }
            ),
            "existing pack cannot be repaired safely: {problem}"
        );
    }
    Ok(())
}

fn checked_destination(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = safe_relative_path(relative)?;
    let path = root.join(relative);
    let canonical_root = fs::canonicalize(root)?;
    let mut ancestor = path.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid repair path"))?;
    }
    ensure!(
        fs::canonicalize(ancestor)?.starts_with(canonical_root),
        "repair path escapes output: {}",
        path.display()
    );
    Ok(path)
}

fn publish(output: &Path, staged: &Path, backup: &Path, changed: &BTreeSet<String>) -> Result<()> {
    fs::create_dir(backup)?;
    // Manifest is the commit marker and must be published last.
    let mut paths: Vec<_> = changed
        .iter()
        .filter(|path| path.as_str() != "conversion-manifest.json")
        .cloned()
        .collect();
    paths.push("conversion-manifest.json".into());
    let mut applied = Vec::new();
    let result = (|| -> Result<()> {
        for relative in paths {
            let destination = checked_destination(output, &relative)?;
            let old = checked_destination(backup, &relative)?;
            let existed = destination.exists();
            fs::create_dir_all(destination.parent().unwrap())?;
            if existed {
                fs::create_dir_all(old.parent().unwrap())?;
                fs::rename(&destination, &old)?;
            }
            applied.push((destination.clone(), old, existed));
            fs::rename(checked_destination(staged, &relative)?, destination)?;
        }
        Ok(())
    })();
    if result.is_err() {
        for (destination, old, existed) in applied.into_iter().rev() {
            if destination.exists() {
                fs::remove_file(&destination)?;
            }
            if existed {
                fs::rename(old, destination)?;
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_rolls_back_and_rejects_escaping_paths() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        let staged = root.path().join("staged");
        fs::create_dir(&output).unwrap();
        fs::create_dir(&staged).unwrap();
        fs::write(output.join("a.luau"), "original").unwrap();
        fs::write(output.join("conversion-manifest.json"), "original manifest").unwrap();
        fs::write(staged.join("a.luau"), "repaired").unwrap();
        let changed = BTreeSet::from(["a.luau".into(), "missing.luau".into()]);
        assert!(publish(&output, &staged, &root.path().join("backup"), &changed).is_err());
        assert_eq!(
            fs::read_to_string(output.join("a.luau")).unwrap(),
            "original"
        );
        assert_eq!(
            fs::read_to_string(output.join("conversion-manifest.json")).unwrap(),
            "original manifest"
        );
        assert!(checked_destination(&output, "../outside").is_err());
    }

    #[test]
    #[ignore = "requires MUDCRAB_REPAIR_DATA, OUTPUT, INSTANCE and PROFILE for an installed failed pack"]
    fn repairs_installed_profile_without_publishing() {
        let value = |name: &str| {
            std::env::var_os(format!("MUDCRAB_REPAIR_{name}"))
                .expect("set repair fixture environment")
        };
        let mut config = PipelineConfig::new(value("DATA"), value("OUTPUT"));
        config.mo2 = Some(mo2::Selection {
            instance_path: value("INSTANCE").into(),
            profile: value("PROFILE").to_string_lossy().into_owned(),
        });
        let before = fs::read(config.output_dir.join("conversion-manifest.json")).unwrap();
        let report = repair_failed(&config, false).unwrap();
        eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
        assert!(!report.published);
        assert_eq!(
            fs::read(config.output_dir.join("conversion-manifest.json")).unwrap(),
            before
        );
        assert!(
            report.failures.is_empty(),
            "unresolved real inputs: {:?}",
            report.failures
        );
    }
}
