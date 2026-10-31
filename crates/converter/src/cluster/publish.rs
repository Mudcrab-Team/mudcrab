//! The base publisher of the cluster geometry store (`mudcrab.clusters`): the one place that adds
//! assets and `cluster_meshes` rows to a package and that derives `cluster-manifest.json` from the
//! final store.
//!
//! Two kinds of write, kept apart on purpose:
//! * **Assets** (`.mcl`, `.mci`, terrain atlases) are immutable and content-addressed. They go
//!   through [`write_asset`]: a rerun that finds the same bytes is a no-op, different bytes are
//!   an error, nothing is ever overwritten.
//! * **The manifest** `cluster-manifest.json` is derived data and is replaced atomically by
//!   [`finalize_base`], never edited by a stage. The caller holds the dataset lock around the
//!   whole publication (asset writes, rows, finalize, terrain manifest, envelope); the lock is
//!   defined with the extension contract and is not taken here.
//!
//! Order of a publication: every stage that adds geometry writes its assets with [`write_asset`]
//! and its rows with [`insert_mesh_row`], committed; then [`finalize_base`] runs once, after the
//! last of them. It recounts the complete store (object and terrain), checks every file a row
//! names, replaces the manifest, and returns the owned tables, indexes and files with their
//! hashes for the extension envelope. Terrain's manifest is outside this store and is written by
//! the terrain stage after it, from the [`BaseFinal`] this returns.

use crate::cache::hash_bytes;
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure, eyre},
};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::json;
use shared::{
    asset_lock::{AssetLock, AssetLockMode, resolve_asset_path},
    cluster::{PAYLOAD_FORMAT_VERSION, canonical_json, hex, parse_hash, read_mci, read_mcl},
    cluster_mapping::{AuthoritativeInputs, validate_mapping},
    extensions::{CLUSTERS, ExtensionEntry, ExtensionHasher, ExtensionOwns, core_build_identity},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};

/// The package-relative name of the base manifest.
pub const CLUSTER_MANIFEST: &str = "cluster-manifest.json";

/// The wrapper version of `mudcrab.clusters`.
pub const BASE_EXTENSION_VERSION: u32 = 1;

/// Tables the base extension owns, R-tree shadow tables included.
pub const BASE_TABLES: [&str; 8] = [
    "cluster_build",
    "cluster_meshes",
    "cluster_chunks",
    "cluster_chunks_spatial",
    "cluster_chunks_spatial_node",
    "cluster_chunks_spatial_parent",
    "cluster_chunks_spatial_rowid",
    "cluster_rejects",
];
/// Names reserved for the instance map, the reference coverage and their header; owned by the
/// base extension once their schema is agreed, listed only when the package has them.
pub const RESERVED_BASE_TABLES: [&str; 3] = [
    "cluster_instance_map",
    "cluster_reference_coverage",
    "cluster_provenance",
];

/// Writes the immutable asset `bytes` at the package-relative `path`. A file already there must
/// hold the same bytes (paths come from content hashes, so a rerun finds identical files);
/// different bytes are an error, never an overwrite. The file appears complete or not at all.
pub fn write_asset(package: &Path, path: &str, bytes: &[u8]) -> Result<()> {
    ensure!(
        Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "{path} is not a package-relative path"
    );
    let destination: PathBuf = package.join(path);
    let parent = destination
        .parent()
        .ok_or_else(|| eyre!("missing output parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = temporary_beside(&destination);
    let result = (|| -> Result<()> {
        write_synced(&temporary, bytes)?;
        match fs::hard_link(&temporary, &destination) {
            Ok(()) => crate::durability::sync_parent(&destination),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    fs::read(&destination)? == bytes,
                    "{} exists with different content; no overwrite",
                    destination.display()
                );
                Ok(())
            }
            Err(error) => {
                Err(error).wrap_err("cannot publish a complete file atomically without overwrite")
            }
        }
    })();
    let _ = fs::remove_file(&temporary);
    result
}

// Keep publication callers on the shared durable-write implementation.
pub(crate) use crate::durability::replace_file;
use crate::durability::{temporary_beside, write_synced};

/// One row of `cluster_meshes`.
#[derive(Debug, Clone, Copy)]
pub struct MeshRow<'a> {
    pub mesh_hash: &'a str,
    pub primitive_index: u32,
    pub payload_path: &'a str,
    pub content_hash: &'a str,
    pub source_triangles: u64,
    pub cluster_count: u32,
    pub vertex_count: u32,
    pub payload_bytes: u64,
    pub material_key: &'a str,
    /// The converted GLB the primitive comes from; empty for generated meshes (terrain blocks).
    pub model_path: &'a str,
    pub material_index: Option<u32>,
    /// `opaque` or `mask`.
    pub alpha_mode: &'a str,
    pub alpha_cutoff: Option<f32>,
    pub double_sided: bool,
}

/// Inserts a `cluster_meshes` row. A second row for the same (mesh, primitive) replaces the first
/// when `replace` is set and is an error otherwise.
pub fn insert_mesh_row(connection: &Connection, row: &MeshRow<'_>, replace: bool) -> Result<()> {
    let verb = if replace {
        "INSERT OR REPLACE"
    } else {
        "INSERT"
    };
    connection
        .execute(
            &format!(
                "{verb} INTO cluster_meshes(mesh_hash, primitive_index, payload_path, content_hash,
                     source_triangles, cluster_count, vertex_count, payload_bytes, material_key,
                     model_path, material_index, alpha_mode, alpha_cutoff, double_sided)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
            ),
            params![
                row.mesh_hash,
                row.primitive_index,
                row.payload_path,
                row.content_hash,
                row.source_triangles as i64,
                row.cluster_count,
                row.vertex_count,
                row.payload_bytes as i64,
                row.material_key,
                row.model_path,
                row.material_index,
                row.alpha_mode,
                row.alpha_cutoff,
                i64::from(row.double_sided),
            ],
        )
        .wrap_err_with(|| format!("cluster_meshes row of {}", row.mesh_hash))?;
    Ok(())
}

/// What the final store holds, as written to `cluster-manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseSummary {
    pub builder_identity: String,
    pub triangle_threshold: u32,
    pub block_side_cells: u32,
    pub meshes: u64,
    pub blocks: u64,
    /// Rejected primitives by reason.
    pub rejects: BTreeMap<String, u64>,
}

/// One file of the base store: its package-relative path, size and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// The finalized base store, for the extension envelope and for terrain's dependency record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseFinal {
    pub summary: BaseSummary,
    /// Owned tables present in the package, sorted.
    pub tables: Vec<String>,
    /// Indexes of the owned tables (R-tree internals excluded), sorted.
    pub indexes: Vec<String>,
    /// Every file under `clusters/` plus the manifest, sorted by path.
    pub files: Vec<OwnedFile>,
    /// Instances in all blocks and bytes of all mesh payloads, for the envelope's stats.
    pub instances: u64,
    pub mesh_payload_bytes: u64,
    /// The `mudcrab.clusters` manifest entry for `conversion-manifest.json`: the ownership lists,
    /// the `mudcrab-ext-hash-v1` hash of every owned table, index and file (including the
    /// manifest this call wrote) and `built_from`, the core `lod_build.build_identity`. It does not
    /// contain the dependent terrain extension's hash.
    pub extension: ExtensionEntry,
}

/// What `mudcrab.clusters` owns, given the owned tables and explicit indexes found in the store.
/// The manifest sits at the package root, outside `clusters/`, so it is listed by name.
pub fn base_owns(tables: &[String], indexes: &[String]) -> ExtensionOwns {
    ExtensionOwns {
        tables: tables.to_vec(),
        indexes: indexes.to_vec(),
        files: vec![CLUSTER_MANIFEST.to_owned(), "clusters/".to_owned()],
        gltf_extensions: Vec::new(),
    }
}

/// Internal inventory shared by certified finalization and uncertified test fixtures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoreFinal {
    pub summary: BaseSummary,
    pub tables: Vec<String>,
    pub indexes: Vec<String>,
    pub files: Vec<OwnedFile>,
    pub instances: u64,
    pub mesh_payload_bytes: u64,
}

/// Whether a finalization is bound to the data set's core build.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CoreBinding {
    /// A data set publication: the core identity must exist and everything binds to it.
    Certified,
    /// A benchmark store with no core data set: no mapping, no wrapper entry.
    #[cfg(test)]
    Uncertified,
}

struct Finalized {
    store: StoreFinal,
    extension: Option<ExtensionEntry>,
}

/// The writer context of one publication: the exclusive dataset lock, taken once at the top of
/// the operation and kept until the whole publication (base store, terrain manifest, conversion
/// manifest) is in place, and the folder the stages write into. That folder is the destination
/// itself for a standalone stage run, or the pipeline's staging tree that is published to the
/// destination afterwards; the lock always guards the destination.
pub struct PublishContext<'a> {
    lock: &'a AssetLock,
    staging: &'a Path,
    overlay: Option<OverlayBinding<'a>>,
    space: Option<&'a std::sync::Arc<crate::derive_space::SpaceGuard>>,
}

/// The base conversion an overlay publication reads: its core tables and files are never copied
/// or linked into the staging folder, which holds only the cluster store.
#[derive(Clone, Copy)]
struct OverlayBinding<'a> {
    base: &'a Path,
    terrain_sources: Option<&'a Path>,
}

/// The cluster tables of an overlay live in this database beside the `clusters/` payloads.
pub const OVERLAY_STORE: &str = shared::cluster_overlay::OVERLAY_DATABASE_FILE;
/// The core database of a conversion (and, in a full-copy package, the cluster store too).
pub const CORE_DATABASE: &str = "skyrim_world.db";
/// The name the base conversion's database is attached under while an overlay is published: the
/// cluster store is `main`, the base `base`. Table names stay unqualified.
pub const CORE_SCHEMA: &str = shared::cluster_overlay::BASE_SCHEMA;

impl<'a> PublishContext<'a> {
    /// Checks that `lock` is the exclusive writer lock of `destination` and that `staging` is an
    /// existing folder. Nothing is acquired here and nothing is re-acquired later.
    pub fn new(lock: &'a AssetLock, destination: &Path, staging: &'a Path) -> Result<Self> {
        ensure!(
            lock.mode() == AssetLockMode::Exclusive,
            "publishing the cluster store needs the exclusive writer lock, not a shared one"
        );
        ensure!(
            resolve_asset_path(lock.asset_dir())? == resolve_asset_path(destination)?,
            "the lock guards {}, not {}",
            lock.asset_dir().display(),
            destination.display()
        );
        ensure!(staging.is_dir(), "{} is not a folder", staging.display());
        Ok(Self {
            lock,
            staging,
            overlay: None,
            space: None,
        })
    }

