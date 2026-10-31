//! Cluster LOD stage of the converter (ADR-0013): classifier, builder, payload writers, tables
//! and the stage report, behind `--lod-mode auto|cluster|baked`.
//!
//! Coordinate conventions of the payloads: mesh space is the converted GLB's, with every node
//! transform and the scene root's Creation-to-runtime rotation baked in, so it is in runtime
//! (Y-up) axes. An instance transform is the placed reference's translation, rotation and scale in
//! the same axes (Creation units). Block bounds, in the `.mci` header and the database, are in
//! Creation axes (x east, y north, z up) like `lod_chunks`, so one R-tree query serves both.

pub mod authority;
pub mod builder;
pub mod classify;
pub mod glb_input;
pub mod identity;
pub mod model;
pub mod model_facts;
pub mod overrides;
pub mod probe;
pub mod publish;
pub mod report;
pub mod resolve;
pub mod scope;
pub mod stage;
pub(crate) mod terrain_atlas;
pub mod terrain_geometry;
pub mod terrain_publish;
pub(crate) mod terrain_sources;
pub mod terrain_stage;
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod test_support;

pub use report::StageReport;

use crate::{
    cache::ConversionManifest,
    config::PipelineConfig,
    derive_extensions::{self, DeriveOptions, DeriveReport},
    pipeline::PipelineReport,
};
use color_eyre::{Result, eyre::ensure};
use serde::{Deserialize, Serialize};
use std::{fmt, path::Path, str::FromStr};

/// Default smallest primitive that gets clusters (ADR-0013 section 5, a proposal until the
/// plain-chain comparison fixes it).
pub const DEFAULT_TRIANGLE_THRESHOLD: u32 = 1024;

/// How the converter prepares distant opaque static objects (`--lod-mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LodMode {
    /// Probe this PC's adapter. Standalone stages select a supported path; full conversions
    /// record the capability and derive an offline overlay after publishing the base.
    Auto,
    /// Build cluster payloads and tables.
    Cluster,
    /// Today's behaviour: nothing is added.
    #[default]
    Baked,
}

impl fmt::Display for LodMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Cluster => "cluster",
            Self::Baked => "baked",
        })
    }
}

impl FromStr for LodMode {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "auto" => Ok(Self::Auto),
            "cluster" => Ok(Self::Cluster),
            "baked" => Ok(Self::Baked),
            other => Err(format!("unknown LOD mode {other}")),
        }
    }
}

/// Whether a conversion asks for a cluster overlay at all: `cluster` or `auto` with LOD on.
/// `baked` and `--no-lod` do no cluster work (no probe, no derive).
pub(crate) fn derive_requested(config: &PipelineConfig) -> bool {
    !config.no_lod && config.lod_mode != LodMode::Baked
}

/// Forced cluster conversions omit native products and require trees and canonical terrain.
/// Auto retains the historical baked base with an optional cluster overlay.
pub(crate) fn cluster_only(config: &PipelineConfig) -> bool {
    config.lod_mode == LodMode::Cluster
}

pub(crate) fn terrain_requested(config: &PipelineConfig) -> bool {
    cluster_only(config) || config.cluster_terrain
}

fn conversion_options(config: &PipelineConfig, base: &Path) -> DeriveOptions {
    DeriveOptions {
        source: base.to_path_buf(),
        label: "default".into(),
        replace: false,
        keep_failed_staging: false,
        terrain: terrain_requested(config),
        terrain_sources: None,
        jobs: config.cpu_jobs,
        // Cluster-only bases have no baked draw path for size-refused primitives.
        // Every nonempty supported primitive must therefore enter the package.
        triangle_threshold: if cluster_only(config) {
            1
        } else {
            config.cluster_triangle_threshold
        },
        include_trees: cluster_only(config),
    }
}

