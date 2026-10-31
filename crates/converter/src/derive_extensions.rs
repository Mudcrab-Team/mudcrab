//! Add optional clusters to an unchanged core. This is not a conversion or a
//! repair: existing incompleteness remains visible in the resulting dataset.
use crate::{
    cache::{CONVERTER_SCHEMA_VERSION, ConversionManifest, hash_file},
    check::{CheckMode, CheckProblem, check_output},
    cluster::{self, publish::PublishContext, stage::StageSettings},
};
use color_eyre::{
    Result,
    eyre::{bail, ensure},
};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use shared::{
    asset_lock::{AssetLock, resolve_asset_path},
    cluster_overlay::ResolvedBuildSettings,
    extensions::{ExtensionEntry, core_build_identity},
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct DeriveOptions {
    pub source: PathBuf,
    pub label: String,
    pub replace: bool,
    pub keep_failed_staging: bool,
    pub terrain: bool,
    /// Raw DDS root, verified against the current core LOD source recipe.
    pub terrain_sources: Option<PathBuf>,
    pub jobs: usize,
    pub triangle_threshold: u32,
    pub include_trees: bool,
}

pub struct DeriveReport {
    pub stage: cluster::StageReport,
    pub output: PathBuf,
    pub incomplete_core: bool,
    pub extensions: Vec<ExtensionEntry>,
    pub notices: Vec<String>,
}

const DERIVED_OWNER_FILE: &str = "OWNER.txt";
const DERIVED_OWNER: &str =
    "owner: cluster overlays for this base conversion\nkeep-until: base is explicitly discarded\n";
/// First line of the owner note every derivation writes into its own staging directory.
const STAGING_OWNER_PREFIX: &str = "owner: cluster overlay derivation\n";

/// The entries of `derived/` that a base rebuild would destroy, by name. Overlays, anything
/// unrecognised, and a staging whose derivation still holds its lock count. The owner note,
/// lock files and a staging left by a dead derivation are this command's own bookkeeping.
pub(crate) fn derived_content_to_discard(derived: &Path) -> Result<Vec<String>> {
    let mut content = Vec::new();
    for entry in fs::read_dir(derived)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = fs::symlink_metadata(entry.path())?;
        let bookkeeping = if is_link(&metadata) {
            false
        } else if metadata.is_file() {
            name == DERIVED_OWNER_FILE || name.ends_with(shared::asset_lock::ASSET_LOCK_FILE_SUFFIX)
        } else {
            metadata.is_dir()
                && name.starts_with(shared::cluster_overlay::OVERLAY_STAGING_PREFIX)
                && is_dead_own_staging(&entry.path())?
        };
        if !bookkeeping {
            content.push(name);
        }
    }
    content.sort();
    Ok(content)
}