    /// A context for a cluster overlay of the conversion `base`. The staging folder holds only
    /// `clusters.db` (created by the caller) and `clusters/`; core tables are read from `base`'s
    /// database through a read-only attachment, core files from `base`, and the raw terrain DDS
    /// files from `terrain_sources` when given (else `base/vfs`). Nothing of the base is copied.
    /// The base must stay under the caller's reader lock for the whole publication.
    pub fn new_overlay(
        lock: &'a AssetLock,
        destination: &Path,
        staging: &'a Path,
        base: &'a Path,
        terrain_sources: Option<&'a Path>,
    ) -> Result<Self> {
        let mut context = Self::new(lock, destination, staging)?;
        ensure!(
            base.join(CORE_DATABASE).is_file(),
            "{} is not a converted base: it has no {CORE_DATABASE}",
            base.display()
        );
        ensure!(
            staging.join(OVERLAY_STORE).is_file(),
            "the overlay staging folder {} has no {OVERLAY_STORE}",
            staging.display()
        );
        let (base_path, staging_path) = (resolve_asset_path(base)?, resolve_asset_path(staging)?);
        ensure!(
            base_path != staging_path && base_path != resolve_asset_path(destination)?,
            "an overlay must be staged and published beside its base, not over it"
        );
        context.overlay = Some(OverlayBinding {
            base,
            terrain_sources,
        });
        Ok(context)
    }

    pub(crate) fn with_space(
        mut self,
        space: &'a std::sync::Arc<crate::derive_space::SpaceGuard>,
    ) -> Self {
        self.space = Some(space);
        self
    }

    pub(crate) fn space(&self) -> Option<&'a std::sync::Arc<crate::derive_space::SpaceGuard>> {
        self.space
    }

    pub(crate) fn replace_file(&self, destination: &Path, bytes: &[u8]) -> Result<()> {
        match self.space {
            Some(space) => space.with_file(destination, bytes.len() as u64, || {
                replace_file(destination, bytes)
            }),
            None => replace_file(destination, bytes),
        }
    }

    /// The folder the stages write into and [`finalize_base`] validates.
    pub fn staging(&self) -> &Path {
        self.staging
    }

    /// The destination dataset the lock guards.
    pub fn destination(&self) -> &Path {
        self.lock.asset_dir()
    }

    /// Whether this publication is a cluster overlay of a separate base conversion.
    pub fn is_overlay(&self) -> bool {
        self.overlay.is_some()
    }

    /// The base conversion of an overlay publication.
    pub fn base(&self) -> Option<&Path> {
        self.overlay.map(|overlay| overlay.base)
    }

    /// The folder that holds the core tables and files: the base of an overlay, else the staging
    /// folder itself (a full-copy package carries its core).
    pub fn core_root(&self) -> &Path {
        self.base().unwrap_or(self.staging)
    }

    /// The core world database.
    pub fn core_database(&self) -> PathBuf {
        self.core_root().join(CORE_DATABASE)
    }

    /// The database that holds the cluster tables: `clusters.db` of an overlay, else the staging
    /// folder's world database.
    pub fn store_path(&self) -> PathBuf {
        if self.is_overlay() {
            self.staging.join(OVERLAY_STORE)
        } else {
            self.staging.join(CORE_DATABASE)
        }
    }

    /// The folder the raw terrain DDS files are read from.
    pub fn terrain_sources(&self) -> PathBuf {
        self.overlay
            .and_then(|overlay| overlay.terrain_sources)
            .map_or_else(|| self.core_root().join("vfs"), Path::to_path_buf)
    }

    /// A read-only connection to the cluster store. For an overlay the core database is attached
    /// read-only as [`CORE_SCHEMA`]; the cluster store must hold no core table, as the shared lookups
    /// refuse a name present in both databases.
    pub fn open_store_read_only(&self) -> Result<Connection> {
        let path = self.store_path();
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .wrap_err_with(|| format!("cannot open {}", path.display()))?;
        if self.is_overlay() {
            attach_read_only(&connection, &self.core_database(), CORE_SCHEMA)?;
        }
        Ok(connection)
    }

    /// A read-only connection to the core database alone.
    pub fn open_core_read_only(&self) -> Result<Connection> {
        let path = self.core_database();
        Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .wrap_err_with(|| format!("cannot open {}", path.display()))
    }

    /// The non-empty `lod_build.build_identity` of the core, if it has one.
    pub fn core_identity(&self) -> Result<Option<String>> {
        Ok(core_build_identity(&self.open_core_read_only()?)?
            .filter(|identity| !identity.is_empty()))
    }
}

/// The database a staging or published folder keeps its cluster tables in: `clusters.db` when the
/// folder is an overlay, else the world database (a full-copy package or a benchmark store).
pub fn store_database(root: &Path) -> PathBuf {
    let overlay = root.join(OVERLAY_STORE);
    if overlay.is_file() {
        overlay
    } else {
        root.join(CORE_DATABASE)
    }
}

// Both publication and runtime attachments use one URI/read-only policy.
pub use shared::cluster_overlay::attach_read_only;
#[cfg(test)]
use shared::cluster_overlay::sqlite_read_only_uri;

/// Publishes the final base store in the context's staging folder: [`finalize_unlocked`] under the
/// context's exclusive writer lock.
pub fn finalize_base(
    context: &PublishContext<'_>,
    authority: Option<&dyn AuthoritativeInputs>,
) -> Result<BaseFinal> {
    let finalized = finalize_unlocked(context, authority, CoreBinding::Certified)?;
    let extension = finalized
        .extension
        .ok_or_else(|| eyre!("a certified finalization must yield the extension entry"))?;
    let store = finalized.store;
    Ok(BaseFinal {
        summary: store.summary,
        tables: store.tables,
        indexes: store.indexes,
        files: store.files,
        instances: store.instances,
        mesh_payload_bytes: store.mesh_payload_bytes,
        extension,
    })
}

/// Validation fixture without a core data set; cannot certify an extension.
#[cfg(test)]
pub(crate) fn finalize_benchmark(context: &PublishContext<'_>) -> Result<StoreFinal> {
    Ok(finalize_unlocked(context, None, CoreBinding::Uncertified)?.store)
}

/// The same uncertified result, with core origin facts for a benchmark that added terrain.
/// Origins validate placement only: this path still refuses reference maps and mints no entry.
#[cfg(test)]
pub(crate) fn finalize_benchmark_with_origins(
    context: &PublishContext<'_>,
    origins: &HashMap<u32, (i32, i32)>,
) -> Result<StoreFinal> {
    let authority = shared::cluster_mapping::FixedAuthority {
        lod_origins: origins.clone(),
        ..Default::default()
    };
    Ok(finalize_unlocked(context, Some(&authority), CoreBinding::Uncertified)?.store)
}