/// Select the final package from the completed world database, before first base publication.
/// The caller owns conversion staging; the new base and overlay are not published yet.
pub(crate) fn geometry_manifest(
    config: &PipelineConfig,
    converted: &Path,
) -> Result<shared::geometry::GeometryManifest> {
    if !cluster_only(config) {
        return Ok(shared::geometry::GeometryManifest::default());
    }
    let options = conversion_options(config, converted);
    let source = rusqlite::Connection::open_with_flags(
        converted.join(publish::CORE_DATABASE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let settings = derive_extensions::resolved_settings(&options, &source)?;
    Ok(shared::geometry::GeometryManifest {
        geometry_mode: shared::geometry::GeometryMode::Cluster,
        cluster_package: Some(format!("derived/clusters-{}", settings.settings_id())),
    })
}

/// Refuses cluster requests a conversion cannot honour, before any work starts.
pub(crate) fn validate_request(config: &PipelineConfig) -> Result<()> {
    ensure!(
        config.cluster_triangle_threshold > 0,
        "cluster triangle threshold must be positive"
    );
    ensure!(
        !derive_requested(config) || config.cpu_jobs > 0,
        "cluster derivation jobs must be positive"
    );
    // Forced cluster mode intentionally supplies terrain, trees and threshold 1.
    // Validate the requested inputs before resolving those byte-affecting options.
    ensure!(
        !cluster_only(config) || !config.no_lod,
        "--lod-mode cluster requires LOD enabled; --no-lod contradicts cluster geometry"
    );
    ensure!(
        !cluster_only(config) || !config.extract_all_archive_files,
        "--extract-all-archive-files contradicts cluster-only geometry; use baked/auto for complete raw archive extraction"
    );
    ensure!(
        !config.cluster_terrain || derive_requested(config),
        "--cluster-terrain requires cluster/auto LOD with LOD enabled"
    );
    Ok(())
}

/// The capability probe of `--lod-mode auto` (and the adapter record of a forced `cluster`),
/// asked once before any LOD work starts, as ADR-0013 section 6 says, and handed to the overlay
/// derivation so the adapter is never asked twice. `None` when no probe is needed.
pub(crate) fn probe_for_pipeline(config: &PipelineConfig) -> Option<probe::ProbeResult> {
    if !derive_requested(config) {
        return None;
    }
    let result = probe::probe_capabilities();
    Some(result)
}

type ProbeReceipt = Option<probe::ProbeResult>;
type DeriveRunner = fn(&DeriveOptions, ProbeReceipt) -> Result<DeriveReport>;

/// The two environment-dependent steps of the post-publish overlay derivation. Production uses
/// [`PostPublish::REAL`]; tests substitute a synthetic capability for the GPU probe and call the
/// real derivation through a CPU-only entry point, so the base is really published and the
/// overlay is really derived.
#[derive(Clone, Copy)]
pub(crate) struct PostPublish {
    pub(crate) probe: fn(&PipelineConfig) -> Option<probe::ProbeResult>,
    pub(crate) derive: DeriveRunner,
}

impl PostPublish {
    pub(crate) const REAL: Self = Self {
        probe: probe_for_pipeline,
        derive: derive_extensions::run_with_probe,
    };

    /// Synthetic capability, real derivation (no GPU, no disk-floor dependence).
    #[cfg(test)]
    pub(crate) const FIXTURE: Self = Self {
        probe: fixture_probe,
        derive: derive_extensions::run_fixture_with_probe,
    };
}

#[cfg(test)]
pub(crate) fn fixture_probe(config: &PipelineConfig) -> Option<probe::ProbeResult> {
    derive_requested(config).then(|| probe::ProbeResult {
        passed: true,
        missing: Vec::new(),
        backend: "synthetic-no-device".into(),
        adapter_name: "CPU fixture: no GPU requested".into(),
        json: serde_json::json!({"test": true}),
    })
}

/// What the post-publish step did.
pub(crate) enum Overlay {
    /// `baked` or `--no-lod`: no cluster work was asked for.
    NotRequested,
    Derived(Box<DeriveReport>),
}

impl Overlay {
    pub(crate) fn record(self, report: &mut PipelineReport) {
        match self {
            Self::NotRequested => {}
            Self::Derived(derived) => {
                report.notices.push(format!(
                    "cluster overlay derived at {}",
                    derived.output.display()
                ));
                report.notices.extend(derived.notices);
                report.cluster = Some(derived.stage);
            }
        }
    }
}

/// The raw DDS root a derive retry must name: only a terrain request, and only when the published
/// base holds no `vfs` of its own (a full conversion's runtime pack never does; a metadata
/// rebuild's output does). `staged_vfs` is the raw DDS root a conversion holds, if one still
/// exists: the detached terrain-sources `vfs`, or the staging `vfs` when it could not be moved.
fn retry_terrain_sources(
    config: &PipelineConfig,
    base: &Path,
    staged_vfs: Option<&Path>,
) -> Result<Option<std::path::PathBuf>> {
    if !terrain_requested(config) || base.join("vfs").try_exists()? {
        return Ok(None);
    }
    Ok(staged_vfs.map(Path::to_path_buf))
}

/// True when a failed derive can only be retried from the conversion's staging folder, which the
/// caller must then keep (it holds the raw terrain DDS the published base does not). Only the
/// case where [`detach_terrain_sources`] could not move them: once detached, staging is not needed.
pub(crate) fn staging_needed_for_retry(
    config: &PipelineConfig,
    base: &Path,
    staged_vfs: Option<&Path>,
) -> bool {
    derive_requested(config)
        && retry_terrain_sources(config, base, staged_vfs).is_ok_and(|sources| sources.is_some())
}

/// The raw terrain DDS of a full conversion, moved out of its staging folder into a folder of
/// their own beside the output before the overlay derive. The conversion staging then holds
/// nothing a retry needs and is removed whole, so a failed derive can never leave a folder that
/// `find_resumable_staging` would offer for resume.
#[derive(Debug)]
pub(crate) struct DetachedTerrainSources {
    folder: std::path::PathBuf,
}

impl DetachedTerrainSources {
    /// The raw DDS root a derive or its retry names.
    pub(crate) fn vfs(&self) -> std::path::PathBuf {
        self.folder.join("vfs")
    }

    /// Removes the folder after the derive consumed it. The overlay is already published, so a
    /// folder that will not go is only a warning.
    pub(crate) fn remove(self) {
        if let Err(error) = std::fs::remove_dir_all(&self.folder) {
            eprintln!(
                "warning: could not remove the raw terrain textures {}: {error}",
                self.folder.display()
            );
        }
    }
}

fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Moves a full conversion's raw `staging/vfs` to `<output>.terrain-sources-<pid>-<stamp>/vfs`
/// (a rename beside the output: no copy, no change to the base) when a terrain derive needs it
/// and the published base holds no `vfs` of its own. `Ok(None)` when no terrain derive needs it.
/// Refuses, leaving everything where it is, when `vfs` is a link or holds the Skyrim Data folder:
/// nothing outside the conversion's own extracted files is ever moved or later removed.
pub(crate) fn detach_terrain_sources(
    config: &PipelineConfig,
    base: &Path,
    staging: &Path,
) -> Result<Option<DetachedTerrainSources>> {
    use color_eyre::eyre::{WrapErr, eyre};
    let staged_vfs = staging.join("vfs");
    if !derive_requested(config)
        || retry_terrain_sources(config, base, Some(&staged_vfs))?.is_none()
    {
        return Ok(None);
    }
    let metadata = std::fs::symlink_metadata(&staged_vfs)
        .wrap_err_with(|| format!("failed to inspect {}", staged_vfs.display()))?;
    ensure!(
        metadata.is_dir() && !is_link(&metadata),
        "the raw terrain textures {} are not a plain folder, so they are not moved",
        staged_vfs.display()
    );
    let vfs_path = std::fs::canonicalize(&staged_vfs)?;
    let data_path = std::fs::canonicalize(&config.data_dir).wrap_err_with(|| {
        format!(
            "could not compare {} with the Skyrim Data folder {}",
            staged_vfs.display(),
            config.data_dir.display()
        )
    })?;
    ensure!(
        !data_path.starts_with(&vfs_path),
        "the raw terrain textures {} hold the Skyrim Data folder {}, so they are not moved",
        staged_vfs.display(),
        config.data_dir.display()
    );
    let name = base
        .file_name()
        .ok_or_else(|| eyre!("the asset output {} has no name", base.display()))?
        .to_string_lossy();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let folder = base.with_file_name(format!(
        "{name}.terrain-sources-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&folder)
        .wrap_err_with(|| format!("failed to create {}", folder.display()))?;
    let owner = folder.join("OWNER.txt");
    let moved = std::fs::write(
        &owner,
        format!(
            "owner: converter, raw terrain textures for the cluster overlay of {}\n\
             keep-until: the overlay derive succeeds, or a retry of `converter derive-clusters \
             --terrain-sources` has\n",
            base.display()
        ),
    )
    .and_then(|()| std::fs::rename(&staged_vfs, folder.join("vfs")));
    if let Err(error) = moved {
        // Nothing moved: drop the folder this call made (OWNER.txt only), never anything else.
        let _ = std::fs::remove_file(&owner);
        let _ = std::fs::remove_dir(&folder);
        return Err(error).wrap_err_with(|| {
            format!(
                "failed to move the raw terrain textures {} to {}",
                staged_vfs.display(),
                folder.display()
            )
        });
    }
    Ok(Some(DetachedTerrainSources { folder }))
}

/// The exact command that derives the overlay again over the published base with this
/// conversion's cluster settings.
fn retry_command(config: &PipelineConfig, base: &Path, terrain_sources: Option<&Path>) -> String {
    let mut command = format!(
        "converter derive-clusters {} --triangle-threshold {} --jobs {}",
        shared::cluster_overlay::quote_argument(&base.display().to_string()),
        conversion_options(config, base).triangle_threshold,
        config.cpu_jobs
    );
    if terrain_requested(config) {
        command.push_str(" --terrain");
    }
    if let Some(sources) = terrain_sources {
        command.push_str(&format!(
            " --terrain-sources {}",
            shared::cluster_overlay::quote_argument(&sources.display().to_string())
        ));
    }
    if cluster_only(config) {
        command.push_str(" --include-trees");
    }
    command.push_str(" --label default --replace");
    command
}

/// Context for any error after the base is published: the base is valid, only the overlay is
/// missing, and the run is not a resumable conversion.
pub(crate) fn overlay_failure_context(
    config: &PipelineConfig,
    base: &Path,
    staged_vfs: Option<&Path>,
) -> String {
    let sources = retry_terrain_sources(config, base, staged_vfs)
        .ok()
        .flatten();
    let mut text = format!(
        "the base conversion at {} is published and valid; the cluster overlay was not derived. \
         This is not a resumable conversion. Derive only the overlay with: {}",
        base.display(),
        retry_command(config, base, sources.as_deref())
    );
    if let Some(sources) = &sources {
        text.push_str(&format!(
            " (the raw terrain textures are kept in {}; delete that folder after a successful retry)",
            sources.parent().unwrap_or(sources).display()
        ));
    }
    text
}

/// Derives the cluster overlay over the just-published base, for `cluster` and `auto` alike: the
/// derivation always builds in cluster mode, offline, and records the adapter probe (even a
/// failed one) as the capability fact of this run. Does nothing in `baked` mode or with
/// `--no-lod`. The caller has released its exclusive lock on
/// `base`; the derivation takes its own shared lock on it and an exclusive lock on the overlay
/// path, reads the base read-only and removes its own staging. `early_probe` is the one adapter
/// probe of this run. On failure the published base is untouched and the error carries the
/// original cause plus the exact retry command.
pub(crate) fn derive_after_publish(
    config: &PipelineConfig,
    base: &Path,
    staged_vfs: Option<&Path>,
    early_probe: Option<probe::ProbeResult>,
    hooks: &PostPublish,
) -> Result<Overlay> {
    if !derive_requested(config) {
        return Ok(Overlay::NotRequested);
    }
    let mut options = conversion_options(config, base);
    options.terrain_sources = retry_terrain_sources(config, base, staged_vfs)?;
    (hooks.derive)(&options, early_probe)
        .map(|derived| Overlay::Derived(Box::new(derived)))
        .map_err(|error| error.wrap_err(overlay_failure_context(config, base, staged_vfs)))
}

/// One certified publication order for full conversion and extension-only derivation.
/// Both geometry producers finish before the final base hash; terrain metadata
/// depends on that final hash. The caller owns the destination's exclusive lock.
pub fn finalize_certified_extensions(
    settings: &stage::StageSettings,
    context: &publish::PublishContext<'_>,
    manifest: &ConversionManifest,
    builder_identity: &str,
    with_terrain: bool,
) -> Result<(Vec<shared::extensions::ExtensionEntry>, Vec<String>)> {
    let staging = context.staging();
    // A full-copy package carries its core in the staging folder; an overlay reads its base in
    // place (database read-only, files under the base) and stages only the cluster store.
    let core_root = context.core_root();
    let authority = authority::CoreAuthority::open(
        context.open_core_read_only()?,
        core_root,
        manifest,
        settings,
        builder_identity.to_owned(),
    )?;
    let mut notices = Vec::new();
    let terrain = if with_terrain {
        if !context.is_overlay() {
            // The DDS source contract must read this run's actual producer entries,
            // not a missing (fresh build) or stale (resumed build) staged manifest.
            // This is private staging under the caller's exclusive publication lock;
            // the complete extension envelope is composed before publication.
            manifest.save(&staging.join("conversion-manifest.json"))?;
        }
        // An overlay's base already holds its own finished manifest, cell cache and LOD recipe.
        let report =
            terrain_stage::run_with_report_and_jobs(core_root, context, None, settings.jobs)?;
        notices.extend(report.notices);
        report.publication
    } else {
        None
    };
    if let Some(proof) = &terrain {
        proof.validate(context)?;
    }
    let base = publish::finalize_base(context, Some(&authority))?;
    let mut extensions = Vec::new();
    if let Some(proof) = &terrain {
        extensions.push(terrain_publish::finalize(context, &base, proof)?);
    }
    extensions.push(base.extension);
    Ok((extensions, notices))
}

#[cfg(test)]
mod detach_tests {
    use super::*;
    use std::fs;

    fn terrain_config(data: &Path, output: &Path) -> PipelineConfig {
        let mut config = PipelineConfig::new(data, output);
        config.lod_mode = LodMode::Cluster;
        config.cluster_terrain = true;
        config
    }

    #[test]
    fn detaching_moves_only_the_raw_vfs_beside_the_output() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        let output = root.path().join("assets");
        let staging = root.path().join("assets.staging-1-2");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::create_dir_all(staging.join("vfs/textures")).unwrap();
        fs::write(staging.join("vfs/textures/a.dds"), b"raw").unwrap();
        fs::write(staging.join("skyrim_world.db"), b"db").unwrap();

        let config = terrain_config(&data, &output);
        let detached = detach_terrain_sources(&config, &output, &staging)
            .unwrap()
            .unwrap();

        assert_eq!(
            fs::read(detached.vfs().join("textures/a.dds")).unwrap(),
            b"raw"
        );
        assert!(detached.vfs().parent().unwrap().join("OWNER.txt").is_file());
        assert!(!staging.join("vfs").exists());
        assert!(staging.join("skyrim_world.db").is_file());
        assert!(
            !detached
                .vfs()
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".staging-")
        );
        let folder = detached.vfs().parent().unwrap().to_path_buf();
        detached.remove();
        assert!(!folder.exists());
        assert!(data.is_dir() && output.is_dir());
    }

    #[test]
    fn nothing_is_detached_unless_a_terrain_derive_needs_the_raw_vfs() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        let output = root.path().join("assets");
        let staging = root.path().join("assets.staging-1-2");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(staging.join("vfs")).unwrap();

        let mut no_terrain = terrain_config(&data, &output);
        no_terrain.lod_mode = LodMode::Auto;
        no_terrain.cluster_terrain = false;
        assert!(
            detach_terrain_sources(&no_terrain, &output, &staging)
                .unwrap()
                .is_none()
        );
        let mut baked = terrain_config(&data, &output);
        baked.lod_mode = LodMode::Baked;
        assert!(
            detach_terrain_sources(&baked, &output, &staging)
                .unwrap()
                .is_none()
        );
        // A base that holds its own vfs is the retry's source, so the staging one is not needed.
        fs::create_dir_all(output.join("vfs")).unwrap();
        let config = terrain_config(&data, &output);
        assert!(
            detach_terrain_sources(&config, &output, &staging)
                .unwrap()
                .is_none()
        );
        assert!(staging.join("vfs").is_dir());
    }

    #[test]
    fn a_vfs_holding_the_data_folder_is_refused_and_left_in_place() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("assets");
        let staging = root.path().join("assets.staging-1-2");
        let data = staging.join("vfs/Data");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(data.join("Skyrim.esm"), b"game").unwrap();

        let config = terrain_config(&data, &output);
        let error = detach_terrain_sources(&config, &output, &staging).unwrap_err();

        assert!(
            format!("{error:#}").contains("hold the Skyrim Data folder"),
            "{error:#}"
        );
        assert!(data.join("Skyrim.esm").is_file());
        let beside: Vec<_> = fs::read_dir(root.path()).unwrap().collect();
        assert_eq!(beside.len(), 2, "a folder was left beside the output");
    }

    #[test]
    fn a_missing_vfs_is_an_error_not_a_silent_skip() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        let output = root.path().join("assets");
        let staging = root.path().join("assets.staging-1-2");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&staging).unwrap();
        let config = terrain_config(&data, &output);
        assert!(detach_terrain_sources(&config, &output, &staging).is_err());
    }
}

