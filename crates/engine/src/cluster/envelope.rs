//! Envelope integrity precheck only: this does not establish authoritative
//! reference coverage, Complete status, payload draw readiness, or atlas validity.
//!
//! The envelope certifies one cluster *overlay* against its base dataset (derived-overlay). Core files
//! (`skyrim_world.db`, `cell_cache.rkyv`, published models) are read from the base; the cluster
//! tables, payloads and manifests are read from the overlay folder. The cluster extension entries
//! are `overlay.json`'s, never the base manifest's, which stays unchanged.

use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, File, Metadata},
    io::Read,
    path::{Component, Path, PathBuf},
};

use color_eyre::eyre::{Result, ensure, eyre};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shared::{
    asset_lock::AssetLock,
    cluster::{canonical_json, parse_hash},
    cluster_overlay::{
        OVERLAY_DATABASE_FILE, OVERLAY_METADATA_FILE, OVERLAY_SCHEMA, OverlayBindingReceipt,
        OverlayMetadata,
    },
    extensions::{
        CLUSTER_TERRAIN, CLUSTERS, DataExtensions, ExtensionEntry, ExtensionHasher,
        core_build_identity, locate_object, ownership_problems,
    },
};

const BASE_TABLES: [&str; 11] = [
    "cluster_build",
    "cluster_meshes",
    "cluster_chunks",
    "cluster_chunks_spatial",
    "cluster_chunks_spatial_node",
    "cluster_chunks_spatial_parent",
    "cluster_chunks_spatial_rowid",
    "cluster_rejects",
    "cluster_instance_map",
    "cluster_reference_coverage",
    "cluster_provenance",
];
const TERRAIN_TABLES: [&str; 3] = [
    "cluster_terrain",
    "cluster_terrain_heights",
    "cluster_terrain_build",
];

/// Performance pragmas affect only this fresh read-only connection. If SQLite cannot apply
/// them, report that and keep available cache settings; every authoritative check still runs.
fn tune_connection(connection: &Connection, overlay_attached: bool) {
    let result =
        shared::cluster_recipe::authority::configure_read_only_cache(connection).and_then(|()| {
            if overlay_attached {
                shared::cluster_recipe::authority::configure_attached_read_only_cache(
                    connection,
                    OVERLAY_SCHEMA,
                )
            } else {
                Ok(())
            }
        });
    if let Err(error) = result {
        bevy::log::warn!(%error, "certification read cache tuning incomplete; retaining all authoritative reads");
    }
}