/// Validates the final committed store of `package`, replaces `cluster-manifest.json` from it
/// and returns the owned tables, indexes and files. Call it once, after the last stage that adds
/// geometry has committed, and hash the extension from the result.
///
/// This is a full check of the store, not only an inventory. It refuses a database that has an
/// unmerged write-ahead log or journal (its file would not be the committed view; only a missing
/// side file counts as none, any other IO error is returned); a stray temporary file under
/// `clusters/`; a spatial index whose entries are not exactly the blocks' keys and bounds (as
/// SQLite stores them, rounded outward) with ids 1..n; a mesh payload that is missing, has another
/// size or content hash than its row, or whose header disagrees with the row or with
/// `cluster_build` (builder identity, mesh, primitive, vertex and cluster counts); an instance
/// payload whose bytes do not hash to its row or whose header disagrees with the row (builder
/// identity, worldspace, tier, anchor, instance count); terrain files that the terrain table does
/// not name exactly (content-hash names, a valid one-record block by this builder for the row's
/// block and mesh, no orphans); and, when any of the three mapping tables exists, all of them must
/// exist, an `authority` must be given, and the mapping must validate against it (the converter's
/// own recomputation of reference inputs, models, primitives, transforms, builder and core
/// identity). Every owned file is read and hashed, and the candidate manifest is built and hashed,
/// before the manifest is replaced, so a failure leaves the previous manifest as it was. Running
/// it again on an unchanged store gives an identical result.
///
/// Private on purpose: it does not take the dataset lock, and nothing may mutate and finalize a
/// store without it. Everything goes through [`finalize_base`].
fn finalize_unlocked(
    context: &PublishContext<'_>,
    authority: Option<&dyn AuthoritativeInputs>,
    binding: CoreBinding,
) -> Result<Finalized> {
    let package = context.staging();
    let database = context.store_path();
    let store_name = database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| eyre!("the cluster store has no file name"))?
        .to_owned();
    for suffix in ["-wal", "-journal"] {
        let side = package.join(format!("{store_name}{suffix}"));
        match fs::metadata(&side) {
            Ok(meta) => ensure!(
                meta.len() == 0,
                "{} holds unmerged writes: finish and close every writer first",
                side.display()
            ),
            // Only a missing side file means "nothing unmerged"; any other error is a real one.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).wrap_err_with(|| format!("cannot inspect {}", side.display()));
            }
        }
    }
    // For an overlay the cluster store is `main` and the core database is attached read-only.
    let connection = context.open_store_read_only()?;
    let (builder_identity, triangle_threshold, block_side_cells): (String, u32, u32) = connection
        .query_row(
            "SELECT builder_identity, triangle_threshold, block_side_cells
             FROM cluster_build WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .wrap_err("the package has no cluster_build row")?;
    ensure!(
        parse_hash(&builder_identity).is_some(),
        "cluster_build.builder_identity is not a SHA-256"
    );

    // The extension is derived from the core build: without its identity there is nothing to bind
    // the store to, and an empty one would bind it to anything.
    let core_identity: Option<String> = match binding {
        CoreBinding::Certified => Some(context.core_identity()?.ok_or_else(|| {
            eyre!(
                "the data set has no lod_build.build_identity; mudcrab.clusters is derived from it"
            )
        })?),
        #[cfg(test)]
        CoreBinding::Uncertified => None,
    };

    let count = |table: &str| -> Result<u64> {
        Ok(
            connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })? as u64,
        )
    };
    let meshes = count("cluster_meshes")?;
    let blocks = count("cluster_chunks")?;
    // The spatial index is the lookup the runtime queries: each block needs exactly its own entry,
    // with its own key and bounds, and the entry ids run 1..n with no gaps.
    ensure!(
        count("cluster_chunks_spatial")? == blocks,
        "cluster_chunks_spatial does not hold one entry per block"
    );
    // SQLite stores R-tree coordinates as 32-bit floats rounded outward (a minimum down, a maximum
    // up), so the exact expected entry is that rounding of the block's bounds; there is no other
    // tolerance.
    let mut joined = 0u64;
    let mut statement = connection.prepare(
        "SELECT c.bounds_min_x, c.bounds_max_x, c.bounds_min_y, c.bounds_max_y,
                s.minX, s.maxX, s.minY, s.maxY
         FROM cluster_chunks c JOIN cluster_chunks_spatial s
             ON s.worldspace_id = c.worldspace_id AND s.tier = c.tier
            AND s.anchor_x = c.anchor_x AND s.anchor_y = c.anchor_y",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let value = |index: usize| row.get::<_, f64>(index);
        let expected = [
            rtree_minimum(value(0)?),
            rtree_maximum(value(1)?),
            rtree_minimum(value(2)?),
            rtree_maximum(value(3)?),
        ];
        let stored = [value(4)?, value(5)?, value(6)?, value(7)?];
        ensure!(
            expected == stored,
            "cluster_chunks_spatial entry differs from its block's bounds"
        );
        joined += 1;
    }
    ensure!(
        joined == blocks,
        "cluster_chunks_spatial entries do not match their blocks' keys"
    );
    let (lowest, highest): (Option<i64>, Option<i64>) = connection.query_row(
        "SELECT MIN(id), MAX(id) FROM cluster_chunks_spatial",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        blocks == 0 || (lowest == Some(1) && highest == Some(blocks as i64)),
        "cluster_chunks_spatial ids are not 1..{blocks}"
    );
    let instances: i64 = connection.query_row(
        "SELECT COALESCE(SUM(instance_count), 0) FROM cluster_chunks",
        [],
        |row| row.get(0),
    )?;

    let identity_bytes = parse_hash(&builder_identity).expect("checked above");
    // Size and SHA-256 of every payload read below, so the file list does not read them twice.
    let mut hashed: HashMap<String, (u64, String)> = HashMap::new();
    let mut mesh_payload_bytes = 0u64;
    let mut statement = connection.prepare(
        "SELECT payload_path, payload_bytes, content_hash, mesh_hash, primitive_index,
                vertex_count, cluster_count FROM cluster_meshes",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let bytes = fs::read(package.join(&path))
            .wrap_err_with(|| format!("mesh payload {path} is missing"))?;
        let (expected_bytes, content_hash): (i64, String) = (row.get(1)?, row.get(2)?);
        ensure!(
            bytes.len() as u64 == expected_bytes as u64,
            "mesh payload {path} is {} bytes, its row says {expected_bytes}",
            bytes.len()
        );
        let digest = hash_bytes(&bytes);
        ensure!(
            digest == content_hash,
            "mesh payload {path} does not hash to its row"
        );
        let view = read_mcl(&bytes).map_err(|error| eyre!("mesh payload {path}: {error}"))?;
        let (mesh_hash, primitive_index): (String, u32) = (row.get(3)?, row.get(4)?);
        ensure!(
            view.builder_identity == identity_bytes,
            "mesh payload {path} was built with another builder identity"
        );
        ensure!(
            hex(&view.mesh_hash) == mesh_hash && view.primitive_index == primitive_index,
            "mesh payload {path} names another mesh or primitive than its row"
        );
        ensure!(
            (view.vertex_count, view.cluster_count) == (row.get(5)?, row.get(6)?),
            "mesh payload {path} has other vertex or cluster counts than its row"
        );
        mesh_payload_bytes += bytes.len() as u64;
        hashed.insert(path, (bytes.len() as u64, digest));
    }
    let mut statement = connection.prepare(
        "SELECT payload_path, content_hash, worldspace_id, tier, anchor_x, anchor_y, instance_count
         FROM cluster_chunks",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let bytes = fs::read(package.join(&path))
            .wrap_err_with(|| format!("instance payload {path} is missing"))?;
        let digest = hash_bytes(&bytes);
        ensure!(
            digest == row.get::<_, String>(1)?,
            "instance payload {path} does not hash to its row"
        );
        let view = read_mci(&bytes).map_err(|error| eyre!("instance payload {path}: {error}"))?;
        ensure!(
            view.builder_identity == identity_bytes,
            "instance payload {path} was built with another builder identity"
        );
        ensure!(
            (
                view.worldspace_id,
                view.tier,
                view.anchor_x,
                view.anchor_y,
                view.instance_count
            ) == (
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?
            ),
            "instance payload {path} has another block key or instance count than its row"
        );
        hashed.insert(path, (bytes.len() as u64, digest));
    }

    let mut rejects = BTreeMap::new();
    let mut statement =
        connection.prepare("SELECT reason, COUNT(*) FROM cluster_rejects GROUP BY reason")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        rejects.insert(row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64);
    }

    let mut tables = Vec::new();
    let mut statement =
        connection.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        if BASE_TABLES.contains(&name.as_str()) || RESERVED_BASE_TABLES.contains(&name.as_str()) {
            tables.push(name);
        }
    }
    tables.sort();
    let mut indexes = Vec::new();
    let mut statement = connection.prepare(
        "SELECT name, tbl_name FROM sqlite_master
         WHERE type = 'index' AND name NOT LIKE 'sqlite_autoindex%'",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let (name, table): (String, String) = (row.get(0)?, row.get(1)?);
        if tables.contains(&table) {
            indexes.push(name);
        }
    }
    indexes.sort();

    // The reference mapping is all or nothing, and when present it must validate.
    let mapping_tables = RESERVED_BASE_TABLES.map(|name| tables.iter().any(|t| t == name));
    ensure!(
        mapping_tables.iter().all(|present| *present) || !mapping_tables.iter().any(|p| *p),
        "the reference mapping tables must all exist or none of them"
    );
    let mapping = if mapping_tables[0] {
        let authority = authority.ok_or_else(|| {
            eyre!("the reference mapping tables are present but no authority was given: coverage cannot be certified")
        })?;
        let core = core_identity
            .as_deref()
            .ok_or_else(|| eyre!("an uncertified store cannot carry the reference mapping"))?;
        validate_mapping(&connection, package, Some(core), authority)
            .map_err(|error| eyre!("{error}"))?;
        let (map_version, coverage_version): (u32, u32) = connection.query_row(
            "SELECT instance_map_version, coverage_version FROM cluster_provenance WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Some((
            map_version,
            coverage_version,
            count("cluster_instance_map")?,
            count("cluster_reference_coverage")?,
        ))
    } else {
        None
    };

    // Find any stray temporary file before anything changes, so a failure leaves it alone.
    let mut paths = Vec::new();
    collect_files(package, &package.join("clusters"), &mut paths)?;
    check_terrain_files(&connection, package, &paths, &identity_bytes, authority)?;

    let summary = BaseSummary {
        builder_identity,
        triangle_threshold,
        block_side_cells,
        meshes,
        blocks,
        rejects,
    };
    let mut manifest = json!({
        "builder_identity": summary.builder_identity,
        "payload_format": PAYLOAD_FORMAT_VERSION,
        "triangle_threshold": summary.triangle_threshold,
        "block_side_cells": summary.block_side_cells,
        "meshes": summary.meshes,
        "blocks": summary.blocks,
        "rejects": summary.rejects,
    });
    if let Some((map_version, coverage_version, map_rows, coverage_rows)) = mapping {
        manifest["instance_map_version"] = json!(map_version);
        manifest["coverage_version"] = json!(coverage_version);
        manifest["instance_map_rows"] = json!(map_rows);
        manifest["coverage_rows"] = json!(coverage_rows);
    }
    if core_identity.is_none() {
        manifest["uncertified"] = json!(true);
    }
    let manifest_bytes = canonical_json(&manifest);

    // Read and hash every owned file, and the candidate manifest, before the manifest is
    // replaced: a read error here fails with the previous manifest still in place.
    let mut files = Vec::with_capacity(paths.len() + 1);
    for path in paths {
        let (bytes, sha256) = match hashed.remove(&path) {
            Some(known) => known,
            None => {
                let bytes = fs::read(package.join(&path))
                    .wrap_err_with(|| format!("cannot read {path} to hash it"))?;
                (bytes.len() as u64, hash_bytes(&bytes))
            }
        };
        files.push(OwnedFile {
            path,
            bytes,
            sha256,
        });
    }
    files.push(OwnedFile {
        path: CLUSTER_MANIFEST.to_owned(),
        bytes: manifest_bytes.len() as u64,
        sha256: hash_bytes(&manifest_bytes),
    });
    files.sort_by(|a, b| a.path.cmp(&b.path));

    // The extension hash covers the candidate manifest, so it is built from the bytes about to be
    // written, still before anything changes.
    let mut stats = BTreeMap::from([
        ("meshes".to_owned(), summary.meshes),
        ("blocks".to_owned(), summary.blocks),
        ("instances".to_owned(), instances as u64),
        ("mesh_payload_bytes".to_owned(), mesh_payload_bytes),
        ("rejects".to_owned(), summary.rejects.values().sum()),
    ]);
    if let Some((_, _, map_rows, coverage_rows)) = mapping {
        stats.insert("instance_map_rows".to_owned(), map_rows);
        stats.insert("coverage_rows".to_owned(), coverage_rows);
    }
    let extension = match &core_identity {
        None => None,
        Some(core) => {
            let mut hasher = ExtensionHasher::new(CLUSTERS, BASE_EXTENSION_VERSION);
            for table in &tables {
                hasher.add_table(&connection, table)?;
            }
            for index in &indexes {
                hasher.add_index(&connection, index)?;
            }
            // Every file's SHA-256 was taken above (the candidate manifest from its own bytes), so
            // no payload is read a second time.
            for file in &files {
                hasher.add_file_digest(&file.path, &file.sha256);
            }
            Some(ExtensionEntry {
                id: CLUSTERS.to_owned(),
                version: BASE_EXTENSION_VERSION,
                owns: base_owns(&tables, &indexes),
                sha256: hasher.finish(),
                built_from: core.clone(),
                stats,
            })
        }
    };
    context.replace_file(&package.join(CLUSTER_MANIFEST), &manifest_bytes)?;
    Ok(Finalized {
        store: StoreFinal {
            summary,
            tables,
            indexes,
            files,
            instances: instances as u64,
            mesh_payload_bytes,
        },
        extension,
    })
}

/// SQLite's `RTREE_EPSILON` factors (`rtree.c`): a value that rounds the wrong way when it is
/// stored as a 32-bit float is multiplied by one of these and rounded again, which can land more
/// than one float away from the exact neighbour. The check reproduces that rule exactly.
const RTREE_ROUND_TOWARDS_ZERO: f64 = 1.0 - 1.0 / 8_388_608.0;
const RTREE_ROUND_AWAY_FROM_ZERO: f64 = 1.0 + 1.0 / 8_388_608.0;

/// What SQLite's R-tree stores for a minimum coordinate (`rtreeValueDown`).
fn rtree_minimum(value: f64) -> f64 {
    let rounded = value as f32;
    if f64::from(rounded) > value {
        let factor = if value < 0.0 {
            RTREE_ROUND_AWAY_FROM_ZERO
        } else {
            RTREE_ROUND_TOWARDS_ZERO
        };
        f64::from((value * factor) as f32)
    } else {
        f64::from(rounded)
    }
}

/// What SQLite's R-tree stores for a maximum coordinate (`rtreeValueUp`).
fn rtree_maximum(value: f64) -> f64 {
    let rounded = value as f32;
    if f64::from(rounded) < value {
        let factor = if value < 0.0 {
            RTREE_ROUND_TOWARDS_ZERO
        } else {
            RTREE_ROUND_AWAY_FROM_ZERO
        };
        f64::from((value * factor) as f32)
    } else {
        f64::from(rounded)
    }
}

/// A path a terrain row names must be a plain package-relative path under `clusters/terrain/`
/// (forward slashes, no `..`, no drive or root) that is among the files this store owns, so a row
/// cannot make the hash inventory read, or skip, a file outside what it hashes.
fn owned_terrain_path(path: &str, owned: &HashSet<&str>, folder: &str) -> Result<()> {
    let plain = !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    ensure!(
        plain && path.starts_with(folder) && owned.contains(path),
        "terrain row names {path}, which is not an owned file under {folder}"
    );
    Ok(())
}