#[cfg(test)]
mod threshold_tests {
    use super::*;

    #[test]
    fn cluster_mode_resolves_every_requested_threshold_to_one() {
        let mut config = PipelineConfig::new("Data", "assets");
        config.lod_mode = LodMode::Cluster;
        for threshold in [1, DEFAULT_TRIANGLE_THRESHOLD, 2048] {
            config.cluster_triangle_threshold = threshold;
            let options = conversion_options(&config, Path::new("assets"));
            assert_eq!(options.triangle_threshold, 1);
            assert!(
                retry_command(&config, Path::new("assets"), None)
                    .contains("--triangle-threshold 1 --jobs")
            );
        }
    }

    #[test]
    fn auto_and_baked_preserve_the_requested_threshold() {
        let mut config = PipelineConfig::new("Data", "assets");
        config.cluster_triangle_threshold = 2048;
        for mode in [LodMode::Auto, LodMode::Baked] {
            config.lod_mode = mode;
            assert_eq!(
                conversion_options(&config, Path::new("assets")).triangle_threshold,
                2048
            );
        }
    }
}

#[cfg(test)]
mod request_regression_tests {
    use super::*;

    #[test]
    fn zero_threshold_is_rejected_before_cluster_threshold_resolution() {
        for mode in [LodMode::Cluster, LodMode::Auto] {
            let mut config = PipelineConfig::new("Data", "assets");
            config.lod_mode = mode;
            config.cluster_triangle_threshold = 0;
            let error = validate_request(&config).unwrap_err();
            assert!(format!("{error:#}").contains("threshold"));
        }
    }