/// A staging this command created whose derivation has ended. A running derivation holds the
/// exclusive lock on its staging for its whole life; a staging without that lock file never had
/// a live holder, so the probe creates nothing.
fn is_dead_own_staging(staging: &Path) -> Result<bool> {
    let lock_file = shared::asset_lock::asset_lock_path(&resolve_asset_path(staging)?);
    if lock_file.try_exists()? {
        match AssetLock::acquire_exclusive(staging) {
            Ok(_idle) => {}
            Err(error) if error.is_held() => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(fs::read_to_string(staging.join(DERIVED_OWNER_FILE))
        .is_ok_and(|note| note.starts_with(STAGING_OWNER_PREFIX)))
}

/// Own the source reader lock and destination writer lock through staging,
/// validation and the existing atomic new-directory publication path.
pub fn run(options: &DeriveOptions) -> Result<DeriveReport> {
    run_with_probe(options, None)
}

/// Requested byte-affecting settings, before terrain applicability is known. This candidate
/// must not select a destination: use [`resolved_settings`] against the locked base first.
/// Scheduling (`jobs`), the label, `replace` and host paths do not change output.
pub(crate) fn requested_settings(options: &DeriveOptions) -> ResolvedBuildSettings {
    ResolvedBuildSettings::new(
        env!("CARGO_PKG_VERSION"),
        shared::cluster::hex(
            &cluster::identity::identity_inputs(options.triangle_threshold).identity(),
        ),
        options.triangle_threshold,
        options.terrain,
        options.include_trees,
    )
}

/// Resolve the actual terrain domain before hashing settings or selecting a destination lock.
/// The caller retains the base lock (or owns private conversion staging) while reading `source`.
/// A selection error is never interpreted as an empty terrain domain.
pub(crate) fn resolved_settings(
    options: &DeriveOptions,
    source: &Connection,
) -> Result<ResolvedBuildSettings> {
    let mut settings = requested_settings(options);
    if settings.terrain {
        settings.terrain = cluster::terrain_stage::has_selected_terrain(source)?;
    }
    Ok(settings)
}

/// `path` without a Windows verbatim prefix (`\\?\Q:\x` becomes `Q:\x`) for the
/// local source-root invocation. UNC verbatim paths and every other path are unchanged.
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with(r"UNC\") => PathBuf::from(rest),
        _ => path,
    }
}

pub(crate) fn run_with_probe(
    options: &DeriveOptions,
    probe: Option<cluster::probe::ProbeResult>,
) -> Result<DeriveReport> {
    run_with_space(options, probe, derive_storage::available_space)
}

fn run_with_space(
    options: &DeriveOptions,
    probe: Option<cluster::probe::ProbeResult>,
    free_space: impl FnMut(&Path) -> Result<u64> + Send + 'static,
) -> Result<DeriveReport> {
    ensure!(
        options.jobs > 0 && options.triangle_threshold > 0,
        "jobs and triangle threshold must be positive"
    );
    ensure!(
        !options.label.trim().is_empty(),
        "overlay label must not be empty"
    );
    let source = resolve_asset_path(&options.source)?;
    let source_lock = AssetLock::acquire_shared(&source)?;
    let source = source_lock.asset_dir();
    let (raw_manifest, manifest, incomplete) = preflight(source)?;
    ensure!(
        manifest.geometry.geometry_mode != shared::geometry::GeometryMode::Cluster
            || options.triangle_threshold == 1,
        "cluster geometry requires --triangle-threshold 1; small primitives have no baked draw path"
    );
    ensure!(
        options.terrain || options.terrain_sources.is_none(),
        "--terrain-sources requires --terrain"
    );
    let connection = Connection::open_with_flags(
        source.join("skyrim_world.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let build_settings = resolved_settings(options, &connection)?;
    build_settings.validate()?;
    // Preserve requested-input validation even when the selected LAND domain is empty.
    let terrain_contract = options
        .terrain
        .then(|| cluster::terrain_sources::TerrainSourceContract::open(source))
        .transpose()?;
    let core_vfs = source.join("vfs");
    let terrain_root = if let Some(contract) = &terrain_contract {
        let has_core_vfs = core_vfs.try_exists()?;
        let root = if has_core_vfs {
            if let Some(explicit) = &options.terrain_sources {
                ensure!(
                    fs::canonicalize(explicit)? == fs::canonicalize(&core_vfs)?,
                    "an existing core VFS cannot be replaced"
                );
            }
            core_vfs
        } else {
            options.terrain_sources.clone().ok_or_else(|| color_eyre::eyre::eyre!(
                "terrain derivation requires --terrain-sources <raw DDS root> when the core has no VFS"))?
        };
        ensure!(
            root.is_dir(),
            "{} does not exist: the terrain atlas bake needs the source landscape textures",
            root.display()
        );
        let root = without_verbatim_prefix(fs::canonicalize(root)?);
        verify_terrain_input_root(&root, contract)?;
        Some(root)
    } else {
        None
    };
    // An absent terrain domain omits publication, not validation of requested inputs.
    if options.terrain && !build_settings.terrain {
        crate::lod::terrain::read_cached_heights(
            &source.join("cell_cache.rkyv"),
            &Default::default(),
        )?;
    }
    let id = build_settings.settings_id();
    let derived = source.join("derived");
    // The guard measures the base folder, not `derived/`: on a base's first derivation `derived/`
    // does not exist yet when the guard samples for the write that creates it, and a free-space
    // query on a missing path fails. `derived/` is a plain directory directly under the base (a
    // link or reparse point is refused below), so both are on the same volume.
    let space = Arc::new(crate::derive_space::SpaceGuard::new(source, free_space)?);
    if derived.try_exists()? {
        ensure!(
            !is_link(&fs::symlink_metadata(&derived)?),
            "derived must not be a link or reparse point"
        );
    } else {
        space.write_file(&derived.join(DERIVED_OWNER_FILE), DERIVED_OWNER.as_bytes())?;
    }
    let output = derived.join(format!("clusters-{id}"));
    let output_lock = AssetLock::acquire_exclusive(&output)?;
    if output.try_exists()? {
        ensure!(
            options.replace,
            "overlay {} already exists; use --replace",
            output.display()
        );
        ensure!(
            !is_link(&fs::symlink_metadata(&output)?),
            "existing overlay must not be linked"
        );
        let previous = shared::cluster_overlay::read_metadata(&output)?;
        ensure!(
            previous.settings_id == id,
            "existing overlay settings id differs from its destination"
        );
    }
    let mut storage_notices =
        derive_storage::admit(source, &derived, &output, build_settings.terrain, |_| {
            space.available()
        })?;
    let result = (|| {
        // Labels name overlays within a base. The claim lasts through publication, and the overlay
        // that held the label under another settings id is retired only once the new one is in place.
        let mut label_claim =
            derive_storage::claim_label(&derived, &output, &options.label, options.replace)?;
        storage_notices.append(&mut label_claim.notices);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let staging = derived.join(format!(".staging-{}-{stamp}", std::process::id()));
        let mut owner = space.with_file(&staging.join("OWNER.txt"), 0, || {
            derive_storage::Staging::create(staging.clone(), options.keep_failed_staging)
        })?;
        space.write_file(
            &staging.join("OWNER.txt"),
            format!("{STAGING_OWNER_PREFIX}keep-until: overlay is explicitly discarded\n")
                .as_bytes(),
        )?;
        // New cluster-only database. Core reads are attached read-only by the stage.
        space.write_file(&staging.join("clusters.db"), b"")?;
        let settings = StageSettings {
            mode: cluster::LodMode::Cluster,
            triangle_threshold: options.triangle_threshold,

            jobs: options.jobs,
            assets_root: Some(source.to_path_buf()),

            read_database: Some(source.join("skyrim_world.db")),
            area: None,
            child_worldspaces: Vec::new(),
            include_trees: options.include_trees,

            map_references: true,
        };
        let context = PublishContext::new_overlay(
            &output_lock,
            &output,
            &staging,
            source,
            terrain_root.as_deref(),
        )?
        .with_space(&space);
        let (stage, _) = cluster::stage::run_stage_guarded(
            &settings,
            probe,
            &staging,
            &output,
            &manifest,
            Some(&space),
        )?;
        ensure!(
            stage.mode == cluster::LodMode::Cluster.to_string(),
            "cluster derivation did not produce cluster geometry"
        );
        let (mut finalized, mut notices) = cluster::finalize_certified_extensions(
            &settings,
            &context,
            &manifest,
            &stage.builder_identity,
            build_settings.terrain,
        )?;
        ensure!(
            build_settings.terrain
                == finalized
                    .iter()
                    .any(|entry| entry.id == shared::extensions::CLUSTER_TERRAIN),
            "terrain publication differs from the resolved terrain domain"
        );
        ensure!(
            stage.builder_identity == build_settings.builder_identity,
            "stage builder identity {} differs from the resolved settings' {}",
            stage.builder_identity,
            build_settings.builder_identity
        );
        notices.append(&mut storage_notices);
        notices.extend(stage.notices.iter().cloned());
        let base_failure_count = manifest.failures.len();
        if base_failure_count > 0 {
            notices.push(format!(
            "the base records {base_failure_count} failed asset conversion(s); the overlay neither repairs nor hides them, and the engine's incomplete-assets policy decides at runtime"
        ));
        }
        for entry in &mut finalized {
            entry
                .stats
                .insert("derived_over_incomplete_core".into(), u64::from(incomplete));
        }
        let core = core_build_identity(&connection)?
            .ok_or_else(|| color_eyre::eyre::eyre!("core identity missing"))?;
        let metadata = shared::cluster_overlay::OverlayMetadata {
            schema_version: shared::cluster_overlay::OVERLAY_SCHEMA_VERSION,
            kind: shared::cluster_overlay::OVERLAY_KIND.into(),
            label: options.label.clone(),
            settings_id: id,
            build_settings,
            // Source identity is the core-bound DDS key/hash recipe. The invocation's
            // location may be temporary and must never become package provenance.
            terrain_sources: None,
            base_core_identity: core,
            base_fingerprint: shared::cluster_overlay::base_fingerprint(source)?,
            base_failure_count,
            builder_identity: stage.builder_identity.clone(),
            created: format!("unix-nanoseconds:{stamp}"),
            owner: fs::read_to_string(staging.join("OWNER.txt"))?,
            extensions: finalized.clone(),
        };
        space.write_file(
            &staging.join("overlay.json"),
            &serde_json::to_vec_pretty(&metadata)?,
        )?;
        ensure!(
            serde_json::from_slice::<Value>(&fs::read(source.join("conversion-manifest.json"))?)?
                == raw_manifest,
            "source manifest changed during derivation"
        );
        drop(connection);
        space.checkpoint()?;
        derive_storage::publish(&staging, &output, options.replace, label_claim.retired())?;
        owner.published();
        Ok(DeriveReport {
            stage,
            output,
            incomplete_core: incomplete,
            extensions: finalized,
            notices,
        })
    })();
    // Inspect after stage connections (including close-time IO) have dropped. Cache-reuse
    // paths and SQLite must never hide a guard refusal behind a generic builder/SQL error.
    space.check_failure()?;
    result
}

/// Exercise the real derive path in CPU-only synthetic converter fixtures.
#[cfg(test)]
pub(crate) fn run_fixture(options: &DeriveOptions) -> Result<DeriveReport> {
    run_fixture_with_probe(
        options,
        Some(cluster::probe::ProbeResult {
            passed: true,
            missing: Vec::new(),
            backend: "synthetic-no-device".into(),
            adapter_name: "CPU fixture: no GPU requested".into(),
            json: serde_json::json!({"test": true}),
        }),
    )
}

#[cfg(test)]
pub(crate) fn run_fixture_with_probe(
    options: &DeriveOptions,
    probe: Option<cluster::probe::ProbeResult>,
) -> Result<DeriveReport> {
    run_with_space(options, probe, |_| Ok(u64::MAX))
}

fn preflight(root: &Path) -> Result<(Value, ConversionManifest, bool)> {
    let raw: Value = serde_json::from_slice(&fs::read(root.join("conversion-manifest.json"))?)?;
    let manifest: ConversionManifest = serde_json::from_value(raw.clone())?;
    let owns_derived = |path: &str| {
        let normalized = path.replace('\\', "/");
        normalized
            .split('/')
            .find(|part| !part.is_empty() && *part != ".")
            .is_some_and(|first| first.eq_ignore_ascii_case("derived"))
    };
    ensure!(
        manifest
            .entries
            .values()
            .all(|entry| !owns_derived(&entry.output))
            && manifest.extensions.iter().all(|extension| extension
                .owns
                .files
                .iter()
                .all(|path| !owns_derived(path))),
        "base manifest must not own files under derived/: overlays are outside the base fingerprint"
    );
    ensure!(
        manifest.schema_version == CONVERTER_SCHEMA_VERSION,
        "derive-clusters requires converter schema {CONVERTER_SCHEMA_VERSION}"
    );
    // Read-only SQLite can still create shared-memory sidecars for a WAL
    // database. A sealed base must have no live journal before any DB open.
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = root.join(format!("skyrim_world.db{suffix}"));
        match fs::symlink_metadata(&sidecar) {
            Ok(_) => bail!(
                "base has SQLite sidecars: {}; checkpoint and close its writer before deriving",
                sidecar.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    ensure!(
        shared::world_assets::validate_world_database(&root.join("skyrim_world.db"))?
            == shared::WORLD_DATABASE_SCHEMA_VERSION,
        "derive-clusters requires the current world database schema"
    );
    let connection = Connection::open_with_flags(
        root.join("skyrim_world.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let core = core_build_identity(&connection)?
        .filter(|identity| !identity.trim().is_empty())
        .ok_or_else(|| color_eyre::eyre::eyre!("core identity is missing or empty"))?;
    // Recorded asset failures make a base incomplete, as the converter writes it. They are
    // admitted like the other retained core limitations; whether such a base may run is the
    // engine's incomplete-assets policy, not this command's.
    let failure_count = manifest.failures.len();
    let incomplete = !manifest.complete;
    ensure!(
        incomplete || (manifest.record_data_complete != Some(false) && failure_count == 0),
        "source completeness flags disagree"
    );
    if incomplete {
        eprintln!(
            "derive-clusters: DEVELOPMENT DATASET: the core is incomplete. {failure_count} recorded asset conversion failure(s) and any record warnings remain; this command certifies only the optional cluster scope, never global data completeness."
        );
    }
    ensure!(
        !incomplete || manifest.record_data_complete == Some(false) || failure_count > 0,
        "source is incomplete for an unestablished reason"
    );
    // Existing incompleteness and exactly the recorded failures are admitted as an explicit
    // retained core limitation. Size, path, schema and every artifact hash still must pass.
    let check = check_output(root, CheckMode::Full, |_, _| {})?;
    ensure!(
        check.problems.iter().all(|problem| match problem {
            CheckProblem::Incomplete => incomplete,
            CheckProblem::RecordedFailures { count } => {
                *count == failure_count && failure_count > 0
            }
            _ => false,
        }),
        "source integrity check failed: {:?}",
        check.problems
    );
    let existing: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE lower(name) GLOB 'cluster_*'",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        existing == 0
            && !root.join("clusters").try_exists()?
            && !root.join("cluster-manifest.json").try_exists()?
            && !root.join("cluster-terrain-manifest.json").try_exists()?,
        "derive-clusters refuses an existing cluster store"
    );
    ensure!(
        !manifest
            .extensions
            .iter()
            .any(|entry| entry.id == shared::extensions::CLUSTERS
                || entry.id == shared::extensions::CLUSTER_TERRAIN),
        "source already declares cluster extensions"
    );
    let specs = crate::extensions::specs();
    let expected = crate::extensions::build_entries(root)?;
    let mut ids = BTreeSet::new();
    ensure!(
        shared::extensions::ownership_problems(&manifest.extensions).is_empty(),
        "source extension ownership conflicts"
    );
    for entry in &manifest.extensions {
        ensure!(
            ids.insert(entry.id.clone()),
            "duplicate source extension {}",
            entry.id
        );
        if let Some(spec) = specs.iter().find(|spec| spec.id == entry.id) {
            ensure!(
                entry.version == spec.version && entry.owns == spec.owns,
                "source built-in {} needs an unsupported migration",
                entry.id
            );
            ensure!(
                expected
                    .iter()
                    .any(|expected| expected.id == entry.id && expected.sha256 == entry.sha256),
                "source built-in {} differs from its supported content contract",
                entry.id
            );
        }
        ensure!(
            entry.built_from == core,
            "source extension {} is stale",
            entry.id
        );
        ensure!(
            crate::extensions::recompute_hash(root, entry)? == entry.sha256,
            "source extension {} has changed",
            entry.id
        );
    }
    ensure!(
        expected.iter().all(|entry| ids.contains(&entry.id)),
        "source omits a built-in extension declaration; this command does not refresh or migrate it"
    );
    Ok((raw, manifest, incomplete))
}

pub(crate) fn is_link(metadata: &fs::Metadata) -> bool {
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

#[cfg(test)]
#[path = "derive_extensions_tests.rs"]
mod tests;

#[cfg(test)]
mod settings_tests {
    use super::*;

    fn options() -> DeriveOptions {
        DeriveOptions {
            source: PathBuf::from("base"),
            label: "default".into(),
            replace: false,
            keep_failed_staging: false,
            terrain: false,
            terrain_sources: None,
            jobs: 1,
            triangle_threshold: 1024,
            include_trees: false,
        }
    }

    #[test]
    fn the_folder_id_is_the_hash_of_the_recorded_resolved_settings() {
        let mut options = options();
        let source = Connection::open_in_memory().unwrap();
        let settings = resolved_settings(&options, &source).unwrap();
        settings.validate().unwrap();
        assert_eq!(
            resolved_settings(&options, &source).unwrap().settings_id(),
            settings.settings_id()
        );
        assert_eq!(settings.converter_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            settings.builder_identity,
            shared::cluster::hex(&cluster::identity::identity_inputs(1024).identity())
        );
        // Scheduling, label, replace and host paths never reach the object.
        options.jobs = 7;
        options.label = "other".into();
        options.replace = true;
        options.keep_failed_staging = true;
        options.terrain_sources = Some(PathBuf::from("E:/raw"));
        assert_eq!(resolved_settings(&options, &source).unwrap(), settings);
        assert_eq!(
            resolved_settings(&options, &source).unwrap().settings_id(),
            settings.settings_id()
        );
        options.include_trees = true;
        assert_ne!(
            resolved_settings(&options, &source).unwrap().settings_id(),
            settings.settings_id()
        );
    }

    #[test]
    fn a_verbatim_prefix_is_dropped_from_a_recorded_host_path() {
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\Q:\raw dds")),
            PathBuf::from(r"Q:\raw dds")
        );
        for unchanged in [r"\\?\UNC\server\share", r"Q:\raw", "/raw"] {
            assert_eq!(
                without_verbatim_prefix(PathBuf::from(unchanged)),
                PathBuf::from(unchanged)
            );
        }
    }
}

// Private helpers for derive_extensions.rs. Source paths/hashes are supplied
// only by TerrainSourceContract after binding them to the current core recipe.
fn verified_input_file(root: &Path, relative: &str) -> Result<PathBuf> {
    for ancestor in std::path::absolute(root)?.ancestors() {
        ensure!(
            !is_link(&fs::symlink_metadata(ancestor)?),
            "terrain source root has a linked ancestor: {}",
            ancestor.display()
        );
    }
    let mut path = root.to_owned();
    ensure!(
        fs::symlink_metadata(root)?.is_dir() && !is_link(&fs::symlink_metadata(root)?),
        "terrain source root is not a regular directory"
    );
    for component in Path::new(relative).components() {
        let std::path::Component::Normal(name) = component else {
            bail!("unsafe terrain input path");
        };
        path.push(name);
        ensure!(
            !is_link(&fs::symlink_metadata(&path)?),
            "linked terrain input path: {}",
            path.display()
        );
    }
    ensure!(
        fs::symlink_metadata(&path)?.is_file(),
        "terrain source is not a regular file: {}",
        path.display()
    );
    Ok(path)
}

fn verify_terrain_input_root(
    root: &Path,
    contract: &cluster::terrain_sources::TerrainSourceContract,
) -> Result<()> {
    for (path, hash) in contract.sources() {
        ensure!(
            hash_file(&verified_input_file(root, path)?)? == hash,
            "terrain source bytes disagree with the current core recipe: {path}"
        );
    }
    Ok(())
}

#[path = "derive_storage.rs"]
mod derive_storage;