/// Checks the terrain files the base store owns against the terrain table that names them.
///
/// * Every path is a plain owned path under `clusters/terrain/instances/` or `.../atlases/`.
/// * The instance file is named by its content hash and is a valid block of one record, built by
///   this builder, for the row's mesh.
/// * Anchors follow the terrain stage's convention: the header holds the LOD-relative block anchor
///   and `tier` is the block side in cells; the row holds the absolute cell, which must equal the
///   worldspace's LOD origin (from `authority`) plus the header anchor times the side. The record
///   sits in that absolute cell and its transform is the identity matrix translated to that cell's
///   world position in runtime axes (`[x, 0, -y]`), compared as f32 bits.
/// * The atlas is checked by file name only (its bytes hash to its name). Decoding the KTX2 and
///   proving its recipe belongs to the terrain finalizer and reader.
/// * No file under `clusters/terrain/` is left unreferenced; a package with terrain files and no
///   terrain table has orphans, and fails. Terrain rows need an `authority` for the LOD origin.
fn check_terrain_files(
    connection: &Connection,
    package: &Path,
    paths: &[String],
    builder: &[u8; 32],
    authority: Option<&dyn AuthoritativeInputs>,
) -> Result<()> {
    let has_table: bool = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'cluster_terrain'",
        [],
        |row| Ok(row.get::<_, i64>(0)? > 0),
    )?;
    let owned: HashSet<&str> = paths.iter().map(String::as_str).collect();
    let mut referenced: HashSet<String> = HashSet::new();
    if has_table {
        let stem_is_hash = |path: &str, bytes: &[u8]| {
            Path::new(path)
                .file_stem()
                .is_some_and(|stem| stem.to_string_lossy() == hash_bytes(bytes))
        };
        let mut statement = connection.prepare(
            "SELECT mesh_hash, primitive_index, worldspace_id, anchor_x, anchor_y, side_cells,
                    instance_path, atlas_path FROM cluster_terrain",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let (mesh_hash, primitive): (String, u32) = (row.get(0)?, row.get(1)?);
            let (world, row_anchor, side): (u32, (i32, i32), u32) =
                (row.get(2)?, (row.get(3)?, row.get(4)?), row.get(5)?);
            let (instance_path, atlas_path): (String, String) = (row.get(6)?, row.get(7)?);
            owned_terrain_path(&instance_path, &owned, "clusters/terrain/instances/")?;
            owned_terrain_path(&atlas_path, &owned, "clusters/terrain/atlases/")?;
            let bytes = fs::read(package.join(&instance_path))
                .wrap_err_with(|| format!("terrain instance file {instance_path} is missing"))?;
            ensure!(
                stem_is_hash(&instance_path, &bytes),
                "terrain instance file {instance_path} is not named by its content hash"
            );
            let view = read_mci(&bytes)
                .map_err(|error| eyre!("terrain instance file {instance_path}: {error}"))?;
            ensure!(
                view.builder_identity == *builder
                    && view.instance_count == 1
                    && view.worldspace_id == world
                    && side > 0
                    && view.tier == side,
                "terrain instance file {instance_path} disagrees with its terrain row"
            );
            let authority = authority.ok_or_else(|| {
                eyre!("terrain rows are present but no authority gave the worldspace LOD origins")
            })?;
            let origin = authority.lod_origin(world).ok_or_else(|| {
                eyre!("worldspace {world:08X} has no LOD origin in the core data")
            })?;
            let side_cells = i64::from(side);
            let absolute = |origin: i32, anchor: i32| {
                i32::try_from(i64::from(origin) + i64::from(anchor) * side_cells)
            };
            let (Ok(cell_x), Ok(cell_y)) = (
                absolute(origin.0, view.anchor_x),
                absolute(origin.1, view.anchor_y),
            ) else {
                return Err(eyre!(
                    "terrain instance file {instance_path} has an anchor outside the cell grid"
                ));
            };
            ensure!(
                (cell_x, cell_y) == row_anchor,
                "terrain row {instance_path} stores anchor {row_anchor:?}, but LOD origin {origin:?} plus header anchor ({}, {}) times {side} is ({cell_x}, {cell_y})",
                view.anchor_x,
                view.anchor_y
            );
            let record = view.instance(0);
            let translation = [
                (f64::from(cell_x) * f64::from(super::classify::CELL_SIZE)) as f32,
                0.0,
                -((f64::from(cell_y) * f64::from(super::classify::CELL_SIZE)) as f32),
            ];
            let expected = [
                1.0,
                0.0,
                0.0,
                translation[0],
                0.0,
                1.0,
                0.0,
                translation[1],
                0.0,
                0.0,
                1.0,
                translation[2],
            ];
            ensure!(
                (record.cell_x, record.cell_y) == (cell_x, cell_y)
                    && record.transform.map(f32::to_bits) == expected.map(f32::to_bits),
                "terrain instance file {instance_path} does not place its block at cell ({cell_x}, {cell_y})"
            );
            let mesh = view.mesh(record.mesh as usize);
            ensure!(
                hex(&mesh.mesh_hash) == mesh_hash && mesh.primitive_index == primitive,
                "terrain instance file {instance_path} places another mesh than its row"
            );
            let known: i64 = connection.query_row(
                "SELECT COUNT(*) FROM cluster_meshes WHERE mesh_hash = ?1 AND primitive_index = ?2",
                params![mesh_hash, primitive],
                |row| row.get(0),
            )?;
            ensure!(
                known == 1,
                "terrain row names a mesh the store does not hold"
            );
            let atlas = fs::read(package.join(&atlas_path))
                .wrap_err_with(|| format!("terrain atlas {atlas_path} is missing"))?;
            ensure!(
                stem_is_hash(&atlas_path, &atlas),
                "terrain atlas {atlas_path} is not named by its content hash"
            );
            referenced.insert(instance_path);
            referenced.insert(atlas_path);
        }
    }
    for path in paths {
        ensure!(
            !path.starts_with("clusters/terrain/") || referenced.contains(path),
            "{path} is a terrain file that no terrain row references"
        );
    }
    Ok(())
}