    #[test]
    fn zero_jobs_are_rejected_before_overlay_derivation() {
        let mut config = PipelineConfig::new("Data", "assets");
        config.lod_mode = LodMode::Auto;
        config.cpu_jobs = 0;
        assert!(validate_request(&config).is_err());
    }

    #[test]
    fn forced_cluster_enables_terrain_and_trees_without_a_separate_flag() {
        let mut config = PipelineConfig::new("Data", "assets");
        config.lod_mode = LodMode::Cluster;
        validate_request(&config).unwrap();
        let options = conversion_options(&config, Path::new("assets"));
        assert!(options.terrain);
        assert!(options.include_trees);
        assert_eq!(options.triangle_threshold, 1);
    }

    #[test]
    fn terrain_request_requires_enabled_cluster_or_auto_lod() {
        for (mode, no_lod) in [
            (LodMode::Baked, false),
            (LodMode::Auto, true),
            (LodMode::Cluster, true),
        ] {
            let mut config = PipelineConfig::new("Data", "assets");
            config.lod_mode = mode;
            config.no_lod = no_lod;
            config.cluster_terrain = true;
            assert!(validate_request(&config).is_err(), "{mode} no_lod={no_lod}");
        }
        let mut config = PipelineConfig::new("Data", "assets");
        config.lod_mode = LodMode::Auto;
        config.cluster_terrain = true;
        validate_request(&config).unwrap();
        assert!(conversion_options(&config, Path::new("assets")).terrain);
    }
}