/// A read-only connection on the base dataset alone: core tables only, nothing attached. The core
/// authority reads through it, so it cannot see (or be answered by) a cluster table.
pub(crate) fn open_base_certification_connection(base: &Path) -> Result<Connection> {
    check_sidecars(base, "skyrim_world.db")?;
    let connection = Connection::open_with_flags(
        base.join("skyrim_world.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    tune_connection(&connection, false);
    Ok(connection)
}

/// A read-only connection on the base with the overlay's `clusters.db` attached read-only, after the
/// overlay's binding to the base was checked: cluster tables keep their unqualified names.
fn open_certification_connection(binding: &OverlayBindingReceipt) -> Result<Connection> {
    let connection = binding.connection()?;
    tune_connection(&connection, true);
    Ok(connection)
}

use shared::cluster_mapping::HashedPayload as CertifiedPayload;

type PayloadCapture = std::sync::Mutex<HashMap<String, CertifiedPayload>>;

pub struct ClusterEnvelope {
    /// The base dataset: `skyrim_world.db`, `cell_cache.rkyv`, published models and textures.
    pub root: PathBuf,
    /// The overlay folder: `clusters.db`, `clusters/` payloads and the cluster manifests.
    pub overlay: PathBuf,
    pub metadata: OverlayMetadata,
    pub core_identity: String,
    pub base: ExtensionEntry,
    pub terrain: Option<ExtensionEntry>,
    pub builder_identity: String,
    payloads: PayloadCapture,
    payload_digests: HashMap<String, [u8; 32]>,
    // Computed from the base cache bytes in this launch's certification I/O task.
    terrain_cell_cache_digest: Option<[u8; 32]>,
    terrain_atlas_digests: HashMap<String, [u8; 32]>,
    binding: OverlayBindingReceipt,
    _lock: AssetLock,
    _overlay_lock: AssetLock,
}

impl ClusterEnvelope {
    /// The cache identity checked against the terrain manifest under this launch's locks.
    /// A missing receipt refuses terrain; no caller may recover it from disk on Update.
    pub(crate) fn terrain_cell_cache_digest(&self) -> Result<[u8; 32]> {
        self.terrain_cell_cache_digest
            .ok_or_else(|| eyre!("terrain cell cache was not certified for this launch"))
    }

    /// Exact certified bytes for a terrain atlas explicitly named by the checked recipe.
    pub(crate) fn terrain_atlas_payload(&self, relative: &str) -> Result<CertifiedPayload> {
        self.terrain_cell_cache_digest()?;
        let expected = self
            .terrain_atlas_digests
            .get(relative)
            .ok_or_else(|| eyre!("atlas has no terrain recipe receipt: {relative}"))?;
        let payload = self.payload(relative)?;
        ensure!(
            payload.digest() == *expected,
            "atlas bytes differ from terrain recipe receipt: {relative}"
        );
        Ok(payload)
    }

    pub(crate) fn payloads(&self) -> Result<HashMap<String, CertifiedPayload>> {
        Ok(self
            .payloads
            .lock()
            .map_err(|_| eyre!("certified payload capture poisoned"))?
            .clone())
    }

    /// A package-owned payload from this launch's certified byte snapshot.
    pub(crate) fn payload(&self, relative: &str) -> Result<CertifiedPayload> {
        let payload = self
            .payloads
            .lock()
            .map_err(|_| eyre!("certified payload capture poisoned"))?
            .get(relative)
            .cloned()
            .ok_or_else(|| eyre!("payload was not certified: {relative}"))?;
        ensure!(
            self.payload_digest(relative) == Some(payload.digest()),
            "certified payload receipt differs from retained digest: {relative}"
        );
        Ok(payload)
    }

    pub(crate) fn payload_digest(&self, relative: &str) -> Option<[u8; 32]> {
        self.payload_digests.get(relative).copied()
    }

    /// Rebuild only: hash newly read bytes against this launch's immutable receipt.
    /// The caller retains CertifiedPackage/Envelope (and both shared source locks)
    /// across this operation. Never use this instead of the initial captured snapshot.
    pub(crate) fn reread_payload(&self, relative: &str) -> Result<CertifiedPayload> {
        let launch_digest = self
            .payload_digest(relative)
            .ok_or_else(|| eyre!("payload was not certified: {relative}"))?;
        safe_relative(relative)?;
        let path = self.overlay.join(relative);
        check_path(&path)?;
        regular(&path)?;
        let payload = CertifiedPayload::new(fs::read(&path)?.into());
        ensure!(
            payload.digest() == launch_digest,
            "reread payload content hash differs from launch certification: {relative}"
        );
        Ok(payload)
    }

    /// The scene now owns needed MCL Arcs. Drop the envelope's payload owners,
    /// including consumed MCI buffers, while retaining every certified digest.
    pub(crate) fn release_consumed_payload_bytes(&self) -> Result<()> {
        self.payloads
            .lock()
            .map_err(|_| eyre!("certified payload capture poisoned"))?
            .clear();
        Ok(())
    }

    /// A fresh certification connection on this envelope's base and overlay.
    pub(crate) fn connection(&self) -> Result<Connection> {
        open_certification_connection(&self.binding)
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}

/// SQLite may create or replay `-wal`/`-shm`/`-journal` sidecars even for a read-only open, which
/// would mutate a sealed dataset: any existing sidecar refuses the open (nothing is checkpointed).
fn check_sidecars(root: &Path, database: &str) -> Result<()> {
    shared::cluster_overlay::refuse_live_sidecars(root, database)
}

/// Hold the reader locks (shared, on the base and on the overlay folder) for the lifetime of this
/// guard. All later readers must use these roots and retain the guard; this is not a free-standing
/// certification. The overlay must be bound to the base (`shared::cluster_overlay::checked_overlay`)
/// before any table or payload is read.
pub fn open(base: &Path, overlay: &Path, terrain: bool) -> Result<ClusterEnvelope> {
    let lock = AssetLock::acquire_shared(base).map_err(|error| eyre!("{error}"))?;
    let overlay_lock = AssetLock::acquire_shared(overlay).map_err(|error| eyre!("{error}"))?;
    // Check the original paths too: AssetLock resolves aliases for lock identity.
    check_path(&absolute(base)?)?;
    check_path(&absolute(overlay)?)?;
    let root = lock.asset_dir().to_owned();
    let overlay = overlay_lock.asset_dir().to_owned();
    ensure!(
        fs::metadata(&root)?.is_dir(),
        "dataset root is not a directory"
    );
    ensure!(
        fs::metadata(&overlay)?.is_dir(),
        "cluster overlay is not a directory"
    );
    for path in ["skyrim_world.db", "conversion-manifest.json"] {
        regular(&root.join(path))?;
    }
    for path in [OVERLAY_DATABASE_FILE, OVERLAY_METADATA_FILE] {
        regular(&overlay.join(path))?;
    }
    check_sidecars(&root, "skyrim_world.db")?;
    check_sidecars(&overlay, OVERLAY_DATABASE_FILE)?;
    // The binding comes before any table or payload is read: an overlay of another base, another
    // conversion or an old full-copy dataset is refused here with the rebuild command.
    let binding = OverlayBindingReceipt::check(&root, &overlay)?;
    let metadata = binding.metadata().clone();
    let connection = open_certification_connection(&binding)?;
    let core_identity = core_build_identity(&connection)
        .map_err(|error| eyre!("{error:#}"))?
        .filter(|identity| !identity.trim().is_empty())
        .ok_or_else(|| eyre!("cluster envelope requires the current nonempty core identity"))?;
    ensure!(
        core_identity == metadata.base_core_identity,
        "cluster overlay core identity differs from its base"
    );
    let base_extensions = DataExtensions::load(&root).map_err(|error| eyre!("{error:#}"))?;
    for entry in &metadata.extensions {
        ensure!(
            entry.id == CLUSTERS || entry.id == CLUSTER_TERRAIN,
            "cluster overlay lists an extension this build does not read: {}",
            entry.id
        );
    }
    // Ownership across the base manifest and the overlay: nothing may be claimed twice, and an
    // overlay may not claim a SQLite object the base also owns or has.
    let every_entry: Vec<&ExtensionEntry> = base_extensions
        .entries()
        .chain(&metadata.extensions)
        .collect();
    // Keep the specific SQL ownership diagnostic before the general declaration guard.
    check_sql_overlap(&every_entry)?;
    ensure!(
        ownership_problems(every_entry.iter().copied()).is_empty(),
        "extension ownership conflicts"
    );
    // Owned files are relative to each side's own root, so the base's entries are compared among
    // themselves (the guard the base manifest always had) and the overlay's among themselves.
    check_file_overlap(base_extensions.entries())?;
    check_file_overlap(metadata.extensions.iter())?;
    let base = metadata
        .extensions
        .iter()
        .find(|entry| entry.id == CLUSTERS)
        .ok_or_else(|| eyre!("cluster base extension missing"))?
        .clone();
    let payloads = PayloadCapture::default();
    check_entry(
        &connection,
        &overlay,
        &base,
        CLUSTERS,
        &core_identity,
        Some(&payloads),
    )?;
    exact(&base.owns.tables, &BASE_TABLES, "base tables")?;
    exact(
        &base.owns.files,
        &["cluster-manifest.json", "clusters/"],
        "base files",
    )?;
    check_indexes(&connection, &base)?;
    let manifest = json_file(&overlay.join("cluster-manifest.json"))?;
    ensure!(
        manifest
            .get("uncertified")
            .is_none_or(|value| value == &Value::Bool(false)),
        "uncertified cluster store"
    );
    for (key, expected) in [
        ("payload_format", 3),
        ("instance_map_version", 1),
        ("coverage_version", 1),
    ] {
        ensure!(
            manifest[key].as_u64() == Some(expected),
            "unsupported base {key}"
        );
    }
    let builder_identity = manifest["builder_identity"]
        .as_str()
        .ok_or_else(|| eyre!("base builder identity missing"))?
        .to_owned();
    ensure!(
        parse_hash(&builder_identity).is_some(),
        "invalid base builder identity"
    );
    ensure!(
        builder_identity == metadata.builder_identity,
        "cluster overlay builder identity differs from its manifest"
    );
    let (count, recorded): (u64, Option<String>) = connection.query_row(
        "SELECT COUNT(*), MIN(builder_identity) FROM cluster_build",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        count == 1 && recorded.as_deref() == Some(builder_identity.as_str()),
        "cluster_build identity differs from manifest"
    );
    let (terrain, terrain_cell_cache_digest, terrain_atlas_digests) = if terrain {
        let entry = metadata
            .extensions
            .iter()
            .find(|entry| entry.id == CLUSTER_TERRAIN)
            .ok_or_else(|| eyre!("cluster terrain extension missing"))?
            .clone();
        check_entry(
            &connection,
            &overlay,
            &entry,
            CLUSTER_TERRAIN,
            &core_identity,
            None,
        )?;
        exact(&entry.owns.tables, &TERRAIN_TABLES, "terrain tables")?;
        exact(
            &entry.owns.files,
            &["cluster-terrain-manifest.json"],
            "terrain files",
        )?;
        ensure!(
            entry.owns.indexes.is_empty(),
            "terrain extension has unexpected indexes"
        );
        let (cache_digest, atlas_digests) = check_terrain_dependency(
            &root,
            &overlay,
            &entry,
            &base,
            &builder_identity,
            &core_identity,
            &payloads,
        )?;
        (Some(entry), Some(cache_digest), atlas_digests)
    } else {
        (None, None, HashMap::new())
    };
    let payloads = payloads
        .into_inner()
        .map_err(|_| eyre!("certified payload capture poisoned"))?;
    let payload_digests = payloads
        .iter()
        .map(|(name, payload)| (name.clone(), payload.digest()))
        .collect();
    Ok(ClusterEnvelope {
        root,
        overlay,
        metadata,
        core_identity,
        base,
        terrain,
        builder_identity,
        payloads: std::sync::Mutex::new(payloads),
        payload_digests,
        terrain_cell_cache_digest,
        terrain_atlas_digests,
        binding,
        _lock: lock,
        _overlay_lock: overlay_lock,
    })
}

type TerrainDependencyDigests = ([u8; 32], HashMap<String, [u8; 32]>);

fn check_terrain_dependency(
    root: &Path,
    overlay: &Path,
    entry: &ExtensionEntry,
    base: &ExtensionEntry,
    builder: &str,
    core: &str,
    payloads: &PayloadCapture,
) -> Result<TerrainDependencyDigests> {
    let manifest = json_file(&overlay.join("cluster-terrain-manifest.json"))?;
    for (key, expected) in [
        ("version", 1),
        ("payload_format_version", 3),
        ("terrain_tables_version", 1),
        ("terrain_recipe_version", 1),
    ] {
        ensure!(
            manifest[key].as_u64() == Some(expected),
            "unsupported terrain {key}"
        );
    }
    ensure!(
        manifest["built_from"].as_str() == Some(core) && entry.built_from == core,
        "terrain core dependency stale"
    );
    ensure!(
        manifest["builder_identity"].as_str() == Some(builder),
        "terrain builder differs from base"
    );
    ensure!(
        manifest["requires"] == json!([{"id":CLUSTERS,"version":1,"sha256":base.sha256}]),
        "terrain final-base dependency stale"
    );
    // The cell cache is a core file: it is read from the base, not the overlay.
    let cache = digest_file(&root.join("cell_cache.rkyv"))?;
    ensure!(
        manifest["cell_cache_sha256"].as_str() == Some(cache.as_str()),
        "terrain core cell cache dependency stale"
    );
    ensure!(
        manifest["blocks"].is_array(),
        "terrain block recipe missing"
    );
    let inputs =
        json!({"recipe":1,"builder":builder,"cell_cache":cache,"blocks":manifest["blocks"]});
    let input_identity = hex(&Sha256::digest(canonical_json(&inputs)));
    ensure!(
        manifest["input_identity"].as_str() == Some(input_identity.as_str()),
        "terrain input identity differs from recipe"
    );
    // Base extension ownership hashed the atlas bytes, not just recipe metadata.
    // Compare the terrain recipe to those same retained typed byte snapshots.
    let captured = payloads
        .lock()
        .map_err(|_| eyre!("certified payload capture poisoned"))?;
    let mut atlas_digests = HashMap::new();
    for block in manifest["blocks"]
        .as_array()
        .ok_or_else(|| eyre!("terrain block recipe missing"))?
    {
        let relative = block["atlas_path"]
            .as_str()
            .ok_or_else(|| eyre!("terrain atlas path missing"))?;
        safe_relative(relative)?;
        let expected = block["atlas_sha256"]
            .as_str()
            .and_then(parse_hash)
            .ok_or_else(|| eyre!("invalid terrain atlas digest: {relative}"))?;
        let payload = captured
            .get(relative)
            .ok_or_else(|| eyre!("terrain atlas was not certified: {relative}"))?;
        ensure!(
            payload.digest() == expected,
            "terrain atlas digest differs from certified bytes: {relative}"
        );
        atlas_digests.insert(relative.to_owned(), expected);
    }
    // This verifies recorded recipe bytes and content identity, not source authority.
    let cache_digest =
        parse_hash(&cache).ok_or_else(|| eyre!("invalid checked terrain cache digest"))?;
    Ok((cache_digest, atlas_digests))
}

/// `root` is the folder the entry's owned files are relative to: the overlay folder.
fn check_entry(
    connection: &Connection,
    root: &Path,
    entry: &ExtensionEntry,
    id: &str,
    core: &str,
    payloads: Option<&PayloadCapture>,
) -> Result<()> {
    ensure!(
        entry.id == id && entry.version == 1 && entry.built_from == core,
        "incompatible or stale {id}"
    );
    ensure!(parse_hash(&entry.sha256).is_some(), "invalid {id} hash");
    ensure!(
        entry.owns.gltf_extensions.is_empty(),
        "{id} cannot own glTF extensions"
    );
    // Every owned table and index must be the overlay database's own, never a base object of the
    // same name (the hasher also refuses an object present in both).
    for (kind, names) in [
        ("table", &entry.owns.tables),
        ("index", &entry.owns.indexes),
    ] {
        for name in names {
            ensure!(
                locate_object(connection, kind, name)
                    .map_err(|error| eyre!("{error:#}"))?
                    .as_deref()
                    == Some(OVERLAY_SCHEMA),
                "{id} {kind} {name} is not in the cluster overlay database"
            );
        }
    }
    ensure!(
        recompute_retaining(
            connection,
            root,
            entry,
            &HashMap::new(),
            std::thread::available_parallelism().map(|count| count.get()),
            payloads
        )? == entry.sha256,
        "{id} content hash differs"
    );
    Ok(())
}

fn exact(actual: &[String], expected: &[&str], what: &str) -> Result<()> {
    let values: BTreeSet<_> = actual.iter().map(String::as_str).collect();
    ensure!(
        values.len() == actual.len() && values == expected.iter().copied().collect(),
        "unexpected {what} ownership"
    );
    Ok(())
}

fn check_indexes(connection: &Connection, entry: &ExtensionEntry) -> Result<()> {
    let mut statement = connection.prepare(&format!("SELECT name, tbl_name FROM {OVERLAY_SCHEMA}.sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_autoindex%'"))?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut expected = BTreeSet::new();
    for row in rows {
        let (index, table) = row?;
        if entry.owns.tables.contains(&table) {
            expected.insert(index);
        }
    }
    let actual: BTreeSet<_> = entry.owns.indexes.iter().cloned().collect();
    ensure!(
        actual.len() == entry.owns.indexes.len() && actual == expected,
        "base explicit index ownership differs from store"
    );
    Ok(())
}

#[cfg(test)]
pub(crate) fn recompute(
    connection: &Connection,
    root: &Path,
    entry: &ExtensionEntry,
) -> Result<String> {
    recompute_recorded(connection, root, entry, &HashMap::new())
}

/// [`recompute`], taking the digest of each owned file listed in `recorded` (relative path to
/// SHA-256 hex) instead of reading the file. Every other owned file is read and hashed.
///
/// The result still equals the certified hash only if every recorded digest is the one the
/// producer hashed, so a wrong or stale record fails here. What it does not prove is that a
/// recorded file's bytes on disk still match: the caller must check each recorded file against
/// its digest before it uses the bytes.
#[cfg(test)]
pub(crate) fn recompute_recorded(
    connection: &Connection,
    root: &Path,
    entry: &ExtensionEntry,
    recorded: &HashMap<String, String>,
) -> Result<String> {
    recompute_recorded_with_parallelism(
        connection,
        root,
        entry,
        recorded,
        std::thread::available_parallelism().map(|count| count.get()),
    )
}

#[cfg(test)]
fn recompute_recorded_with_parallelism(
    connection: &Connection,
    root: &Path,
    entry: &ExtensionEntry,
    recorded: &HashMap<String, String>,
    parallelism: std::io::Result<usize>,
) -> Result<String> {
    recompute_retaining(connection, root, entry, recorded, parallelism, None)
}

fn recompute_retaining(
    connection: &Connection,
    root: &Path,
    entry: &ExtensionEntry,
    recorded: &HashMap<String, String>,
    parallelism: std::io::Result<usize>,
    payloads: Option<&PayloadCapture>,
) -> Result<String> {
    let overlap = parallelism
        .as_ref()
        .ok()
        .copied()
        .filter(|&count| count > 3);
    let (mut hasher, files) = if let Some(available) = overlap {
        std::thread::scope(|scope| -> Result<_> {
            let worker = std::thread::Builder::new()
                .name("cluster-envelope-files".into())
                .spawn_scoped(scope, || {
                    owned_file_digests(root, entry, recorded, Ok(available - 3), payloads)
                });
            // Capture rather than propagate: even a table error must join every file worker.
            let tables = table_and_index_hasher(connection, entry);
            let files = match worker {
                Ok(worker) => worker
                    .join()
                    .map_err(|_| eyre!("cluster envelope owned-file worker panicked"))
                    .and_then(|result| result),
                Err(error) => {
                    bevy::log::warn!(%error, "envelope file overlap unavailable; retaining original serial file checks");
                    if tables.is_ok() {
                        owned_file_digests(root, entry, recorded, Ok(available - 2), payloads)
                    } else {
                        Ok(Vec::new())
                    }
                }
            };
            // The unchanged table/index order wins over any concurrent filesystem failure.
            Ok((tables?, files?))
        })?
    } else {
        let hasher = table_and_index_hasher(connection, entry)?;
        let workers = parallelism.map(|count| count.saturating_sub(2).max(1));
        let files = owned_file_digests(root, entry, recorded, workers, payloads)?;
        (hasher, files)
    };
    for (relative, digest) in files {
        hasher.add_file_digest(&relative, &digest);
    }
    let digest = hasher.finish();
    Ok(digest)
}

fn table_and_index_hasher(
    connection: &Connection,
    entry: &ExtensionEntry,
) -> Result<ExtensionHasher> {
    let mut hasher = ExtensionHasher::new(&entry.id, entry.version);
    for table in &entry.owns.tables {
        hasher
            .add_table(connection, table)
            .map_err(|error| eyre!("{error:#}"))?;
    }
    for index in &entry.owns.indexes {
        hasher
            .add_index(connection, index)
            .map_err(|error| eyre!("{error:#}"))?;
    }
    bevy::log::info!(extension = %entry.id, phase = "tables_and_indexes",
        tables = entry.owns.tables.len(), indexes = entry.owns.indexes.len(),
        "cluster envelope tables and indexes checked");
    Ok(hasher)
}

fn owned_file_digests(
    root: &Path,
    entry: &ExtensionEntry,
    recorded: &HashMap<String, String>,
    workers: std::io::Result<usize>,
    payloads: Option<&PayloadCapture>,
) -> Result<Vec<(String, String)>> {
    let mut files = BTreeSet::new();
    for owned in &entry.owns.files {
        let directory = owned.ends_with('/');
        let relative = if directory {
            &owned[..owned.len() - 1]
        } else {
            owned.as_str()
        };
        safe_relative(relative)?;
        let path = root.join(relative);
        if directory {
            // ext-hash-v1 owns the files under a folder, not its existence. An empty
            // certified stage may never create clusters/. Check its parent first so
            // absence cannot conceal an existing linked ancestor or an I/O error.
            check_path(
                path.parent()
                    .ok_or_else(|| eyre!("owned folder has no parent"))?,
            )?;
            match fs::symlink_metadata(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            }
        }
        check_path(&path)?;
        if directory {
            ensure!(
                fs::metadata(&path)?.is_dir(),
                "owned folder missing: {owned}"
            );
            collect(root, &path, &mut files)?;
        } else {
            regular(&path)?;
            ensure!(files.insert(relative.to_owned()), "duplicate owned file");
        }
    }
    let file_count = files.len();
    let (listed, read): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|relative| recorded.contains_key(relative));
    let mut digests: Vec<_> = listed
        .iter()
        .map(|relative| (relative.clone(), recorded[relative].clone()))
        .collect();
    // Even a failed worker-count lookup retains its original position after the file walk.
    let workers = workers?;
    let mut bytes = 0u64;
    for (relative, result) in read
        .iter()
        .zip(hash_owned_files_retaining(root, &read, workers, payloads)?)
    {
        let (digest, size) = result?;
        digests.push((relative.clone(), digest));
        bytes += size;
    }
    bevy::log::info!(extension = %entry.id, phase = "owned_files",
        files = file_count, recorded = listed.len(), read = read.len(), bytes,
        "cluster envelope owned files checked");
    Ok(digests)
}

/// Hash all the same bytes, retaining sorted results so concurrent failures have
/// the same first error as the serial ownership walk. The source lock outlives
/// every worker, and digest_file still checks every ancestor and regular file.
#[cfg(test)]
fn hash_owned_files(
    root: &Path,
    files: &[String],
    workers: usize,
) -> Result<Vec<Result<(String, u64)>>> {
    hash_owned_files_retaining(root, files, workers, None)
}

fn hash_owned_files_retaining(
    root: &Path,
    files: &[String],
    workers: usize,
    payloads: Option<&PayloadCapture>,
) -> Result<Vec<Result<(String, u64)>>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..workers.max(1).min(files.len()) {
            let next = &next;
            handles.push(std::thread::Builder::new().spawn_scoped(scope, move || {
                let mut results = Vec::new();
                let mut buffer = vec![0u8; 1024 * 1024];
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(relative) = files.get(index) else {
                        break;
                    };
                    let result = (|| {
                        let path = root.join(relative);
                        if let Some(payloads) = payloads.filter(|_| {
                            relative.ends_with(".mcl")
                                || relative.ends_with(".mci")
                                || Path::new(relative)
                                    .extension()
                                    .and_then(|value| value.to_str())
                                    .is_some_and(|extension| {
                                        extension.eq_ignore_ascii_case("dds")
                                            || extension.eq_ignore_ascii_case("ktx2")
                                    })
                        }) {
                            // Retain the exact bytes this digest authorizes, not a digest to
                            // be attached to a later reread. Locks live with the envelope.
                            regular(&path)?;
                            let payload = CertifiedPayload::new(fs::read(&path)?.into());
                            let digest = payload.digest();
                            let size = payload.bytes().len() as u64;
                            payloads
                                .lock()
                                .map_err(|_| eyre!("certified payload capture poisoned"))?
                                .insert(relative.clone(), payload);
                            Ok((hex(&digest), size))
                        } else {
                            let digest = digest_file_buffered(&path, &mut buffer)?;
                            let bytes = fs::metadata(path).map_or(0, |metadata| metadata.len());
                            Ok((digest, bytes))
                        }
                    })();
                    results.push((index, result));
                }
                results
            })?);
        }
        let mut results = Vec::with_capacity(files.len());
        // Join every handle even after a panic: scope must not auto-join a
        // second unobserved panic and unwind instead of returning a refusal.
        let mut panicked = false;
        for handle in handles {
            match handle.join() {
                Ok(found) => results.extend(found),
                Err(_) => panicked = true,
            }
        }
        ensure!(!panicked, "owned-file hash worker panicked");
        results.sort_unstable_by_key(|(index, _)| *index);
        Ok(results.into_iter().map(|(_, result)| result).collect())
    })
}