/// Collects the package-relative paths (forward slashes) of every file under `directory`; a
/// leftover temporary file means a writer did not finish, which is an error.
fn collect_files(package: &Path, directory: &Path, out: &mut Vec<String>) -> Result<()> {
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).wrap_err_with(|| format!("cannot list {}", directory.display()));
        }
    };
    ensure!(
        metadata.is_dir() && !crate::derive_extensions::is_link(&metadata),
        "package inventory refuses linked or non-directory path {}",
        directory.display()
    );
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            !crate::derive_extensions::is_link(&metadata),
            "package inventory refuses linked path {}",
            path.display()
        );
        if metadata.is_dir() {
            collect_files(package, &path, out)?;
            continue;
        }
        ensure!(
            metadata.is_file(),
            "package inventory refuses non-file {}",
            path.display()
        );
        let relative = path
            .strip_prefix(package)
            .wrap_err("a file outside the package")?;
        let relative = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        ensure!(
            !relative.ends_with(".tmp"),
            "{relative} is an unfinished write: remove it and publish again"
        );
        out.push(relative);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{
        cluster::{CLUSTER_TABLES_DDL, instance_payload_path},
        cluster_mapping::{
            CLUSTER_MAPPING_DDL, COVERAGE_VERSION, DIGEST_VERSION, Eligibility, FixedAuthority,
            INSTANCE_MAP_VERSION, MaterialSource, ReferenceInputs, ReferenceSnapshot, input_digest,
        },
        extensions::ownership_problems,
    };

    // A real payload pair from the committed fixture: one mesh and one block of three instances.
    const MESH: &[u8] = include_bytes!("../../../shared/tests/data/cluster/fixture.mcl");
    const BLOCK: &[u8] = include_bytes!("../../../shared/tests/data/cluster/fixture.mci");
    const MESH_PATH: &str = "clusters/meshes/ab/x-p0.mcl";

    fn identity_hex() -> String {
        hex(&read_mcl(MESH).unwrap().builder_identity)
    }

    fn block_path() -> String {
        let view = read_mci(BLOCK).unwrap();
        instance_payload_path(
            view.worldspace_id,
            view.tier as i32,
            view.anchor_x,
            view.anchor_y,
        )
    }

    /// The cluster tables, the build row and one mesh in `directory/<database>`: what an overlay's
    /// `clusters.db` holds, with no core table.
    fn store(directory: &Path, database: &str) -> Connection {
        let connection = Connection::open(directory.join(database)).unwrap();
        connection.execute_batch(CLUSTER_TABLES_DDL).unwrap();
        connection
            .execute(
                "INSERT INTO cluster_build(id, builder_identity, triangle_threshold,
                     block_side_cells, lod_mode) VALUES (1, ?1, 0, 16, 'cluster')",
                [identity_hex()],
            )
            .unwrap();
        let view = read_mcl(MESH).unwrap();
        write_asset(directory, MESH_PATH, MESH).unwrap();
        insert_mesh_row(
            &connection,
            &MeshRow {
                mesh_hash: &hex(&view.mesh_hash),
                primitive_index: view.primitive_index,
                payload_path: MESH_PATH,
                content_hash: &hash_bytes(MESH),
                source_triangles: 10,
                cluster_count: view.cluster_count,
                vertex_count: view.vertex_count,
                payload_bytes: MESH.len() as u64,
                material_key: "m",
                model_path: "",
                material_index: None,
                alpha_mode: "opaque",
                alpha_cutoff: None,
                double_sided: false,
            },
            false,
        )
        .unwrap();
        connection
    }

    const LOD_BUILD: &str = "CREATE TABLE lod_build (build_identity TEXT NOT NULL)";

    fn package(directory: &Path) -> Connection {
        let connection = store(directory, CORE_DATABASE);
        connection
            .execute_batch(&format!(
                "{LOD_BUILD}; INSERT INTO lod_build VALUES ('{}')",
                "cd".repeat(32)
            ))
            .unwrap();
        connection
    }

    fn add_block(directory: &Path, connection: &Connection) {
        let view = read_mci(BLOCK).unwrap();
        let path = block_path();
        write_asset(directory, &path, BLOCK).unwrap();
        connection
            .execute(
                "INSERT INTO cluster_chunks(worldspace_id, tier, anchor_x, anchor_y, payload_path,
                     content_hash, input_key, bounds_min_x, bounds_min_y, bounds_min_z, bounds_max_x,
                     bounds_max_y, bounds_max_z, instance_count, source_cells)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'k', 0, 0, 0, 1, 1, 1, ?7, '')",
                params![
                    view.worldspace_id,
                    view.tier,
                    view.anchor_x,
                    view.anchor_y,
                    path,
                    hash_bytes(BLOCK),
                    view.instance_count
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO cluster_chunks_spatial(id, minX, maxX, minY, maxY, worldspace_id, tier,
                     anchor_x, anchor_y) VALUES (1, 0, 1, 0, 1, ?1, ?2, ?3, ?4)",
                params![
                    view.worldspace_id,
                    view.tier,
                    view.anchor_x,
                    view.anchor_y
                ],
            )
            .unwrap();
    }

    /// Provenance, one map row per record of the block, and for each reference an `expanded` state
    /// and one clustered primitive row. Returns what the converter's own inputs would say.
    fn add_mapping(connection: &Connection) -> FixedAuthority {
        let mut authority = FixedAuthority {
            core_build_identity: Some("cd".repeat(32)),
            builder_identity: identity_hex(),
            classifier_scope: "{\"test\":true}".to_owned(),
            ..FixedAuthority::default()
        };
        connection.execute_batch(CLUSTER_MAPPING_DDL).unwrap();
        connection
            .execute(
                "INSERT INTO cluster_provenance(id, instance_map_version, coverage_version,
                     digest_version, digest_algorithm, built_from, builder_identity, classifier_scope)
                 VALUES (1, ?1, ?2, ?3, 'sha256', ?4, ?5, '{\"test\":true}')",
                params![
                    INSTANCE_MAP_VERSION,
                    COVERAGE_VERSION,
                    DIGEST_VERSION,
                    "cd".repeat(32),
                    identity_hex()
                ],
            )
            .unwrap();
        let view = read_mci(BLOCK).unwrap();
        for ordinal in 0..view.instance_count {
            let record = view.instance(ordinal as usize);
            let mesh = view.mesh(record.mesh as usize);
            let model = hex(&mesh.mesh_hash);
            let reference = 1000 + ordinal;
            // A record placed in a child worldspace names it; otherwise the block's own.
            let placed = if record.source_worldspace_id != 0 {
                record.source_worldspace_id
            } else {
                view.worldspace_id
            };
            let digest = input_digest(&ReferenceInputs {
                reference_id: reference,
                base_form_id: record.base_form_id,
                record_type: "STAT",
                placed_worldspace_id: placed,
                source_cell_id: 1,
                model_path: None,
                model_hash: Some(&model),
                position: [0.0; 3],
                rotation: [0.0; 3],
                scale: 1.0,
                header_flags: 0,
                enable_parent_id: None,
                snow_material: 0,
                data_hash: None,
                base_record_hash: None,
                texture_set_hash: None,
            });
            authority
                .models
                .entry(model.clone())
                .or_default()
                .insert(mesh.primitive_index);
            authority
                .materials
                .entry(model.clone())
                .or_default()
                .insert(
                    mesh.primitive_index,
                    MaterialSource {
                        model_path: String::new(),
                        material_index: None,
                        material_key: "m".to_owned(),
                        alpha_mask: false,
                        alpha_cutoff: None,
                        double_sided: false,
                    },
                );
            authority.references.insert(
                reference,
                ReferenceSnapshot {
                    base_form_id: record.base_form_id,
                    snow_material: record.snow_material,
                    placed_worldspace_id: placed,
                    source_cell_id: 1,
                    cell: (record.cell_x, record.cell_y),
                    block: (view.worldspace_id, view.tier, view.anchor_x, view.anchor_y),
                    digest: digest.clone(),
                    model: Some(model.clone()),
                    transform: Some(record.transform),
                    eligibility: Eligibility::Eligible,
                },
            );
            connection
                .execute(
                    "INSERT INTO cluster_instance_map(worldspace_id, tier, anchor_x, anchor_y,
                         chunk_content_hash, ordinal, reference_id, placed_worldspace_id, cell_x, cell_y,
                         model_hash, primitive_index, input_digest)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        view.worldspace_id,
                        view.tier,
                        view.anchor_x,
                        view.anchor_y,
                        hash_bytes(BLOCK),
                        ordinal,
                        reference,
                        placed,
                        record.cell_x,
                        record.cell_y,
                        model,
                        mesh.primitive_index,
                        digest
                    ],
                )
                .unwrap();
            for (scope, model_hash, primitive, status) in [
                ("reference", model.as_str(), -1i64, "expanded"),
                (
                    "primitive",
                    model.as_str(),
                    i64::from(mesh.primitive_index),
                    "clustered",
                ),
            ] {
                connection
                    .execute(
                        "INSERT INTO cluster_reference_coverage VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, '', ?7)",
                        params![reference, placed, scope, model_hash, primitive, status, digest],
                    )
                    .unwrap();
            }
        }
        authority
    }

    /// `finalize_base` under the exclusive lock, as a publisher calls it.
    fn finalize(
        directory: &Path,
        authority: Option<&dyn AuthoritativeInputs>,
    ) -> Result<BaseFinal> {
        let lock = AssetLock::acquire_exclusive(directory)?;
        finalize_base(
            &PublishContext::new(&lock, directory, directory)?,
            authority,
        )
    }

    fn manifest(directory: &Path) -> Vec<u8> {
        fs::read(directory.join(CLUSTER_MANIFEST)).unwrap()
    }

    #[test]
    fn an_asset_is_written_once_and_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        write_asset(directory.path(), "clusters/a.bin", b"one").unwrap();
        write_asset(directory.path(), "clusters/a.bin", b"one").unwrap();
        assert!(write_asset(directory.path(), "clusters/a.bin", b"two").is_err());
        assert!(write_asset(directory.path(), "../escape.bin", b"x").is_err());
        assert_eq!(
            fs::read(directory.path().join("clusters/a.bin")).unwrap(),
            b"one"
        );
    }

    #[test]
    fn the_manifest_is_derived_from_the_final_store() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        connection
            .execute(
                "INSERT INTO cluster_rejects(mesh_hash, primitive_index, reason) VALUES ('aa', 1, 'skinned')",
                [],
            )
            .unwrap();
        let first = finalize(directory.path(), None).unwrap();
        assert_eq!((first.summary.meshes, first.summary.blocks), (1, 0));
        assert_eq!(first.summary.rejects["skinned"], 1);
        let before = manifest(directory.path());
        // A later stage adds a block: the manifest follows the store.
        add_block(directory.path(), &connection);
        drop(connection);
        let second = finalize(directory.path(), None).unwrap();
        assert_eq!(second.summary.blocks, 1);
        assert_eq!(second.instances, 3);
        assert_eq!(second.mesh_payload_bytes, MESH.len() as u64);
        assert_ne!(manifest(directory.path()), before);
        // The returned hash of the manifest is the hash of the bytes now on disk.
        let listed = second
            .files
            .iter()
            .find(|file| file.path == CLUSTER_MANIFEST)
            .unwrap();
        assert_eq!(listed.sha256, hash_bytes(&manifest(directory.path())));
    }

    #[test]
    fn a_rerun_on_an_unchanged_store_is_identical() {
        let directory = tempfile::tempdir().unwrap();
        drop(package(directory.path()));
        let first = finalize(directory.path(), None).unwrap();
        // A rerun of the stages writes the same assets again and finalizes again.
        write_asset(directory.path(), MESH_PATH, MESH).unwrap();
        assert_eq!(first, finalize(directory.path(), None).unwrap());
        assert_eq!(
            first
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["cluster-manifest.json", MESH_PATH]
        );
        let mesh = &first.files[1];
        assert_eq!(mesh.bytes, MESH.len() as u64);
        assert_eq!(mesh.sha256, hash_bytes(MESH));
        assert!(
            first
                .tables
                .contains(&"cluster_chunks_spatial_node".to_owned())
        );
        assert!(!first.tables.contains(&"cluster_instance_map".to_owned()));
        assert!(
            !String::from_utf8(manifest(directory.path()))
                .unwrap()
                .contains("instance_map_version")
        );
    }

    #[test]
    fn a_failed_finalize_leaves_the_previous_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_block(directory.path(), &connection);
        finalize(directory.path(), None).unwrap();
        let kept = manifest(directory.path());
        let mesh = directory.path().join(MESH_PATH);
        let block = directory.path().join(block_path());

        // Each damage is refused and none of them touches the manifest; each is undone after.
        let mut flipped = MESH.to_vec();
        flipped[MESH.len() / 2] ^= 1;
        fs::write(&mesh, &flipped).unwrap();
        assert!(
            finalize(directory.path(), None).is_err(),
            "same size, other content"
        );
        fs::write(&mesh, &MESH[..MESH.len() - 8]).unwrap();
        assert!(finalize(directory.path(), None).is_err(), "resized payload");
        fs::remove_file(&mesh).unwrap();
        assert!(finalize(directory.path(), None).is_err(), "missing payload");
        fs::write(&mesh, MESH).unwrap();

        let mut other_block = BLOCK.to_vec();
        *other_block.last_mut().unwrap() ^= 1;
        fs::write(&block, &other_block).unwrap();
        assert!(
            finalize(directory.path(), None).is_err(),
            "instance payload content"
        );
        fs::write(&block, BLOCK).unwrap();

        connection
            .execute(
                "UPDATE cluster_meshes SET vertex_count = vertex_count + 1",
                [],
            )
            .unwrap();
        assert!(
            finalize(directory.path(), None).is_err(),
            "row disagrees with the header"
        );
        connection
            .execute(
                "UPDATE cluster_meshes SET vertex_count = vertex_count - 1",
                [],
            )
            .unwrap();
        connection
            .execute("UPDATE cluster_chunks SET instance_count = 9", [])
            .unwrap();
        assert!(
            finalize(directory.path(), None).is_err(),
            "chunk row disagrees with the header"
        );
        connection
            .execute("UPDATE cluster_chunks SET instance_count = 3", [])
            .unwrap();

        let stray = directory.path().join("clusters/meshes/ab/.cluster-1-0.tmp");
        fs::write(&stray, b"x").unwrap();
        assert!(
            finalize(directory.path(), None).is_err(),
            "unfinished write"
        );
        assert_eq!(manifest(directory.path()), kept);
        fs::remove_file(&stray).unwrap();
        drop(connection);
        finalize(directory.path(), None).unwrap();
    }

    #[test]
    fn unmerged_writes_are_refused_until_the_writer_closes() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap();
        connection
            .execute(
                "INSERT INTO cluster_rejects(mesh_hash, primitive_index, reason) VALUES ('aa', 2, 'skinned')",
                [],
            )
            .unwrap();
        // The insert is committed but sits in the write-ahead log: the file alone is not the view.
        assert!(finalize(directory.path(), None).is_err());
        drop(connection);
        assert_eq!(
            finalize(directory.path(), None).unwrap().summary.rejects["skinned"],
            1
        );
    }

    #[test]
    fn a_valid_mapping_is_declared_in_the_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_block(directory.path(), &connection);
        let authority = add_mapping(&connection);
        drop(connection);
        // Mapping tables with no authority to check them against cannot be certified.
        assert!(finalize(directory.path(), None).is_err());
        assert!(!directory.path().join(CLUSTER_MANIFEST).exists());
        let done = finalize(directory.path(), Some(&authority)).unwrap();
        for table in RESERVED_BASE_TABLES {
            assert!(done.tables.contains(&table.to_owned()));
        }
        for index in [
            "cluster_instance_map_source",
            "cluster_reference_coverage_state",
        ] {
            assert!(done.indexes.contains(&index.to_owned()), "{index}");
        }
        let text = String::from_utf8(manifest(directory.path())).unwrap();
        assert!(text.contains("\"instance_map_version\":1"), "{text}");
        assert!(text.contains("\"coverage_version\":1"), "{text}");
        assert!(text.contains("\"instance_map_rows\":3"), "{text}");
        assert!(text.contains("\"coverage_rows\":6"), "{text}");
        // An authority that disagrees refuses the publication and keeps the manifest.
        let kept = manifest(directory.path());
        let mut wrong = authority.clone();
        wrong.references.get_mut(&1000).unwrap().digest = "00".repeat(32);
        assert!(finalize(directory.path(), Some(&wrong)).is_err());
        assert_eq!(manifest(directory.path()), kept);
    }

    #[test]
    fn a_broken_or_partial_mapping_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_block(directory.path(), &connection);
        let authority = add_mapping(&connection);
        finalize(directory.path(), Some(&authority)).unwrap();
        let kept = manifest(directory.path());
        // A map row that disagrees with its record.
        connection
            .execute(
                "UPDATE cluster_instance_map SET cell_x = cell_x + 1 WHERE ordinal = 1",
                [],
            )
            .unwrap();
        assert!(finalize(directory.path(), Some(&authority)).is_err());
        assert_eq!(manifest(directory.path()), kept);
        connection
            .execute(
                "UPDATE cluster_instance_map SET cell_x = cell_x - 1 WHERE ordinal = 1",
                [],
            )
            .unwrap();
        // Only some of the mapping tables.
        connection
            .execute_batch("DROP TABLE cluster_reference_coverage")
            .unwrap();
        assert!(finalize(directory.path(), Some(&authority)).is_err());
        assert_eq!(manifest(directory.path()), kept);
    }

    #[test]
    fn only_the_exclusive_lock_of_that_destination_may_publish() {
        let directory = tempfile::tempdir().unwrap();
        drop(package(directory.path()));
        let reader = AssetLock::acquire_shared(directory.path()).unwrap();
        assert!(PublishContext::new(&reader, directory.path(), directory.path()).is_err());
        drop(reader);
        let other = tempfile::tempdir().unwrap();
        let elsewhere = AssetLock::acquire_exclusive(other.path()).unwrap();
        assert!(PublishContext::new(&elsewhere, directory.path(), directory.path()).is_err());
        drop(elsewhere);
        assert!(!directory.path().join(CLUSTER_MANIFEST).exists());
        finalize(directory.path(), None).unwrap();
    }

    #[test]
    fn a_staging_folder_is_finalized_under_the_lock_of_its_destination() {
        // The pipeline writes into a staging tree and publishes it to the destination later; the
        // lock guards the destination, the finalizer works in the staging tree.
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("dataset");
        let staging = parent.path().join("dataset.staging");
        fs::create_dir_all(&destination).unwrap();
        fs::create_dir_all(&staging).unwrap();
        drop(package(&staging));
        let lock = AssetLock::acquire_exclusive(&destination).unwrap();
        let context = PublishContext::new(&lock, &destination, &staging).unwrap();
        assert_eq!(context.staging(), staging);
        finalize_base(&context, None).unwrap();
        assert!(staging.join(CLUSTER_MANIFEST).is_file());
        assert!(!destination.join(CLUSTER_MANIFEST).exists());
        assert!(PublishContext::new(&lock, &destination, &parent.path().join("missing")).is_err());
    }

    #[test]
    fn a_spatial_entry_must_match_its_block_exactly() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_block(directory.path(), &connection);
        finalize(directory.path(), None).unwrap();
        let kept = manifest(directory.path());
        for damage in [
            "UPDATE cluster_chunks_spatial SET minX = 0.5",
            "UPDATE cluster_chunks_spatial SET maxY = 2",
            "UPDATE cluster_chunks_spatial SET anchor_x = anchor_x + 16",
            "UPDATE cluster_chunks_spatial SET id = 7",
        ] {
            let backup: (i64, f64, f64, f64, f64, i64, i64, i64, i64) = connection
                .query_row(
                    "SELECT id, minX, maxX, minY, maxY, worldspace_id, tier, anchor_x, anchor_y
                     FROM cluster_chunks_spatial",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                        ))
                    },
                )
                .unwrap();
            connection.execute(damage, []).unwrap();
            assert!(finalize(directory.path(), None).is_err(), "{damage}");
            assert_eq!(manifest(directory.path()), kept);
            connection
                .execute("DELETE FROM cluster_chunks_spatial", [])
                .unwrap();
            connection
                .execute(
                    "INSERT INTO cluster_chunks_spatial(id, minX, maxX, minY, maxY, worldspace_id,
                         tier, anchor_x, anchor_y) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        backup.0, backup.1, backup.2, backup.3, backup.4, backup.5, backup.6,
                        backup.7, backup.8
                    ],
                )
                .unwrap();
        }
        finalize(directory.path(), None).unwrap();
    }

    #[test]
    fn bounds_that_a_float_cannot_hold_are_rounded_outward_exactly() {
        // 0.1 and 0.7 are not 32-bit floats: SQLite stores the minimum rounded down and the maximum
        // rounded up, and the check expects exactly that, with no other tolerance.
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_block(directory.path(), &connection);
        connection
            .execute(
                "UPDATE cluster_chunks SET bounds_min_x = 0.1, bounds_max_x = 0.7,
                     bounds_min_y = -0.3, bounds_max_y = 1e-45",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE cluster_chunks_spatial SET minX = 0.1, maxX = 0.7, minY = -0.3, maxY = 1e-45",
                [],
            )
            .unwrap();
        finalize(directory.path(), None).unwrap();
        // The nearest float rather than the outward one is not what SQLite would have stored.
        assert!(rtree_minimum(0.1) < 0.1 && rtree_maximum(0.7) > 0.7);
        assert_eq!(rtree_minimum(0.5), 0.5);
        assert_eq!(rtree_maximum(0.5), 0.5);
    }

    /// The worldspace of the terrain fixture has a nonzero, negative-x LOD origin, as real
    /// worldspaces do: the header holds the LOD-relative anchor, the row the absolute cell.
    const TERRAIN_ORIGIN: (i32, i32) = (-24, 40);
    const TERRAIN_SIDE: u32 = 16;
    const TERRAIN_HEADER_ANCHOR: (i32, i32) = (2, -1);
    const TERRAIN_CELL: (i32, i32) = (8, 24);

    fn terrain_authority() -> FixedAuthority {
        let mut authority = FixedAuthority::default();
        authority.lod_origins.insert(60, TERRAIN_ORIGIN);
        authority
    }

    /// A terrain block in the shape the terrain stage writes it: an instance file with one
    /// record, an atlas, both named by their content hashes, and the terrain row naming them.
    fn add_terrain(directory: &Path, connection: &Connection) -> (String, String) {
        let view = read_mcl(MESH).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE cluster_terrain (mesh_hash TEXT NOT NULL, primitive_index INTEGER NOT NULL,
                     worldspace_id INTEGER NOT NULL, anchor_x INTEGER NOT NULL, anchor_y INTEGER NOT NULL,
                     side_cells INTEGER NOT NULL, instance_path TEXT NOT NULL, atlas_path TEXT NOT NULL,
                     source_cells TEXT NOT NULL)",
            )
            .unwrap();
        let instance = shared::cluster::write_mci(&shared::cluster::MciInputs {
            builder_identity: view.builder_identity,
            worldspace_id: 60,
            tier: TERRAIN_SIDE,
            anchor_x: TERRAIN_HEADER_ANCHOR.0,
            anchor_y: TERRAIN_HEADER_ANCHOR.1,
            instances: &[shared::cluster::InstanceInput {
                mesh: shared::cluster::MeshRef {
                    mesh_hash: view.mesh_hash,
                    primitive_index: view.primitive_index,
                },
                transform: [
                    1.0,
                    0.0,
                    0.0,
                    (f64::from(TERRAIN_CELL.0) * 4096.0) as f32,
                    0.0,
                    1.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                    -((f64::from(TERRAIN_CELL.1) * 4096.0) as f32),
                ],
                cell_x: TERRAIN_CELL.0,
                cell_y: TERRAIN_CELL.1,
                stable_id: [0; 16],
                base_form_id: 0,
                snow_material: 0,
                source_worldspace_id: 0,
                world_min: [0.0; 3],
                world_max: [1.0; 3],
            }],
        })
        .unwrap();
        let instance_path = format!(
            "clusters/terrain/instances/ab/{}.mci",
            hash_bytes(&instance)
        );
        let atlas = b"atlas bytes".to_vec();
        let atlas_path = format!("clusters/terrain/atlases/cd/{}.ktx2", hash_bytes(&atlas));
        write_asset(directory, &instance_path, &instance).unwrap();
        write_asset(directory, &atlas_path, &atlas).unwrap();
        connection
            .execute(
                "INSERT INTO cluster_terrain VALUES (?1, ?2, 60, ?3, ?4, ?5, ?6, ?7, '')",
                params![
                    hex(&view.mesh_hash),
                    view.primitive_index,
                    TERRAIN_CELL.0,
                    TERRAIN_CELL.1,
                    TERRAIN_SIDE,
                    instance_path,
                    atlas_path
                ],
            )
            .unwrap();
        (instance_path, atlas_path)
    }

    #[test]
    fn terrain_files_must_be_what_the_terrain_table_says() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        let (instance_path, atlas_path) = add_terrain(directory.path(), &connection);
        let authority = terrain_authority();
        finalize(directory.path(), Some(&authority)).unwrap();
        let kept = manifest(directory.path());

        // An orphan file under clusters/terrain.
        write_asset(directory.path(), "clusters/terrain/atlases/ef/x.ktx2", b"x").unwrap();
        assert!(finalize(directory.path(), Some(&authority)).is_err());
        fs::remove_file(directory.path().join("clusters/terrain/atlases/ef/x.ktx2")).unwrap();

        // A row whose file is not named by its hash, or whose content was changed.
        fs::write(directory.path().join(&atlas_path), b"changed").unwrap();
        assert!(finalize(directory.path(), Some(&authority)).is_err());
        fs::write(directory.path().join(&atlas_path), b"atlas bytes").unwrap();
        let mut block = fs::read(directory.path().join(&instance_path)).unwrap();
        *block.last_mut().unwrap() ^= 1;
        fs::write(directory.path().join(&instance_path), block).unwrap();
        assert!(finalize(directory.path(), Some(&authority)).is_err());
        assert_eq!(manifest(directory.path()), kept);
    }

    #[test]
    fn terrain_anchors_follow_the_lod_origin_not_the_header() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_terrain(directory.path(), &connection);
        let authority = terrain_authority();
        // Nonzero, negative origin: header anchor (2, -1) times side 16 from (-24, 40) is (8, 24).
        finalize(directory.path(), Some(&authority)).unwrap();
        let kept = manifest(directory.path());

        // The row holding the LOD-relative anchor (what the header holds) is the old, wrong rule.
        connection
            .execute("UPDATE cluster_terrain SET anchor_x = 2, anchor_y = -1", [])
            .unwrap();
        assert!(finalize(directory.path(), Some(&authority)).is_err());
        connection
            .execute(
                "UPDATE cluster_terrain SET anchor_x = ?1, anchor_y = ?2",
                params![TERRAIN_CELL.0, TERRAIN_CELL.1],
            )
            .unwrap();
        finalize(directory.path(), Some(&authority)).unwrap();

        // Another origin, an origin the data does not have, a row side that differs from the
        // header tier, and no authority at all each refuse.
        let mut other = terrain_authority();
        other.lod_origins.insert(60, (-23, 40));
        assert!(finalize(directory.path(), Some(&other)).is_err());
        other.lod_origins.clear();
        assert!(finalize(directory.path(), Some(&other)).is_err());
        assert!(finalize(directory.path(), None).is_err());
        connection
            .execute("UPDATE cluster_terrain SET side_cells = 8", [])
            .unwrap();
        assert!(finalize(directory.path(), Some(&authority)).is_err());
        connection
            .execute("UPDATE cluster_terrain SET side_cells = 16", [])
            .unwrap();
        assert_eq!(manifest(directory.path()), kept);
    }

    #[test]
    fn terrain_paths_must_be_plain_owned_terrain_paths() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        let (instance_path, atlas_path) = add_terrain(directory.path(), &connection);
        let authority = terrain_authority();
        finalize(directory.path(), Some(&authority)).unwrap();
        let kept = manifest(directory.path());
        // A real file elsewhere in the package, an absolute path, a path that climbs out, and a
        // backslash path may each hold the right bytes, but none is an owned terrain path.
        let outside = directory.path().join("outside.mci");
        fs::copy(directory.path().join(&instance_path), &outside).unwrap();
        for bad in [
            MESH_PATH.to_owned(),
            outside.to_string_lossy().replace('\\', "/"),
            "clusters/terrain/instances/../../../outside.mci".to_owned(),
            instance_path.replace('/', "\\"),
        ] {
            connection
                .execute("UPDATE cluster_terrain SET instance_path = ?1", [&bad])
                .unwrap();
            assert!(
                finalize(directory.path(), Some(&authority)).is_err(),
                "{bad}"
            );
        }
        connection
            .execute(
                "UPDATE cluster_terrain SET instance_path = ?1",
                [&instance_path],
            )
            .unwrap();
        for bad in [MESH_PATH.to_owned(), "/etc/atlas.ktx2".to_owned()] {
            connection
                .execute("UPDATE cluster_terrain SET atlas_path = ?1", [&bad])
                .unwrap();
            assert!(
                finalize(directory.path(), Some(&authority)).is_err(),
                "{bad}"
            );
        }
        connection
            .execute("UPDATE cluster_terrain SET atlas_path = ?1", [&atlas_path])
            .unwrap();
        assert_eq!(manifest(directory.path()), kept);
    }

    #[test]
    fn terrain_files_without_a_terrain_table_are_orphans() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_terrain(directory.path(), &connection);
        connection
            .execute_batch("DROP TABLE cluster_terrain")
            .unwrap();
        assert!(finalize(directory.path(), Some(&terrain_authority())).is_err());
    }

    #[test]
    fn the_core_identity_must_be_current_and_non_empty() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_block(directory.path(), &connection);
        let authority = add_mapping(&connection);
        drop(connection);
        finalize(directory.path(), Some(&authority)).unwrap();
        let kept = manifest(directory.path());
        // Stale, empty and missing core identities each refuse the publication and leave the
        // previous manifest as it was.
        for core in [Some("ee".repeat(32)), Some(String::new()), None] {
            let mut other = authority.clone();
            other.core_build_identity = core.clone();
            assert!(
                finalize(directory.path(), Some(&other)).is_err(),
                "{core:?}"
            );
            assert_eq!(manifest(directory.path()), kept);
        }
        // The builder identity of the converter that publishes must match as well.
        let mut other = authority.clone();
        other.builder_identity = "11".repeat(32);
        assert!(finalize(directory.path(), Some(&other)).is_err());
        assert_eq!(manifest(directory.path()), kept);
    }

    /// The owns lists of the five extensions `opt/one-data-set` writes today
    /// (`converter/src/extensions.rs` at d2dd5d01), copied so the ownership check runs here.
    fn existing_extensions() -> Vec<ExtensionEntry> {
        let entry = |id: &str, tables: &[&str], indexes: &[&str], files: &[&str], gltf: &[&str]| {
            let list = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
            ExtensionEntry {
                id: id.to_owned(),
                version: 1,
                owns: ExtensionOwns {
                    tables: list(tables),
                    indexes: list(indexes),
                    files: list(files),
                    gltf_extensions: list(gltf),
                },
                sha256: String::new(),
                built_from: String::new(),
                stats: BTreeMap::new(),
            }
        };
        vec![
            entry(
                "mudcrab.lod_inventory",
                &[
                    "lod_block",
                    "lod_grid",
                    "lod_tree_instance",
                    "lod_tree_type",
                ],
                &["idx_lod_tree_block"],
                &["meshes/terrain/"],
                &[],
            ),
            entry("mudcrab.door_links", &["door_links"], &[], &[], &[]),
            entry("mudcrab.space_lighting", &["space_lighting"], &[], &[], &[]),
            entry(
                "mudcrab.directional_snow",
                &["matos", "static_materials"],
                &[],
                &[],
                &[],
            ),
            entry(
                "mudcrab.material",
                &[],
                &[],
                &[],
                &["OPEN_SKYRIM_material_animation"],
            ),
        ]
    }

    #[test]
    fn the_extension_entry_is_owned_hashed_and_bound_to_the_core() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_block(directory.path(), &connection);
        let authority = add_mapping(&connection);
        drop(connection);
        let done = finalize(directory.path(), Some(&authority)).unwrap();
        let entry = &done.extension;
        assert_eq!((entry.id.as_str(), entry.version), (CLUSTERS, 1));
        assert_eq!(entry.built_from, "cd".repeat(32));
        assert_eq!(entry.owns.tables, done.tables);
        assert_eq!(entry.owns.indexes, done.indexes);
        assert_eq!(entry.owns.files, ["cluster-manifest.json", "clusters/"]);
        assert!(entry.owns.tables.iter().any(|t| t == "cluster_provenance"));
        assert!(
            entry
                .owns
                .indexes
                .iter()
                .any(|i| i == "cluster_instance_map_source")
        );
        assert_eq!(entry.stats["instance_map_rows"], 3);

        // The hash is the hasher's over the finished package: tables, indexes, and every file
        // under clusters/ plus the manifest as written, found by walking the folder.
        let connection = Connection::open_with_flags(
            directory.path().join("skyrim_world.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut hasher = ExtensionHasher::new(CLUSTERS, 1);
        for table in &entry.owns.tables {
            hasher.add_table(&connection, table).unwrap();
        }
        for index in &entry.owns.indexes {
            hasher.add_index(&connection, index).unwrap();
        }
        let mut walked = vec![CLUSTER_MANIFEST.to_owned()];
        collect_files(
            directory.path(),
            &directory.path().join("clusters"),
            &mut walked,
        )
        .unwrap();
        for path in &walked {
            hasher.add_file(path, &fs::read(directory.path().join(path)).unwrap());
        }
        assert_eq!(entry.sha256, hasher.finish());
        // The manifest on disk is exactly the bytes that were hashed before it was written.
        let written = fs::read(directory.path().join(CLUSTER_MANIFEST)).unwrap();
        let listed = done
            .files
            .iter()
            .find(|f| f.path == CLUSTER_MANIFEST)
            .unwrap();
        assert_eq!(listed.sha256, hash_bytes(&written));
        assert_eq!(listed.bytes, written.len() as u64);

        // A second run on the same store gives the same entry; a changed payload byte or a
        // changed row changes the hash.
        let again = finalize(directory.path(), Some(&authority)).unwrap();
        assert_eq!(again.extension, done.extension);
        let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        connection
            .execute("UPDATE cluster_build SET triangle_threshold = 0", [])
            .unwrap();
        connection
            .execute(
                "UPDATE cluster_meshes SET source_triangles = source_triangles + 1",
                [],
            )
            .unwrap();
        drop(connection);
        let changed = finalize(directory.path(), Some(&authority)).unwrap();
        assert_ne!(changed.extension.sha256, done.extension.sha256);

        // No other extension owns what this one owns, and it owns nothing of theirs.
        let mut all = existing_extensions();
        all.push(done.extension.clone());
        assert_eq!(ownership_problems(&all), Vec::<String>::new());
    }

    #[test]
    fn the_store_must_be_bound_to_a_core_build_identity() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_block(directory.path(), &connection);
        let authority = add_mapping(&connection);
        finalize(directory.path(), Some(&authority)).unwrap();
        let kept = manifest(directory.path());
        // Another core identity than the provenance names, an empty one, and none at all.
        for sql in [
            "UPDATE lod_build SET build_identity = 'ee'",
            "UPDATE lod_build SET build_identity = ''",
            "DELETE FROM lod_build",
            "DROP TABLE lod_build",
        ] {
            connection.execute_batch(sql).unwrap();
            assert!(
                finalize(directory.path(), Some(&authority)).is_err(),
                "{sql}"
            );
            assert_eq!(manifest(directory.path()), kept);
        }
    }

    #[test]
    fn a_finalized_staging_folder_publishes_with_the_same_extension_hash() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("staging");
        let destination = root.path().join("dataset");
        fs::create_dir_all(&staging).unwrap();
        let connection = package(&staging);
        add_block(&staging, &connection);
        let authority = add_mapping(&connection);
        drop(connection);
        let lock = AssetLock::acquire_exclusive(&destination).unwrap();
        let context = PublishContext::new(&lock, &destination, &staging).unwrap();

        // A damaged payload fails the finalization, so nothing is published.
        let block = staging.join(block_path());
        let good = fs::read(&block).unwrap();
        fs::write(&block, b"damaged").unwrap();
        assert!(finalize_base(&context, Some(&authority)).is_err());
        assert!(!staging.join(CLUSTER_MANIFEST).exists());
        fs::write(&block, good).unwrap();

        let done = finalize_base(&context, Some(&authority)).unwrap();
        crate::pipeline::publish_new_directory(&staging, &destination, &lock).unwrap();
        assert!(!staging.exists());

        // The package at the destination hashes to the entry computed in staging.
        let connection = Connection::open_with_flags(
            destination.join("skyrim_world.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut hasher = ExtensionHasher::new(CLUSTERS, BASE_EXTENSION_VERSION);
        for table in &done.extension.owns.tables {
            hasher.add_table(&connection, table).unwrap();
        }
        for index in &done.extension.owns.indexes {
            hasher.add_index(&connection, index).unwrap();
        }
        let mut walked = vec![CLUSTER_MANIFEST.to_owned()];
        collect_files(&destination, &destination.join("clusters"), &mut walked).unwrap();
        for path in &walked {
            hasher.add_file(path, &fs::read(destination.join(path)).unwrap());
        }
        assert_eq!(hasher.finish(), done.extension.sha256);
        assert_eq!(
            core_build_identity(&connection).unwrap().as_deref(),
            Some(done.extension.built_from.as_str())
        );
    }

    #[test]
    fn a_finalized_staging_folder_replaces_a_finalized_destination() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("staging");
        let destination = root.path().join("dataset");

        // The published dataset: a finalized store without the mapping.
        fs::create_dir_all(&destination).unwrap();
        let connection = package(&destination);
        add_block(&destination, &connection);
        drop(connection);
        let old = {
            let lock = AssetLock::acquire_exclusive(&destination).unwrap();
            let context = PublishContext::new(&lock, &destination, &destination).unwrap();
            finalize_base(&context, None).unwrap()
        };

        // An earlier conversion, as the publication requires of a folder it replaces.
        for folder in [&destination, &staging] {
            fs::create_dir_all(folder).unwrap();
            fs::write(
                folder.join("conversion-manifest.json"),
                serde_json::to_vec(&crate::cache::ConversionManifest::default()).unwrap(),
            )
            .unwrap();
        }

        // The next run stages a store with the mapping and finalizes it there.
        fs::create_dir_all(&staging).unwrap();
        let connection = package(&staging);
        add_block(&staging, &connection);
        let authority = add_mapping(&connection);
        drop(connection);
        let new = {
            let lock = AssetLock::acquire_exclusive(&destination).unwrap();
            let context = PublishContext::new(&lock, &destination, &staging).unwrap();
            finalize_base(&context, Some(&authority)).unwrap()
        };
        assert_ne!(old.extension.sha256, new.extension.sha256);
        let staged_manifest = manifest(&staging);

        crate::pipeline::publish_directory(&staging, &destination).unwrap();

        // The destination is the new store, with nothing of the old one left beside it: no backup
        // folder, no publication record.
        assert!(!staging.exists());
        assert_eq!(manifest(&destination), staged_manifest);
        // (The lock files beside it belong to the lock mechanism, not to the data.)
        let leftovers: Vec<String> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| !name.ends_with(".lock"))
            .collect();
        assert_eq!(leftovers, ["dataset"], "{leftovers:?}");
        let connection = Connection::open_with_flags(
            destination.join("skyrim_world.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut hasher = ExtensionHasher::new(CLUSTERS, BASE_EXTENSION_VERSION);
        for table in &new.extension.owns.tables {
            hasher.add_table(&connection, table).unwrap();
        }
        for index in &new.extension.owns.indexes {
            hasher.add_index(&connection, index).unwrap();
        }
        let mut walked = vec![CLUSTER_MANIFEST.to_owned()];
        collect_files(&destination, &destination.join("clusters"), &mut walked).unwrap();
        for path in &walked {
            hasher.add_file(path, &fs::read(destination.join(path)).unwrap());
        }
        assert_eq!(hasher.finish(), new.extension.sha256);
    }

    #[test]
    fn a_benchmark_store_is_marked_and_mints_no_entry() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_block(directory.path(), &connection);
        // A benchmark work folder holds no core data set at all.
        connection.execute_batch("DROP TABLE lod_build").unwrap();
        let lock = AssetLock::acquire_exclusive(directory.path()).unwrap();
        let context = PublishContext::new(&lock, directory.path(), directory.path()).unwrap();
        assert!(finalize_base(&context, None).is_err());
        let done = finalize_benchmark(&context).unwrap();
        assert_eq!(done.summary.blocks, 1);
        let text = String::from_utf8(manifest(directory.path())).unwrap();
        assert!(text.contains("\"uncertified\":true"), "{text}");
        // The benchmark path cannot carry a mapping: that needs the core build.
        let authority = add_mapping(&connection);
        drop(authority);
        assert!(finalize_benchmark(&context).is_err());
    }

    #[test]
    fn a_store_without_mapping_tables_also_needs_a_core_identity() {
        let directory = tempfile::tempdir().unwrap();
        let connection = package(directory.path());
        add_block(directory.path(), &connection);
        // No mapping tables and no authority: the certified path still binds to the core.
        let done = finalize(directory.path(), None).unwrap();
        assert_eq!(done.extension.built_from, "cd".repeat(32));
        let kept = manifest(directory.path());
        for sql in [
            "UPDATE lod_build SET build_identity = ''",
            "DELETE FROM lod_build",
            "DROP TABLE lod_build",
        ] {
            connection.execute_batch(sql).unwrap();
            assert!(finalize(directory.path(), None).is_err(), "{sql}");
            assert_eq!(manifest(directory.path()), kept);
        }
    }

    // Overlay ----------------------------------------------------------------------------------

    /// A base conversion holding only a core database with a build identity.
    fn base_with_core(directory: &Path) {
        let core = Connection::open(directory.join(CORE_DATABASE)).unwrap();
        core.execute_batch(&format!(
            "{LOD_BUILD}; INSERT INTO lod_build VALUES ('{}')",
            "cd".repeat(32)
        ))
        .unwrap();
    }

    #[test]
    fn an_overlay_store_finalizes_to_the_envelope_of_the_same_full_copy_store() {
        let full = tempfile::tempdir().unwrap();
        drop(package(full.path()));
        let expected = finalize(full.path(), None).unwrap();

        let base = tempfile::tempdir().unwrap();
        base_with_core(base.path());
        let before = fs::read(base.path().join(CORE_DATABASE)).unwrap();
        let overlay = tempfile::tempdir().unwrap();
        drop(store(overlay.path(), OVERLAY_STORE));
        let lock = AssetLock::acquire_exclusive(overlay.path()).unwrap();
        let context =
            PublishContext::new_overlay(&lock, overlay.path(), overlay.path(), base.path(), None)
                .unwrap();
        let actual = finalize_base(&context, None).unwrap();

        assert_eq!(
            actual, expected,
            "same content, same envelope, either layout"
        );
        assert_eq!(actual.extension.built_from, "cd".repeat(32));
        assert!(!overlay.path().join(CORE_DATABASE).exists());
        assert_eq!(
            fs::read(base.path().join(CORE_DATABASE)).unwrap(),
            before,
            "the base database is never written"
        );
        let core = Connection::open(base.path().join(CORE_DATABASE)).unwrap();
        let cluster_tables: i64 = core
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'cluster_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cluster_tables, 0);
        assert!(!base.path().join(CLUSTER_MANIFEST).exists());
    }

    #[test]
    fn an_overlay_without_a_core_identity_is_not_certified() {
        let base = tempfile::tempdir().unwrap();
        let core = Connection::open(base.path().join(CORE_DATABASE)).unwrap();
        core.execute_batch(&format!("{LOD_BUILD}; INSERT INTO lod_build VALUES ('')"))
            .unwrap();
        drop(core);
        let overlay = tempfile::tempdir().unwrap();
        drop(store(overlay.path(), OVERLAY_STORE));
        let lock = AssetLock::acquire_exclusive(overlay.path()).unwrap();
        let context =
            PublishContext::new_overlay(&lock, overlay.path(), overlay.path(), base.path(), None)
                .unwrap();
        assert!(finalize_base(&context, None).is_err());
        assert!(!overlay.path().join(CLUSTER_MANIFEST).exists());
    }

    #[test]
    fn an_overlay_context_needs_a_base_a_store_and_separate_folders() {
        let base = tempfile::tempdir().unwrap();
        let overlay = tempfile::tempdir().unwrap();
        let lock = AssetLock::acquire_exclusive(overlay.path()).unwrap();
        let new = |base: &Path, staging: &Path| {
            PublishContext::new_overlay(&lock, overlay.path(), staging, base, None).map(|_| ())
        };
        // No core database in the base, no clusters.db in the staging folder.
        assert!(new(base.path(), overlay.path()).is_err());
        base_with_core(base.path());
        assert!(new(base.path(), overlay.path()).is_err());
        drop(store(overlay.path(), OVERLAY_STORE));
        new(base.path(), overlay.path()).unwrap();
        // The base is never the staging folder.
        drop(store(base.path(), OVERLAY_STORE));
        assert!(new(base.path(), base.path()).is_err());
        let context =
            PublishContext::new_overlay(&lock, overlay.path(), overlay.path(), base.path(), None)
                .unwrap();
        assert_eq!(context.base(), Some(base.path()));
        assert_eq!(context.core_root(), base.path());
        assert_eq!(context.store_path(), overlay.path().join(OVERLAY_STORE));
        assert_eq!(context.terrain_sources(), base.path().join("vfs"));
        let explicit = overlay.path().join("raw");
        let context = PublishContext::new_overlay(
            &lock,
            overlay.path(),
            overlay.path(),
            base.path(),
            Some(&explicit),
        )
        .unwrap();
        assert_eq!(context.terrain_sources(), explicit);
    }

    #[test]
    fn the_attached_core_is_readable_but_never_writable() {
        let root = tempfile::tempdir().unwrap();
        // A folder name with characters that a SQLite URI would otherwise misread.
        let base = root.path().join("base #1 %20");
        fs::create_dir(&base).unwrap();
        base_with_core(&base);
        let overlay = root.path().join("overlay");
        fs::create_dir(&overlay).unwrap();
        let writable = Connection::open(overlay.join(OVERLAY_STORE)).unwrap();
        attach_read_only(&writable, &base.join(CORE_DATABASE), CORE_SCHEMA).unwrap();
        // The unqualified core table resolves through the attachment.
        let identity: String = writable
            .query_row("SELECT build_identity FROM lod_build", [], |row| row.get(0))
            .unwrap();
        assert_eq!(identity, "cd".repeat(32));
        // The cluster store stays writable; the core does not.
        writable.execute_batch("CREATE TABLE mine (a)").unwrap();
        assert!(
            writable
                .execute("UPDATE lod_build SET build_identity = 'x'", [])
                .is_err()
        );
        assert!(
            writable
                .execute_batch("CREATE TABLE base.theirs (a)")
                .is_err()
        );
        assert!(
            attach_read_only(&writable, &base.join("missing.db"), "other").is_err(),
            "a missing file is refused, never created"
        );
        assert!(!base.join("missing.db").exists());
    }

    #[test]
    fn a_sqlite_uri_encodes_every_unsafe_byte() {
        assert_eq!(
            sqlite_read_only_uri(Path::new("/a b/c#d%/e?.db")),
            "file:///a%20b/c%23d%25/e%3F.db?mode=ro"
        );
        assert_eq!(
            sqlite_read_only_uri(Path::new("Q:\\x y\\z.db")),
            "file:///Q:/x%20y/z.db?mode=ro"
        );
        assert_eq!(
            sqlite_read_only_uri(Path::new("\\\\?\\Q:\\x\\z.db")),
            "file:///Q:/x/z.db?mode=ro"
        );
    }
}

#[cfg(test)]
mod inventory_regressions {
    use super::*;

    #[test]
    fn rtree_rounding_matches_sqlite_across_signed_world_coordinates() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE VIRTUAL TABLE bounds USING rtree(id, minX, maxX)")
            .unwrap();
        for value in [
            -1e12, -65536.1, -4096.7, -0.7, -0.3, -1e-45, 0.0, 1e-45, 0.1, 0.7, 4096.7, 65536.1,
            1e12,
        ] {
            db.execute("INSERT OR REPLACE INTO bounds VALUES (1, ?1, ?1)", [value])
                .unwrap();
            let actual: (f64, f64) = db
                .query_row("SELECT minX, maxX FROM bounds", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .unwrap();
            assert_eq!(
                actual,
                (rtree_minimum(value), rtree_maximum(value)),
                "coordinate {value}"
            );
        }
    }

    #[test]
    fn file_inventory_refuses_linked_subdirectories_and_linked_roots() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("foreign.mcl"), b"outside").unwrap();
        let clusters = root.path().join("clusters");
        fs::create_dir(&clusters).unwrap();
        let link = clusters.join("linked");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        #[cfg(windows)]
        assert!(
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(outside.path())
                .output()
                .unwrap()
                .status
                .success()
        );
        let mut files = Vec::new();
        assert!(collect_files(root.path(), &clusters, &mut files).is_err());
        assert!(files.is_empty());
        assert!(collect_files(root.path(), &link, &mut files).is_err());
        // Remove only the link; never traverse the external fixture during cleanup.
        #[cfg(windows)]
        fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        fs::remove_file(&link).unwrap();
        assert!(outside.path().join("foreign.mcl").is_file());
    }
}