fn safe_relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty() && !path.contains(['\\', ':', '\0']) && !path.contains("//"),
        "unsafe owned path"
    );
    ensure!(
        Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "unsafe owned path"
    );
    // components() normalizes embedded '.', so reject those explicitly too.
    ensure!(
        path.split('/').all(|part| !matches!(part, "." | ".." | "")),
        "unsafe owned path"
    );
    Ok(())
}

fn check_path(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        ensure!(
            !matches!(component, Component::ParentDir),
            "parent traversal in dataset path"
        );
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let metadata = fs::symlink_metadata(&current)?;
        ensure!(
            !metadata.file_type().is_symlink() && !is_reparse(&metadata),
            "dataset path traverses a link/reparse point: {}",
            current.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse(_: &Metadata) -> bool {
    false
}

fn regular(path: &Path) -> Result<()> {
    check_path(path)?;
    ensure!(
        fs::metadata(path)?.is_file(),
        "owned path is not a regular file: {}",
        path.display()
    );
    Ok(())
}

fn collect(root: &Path, directory: &Path, files: &mut BTreeSet<String>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        // The caller checked the owned folder's ancestors; each entry below it is checked here
        // from its own listing metadata, which does not follow links, before it is entered.
        let metadata = entry.metadata()?;
        ensure!(
            !metadata.file_type().is_symlink() && !is_reparse(&metadata),
            "dataset path traverses a link/reparse point: {}",
            path.display()
        );
        if metadata.is_dir() {
            collect(root, &path, files)?;
        } else {
            ensure!(metadata.is_file(), "non-regular owned file");
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .ok_or_else(|| eyre!("non-UTF8 owned path"))?
                .replace('\\', "/");
            safe_relative(&relative)?;
            ensure!(files.insert(relative), "overlapping owned file claims");
        }
    }
    Ok(())
}

fn digest_file(path: &Path) -> Result<String> {
    digest_file_buffered(path, &mut vec![0u8; 1024 * 1024])
}

fn digest_file_buffered(path: &Path, buffer: &mut [u8]) -> Result<String> {
    regular(path)?;
    let mut reader = File::open(path)?;
    let mut hasher = Sha256::new();
    loop {
        let count = reader.read(buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn json_file(path: &Path) -> Result<Value> {
    regular(path)?;
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// SQLite object names are ASCII case-insensitive. Tables and indexes share a schema namespace
/// across the base and the overlay; spelling a claim differently cannot create an owner.
fn check_sql_overlap(entries: &[&ExtensionEntry]) -> Result<()> {
    let mut sql_claims = std::collections::BTreeMap::<String, &str>::new();
    for entry in entries {
        for name in entry.owns.tables.iter().chain(&entry.owns.indexes) {
            let canonical = name.to_ascii_lowercase();
            if let Some(owner) = sql_claims.insert(canonical, &entry.id) {
                ensure!(owner == entry.id, "overlapping SQLite object ownership");
            }
        }
    }
    Ok(())
}

fn check_file_overlap<'a>(entries: impl Iterator<Item = &'a ExtensionEntry>) -> Result<()> {
    let entries: Vec<&ExtensionEntry> = entries.collect();
    check_sql_overlap(&entries)?;
    let mut claims: Vec<(&str, String)> = Vec::new();
    for entry in entries {
        for path in &entry.owns.files {
            // Validate every extension, not just clusters. An alias in another
            // entry must not evade the folder-prefix comparison below.
            let relative = path.strip_suffix('/').unwrap_or(path);
            safe_relative(relative)?;
            let path = path.to_ascii_lowercase();
            for (owner, old) in &claims {
                ensure!(
                    *owner == entry.id
                        || !(old == &path
                            || (old.ends_with('/') && path.starts_with(old))
                            || (path.ends_with('/') && old.starts_with(&path))),
                    "overlapping extension file ownership"
                );
            }
            claims.push((&entry.id, path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::cluster_overlay::{
        OVERLAY_KIND, OVERLAY_SCHEMA_VERSION, ResolvedBuildSettings, base_fingerprint,
        list_overlays, overlay_dir, resolve_overlay,
    };
    use shared::extensions::ExtensionOwns;
    use std::collections::BTreeMap;

    const BUILDER: &str = "abababababababababababababababababababababababababababababababab";

    /// Synthetic build settings of the fixture overlay (this fixture is not a derive run). They are
    /// constant, so the folder name is stable across `save` calls whatever extensions an entry
    /// list claims; `threshold` tells fixture overlays apart.
    fn fixture_settings(threshold: u32) -> ResolvedBuildSettings {
        ResolvedBuildSettings::new("fixture", BUILDER, threshold, true, false)
    }

    /// The fixture overlay folder under the base `root`: the settings id is the folder name.
    fn overlay_of(root: &Path) -> PathBuf {
        overlay_dir(root, &fixture_settings(1024).settings_id())
    }

    fn overlay_connection(root: &Path) -> Connection {
        Connection::open(overlay_of(root).join(OVERLAY_DATABASE_FILE)).unwrap()
    }

    fn copy_directory(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_directory(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// The converter's own hash of an overlay's extension: it still reads one folder holding the
    /// cluster tables as `skyrim_world.db`, so the overlay is copied into that shape.
    fn converter_hash(overlay: &Path, entry: &ExtensionEntry) -> String {
        let copy = tempfile::tempdir().unwrap();
        copy_directory(overlay, copy.path());
        fs::rename(
            copy.path().join(OVERLAY_DATABASE_FILE),
            copy.path().join("skyrim_world.db"),
        )
        .unwrap();
        converter::extensions::recompute_hash(copy.path(), entry).unwrap()
    }

    #[test]
    fn overlap_matches_original_digest_with_recorded_and_fresh_files() {
        let (directory, entries) = fixture();
        let root = directory.path();
        let overlay = overlay_of(root);
        let connection = overlay_connection(root);
        let recorded = HashMap::from([(
            "clusters/fixture.bin".to_owned(),
            digest_file(&overlay.join("clusters/fixture.bin")).unwrap(),
        )]);
        let expected = converter_hash(&overlay, &entries[0]);
        for parallelism in [1, 2, 3, 8] {
            for supplied in [&HashMap::new(), &recorded] {
                assert_eq!(
                    recompute_recorded_with_parallelism(
                        &connection,
                        &overlay,
                        &entries[0],
                        supplied,
                        Ok(parallelism),
                    )
                    .unwrap(),
                    expected
                );
            }
        }
    }

    fn serial_and_overlap_error(
        connection: &Connection,
        root: &Path,
        entry: &ExtensionEntry,
    ) -> String {
        let serial =
            recompute_recorded_with_parallelism(connection, root, entry, &HashMap::new(), Ok(1))
                .unwrap_err()
                .to_string();
        let parallel =
            recompute_recorded_with_parallelism(connection, root, entry, &HashMap::new(), Ok(8))
                .unwrap_err()
                .to_string();
        assert_eq!(parallel, serial);
        serial
    }

    #[test]
    fn overlap_preserves_table_error_before_a_missing_owned_file() {
        let (directory, mut entries) = fixture();
        let overlay = overlay_of(directory.path());
        let connection = overlay_connection(directory.path());
        entries[0].owns.tables.push("missing_table".into());
        fs::remove_file(overlay.join("cluster-manifest.json")).unwrap();
        assert!(
            serial_and_overlap_error(&connection, &overlay, &entries[0]).contains("missing_table")
        );
    }

    #[test]
    fn overlap_preserves_index_error_before_an_unsafe_owned_path() {
        let (directory, mut entries) = fixture();
        let overlay = overlay_of(directory.path());
        let connection = overlay_connection(directory.path());
        entries[0].owns.indexes.push("missing_index".into());
        entries[0].owns.files.push("../unsafe".into());
        assert!(
            serial_and_overlap_error(&connection, &overlay, &entries[0]).contains("missing_index")
        );
    }

    #[test]
    fn overlap_keeps_the_first_owned_path_error_when_two_files_are_invalid() {
        let (directory, mut entries) = fixture();
        let overlay = overlay_of(directory.path());
        let connection = overlay_connection(directory.path());
        let first = overlay.join("a-invalid-file");
        fs::create_dir(&first).unwrap();
        fs::write(
            first.join("OWNER.txt"),
            "owner: envelope overlap regression fixture\n",
        )
        .unwrap();
        entries[0].owns.files = vec!["a-invalid-file".into(), "z-missing-file".into()];
        assert!(
            serial_and_overlap_error(&connection, &overlay, &entries[0]).contains("a-invalid-file")
        );
    }

    #[test]
    fn parallel_owned_hashes_match_serial_bytes_and_first_refusal() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let files: Vec<_> = (0..19).map(|index| format!("file-{index:02}")).collect();
        for (index, file) in files.iter().enumerate() {
            fs::write(root.join(file), vec![index as u8; index * 9173]).unwrap();
        }
        let serial = |files: &[String]| {
            files
                .iter()
                .map(|file| {
                    let path = root.join(file);
                    digest_file(&path).map(|digest| (digest, fs::metadata(path).unwrap().len()))
                })
                .collect::<Vec<_>>()
        };
        let expected: Vec<_> = serial(&files).into_iter().map(Result::unwrap).collect();
        for workers in [1, 2, 8, 32] {
            let found: Vec<_> = hash_owned_files(root, &files, workers)
                .unwrap()
                .into_iter()
                .map(Result::unwrap)
                .collect();
            assert_eq!(found, expected);
        }
        // Two different failures must still report the earlier sorted path.
        fs::remove_file(root.join(&files[3])).unwrap();
        fs::create_dir(root.join(&files[3])).unwrap();
        fs::remove_file(root.join(&files[9])).unwrap();
        let expected = serial(&files)
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap_err();
        for workers in [1, 2, 8, 32] {
            let error = hash_owned_files(root, &files, workers)
                .unwrap()
                .into_iter()
                .collect::<Result<Vec<_>>>()
                .unwrap_err();
            assert_eq!(error.to_string(), expected.to_string());
        }
        assert!(hash_owned_files(root, &[], 8).unwrap().is_empty());
    }

    /// Writes `overlay.json` for the fixture overlay of `root` with `entries`, bound to the base as
    /// it is now (the base manifest and database are fingerprinted at this moment).
    fn save(root: &Path, entries: &[ExtensionEntry]) {
        let metadata = OverlayMetadata {
            schema_version: OVERLAY_SCHEMA_VERSION,
            kind: OVERLAY_KIND.into(),
            label: "fixture".into(),
            settings_id: fixture_settings(1024).settings_id(),
            build_settings: fixture_settings(1024),
            terrain_sources: None,
            base_core_identity: "core".into(),
            base_fingerprint: base_fingerprint(root).unwrap(),
            base_failure_count: 0,
            builder_identity: BUILDER.into(),
            created: "2026-10-20T00:00:00Z".into(),
            owner: "envelope test fixture".into(),
            extensions: entries.to_vec(),
        };
        fs::write(
            overlay_of(root).join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
    }

    /// Replaces the base manifest's extensions, then rebinds the overlay to the changed base so
    /// the binding check passes and the entry under test is what refuses.
    fn set_base_extensions(
        root: &Path,
        base_entries: &[ExtensionEntry],
        overlay: &[ExtensionEntry],
    ) {
        fs::write(
            root.join("conversion-manifest.json"),
            serde_json::to_vec(&json!({"extensions":base_entries})).unwrap(),
        )
        .unwrap();
        save(root, overlay);
    }

    // Deliberately minimal tables: success here proves ONLY envelope integrity.
    fn fixture() -> (tempfile::TempDir, Vec<ExtensionEntry>) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let overlay = overlay_of(root);
        fs::create_dir_all(overlay.join("clusters")).unwrap();
        fs::write(overlay.join("clusters/fixture.bin"), b"not a payload").unwrap();
        // The base: core identity, manifest, cell cache. No cluster table or extension.
        fs::write(root.join("cell_cache.rkyv"), b"not a cache").unwrap();
        fs::write(
            root.join("conversion-manifest.json"),
            serde_json::to_vec(&json!({"extensions":[]})).unwrap(),
        )
        .unwrap();
        Connection::open(root.join("skyrim_world.db"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE lod_build(build_identity TEXT); INSERT INTO lod_build VALUES('core');",
            )
            .unwrap();
        // The overlay: the cluster tables only.
        let connection = Connection::open(overlay.join(OVERLAY_DATABASE_FILE)).unwrap();
        connection
            .execute_batch("CREATE TABLE cluster_build(builder_identity TEXT);")
            .unwrap();
        connection
            .execute("INSERT INTO cluster_build VALUES(?1)", [BUILDER])
            .unwrap();
        for table in BASE_TABLES.into_iter().chain(TERRAIN_TABLES) {
            if table != "cluster_build" {
                connection
                    .execute_batch(&format!("CREATE TABLE {table}(fixture INTEGER);"))
                    .unwrap();
            }
        }
        fs::write(overlay.join("cluster-manifest.json"), serde_json::to_vec(&json!({"payload_format":3,"instance_map_version":1,"coverage_version":1,"builder_identity":BUILDER})).unwrap()).unwrap();
        let mut base = ExtensionEntry {
            id: CLUSTERS.into(),
            version: 1,
            owns: ExtensionOwns {
                tables: BASE_TABLES.map(str::to_owned).to_vec(),
                files: vec!["cluster-manifest.json".into(), "clusters/".into()],
                ..Default::default()
            },
            sha256: String::new(),
            built_from: "core".into(),
            stats: BTreeMap::new(),
        };
        base.sha256 = recompute(&connection, &overlay, &base).unwrap();
        let cache = digest_file(&root.join("cell_cache.rkyv")).unwrap();
        let inputs = json!({"recipe":1,"builder":BUILDER,"cell_cache":cache,"blocks":[]});
        fs::write(overlay.join("cluster-terrain-manifest.json"), serde_json::to_vec(&json!({"version":1,"payload_format_version":3,"terrain_tables_version":1,"terrain_recipe_version":1,"built_from":"core","builder_identity":BUILDER,"cell_cache_sha256":cache,"input_identity":hex(&Sha256::digest(canonical_json(&inputs))),"requires":[{"id":CLUSTERS,"version":1,"sha256":base.sha256}],"blocks":[]})).unwrap()).unwrap();
        let mut terrain = ExtensionEntry {
            id: CLUSTER_TERRAIN.into(),
            version: 1,
            owns: ExtensionOwns {
                tables: TERRAIN_TABLES.map(str::to_owned).to_vec(),
                files: vec!["cluster-terrain-manifest.json".into()],
                ..Default::default()
            },
            sha256: String::new(),
            built_from: "core".into(),
            stats: BTreeMap::new(),
        };
        terrain.sha256 = recompute(&connection, &overlay, &terrain).unwrap();
        let entries = vec![base, terrain];
        drop(connection);
        save(root, &entries);
        (temp, entries)
    }

    /// `open` on the fixture overlay.
    fn open_fixture_overlay(root: &Path, terrain: bool) -> Result<ClusterEnvelope> {
        open(root, &overlay_of(root), terrain)
    }

    #[test]
    fn sql_atlas_requires_a_terrain_recipe_receipt() {
        let (temp, mut entries, _) = terrain_snapshot_terrain_fixture();
        let root = temp.path();
        {
            let envelope = open_fixture_overlay(root, true).unwrap();
            terrain_snapshot_load_terrain(&envelope)
                .expect("matching terrain recipe is a valid control");
        }
        let path = overlay_of(root).join("cluster-terrain-manifest.json");
        let mut manifest = json_file(&path).unwrap();
        manifest["blocks"] = json!([]);
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        terrain_snapshot_refresh_terrain(root, &mut entries);
        let envelope = open_fixture_overlay(root, true).unwrap();
        let error = match terrain_snapshot_load_terrain(&envelope) {
            Err(error) => error,
            Ok(_) => panic!("SQL-selected atlas must have a terrain recipe receipt"),
        };
        assert!(error.contains("terrain recipe"), "wrong refusal: {error}");
    }

    #[test]
    fn sql_atlas_cannot_select_a_different_path_from_the_terrain_recipe() {
        let (temp, mut entries, atlas) = terrain_snapshot_terrain_fixture();
        let root = temp.path();
        let overlay = overlay_of(root);
        {
            let envelope = open_fixture_overlay(root, true).unwrap();
            terrain_snapshot_load_terrain(&envelope)
                .expect("matching terrain recipe is a valid control");
        }
        let other = "clusters/terrain/atlases/other.ktx2";
        fs::write(overlay.join(other), atlas).unwrap();
        let connection = overlay_connection(root);
        connection
            .execute("UPDATE cluster_terrain SET atlas_path=?1", [other])
            .unwrap();
        drop(connection);
        terrain_snapshot_refresh_terrain(root, &mut entries);
        let envelope = open_fixture_overlay(root, true).unwrap();
        let error = match terrain_snapshot_load_terrain(&envelope) {
            Err(error) => error,
            Ok(_) => {
                panic!("certified atlas bytes alone do not authorize an undeclared terrain path")
            }
        };
        assert!(error.contains("terrain recipe"), "wrong refusal: {error}");
    }

    // Real atlas producer, extension hashes and terrain build receipt. This is
    // still an envelope fixture, not a source/mapping authority certificate.
    const TERRAIN_FIXTURE_ATLAS: &str = "clusters/terrain/atlases/fixture.ktx2";

    fn terrain_snapshot_refresh_terrain(root: &Path, entries: &mut [ExtensionEntry]) {
        let overlay = overlay_of(root);
        let connection = overlay_connection(root);
        entries[0].sha256 = recompute(&connection, &overlay, &entries[0]).unwrap();
        let path = overlay.join("cluster-terrain-manifest.json");
        let mut manifest = json_file(&path).unwrap();
        manifest["requires"] = json!([{"id":CLUSTERS,"version":1,"sha256":entries[0].sha256}]);
        let inputs = json!({"recipe":1,"builder":BUILDER,
            "cell_cache":manifest["cell_cache_sha256"],"blocks":manifest["blocks"]});
        manifest["input_identity"] = json!(hex(&Sha256::digest(canonical_json(&inputs))));
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        entries[1].sha256 = recompute(&connection, &overlay, &entries[1]).unwrap();
        drop(connection);
        save(root, entries);
    }

    fn terrain_snapshot_terrain_fixture() -> (tempfile::TempDir, Vec<ExtensionEntry>, Vec<u8>) {
        let (temp, mut entries) = fixture();
        let root = temp.path();
        let overlay = overlay_of(root);
        let dds = dummy_content::dds::generate(
            &dummy_content::dds::Spec::new(dummy_content::dds::Format::X8R8G8B8, 4, 4)
                .with_mip_levels(3),
            &mut dummy_content::rng::Rng::new(0),
        )
        .unwrap();
        let atlas = converter::texture::TextureConverter::convert(
            &dds,
            converter::texture::TextureEncoding::ColorSrgb,
        )
        .unwrap();
        fs::create_dir_all(overlay.join("clusters/terrain/atlases")).unwrap();
        fs::write(overlay.join(TERRAIN_FIXTURE_ATLAS), &atlas).unwrap();
        let connection = overlay_connection(root);
        connection
            .execute_batch(
                "DROP TABLE cluster_terrain; DROP TABLE cluster_terrain_heights;
             DROP TABLE cluster_terrain_build;
             CREATE TABLE cluster_terrain (mesh_hash TEXT, primitive_index INTEGER,
                worldspace_id INTEGER, anchor_x INTEGER, anchor_y INTEGER,
                side_cells INTEGER, instance_path TEXT, atlas_path TEXT, source_cells TEXT);
             CREATE TABLE cluster_terrain_heights (worldspace_id INTEGER,
                grid_x INTEGER, grid_y INTEGER, heights BLOB);
             CREATE TABLE cluster_terrain_build (version INTEGER, cell_cache_hash TEXT);",
            )
            .unwrap();
        connection.execute(
            "INSERT INTO cluster_terrain VALUES ('terrain-snapshot-fixture',0,7,0,0,1,'clusters/fixture.mci',?1,'[[0,0]]')",
            [TERRAIN_FIXTURE_ATLAS],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO cluster_terrain_heights VALUES (7,0,0,?1)",
                [vec![0u8; 33 * 33 * 4]],
            )
            .unwrap();
        let cache = digest_file(&root.join("cell_cache.rkyv")).unwrap();
        connection
            .execute("INSERT INTO cluster_terrain_build VALUES (1,?1)", [&cache])
            .unwrap();
        drop(connection);
        let path = overlay.join("cluster-terrain-manifest.json");
        let mut manifest = json_file(&path).unwrap();
        manifest["blocks"] = json!([{"worldspace":7,"anchor":[0,0],"side_cells":1,
            "input_key":"terrain-snapshot-fixture","source_cells":[[0,0]],"atlas_path":TERRAIN_FIXTURE_ATLAS,
            "atlas_sha256":hex(&Sha256::digest(&atlas)),"atlas_side":4}]);
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        terrain_snapshot_refresh_terrain(root, &mut entries);
        (temp, entries, atlas)
    }

    fn terrain_snapshot_load_terrain(
        envelope: &ClusterEnvelope,
    ) -> Result<
        (
            super::super::terrain::ClusterTerrain,
            bevy::prelude::Assets<bevy::prelude::Image>,
        ),
        String,
    > {
        let mut images = bevy::prelude::Assets::default();
        let terrain = super::super::terrain::ClusterTerrain::load_certified(
            envelope,
            &mut images,
            &mut bevy::prelude::Assets::default(),
        )?;
        Ok((terrain, images))
    }

    #[test]
    fn terrain_snapshot_certified_atlas_bytes_and_cache_identity_survive_disk_replacement_or_removal()
     {
        for mutation in ["replace", "remove"] {
            let (temp, _, original) = terrain_snapshot_terrain_fixture();
            let root = temp.path();
            let envelope = open_fixture_overlay(root, true).unwrap();
            // Model a writer ignoring the retained source locks. The receipt must
            // continue to authorize only the original launch snapshot.
            for path in [
                root.join("cell_cache.rkyv"),
                envelope.overlay.join(TERRAIN_FIXTURE_ATLAS),
            ] {
                if mutation == "replace" {
                    fs::write(path, b"changed after certification").unwrap();
                } else {
                    fs::remove_file(path).unwrap();
                }
            }
            let (terrain, images) = terrain_snapshot_load_terrain(&envelope).unwrap();
            assert_eq!(terrain.blocks.len(), 1, "{mutation}");
            let (_, actual) = images.iter().next().unwrap();
            let expected = bevy::prelude::Image::from_buffer(
                &original,
                bevy::image::ImageType::Extension("ktx2"),
                bevy::image::CompressedImageFormats::BC,
                true,
                bevy::image::ImageSampler::Default,
                bevy::asset::RenderAssetUsages::RENDER_WORLD,
            )
            .unwrap();
            assert_eq!(
                actual.data, expected.data,
                "{mutation}: exact certified image bytes"
            );
            assert_eq!(
                actual.texture_descriptor.size,
                expected.texture_descriptor.size
            );
            assert_eq!(
                actual.texture_descriptor.format,
                expected.texture_descriptor.format
            );
            assert_eq!(
                actual.texture_descriptor.mip_level_count,
                expected.texture_descriptor.mip_level_count
            );
            assert_eq!(
                terrain
                    .canonical_heights(7, bevy::math::IVec2::ZERO)
                    .unwrap(),
                &[0.0; 33 * 33]
            );
        }
    }

    #[test]
    fn terrain_snapshot_missing_atlas_snapshot_never_falls_back_to_good_disk_bytes() {
        let (temp, _, _) = terrain_snapshot_terrain_fixture();
        let envelope = open_fixture_overlay(temp.path(), true).unwrap();
        envelope.payloads.lock().unwrap().clear();
        let error = terrain_snapshot_load_terrain(&envelope)
            .err()
            .expect("missing snapshot must refuse terrain");
        assert!(error.contains("not certified"), "{error}");
    }

    #[test]
    fn terrain_snapshot_atlas_snapshot_digest_must_match_the_launch_receipt() {
        let (temp, _, _) = terrain_snapshot_terrain_fixture();
        let mut envelope = open_fixture_overlay(temp.path(), true).unwrap();
        envelope
            .payload_digests
            .insert(TERRAIN_FIXTURE_ATLAS.into(), [0; 32]);
        let error = terrain_snapshot_load_terrain(&envelope)
            .err()
            .expect("changed digest must refuse terrain");
        assert!(error.contains("receipt differs"), "{error}");
    }

    #[test]
    fn terrain_snapshot_terrain_requires_a_launch_cache_receipt_even_when_disk_is_valid() {
        let (temp, _, _) = terrain_snapshot_terrain_fixture();
        let envelope = open_fixture_overlay(temp.path(), false).unwrap();
        let error = terrain_snapshot_load_terrain(&envelope)
            .err()
            .expect("terrain was not certified");
        assert!(error.contains("not certified"), "{error}");
    }

    #[test]
    fn terrain_snapshot_terrain_build_hash_must_match_the_checked_launch_cache() {
        let (temp, mut entries, _) = terrain_snapshot_terrain_fixture();
        overlay_connection(temp.path())
            .execute(
                "UPDATE cluster_terrain_build SET cell_cache_hash=?1",
                ["00".repeat(32)],
            )
            .unwrap();
        terrain_snapshot_refresh_terrain(temp.path(), &mut entries);
        let envelope = open_fixture_overlay(temp.path(), true).unwrap();
        let error = terrain_snapshot_load_terrain(&envelope)
            .err()
            .expect("wrong terrain build must refuse");
        assert!(error.contains("was built from cell cache"), "{error}");
    }

    #[test]
    fn terrain_snapshot_uncertified_atlas_and_cache_corruption_or_absence_still_refuse() {
        for target in ["atlas", "cache"] {
            for mutation in ["replace", "remove"] {
                let (temp, _, _) = terrain_snapshot_terrain_fixture();
                let path = if target == "atlas" {
                    overlay_of(temp.path()).join(TERRAIN_FIXTURE_ATLAS)
                } else {
                    temp.path().join("cell_cache.rkyv")
                };
                if mutation == "replace" {
                    fs::write(path, b"corrupt before certification").unwrap();
                } else {
                    fs::remove_file(path).unwrap();
                }
                assert!(
                    open_fixture_overlay(temp.path(), true).is_err(),
                    "{target}: {mutation}"
                );
            }
        }
    }

    #[test]
    fn terrain_snapshot_rehashed_base_does_not_authorize_a_stale_manifest_atlas_digest() {
        let (temp, mut entries, _) = terrain_snapshot_terrain_fixture();
        fs::write(
            overlay_of(temp.path()).join(TERRAIN_FIXTURE_ATLAS),
            b"new atlas bytes",
        )
        .unwrap();
        // Rehash both extensions/dependencies while preserving the old atlas recipe digest.
        terrain_snapshot_refresh_terrain(temp.path(), &mut entries);
        let error = open_fixture_overlay(temp.path(), true)
            .err()
            .expect("stale atlas digest");
        assert!(
            error.to_string().contains("atlas digest differs"),
            "{error:#}"
        );
    }

    #[test]
    fn precheck_accepts_integrity_without_claiming_payload_or_mapping_validity() {
        let (temp, _) = fixture();
        let envelope = open_fixture_overlay(temp.path(), true).unwrap();
        assert!(envelope.terrain.is_some());
        assert_eq!(envelope.core_identity, "core");
        assert!(
            envelope
                .overlay
                .ends_with(format!("clusters-{}", fixture_settings(1024).settings_id()))
        );
    }

    #[test]
    fn base_mutation_and_rehashed_base_with_stale_terrain_are_refused() {
        let (temp, mut entries) = fixture();
        let root = temp.path();
        let overlay = overlay_of(root);
        fs::write(overlay.join("clusters/fixture.bin"), b"changed").unwrap();
        assert!(open_fixture_overlay(root, false).is_err());
        let connection = overlay_connection(root);
        entries[0].sha256 = recompute(&connection, &overlay, &entries[0]).unwrap();
        save(root, &entries);
        drop(connection);
        assert!(open_fixture_overlay(root, false).is_ok());
        assert!(open_fixture_overlay(root, true).is_err());
    }

    #[test]
    fn missing_owned_folder_and_stale_cache_are_refused() {
        let (temp, _) = fixture();
        let overlay = overlay_of(temp.path());
        // The cell cache is a core file: it is read from the base.
        fs::write(temp.path().join("cell_cache.rkyv"), b"changed").unwrap();
        assert!(open_fixture_overlay(temp.path(), true).is_err());
        fs::remove_file(overlay.join("clusters/fixture.bin")).unwrap();
        fs::remove_dir(overlay.join("clusters")).unwrap();
        assert!(open_fixture_overlay(temp.path(), false).is_err());
    }

    #[test]
    fn recorded_digests_stand_in_for_reading_only_when_they_match() {
        let (temp, entries) = fixture();
        let root = temp.path();
        let overlay = overlay_of(root);
        let connection = overlay_connection(root);
        let path = "clusters/fixture.bin";
        let digest = digest_file(&overlay.join(path)).unwrap();
        let recorded = HashMap::from([(path.to_owned(), digest)]);
        assert_eq!(
            recompute_recorded(&connection, &overlay, &entries[0], &recorded).unwrap(),
            entries[0].sha256
        );
        // The recorded digest is taken as is: the file itself is not read.
        fs::write(overlay.join(path), b"changed").unwrap();
        assert_eq!(
            recompute_recorded(&connection, &overlay, &entries[0], &recorded).unwrap(),
            entries[0].sha256
        );
        // A record that is not what the producer hashed changes the extension hash.
        let wrong = HashMap::from([(path.to_owned(), "0".repeat(64))]);
        assert_ne!(
            recompute_recorded(&connection, &overlay, &entries[0], &wrong).unwrap(),
            entries[0].sha256
        );
    }

    #[test]
    fn an_empty_owned_folder_and_its_absence_have_the_same_extension_hash() {
        let (temp, mut entries) = fixture();
        let root = temp.path();
        let overlay = overlay_of(root);
        fs::remove_file(overlay.join("clusters/fixture.bin")).unwrap();
        let connection = overlay_connection(root);
        let empty_hash = recompute(&connection, &overlay, &entries[0]).unwrap();
        entries[0].sha256 = empty_hash.clone();
        save(root, &entries);
        assert!(open_fixture_overlay(root, false).is_ok());
        fs::remove_dir(overlay.join("clusters")).unwrap();
        assert_eq!(
            recompute(&connection, &overlay, &entries[0]).unwrap(),
            empty_hash
        );
        assert_eq!(converter_hash(&overlay, &entries[0]), empty_hash);
        assert!(open_fixture_overlay(root, false).is_ok());
        // Folder absence covers no bytes; an explicitly owned file remains mandatory.
        fs::remove_file(overlay.join("cluster-manifest.json")).unwrap();
        assert!(open_fixture_overlay(root, false).is_err());
    }

    #[test]
    fn unsafe_paths_and_wrong_ownership_are_refused() {
        for path in [
            "../file",
            "/file",
            "a/../file",
            "a/./file",
            "a\\file",
            "C:file",
            "a//file",
            "",
        ] {
            assert!(safe_relative(path).is_err(), "{path}");
        }
        let (temp, mut entries) = fixture();
        entries[0]
            .owns
            .gltf_extensions
            .push("OPEN_SKYRIM_material".into());
        save(temp.path(), &entries);
        assert!(open_fixture_overlay(temp.path(), false).is_err());
    }

    #[test]
    fn stale_core_and_overlapping_folder_claims_are_refused() {
        let (temp, mut entries) = fixture();
        entries[0].built_from = "another-core".into();
        save(temp.path(), &entries);
        assert!(open_fixture_overlay(temp.path(), false).is_err());
        entries[0].built_from = "core".into();
        // The terrain entry claims a folder inside the base entry's `clusters/`.
        entries[1].owns.files.push("clusters/sub/".into());
        save(temp.path(), &entries);
        let error = format!(
            "{:#}",
            open_fixture_overlay(temp.path(), false).err().unwrap()
        );
        assert!(
            error.contains(concat!(
                "overlay.json has conflicting extension ownership: ",
                "file clusters/sub/ of mudcrab.cluster_terrain overlaps ",
                "file clusters/ of mudcrab.clusters"
            )),
            "{error}"
        );
    }

    #[test]
    fn other_extension_cannot_alias_cluster_file_or_sql_ownership() {
        let (temp, entries) = fixture();
        for path in [
            "clusters\\fixture.bin",
            "clusters/./fixture.bin",
            "clusters/../clusters/fixture.bin",
            "CLUSTERS/fixture.bin",
            "clusters//fixture.bin",
            "clusters//",
        ] {
            let mut claims = entries.clone();
            let mut other = claims[1].clone();
            other.id = "other.extension".into();
            other.owns = ExtensionOwns {
                files: vec![path.into()],
                ..Default::default()
            };
            claims.push(other);
            assert!(check_file_overlap(claims.iter()).is_err(), "{path}");
            // Inside the overlay certificate an unknown extension is refused outright.
            save(temp.path(), &claims);
            assert!(open_fixture_overlay(temp.path(), false).is_err(), "{path}");
        }
        // A base-manifest extension claiming an overlay table name, however spelled, is refused:
        // SQLite object names are one namespace across the base and the overlay.
        let mut other = entries[1].clone();
        other.id = "other.extension".into();
        other.owns = ExtensionOwns {
            tables: vec!["CLUSTER_MESHES".into()],
            ..Default::default()
        };
        let mut claims = entries.clone();
        claims.push(other.clone());
        assert!(check_file_overlap(claims.iter()).is_err());
        set_base_extensions(temp.path(), &[other], &entries);
        let error = open_fixture_overlay(temp.path(), false).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("overlapping SQLite object ownership"),
            "{error:#}"
        );

        let mut claims = entries;
        claims[0].owns.indexes.push("cluster_map_index".into());
        let mut other = claims[1].clone();
        other.id = "other.extension".into();
        other.owns = ExtensionOwns {
            indexes: vec!["CLUSTER_MAP_INDEX".into()],
            ..Default::default()
        };
        claims.push(other);
        assert!(check_file_overlap(claims.iter()).is_err());
    }

    #[test]
    fn the_base_manifests_own_overlapping_claims_are_still_refused() {
        // Folder claims in the base manifest are compared among themselves, as they always were.
        let (temp, entries) = fixture();
        let mut first = entries[1].clone();
        first.id = "other.first".into();
        first.owns = ExtensionOwns {
            files: vec!["shared/".into()],
            ..Default::default()
        };
        let mut second = first.clone();
        second.id = "other.second".into();
        second.owns.files = vec!["shared/inner/".into()];
        set_base_extensions(temp.path(), &[first.clone(), second], &entries);
        let base_extensions = DataExtensions::load(temp.path()).unwrap();
        assert_eq!(
            ownership_problems(base_extensions.entries()),
            ["file shared/inner/ of other.second overlaps file shared/ of other.first"]
        );
        let error = format!(
            "{:#}",
            open_fixture_overlay(temp.path(), false).err().unwrap()
        );
        assert_eq!(error, "extension ownership conflicts");
        // The same claims without overlap open: only the overlap refused.
        let mut second = first.clone();
        second.id = "other.second".into();
        second.owns.files = vec!["elsewhere/".into()];
        set_base_extensions(temp.path(), &[first, second], &entries);
        assert!(open_fixture_overlay(temp.path(), false).is_ok());
    }

    #[test]
    fn a_base_table_named_like_an_overlay_table_is_refused_not_preferred() {
        let (temp, entries) = fixture();
        let root = temp.path();
        // A base table with a certified overlay table's name is refused, not preferred.
        Connection::open(root.join("skyrim_world.db"))
            .unwrap()
            .execute_batch("CREATE TABLE cluster_meshes(x INTEGER);")
            .unwrap();
        save(root, &entries);
        let error = format!("{:#}", open_fixture_overlay(root, false).err().unwrap());
        assert!(error.contains("old full-copy cluster dataset"), "{error}");
    }

    #[test]
    fn an_overlay_of_another_conversion_or_manifest_is_refused_until_rebuilt() {
        let (temp, entries) = fixture();
        let root = temp.path();
        assert!(open_fixture_overlay(root, true).is_ok());
        // Manifest bytes of the base changed.
        let manifest = root.join("conversion-manifest.json");
        let mut bytes = fs::read(&manifest).unwrap();
        bytes.push(b'\n');
        fs::write(&manifest, bytes).unwrap();
        let error = format!("{:#}", open_fixture_overlay(root, false).err().unwrap());
        assert!(
            error.contains("built for another conversion of this base")
                && error.contains("converter derive-clusters"),
            "{error}"
        );
        // Rebuilt for the manifest as it is now: accepted again (only the binding refused).
        save(root, &entries);
        assert!(open_fixture_overlay(root, true).is_ok());
        // Core identity changed.
        Connection::open(root.join("skyrim_world.db"))
            .unwrap()
            .execute("UPDATE lod_build SET build_identity = 'other-core'", [])
            .unwrap();
        let error = format!("{:#}", open_fixture_overlay(root, false).err().unwrap());
        assert!(error.contains("another conversion of this base"), "{error}");
        Connection::open(root.join("skyrim_world.db"))
            .unwrap()
            .execute("UPDATE lod_build SET build_identity = 'core'", [])
            .unwrap();
        assert!(open_fixture_overlay(root, true).is_ok());
    }

    #[test]
    fn old_full_copy_datasets_are_refused_with_the_rebuild_command() {
        let (temp, entries) = fixture();
        let root = temp.path();
        let base_db = root.join("skyrim_world.db");
        // Cluster tables inside the base database.
        Connection::open(&base_db)
            .unwrap()
            .execute_batch("CREATE TABLE cluster_chunks(x INTEGER);")
            .unwrap();
        save(root, &entries);
        let error = format!("{:#}", open_fixture_overlay(root, false).err().unwrap());
        assert!(
            error.contains("old full-copy cluster dataset")
                && error.contains("converter derive-clusters"),
            "{error}"
        );
        Connection::open(&base_db)
            .unwrap()
            .execute_batch("DROP TABLE cluster_chunks;")
            .unwrap();
        assert!(open_fixture_overlay(root, false).is_ok());
        // Cluster extension entries in the base manifest.
        set_base_extensions(root, &entries, &entries);
        let error = format!("{:#}", open_fixture_overlay(root, false).err().unwrap());
        assert!(error.contains("old full-copy cluster dataset"), "{error}");
    }

    #[test]
    fn two_overlays_need_a_label_and_each_opens_for_its_own_selector() {
        let (temp, _) = fixture();
        let root = temp.path();
        let first = overlay_of(root);
        assert_eq!(resolve_overlay(root, "").unwrap(), first);
        let second = overlay_dir(root, &fixture_settings(2048).settings_id());
        copy_directory(&first, &second);
        let mut metadata = shared::cluster_overlay::read_metadata(&first).unwrap();
        metadata.label = "dense".into();
        metadata.build_settings = fixture_settings(2048);
        metadata.settings_id = metadata.build_settings.settings_id();
        fs::write(
            second.join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        assert_eq!(list_overlays(root).unwrap().len(), 2);
        let error = format!("{:#}", resolve_overlay(root, "").unwrap_err());
        assert!(error.contains("2 cluster overlays"), "{error}");
        assert_eq!(resolve_overlay(root, "fixture").unwrap(), first);
        assert_eq!(resolve_overlay(root, "dense").unwrap(), second);
        for overlay in [&first, &second] {
            assert!(open(root, overlay, false).is_ok());
        }
    }

    #[test]
    fn the_certification_connection_cannot_write_either_database() {
        let (temp, _) = fixture();
        let envelope = open_fixture_overlay(temp.path(), false).unwrap();
        let connection = envelope.connection().unwrap();
        assert!(
            connection
                .execute("INSERT INTO clusters.cluster_build VALUES ('x')", [])
                .is_err()
        );
        assert!(
            connection
                .execute("UPDATE lod_build SET build_identity = 'x'", [])
                .is_err()
        );
        assert!(
            connection
                .execute("CREATE TABLE clusters.extra(x)", [])
                .is_err()
        );
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM cluster_build", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn live_sidecars_of_the_base_or_the_overlay_refuse_the_open_untouched() {
        let (temp, _) = fixture();
        let root = temp.path();
        for sidecar in [
            root.join("skyrim_world.db-wal"),
            root.join("skyrim_world.db-shm"),
            root.join("skyrim_world.db-journal"),
            overlay_of(root).join("clusters.db-wal"),
            overlay_of(root).join("clusters.db-shm"),
            overlay_of(root).join("clusters.db-journal"),
        ] {
            fs::write(&sidecar, b"live").unwrap();
            let error = format!("{:#}", open_fixture_overlay(root, false).err().unwrap());
            assert!(
                error.contains("not sealed"),
                "{} {error}",
                sidecar.display()
            );
            assert_eq!(fs::read(&sidecar).unwrap(), b"live");
            fs::remove_file(&sidecar).unwrap();
        }
        assert!(open_fixture_overlay(root, false).is_ok());
    }

    #[test]
    fn envelope_retains_both_shared_locks_until_dropped() {
        let (temp, _) = fixture();
        let overlay = overlay_of(temp.path());
        let envelope = open_fixture_overlay(temp.path(), true).unwrap();
        for held in [temp.path(), overlay.as_path()] {
            let error = AssetLock::acquire_exclusive(held).unwrap_err();
            assert!(error.is_held(), "expected writer exclusion, got {error:?}");
        }
        drop(envelope);
        for held in [temp.path(), overlay.as_path()] {
            let writer = AssetLock::acquire_exclusive(held).unwrap();
            drop(writer);
        }
    }

    #[test]
    fn later_connections_reuse_the_launch_receipt_and_refuse_a_swapped_core() {
        let (temp, _) = fixture();
        let envelope = open_fixture_overlay(temp.path(), true).unwrap();
        // Simulate changes that an uncooperative process makes despite the retained reader lock.
        // A later world connection must not re-read manifests, but must reject a changed core.
        fs::write(temp.path().join("conversion-manifest.json"), b"not JSON").unwrap();
        assert!(envelope.connection().is_ok());
        let connection = Connection::open(temp.path().join("skyrim_world.db")).unwrap();
        connection
            .execute("UPDATE lod_build SET build_identity = 'swapped-core'", [])
            .unwrap();
        drop(connection);
        let error = format!("{:#}", envelope.connection().unwrap_err());
        assert!(error.contains("another conversion of this base"), "{error}");
    }

    #[test]
    fn a_foreign_overlay_with_a_corrupt_database_reports_binding_first() {
        let (temp, _) = fixture();
        let overlay = overlay_of(temp.path());
        let mut metadata = shared::cluster_overlay::read_metadata(&overlay).unwrap();
        metadata.base_core_identity = "foreign-core".into();
        fs::write(
            overlay.join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        fs::write(overlay.join(OVERLAY_DATABASE_FILE), b"not a database").unwrap();
        let error = format!(
            "{:#}",
            open_fixture_overlay(temp.path(), true).err().unwrap()
        );
        assert!(error.contains("another conversion of this base"), "{error}");
        assert!(!error.contains("not a database"), "{error}");
    }
}

#[cfg(test)]
mod generic_inventory_tests {
    use super::*;
    use shared::extensions::ExtensionOwns;
    use std::collections::BTreeMap;

    fn owner(root: &Path) {
        fs::write(root.join("OWNER.txt"), "owner: envelope fixture\n").unwrap();
    }

    fn owned_tempdir() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        owner(directory.path());
        directory
    }

    fn owned_dir(path: &Path) {
        fs::create_dir(path).unwrap();
        owner(path);
    }

    fn dataset_connection(root: &Path) -> Connection {
        Connection::open(root.join("skyrim_world.db")).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, Vec<ExtensionEntry>) {
        let directory = owned_tempdir();
        let root = directory.path();
        owned_dir(&root.join("assets"));
        fs::write(root.join("assets/fixture.bin"), b"generic payload bytes").unwrap();
        fs::write(root.join("manifest.json"), b"generic inventory manifest").unwrap();
        let connection = dataset_connection(root);
        connection
            .execute_batch(
                "CREATE TABLE inventory(id INTEGER PRIMARY KEY, path TEXT NOT NULL);
             INSERT INTO inventory VALUES (2, 'second'), (1, 'first');
             CREATE INDEX inventory_path ON inventory(path);",
            )
            .unwrap();
        let mut entry = ExtensionEntry {
            id: "test.inventory".into(),
            version: 1,
            owns: ExtensionOwns {
                tables: vec!["inventory".into()],
                indexes: vec!["inventory_path".into()],
                files: vec!["manifest.json".into(), "assets/".into()],
                ..Default::default()
            },
            sha256: String::new(),
            built_from: "generic-core".into(),
            stats: BTreeMap::new(),
        };
        entry.sha256 = converter::extensions::recompute_hash(root, &entry).unwrap();
        assert_eq!(recompute(&connection, root, &entry).unwrap(), entry.sha256);
        (directory, vec![entry])
    }
    #[test]
    fn overlap_matches_original_digest_with_recorded_and_fresh_files() {
        let (directory, entries) = fixture();
        let root = directory.path();
        let overlay = root.to_path_buf();
        let connection = dataset_connection(root);
        let recorded = HashMap::from([(
            "assets/fixture.bin".to_owned(),
            digest_file(&overlay.join("assets/fixture.bin")).unwrap(),
        )]);
        let expected = converter::extensions::recompute_hash(&overlay, &entries[0]).unwrap();
        for parallelism in [1, 2, 3, 8] {
            for supplied in [&HashMap::new(), &recorded] {
                assert_eq!(
                    recompute_recorded_with_parallelism(
                        &connection,
                        &overlay,
                        &entries[0],
                        supplied,
                        Ok(parallelism),
                    )
                    .unwrap(),
                    expected
                );
            }
        }
    }

    fn serial_and_overlap_error(
        connection: &Connection,
        root: &Path,
        entry: &ExtensionEntry,
    ) -> String {
        let serial =
            recompute_recorded_with_parallelism(connection, root, entry, &HashMap::new(), Ok(1))
                .unwrap_err()
                .to_string();
        let parallel =
            recompute_recorded_with_parallelism(connection, root, entry, &HashMap::new(), Ok(8))
                .unwrap_err()
                .to_string();
        assert_eq!(parallel, serial);
        serial
    }

    #[test]
    fn overlap_preserves_table_error_before_a_missing_owned_file() {
        let (directory, mut entries) = fixture();
        let overlay = directory.path().to_path_buf();
        let connection = dataset_connection(directory.path());
        entries[0].owns.tables.push("missing_table".into());
        fs::remove_file(overlay.join("manifest.json")).unwrap();
        assert!(
            serial_and_overlap_error(&connection, &overlay, &entries[0]).contains("missing_table")
        );
    }

    #[test]
    fn overlap_preserves_index_error_before_an_unsafe_owned_path() {
        let (directory, mut entries) = fixture();
        let overlay = directory.path().to_path_buf();
        let connection = dataset_connection(directory.path());
        entries[0].owns.indexes.push("missing_index".into());
        entries[0].owns.files.push("../unsafe".into());
        assert!(
            serial_and_overlap_error(&connection, &overlay, &entries[0]).contains("missing_index")
        );
    }

    #[test]
    fn overlap_keeps_the_first_owned_path_error_when_two_files_are_invalid() {
        let (directory, mut entries) = fixture();
        let overlay = directory.path().to_path_buf();
        let connection = dataset_connection(directory.path());
        let first = overlay.join("a-invalid-file");
        fs::create_dir(&first).unwrap();
        fs::write(
            first.join("OWNER.txt"),
            "owner: envelope overlap regression fixture\n",
        )
        .unwrap();
        entries[0].owns.files = vec!["a-invalid-file".into(), "z-missing-file".into()];
        assert!(
            serial_and_overlap_error(&connection, &overlay, &entries[0]).contains("a-invalid-file")
        );
    }

    #[test]
    fn parallel_owned_hashes_match_serial_bytes_and_first_refusal() {
        let directory = owned_tempdir();
        let root = directory.path();
        let files: Vec<_> = (0..19).map(|index| format!("file-{index:02}")).collect();
        for (index, file) in files.iter().enumerate() {
            fs::write(root.join(file), vec![index as u8; index * 9173]).unwrap();
        }
        let serial = |files: &[String]| {
            files
                .iter()
                .map(|file| {
                    let path = root.join(file);
                    digest_file(&path).map(|digest| (digest, fs::metadata(path).unwrap().len()))
                })
                .collect::<Vec<_>>()
        };
        let expected: Vec<_> = serial(&files).into_iter().map(Result::unwrap).collect();
        for workers in [1, 2, 8, 32] {
            let found: Vec<_> = hash_owned_files(root, &files, workers)
                .unwrap()
                .into_iter()
                .map(Result::unwrap)
                .collect();
            assert_eq!(found, expected);
        }
        // Two different failures must still report the earlier sorted path.
        fs::remove_file(root.join(&files[3])).unwrap();
        fs::create_dir(root.join(&files[3])).unwrap();
        owner(&root.join(&files[3]));
        fs::remove_file(root.join(&files[9])).unwrap();
        let expected = serial(&files)
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap_err();
        for workers in [1, 2, 8, 32] {
            let error = hash_owned_files(root, &files, workers)
                .unwrap()
                .into_iter()
                .collect::<Result<Vec<_>>>()
                .unwrap_err();
            assert_eq!(error.to_string(), expected.to_string());
        }
        assert!(hash_owned_files(root, &[], 8).unwrap().is_empty());
    }

    #[test]
    fn recorded_digests_stand_in_for_reading_only_when_they_match() {
        let (directory, entries) = fixture();
        let root = directory.path();
        let connection = dataset_connection(root);
        let path = "assets/fixture.bin";
        let recorded = HashMap::from([(path.to_owned(), digest_file(&root.join(path)).unwrap())]);
        assert_eq!(
            recompute_recorded(&connection, root, &entries[0], &recorded).unwrap(),
            entries[0].sha256
        );
        fs::write(root.join(path), b"changed").unwrap();
        assert_eq!(
            recompute_recorded(&connection, root, &entries[0], &recorded).unwrap(),
            entries[0].sha256
        );
        assert_ne!(
            recompute(&connection, root, &entries[0]).unwrap(),
            entries[0].sha256
        );
        let wrong = HashMap::from([(path.to_owned(), "0".repeat(64))]);
        assert_ne!(
            recompute_recorded(&connection, root, &entries[0], &wrong).unwrap(),
            entries[0].sha256
        );
    }

    #[test]
    fn an_empty_owned_folder_and_its_absence_have_the_same_extension_hash() {
        let (directory, mut entries) = fixture();
        let root = directory.path();
        let connection = dataset_connection(root);
        entries[0].owns.files = vec!["empty/".into(), "manifest.json".into()];
        let absent_hash = recompute(&connection, root, &entries[0]).unwrap();
        owned_dir(&root.join("empty"));
        // Every created directory receives OWNER.txt. Move its marker to the owned test
        // root for the deliberate empty-directory mutation; otherwise it is not empty.
        fs::rename(root.join("empty/OWNER.txt"), root.join("empty-OWNER.txt")).unwrap();
        let empty_result = recompute(&connection, root, &entries[0]);
        // Restore the directory's marker before any assertions can unwind.
        fs::rename(root.join("empty-OWNER.txt"), root.join("empty/OWNER.txt")).unwrap();
        let empty_hash = empty_result.unwrap();
        assert_eq!(empty_hash, absent_hash);
        fs::rename(root.join("empty/OWNER.txt"), root.join("empty-OWNER.txt")).unwrap();
        let producer = converter::extensions::recompute_hash(root, &entries[0]);
        fs::rename(root.join("empty-OWNER.txt"), root.join("empty/OWNER.txt")).unwrap();
        assert_eq!(producer.unwrap(), empty_hash);
        entries[0].owns.files = vec!["missing-mandatory-file".into()];
        assert!(recompute(&connection, root, &entries[0]).is_err());
    }

    #[test]
    fn unsafe_owned_paths_are_refused() {
        let (directory, mut entries) = fixture();
        let connection = dataset_connection(directory.path());
        for path in [
            "../file",
            "/file",
            "a/../file",
            "a/./file",
            "a\\file",
            "C:file",
            "a//file",
            "",
            "a\0file",
        ] {
            assert!(safe_relative(path).is_err(), "{path:?}");
            entries[0].owns.files = vec![path.into()];
            assert!(
                recompute(&connection, directory.path(), &entries[0]).is_err(),
                "{path:?}"
            );
        }
        assert!(check_path(&directory.path().join("assets/../manifest.json")).is_err());
    }

    #[test]
    fn nonregular_explicit_files_and_folder_claims_are_refused() {
        let (directory, mut entries) = fixture();
        let connection = dataset_connection(directory.path());
        entries[0].owns.files = vec!["assets".into()];
        assert!(
            serial_and_overlap_error(&connection, directory.path(), &entries[0])
                .contains("not a regular file")
        );
        entries[0].owns.files = vec!["manifest.json/".into()];
        assert!(
            serial_and_overlap_error(&connection, directory.path(), &entries[0])
                .contains("owned folder missing")
        );
    }

    #[test]
    fn worker_count_error_remains_after_tables_indexes_and_file_walk() {
        let (directory, mut entries) = fixture();
        let connection = dataset_connection(directory.path());
        let run = |entry: &ExtensionEntry| {
            recompute_recorded_with_parallelism(
                &connection,
                directory.path(),
                entry,
                &HashMap::new(),
                Err(std::io::Error::other("worker-count lookup failed")),
            )
            .unwrap_err()
            .to_string()
        };
        assert!(run(&entries[0]).contains("worker-count lookup failed"));
        entries[0].owns.files = vec!["../unsafe".into()];
        assert!(run(&entries[0]).contains("unsafe owned path"));
        entries[0].owns.indexes.push("missing_index".into());
        assert!(run(&entries[0]).contains("missing_index"));
        entries[0].owns.tables.push("missing_table".into());
        assert!(run(&entries[0]).contains("missing_table"));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn linked_files_folders_and_ancestors_are_refused_even_when_recorded_or_absent() {
        let (directory, mut entries) = fixture();
        let root = directory.path();
        let connection = dataset_connection(root);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("assets/fixture.bin"), root.join("linked-file"))
                .unwrap();
            std::os::unix::fs::symlink(root.join("assets"), root.join("linked-dir")).unwrap();
        }
        #[cfg(windows)]
        {
            // No permission-based skip: Windows requires symlink rights or Developer Mode.
            std::os::windows::fs::symlink_file(
                root.join("assets/fixture.bin"),
                root.join("linked-file"),
            )
            .unwrap();
            std::os::windows::fs::symlink_dir(root.join("assets"), root.join("linked-dir"))
                .unwrap();
        }
        for path in [
            "linked-file",
            "linked-dir/",
            "linked-dir/fixture.bin",
            "linked-dir/absent/",
        ] {
            entries[0].owns.files = vec![path.into()];
            let recorded = HashMap::from([(path.to_owned(), "0".repeat(64))]);
            for parallelism in [1, 8] {
                let error = recompute_recorded_with_parallelism(
                    &connection,
                    root,
                    &entries[0],
                    &recorded,
                    Ok(parallelism),
                )
                .unwrap_err();
                assert!(
                    error.to_string().contains("link/reparse point"),
                    "{error:#}"
                );
            }
        }
        entries[0].owns.files = vec!["assets/".into()];
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("manifest.json"), root.join("assets/nested-link"))
            .unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(
            root.join("manifest.json"),
            root.join("assets/nested-link"),
        )
        .unwrap();
        assert!(
            serial_and_overlap_error(&connection, root, &entries[0]).contains("link/reparse point")
        );
        #[cfg(windows)]
        {
            assert!(is_reparse(
                &fs::symlink_metadata(root.join("linked-file")).unwrap()
            ));
            assert!(is_reparse(
                &fs::symlink_metadata(root.join("linked-dir")).unwrap()
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn nonregular_walk_entry_is_refused() {
        let (directory, entries) = fixture();
        let connection = dataset_connection(directory.path());
        let _socket =
            std::os::unix::net::UnixListener::bind(directory.path().join("assets/socket")).unwrap();
        assert!(
            serial_and_overlap_error(&connection, directory.path(), &entries[0])
                .contains("non-regular owned file")
        );
    }

    #[test]
    fn declared_table_index_and_path_order_determine_first_refusal() {
        let (directory, mut entries) = fixture();
        let root = directory.path();
        let connection = dataset_connection(root);
        entries[0].owns.tables = vec!["z-first-table".into(), "a-second-table".into()];
        entries[0].owns.indexes = vec!["z-first-index".into(), "a-second-index".into()];
        entries[0].owns.files = vec!["../unsafe".into()];
        assert!(serial_and_overlap_error(&connection, root, &entries[0]).contains("z-first-table"));
        entries[0].owns.tables = vec!["inventory".into()];
        assert!(serial_and_overlap_error(&connection, root, &entries[0]).contains("z-first-index"));
        entries[0].owns.indexes = vec!["inventory_path".into()];
        owned_dir(&root.join("z-first-path"));
        entries[0].owns.files = vec!["z-first-path".into(), "a-second-path".into()];
        assert!(serial_and_overlap_error(&connection, root, &entries[0]).contains("z-first-path"));
    }

    #[test]
    fn duplicate_explicit_and_overlapping_directory_file_claims_are_refused() {
        let (directory, mut entries) = fixture();
        let root = directory.path();
        let connection = dataset_connection(root);
        entries[0].owns.files = vec!["manifest.json".into(), "manifest.json".into()];
        assert!(
            serial_and_overlap_error(&connection, root, &entries[0])
                .contains("duplicate owned file")
        );
        entries[0].owns.files = vec!["assets/fixture.bin".into(), "assets/".into()];
        assert!(
            serial_and_overlap_error(&connection, root, &entries[0])
                .contains("overlapping owned file claims")
        );
    }
}
