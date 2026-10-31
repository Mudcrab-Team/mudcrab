//! Cluster overlay datasets.
//!
//! A cluster build never copies or edits the converted dataset it is derived from (the *base*). It
//! writes an *overlay* next to it:
//!
//! ```text
//! <base>/derived/clusters-<settings-id>/
//!     overlay.json                 who built this, for which base (OverlayMetadata)
//!     clusters.db                  the cluster tables only
//!     clusters/  cluster-manifest.json  cluster-terrain-manifest.json   payloads and manifests
//! ```
//!
//! The base's `skyrim_world.db`, files and `conversion-manifest.json` stay byte for byte what the
//! base conversion wrote; `derived/` is outside the base fingerprint by construction. The cluster
//! extension entries (`mudcrab.clusters`, `mudcrab.cluster_terrain`) are certified by
//! `overlay.json`'s `extensions`, not by the base manifest.
//!
//! A reader opens the base read-only and attaches `clusters.db` read-only as [`OVERLAY_SCHEMA`], so
//! queries keep their unqualified table names ([`open_overlay_database`]). A cluster stage does the
//! reverse: `clusters.db` is `main` (writable) and the base is attached read-only as [`BASE_SCHEMA`].
//! Every schema introspection in this crate (`core_build_identity`, the extension hasher,
//! `locate_object`) therefore looks at all attached databases and refuses an object that exists in
//! two of them.
//!
//! Binding: an overlay is only used for the base it records. [`checked_overlay`] compares the
//! recorded core identity and base fingerprint with the base on disk before anything is read.
//! Callers hold the asset locks (`AssetLock` shared on the base and on the overlay folder) around
//! these calls; nothing here takes a lock.

use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail, ensure, eyre};
use rusqlite::{Connection, DatabaseName, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cluster::hex;
use crate::extensions::{
    CLUSTER_TERRAIN, CLUSTERS, ExtensionEntry, core_build_identity, ownership_problems,
};

/// `OverlayMetadata::kind` of a cluster overlay.
pub const OVERLAY_KIND: &str = "mudcrab.cluster_overlay";
/// The only `OverlayMetadata::schema_version` this build reads and writes.
pub const OVERLAY_SCHEMA_VERSION: u32 = 1;
/// Folder of a base that holds its overlays.
pub const DERIVED_DIR: &str = "derived";
/// An overlay folder is `clusters-<settings-id>`.
pub const OVERLAY_DIR_PREFIX: &str = "clusters-";
/// A stage builds in `<base>/derived/.staging-<pid>-<time>` and renames it on success.
pub const OVERLAY_STAGING_PREFIX: &str = ".staging-";
pub const OVERLAY_METADATA_FILE: &str = "overlay.json";
pub const OVERLAY_DATABASE_FILE: &str = "clusters.db";
/// Name the overlay database is attached under when the base is the main database.
pub const OVERLAY_SCHEMA: &str = "clusters";
/// Name the base database is attached under when the overlay is the main database.
pub const BASE_SCHEMA: &str = "base";
/// First line of the base fingerprint stream.
pub const FINGERPRINT_ALGORITHM: &str = "mudcrab-overlay-base-fingerprint-v1";

const BASE_DATABASE_FILE: &str = "skyrim_world.db";
const BASE_MANIFEST_FILE: &str = "conversion-manifest.json";

/// `ResolvedBuildSettings::contract` of this build.
pub const SETTINGS_CONTRACT: &str = "mudcrab.cluster-overlay.settings.v1";
/// Hex digits of a settings id: the first 16 of the SHA-256 of the canonical settings JSON.
pub const SETTINGS_ID_LEN: usize = 16;

/// Every setting that decides the bytes of a cluster overlay, resolved to its final value. This
/// object is recorded verbatim in `overlay.json` and is the only input of
/// [`ResolvedBuildSettings::settings_id`]: the hash is of this object's canonical JSON, so the
/// recorded settings and the folder name cannot drift apart.
///
/// Scheduling (`--jobs`), the label, `--replace` and host paths (`terrain_sources`) do not change
/// the output and are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedBuildSettings {
    /// Always [`SETTINGS_CONTRACT`].
    pub contract: String,
    /// The converter build that wrote the overlay.
    pub converter_version: String,
    /// The cluster builder identity of that converter build (64 lowercase hex digits).
    pub builder_identity: String,
    /// `--triangle-threshold`.
    pub triangle_threshold: u32,
    /// `--terrain`.
    pub terrain: bool,
    /// `--include-trees`.
    pub include_trees: bool,
}

impl ResolvedBuildSettings {
    pub fn new(
        converter_version: impl Into<String>,
        builder_identity: impl Into<String>,
        triangle_threshold: u32,
        terrain: bool,
        include_trees: bool,
    ) -> Self {
        Self {
            contract: SETTINGS_CONTRACT.into(),
            converter_version: converter_version.into(),
            builder_identity: builder_identity.into(),
            triangle_threshold,
            terrain,
            include_trees,
        }
    }

    /// Canonical JSON (sorted keys, no whitespace) of exactly the recorded object.
    pub fn canonical_json(&self) -> Vec<u8> {
        let value = serde_json::to_value(self).expect("plain build settings serialise");
        crate::cluster::canonical_json(&value)
    }

    /// The overlay folder id: [`SETTINGS_ID_LEN`] hex digits of the SHA-256 of
    /// [`Self::canonical_json`].
    pub fn settings_id(&self) -> String {
        hex(&Sha256::digest(self.canonical_json()))[..SETTINGS_ID_LEN].to_owned()
    }

    /// Shape check of a recorded or freshly resolved object.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.contract == SETTINGS_CONTRACT,
            "build settings contract is {:?}, but this build reads {SETTINGS_CONTRACT}",
            self.contract
        );
        ensure!(
            !self.converter_version.trim().is_empty(),
            "build settings have an empty converter_version"
        );
        ensure!(
            self.builder_identity.len() == 64
                && self
                    .builder_identity
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
            "build settings builder_identity is not 64 lowercase hex digits"
        );
        ensure!(
            self.triangle_threshold > 0,
            "build settings triangle_threshold must be positive"
        );
        Ok(())
    }

    /// The command that rebuilds an overlay with these settings: the exact flags that resolve to
    /// them. `terrain_sources` is the host path of the raw DDS root a base without its own VFS
    /// needs; it is passed through, never hashed.
    pub fn derive_command(
        &self,
        base: &Path,
        label: &str,
        terrain_sources: Option<&Path>,
    ) -> String {
        let mut command = format!(
            "converter derive-clusters {} --triangle-threshold {} --label {} --replace",
            quote_argument(&command_path(base)),
            self.triangle_threshold,
            quote_argument(label)
        );
        if self.terrain {
            command.push_str(" --terrain");
            if let Some(sources) = terrain_sources {
                command.push_str(" --terrain-sources ");
                command.push_str(&quote_argument(&sources.display().to_string()));
            }
        }
        if self.include_trees {
            command.push_str(" --include-trees");
        }
        command
    }
}

/// `text` as one PowerShell single-quoted argument: nothing inside is expanded (spaces, `$`,
/// backticks and double quotes stay literal) and every apostrophe is doubled. PowerShell also
/// reads the typographic single quotes U+2018 to U+201B as quote characters, so those are doubled
/// too.
pub fn quote_argument(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for character in text.chars() {
        quoted.push(character);
        if matches!(character, '\'' | '\u{2018}'..='\u{201B}') {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

fn command_path(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    }
}

/// `overlay.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayMetadata {
    pub schema_version: u32,
    /// Always [`OVERLAY_KIND`].
    pub kind: String,
    /// Human name chosen with `--label` (default `default`); unique within the base.
    pub label: String,
    /// Mirror of `build_settings.settings_id()` (the folder name suffix); checked equal on read.
    pub settings_id: String,
    /// The resolved build settings the overlay was built with; the only source of `settings_id`.
    pub build_settings: ResolvedBuildSettings,
    /// Legacy recovery location from older overlays, accepted for diagnostic compatibility.
    /// New publishers omit host paths. The core-bound DDS recipe identifies source bytes;
    /// an invocation supplies their location through `--terrain-sources` when no core VFS exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain_sources: Option<PathBuf>,
    /// The base's core build identity (`lod_build.build_identity`) this overlay was built from.
    pub base_core_identity: String,
    /// [`base_fingerprint`] of the base this overlay was built from.
    pub base_fingerprint: String,
    /// Recorded asset conversion failures retained by the base. Runtime admission remains the
    /// engine's incomplete-assets policy; an overlay does not repair or hide these failures.
    #[serde(default)]
    pub base_failure_count: usize,
    /// Mirror of `build_settings.builder_identity`; checked equal on read.
    pub builder_identity: String,
    /// When the overlay was published (free text, RFC 3339 by convention).
    pub created: String,
    /// What this folder is and who may delete it (the `OWNER.txt` text).
    pub owner: String,
    /// The cluster extension entries (`mudcrab.clusters`, optionally `mudcrab.cluster_terrain`): the
    /// envelope certificate. Their `owns` paths are relative to the overlay folder.
    pub extensions: Vec<ExtensionEntry>,
}

impl OverlayMetadata {
    /// The full command that rebuilds this overlay from `base`, reconstructed from the recorded
    /// settings, label and terrain sources.
    pub fn derive_command(&self, base: &Path) -> String {
        let sources = self.terrain_sources.as_deref().or_else(|| {
            (self.build_settings.terrain && !base.join("vfs").is_dir())
                .then_some(Path::new("<raw-dds-root>"))
        });
        self.build_settings
            .derive_command(base, &self.label, sources)
    }

    /// The sentence a refusal of a stale or foreign overlay ends with. A terrain build whose raw
    /// DDS root was a host path (the base has no VFS of its own) adds what the operator must do
    /// about it: the overlay keeps no copy of those bytes, and a temporary root (the full
    /// conversion pipeline's) is gone after a successful derive.
    pub fn rebuild_hint(&self, base: &Path) -> String {
        let mut hint = format!("rebuild it with {}", self.derive_command(base));
        if self.build_settings.terrain {
            if let Some(sources) = &self.terrain_sources {
                hint.push_str(&format!(
                    "; the terrain build read raw sources from {}, which the overlay does not keep: \
                     restore or provide the matching raw DDS sources there, or pass their folder \
                     with --terrain-sources",
                    sources.display()
                ));
            } else if !base.join("vfs").is_dir() {
                hint.push_str(
                    "; replace <raw-dds-root> with the matching raw DDS sources; \
                     their asset keys and hashes must match the base's terrain recipe",
                );
            }
        }
        hint
    }
}

/// One overlay found under a base.
#[derive(Clone, Debug)]
pub struct FoundOverlay {
    pub dir: PathBuf,
    pub metadata: OverlayMetadata,
}

/// The sentence every refusal of a stale or foreign overlay ends with.
pub fn rebuild_command(base: &Path) -> String {
    format!(
        "rebuild it with converter derive-clusters {}",
        quote_argument(&command_path(base))
    )
}

/// `<base>/derived`.
pub fn derived_dir(base: &Path) -> PathBuf {
    base.join(DERIVED_DIR)
}

/// The published folder of the overlay with `settings_id` under `base`.
pub fn overlay_dir(base: &Path, settings_id: &str) -> PathBuf {
    derived_dir(base).join(format!("{OVERLAY_DIR_PREFIX}{settings_id}"))
}

fn regular_file(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "{} is not a regular file",
        path.display()
    );
    Ok(())
}

/// Refuses `<dir>/<database>` when a `-wal`, `-shm` or `-journal` sidecar exists beside it. A
/// sealed dataset has none; SQLite may create or replay sidecars even for a read-only open, so a
/// live one means a writer is running or died, and opening would mutate the dataset. Nothing is
/// checkpointed or removed here.
pub fn refuse_live_sidecars(dir: &Path, database: &str) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let path = dir.join(format!("{database}{suffix}"));
        match fs::symlink_metadata(&path) {
            Ok(_) => bail!(
                "{} exists beside the database: the dataset is not sealed (a writer is running or died). Finish or roll back that writer, then retry; nothing was opened",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        }
    }
    Ok(())
}

fn open_base_read_only(base: &Path) -> Result<Connection> {
    let database = base.join(BASE_DATABASE_FILE);
    regular_file(&database)?;
    refuse_live_sidecars(base, BASE_DATABASE_FILE)?;
    Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {}", database.display()))
}

/// The base's nonempty core build identity.
pub fn base_core_identity(base: &Path) -> Result<String> {
    let connection = open_base_read_only(base)?;
    core_build_identity(&connection)?
        .filter(|identity| !identity.trim().is_empty())
        .ok_or_else(|| {
            eyre!(
                "{} has no core build identity (`lod_build`); it is not a converted dataset",
                base.display()
            )
        })
}

/// The base's identity as an overlay records it: its core identity plus the hash of
/// `conversion-manifest.json`, as 64 lowercase hex digits. Anything the base conversion rewrites
/// (database, extensions, published files) changes the manifest or the core identity; the
/// overlay folder is outside both.
pub fn base_fingerprint(base: &Path) -> Result<String> {
    let identity = base_core_identity(base)?;
    let manifest_path = base.join(BASE_MANIFEST_FILE);
    regular_file(&manifest_path)?;
    let manifest =
        fs::read(&manifest_path).with_context(|| format!("reading {}", manifest_path.display()))?;
    let mut digest = Sha256::new();
    digest.update(FINGERPRINT_ALGORITHM.as_bytes());
    digest.update(b"\n");
    digest.update(identity.as_bytes());
    digest.update(b"\n");
    digest.update(hex(&Sha256::digest(&manifest)).as_bytes());
    digest.update(b"\n");
    Ok(hex(&digest.finalize()))
}

/// Refuses a dataset that carries cluster data inside the base itself (an old full-copy cluster
/// dataset): cluster tables in `skyrim_world.db`, cluster extensions in its manifest, or the cluster
/// manifest at its root (a reserved `cluster_*` view counts like a table). There is no reader for it.
pub fn refuse_full_copy(base: &Path) -> Result<()> {
    // Do not materialize the potentially large asset journal just to inspect extension IDs.
    #[derive(Deserialize)]
    struct ExtensionId {
        id: String,
    }
    #[derive(Deserialize)]
    struct Manifest {
        #[serde(default)]
        extensions: Vec<ExtensionId>,
    }
    let full_copy = || {
        eyre!(
            "{} is an old full-copy cluster dataset (cluster data inside the base conversion); \
             there is no reader for it. Convert the base again, then {}",
            base.display(),
            rebuild_command(base)
        )
    };
    if fs::symlink_metadata(base.join("cluster-manifest.json")).is_ok() {
        return Err(full_copy());
    }
    let connection = open_base_read_only(base)?;
    let tables: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') \
         AND name LIKE 'cluster\\_%' ESCAPE '\\')",
        [],
        |row| row.get(0),
    )?;
    if tables {
        return Err(full_copy());
    }
    let manifest_path = base.join(BASE_MANIFEST_FILE);
    // An unreadable manifest is the caller's own validation to report, not a full-copy verdict.
    if let Some(manifest) = fs::read(&manifest_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
    {
        let claims = manifest
            .extensions
            .iter()
            .any(|entry| matches!(entry.id.as_str(), CLUSTERS | CLUSTER_TERRAIN));
        if claims {
            return Err(full_copy());
        }
    }
    Ok(())
}

/// Reads and validates a published `<overlay>/overlay.json`, including its settings-addressed
/// folder name and certificate versions. Staging directories are not published overlays; producers
/// serialize their metadata before renaming into the final folder. The base binding is
/// [`checked_overlay`]'s.
pub fn read_metadata(overlay: &Path) -> Result<OverlayMetadata> {
    let path = overlay.join(OVERLAY_METADATA_FILE);
    regular_file(&path)?;
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let metadata: OverlayMetadata = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a cluster overlay description", path.display()))?;
    ensure!(
        metadata.kind == OVERLAY_KIND,
        "{} has kind {:?}, not {OVERLAY_KIND}",
        path.display(),
        metadata.kind
    );
    ensure!(
        metadata.schema_version == OVERLAY_SCHEMA_VERSION,
        "{} has overlay schema version {}, but this build reads version {OVERLAY_SCHEMA_VERSION}",
        path.display(),
        metadata.schema_version
    );
    for (name, value) in [
        ("label", &metadata.label),
        ("settings_id", &metadata.settings_id),
        ("base_core_identity", &metadata.base_core_identity),
        ("base_fingerprint", &metadata.base_fingerprint),
        ("builder_identity", &metadata.builder_identity),
    ] {
        ensure!(
            !value.trim().is_empty(),
            "{} has an empty {name}",
            path.display()
        );
    }
    metadata
        .build_settings
        .validate()
        .with_context(|| format!("{} has invalid build settings", path.display()))?;
    ensure!(
        metadata.builder_identity == metadata.build_settings.builder_identity,
        "{} records builder identity {}, but its build settings record {}",
        path.display(),
        metadata.builder_identity,
        metadata.build_settings.builder_identity
    );
    let settings_id = metadata.build_settings.settings_id();
    ensure!(
        metadata.settings_id == settings_id,
        "{} records settings id {}, but its build settings hash to {settings_id}",
        path.display(),
        metadata.settings_id
    );
    let expected_folder = format!("{OVERLAY_DIR_PREFIX}{settings_id}");
    ensure!(
        overlay.file_name() == Some(std::ffi::OsStr::new(&expected_folder)),
        "{} has a folder name that differs from its build settings; expected {expected_folder}",
        overlay.display()
    );
    if let Some(sources) = &metadata.terrain_sources {
        ensure!(
            metadata.build_settings.terrain && !sources.as_os_str().is_empty(),
            "{} records terrain sources for a build without terrain (or an empty path)",
            path.display()
        );
    }
    let problems = ownership_problems(&metadata.extensions);
    ensure!(
        problems.is_empty(),
        "{} has conflicting extension ownership: {}",
        path.display(),
        problems.join("; ")
    );
    let mut ids = std::collections::BTreeSet::new();
    for entry in &metadata.extensions {
        if matches!(entry.id.as_str(), CLUSTERS | CLUSTER_TERRAIN) {
            ensure!(
                entry.version == 1,
                "{} declares {} wrapper version {}, but this build reads version 1",
                path.display(),
                entry.id,
                entry.version
            );
        }
        ensure!(
            ids.insert(entry.id.as_str()),
            "{} lists extension {} twice",
            path.display(),
            entry.id
        );
    }
    ensure!(
        ids.contains(CLUSTERS),
        "{} requires mudcrab.clusters wrapper version 1",
        path.display()
    );
    ensure!(
        metadata.build_settings.terrain == ids.contains(CLUSTER_TERRAIN),
        "{} terrain extension declaration must match its terrain build setting",
        path.display()
    );
    Ok(metadata)
}

/// Every overlay folder (`clusters-*`) under `<base>/derived`, sorted by folder name. A folder whose
/// description cannot be read is an error naming it, never skipped: an unreadable overlay must not
/// make another look like the only one.
pub fn list_overlays(base: &Path) -> Result<Vec<FoundOverlay>> {
    let derived = derived_dir(base);
    let entries = match fs::read_dir(&derived) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", derived.display())),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(OVERLAY_DIR_PREFIX) && entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    dirs.into_iter()
        .map(|dir| {
            let metadata = read_metadata(&dir)
                .with_context(|| format!("cluster overlay {} is unreadable", dir.display()))?;
            Ok(FoundOverlay { dir, metadata })
        })
        .collect()
}

/// The overlay folder `selector` names for `base`.
///
/// * empty: the only overlay under `<base>/derived`; none or several is an error saying so;
/// * otherwise a label, a settings id or a folder name of one overlay under `<base>/derived`
///   (several matches is an error);
/// * otherwise an existing folder holding `overlay.json`, relative to the working directory or to
///   `base`.
///
/// This only finds the folder. [`checked_overlay`] decides whether it belongs to `base`.
pub fn resolve_overlay(base: &Path, selector: &str) -> Result<PathBuf> {
    refuse_full_copy(base)?;
    let selector = selector.trim();
    if selector.is_empty() {
        let overlays = list_overlays(base)?;
        return match overlays.as_slice() {
            [only] => Ok(only.dir.clone()),
            [] => bail!(
                "no cluster overlay under {}; build one with converter derive-clusters {}",
                derived_dir(base).display(),
                base.display()
            ),
            several => bail!(
                "{} cluster overlays under {}; choose one with --clusters <label|settings-id|path>: {}",
                several.len(),
                derived_dir(base).display(),
                describe(several)
            ),
        };
    }
    // Retain direct candidates as a fallback after matching readable labels and ids, preserving
    // their established precedence. A selected unreadable description reports its own error.
    let mut candidates = Vec::new();
    if selector.len() == SETTINGS_ID_LEN && selector.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        candidates.push(overlay_dir(base, selector));
    }
    if selector.starts_with(OVERLAY_DIR_PREFIX) {
        candidates.push(derived_dir(base).join(selector));
    }
    for candidate in [PathBuf::from(selector), base.join(selector)] {
        if candidate.join(OVERLAY_METADATA_FILE).is_file() {
            candidates.push(candidate);
        }
    }
    // Labels require an inventory, but unreadable descriptions cannot match a known label.
    // Bare selection above remains strict: it must never mistake another overlay for the only one.
    let mut overlays = Vec::new();
    if derived_dir(base).is_dir() {
        for entry in fs::read_dir(derived_dir(base))? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(OVERLAY_DIR_PREFIX))
                && let Ok(metadata) = read_metadata(&entry.path())
            {
                overlays.push(FoundOverlay {
                    dir: entry.path(),
                    metadata,
                });
            }
        }
    }
    overlays.sort_by(|left, right| left.dir.cmp(&right.dir));
    let matching: Vec<&FoundOverlay> = overlays
        .iter()
        .filter(|found| {
            found.metadata.label == selector
                || found.metadata.settings_id == selector
                || found
                    .dir
                    .file_name()
                    .is_some_and(|name| name.to_str() == Some(selector))
        })
        .collect();
    match matching.as_slice() {
        [one] => return Ok(one.dir.clone()),
        [] => {}
        several => bail!(
            "--clusters {selector} matches {} overlays under {}: {}; use the settings id or the folder path",
            several.len(),
            derived_dir(base).display(),
            describe(several.iter().copied())
        ),
    }
    for candidate in candidates {
        if candidate.is_dir() {
            read_metadata(&candidate)?;
            return Ok(candidate);
        }
    }
    bail!(
        "no cluster overlay {selector:?} for {}; found: {}",
        base.display(),
        if overlays.is_empty() {
            "none".to_owned()
        } else {
            describe(&overlays)
        }
    )
}

fn describe<'a>(overlays: impl IntoIterator<Item = &'a FoundOverlay>) -> String {
    overlays
        .into_iter()
        .map(|found| {
            format!(
                "{} (label {}, settings id {})",
                found.dir.display(),
                found.metadata.label,
                found.metadata.settings_id
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Checks that `overlay` was built for exactly this conversion of `base` and returns its
/// description. Refuses, naming the rebuild command: an old full-copy base, an overlay whose
/// recorded core identity or base fingerprint is not the base's now, an overlay whose cluster
/// extensions were derived from another core build, a missing `clusters.db`.
pub fn checked_overlay(base: &Path, overlay: &Path) -> Result<OverlayMetadata> {
    refuse_full_copy(base)?;
    let metadata = read_metadata(overlay)?;
    regular_file(&overlay.join(OVERLAY_DATABASE_FILE))?;
    refuse_live_sidecars(overlay, OVERLAY_DATABASE_FILE)?;
    let foreign = || {
        eyre!(
            "this cluster overlay was built for another conversion of this base; {}",
            metadata.rebuild_hint(base)
        )
    };
    let identity = base_core_identity(base)?;
    if metadata.base_core_identity != identity {
        return Err(foreign());
    }
    if metadata.base_fingerprint != base_fingerprint(base)? {
        return Err(foreign());
    }
    for entry in &metadata.extensions {
        if entry.built_from != identity {
            return Err(foreign());
        }
    }
    Ok(metadata)
}

/// A process-local receipt of one complete binding check. Callers retain the base and overlay
/// reader locks for its lifetime. Later connections reuse these checked identities without
/// re-reading the manifest, but still compare the core identity on their own base connection.
#[derive(Debug)]
pub struct OverlayBindingReceipt {
    base: PathBuf,
    overlay: PathBuf,
    metadata: OverlayMetadata,
}

impl OverlayBindingReceipt {
    pub fn check(base: &Path, overlay: &Path) -> Result<Self> {
        let metadata = checked_overlay(base, overlay)?;
        Ok(Self {
            base: fs::canonicalize(base)?,
            overlay: fs::canonicalize(overlay)?,
            metadata,
        })
    }

    pub fn metadata(&self) -> &OverlayMetadata {
        &self.metadata
    }

    /// Opens a fresh read-only connection. The cheap core query runs before ATTACH, so a base
    /// swapped under a running engine is refused before any overlay database read.
    pub fn connection(&self) -> Result<Connection> {
        let connection = open_base_read_only(&self.base)?;
        // The full check already established the base schema. Qualify main explicitly: this is
        // one cheap query on this connection, and no overlay can answer it.
        let identity = connection
            .query_row(
                "SELECT build_identity FROM main.lod_build LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .wrap_err("reading the base core identity for the cluster binding receipt")?;
        ensure!(
            identity.as_deref() == Some(self.metadata.base_core_identity.as_str()),
            "this cluster overlay was built for another conversion of this base; {}",
            self.metadata.rebuild_hint(&self.base)
        );
        regular_file(&self.overlay.join(OVERLAY_DATABASE_FILE))?;
        refuse_live_sidecars(&self.overlay, OVERLAY_DATABASE_FILE)?;
        attach_read_only(
            &connection,
            &self.overlay.join(OVERLAY_DATABASE_FILE),
            OVERLAY_SCHEMA,
        )?;
        ensure!(
            connection.is_readonly(DatabaseName::Main)?,
            "the cluster overlay connection is not read-only"
        );
        Ok(connection)
    }
}

/// `path` as a SQLite read-only URI (`file:///Q:/a%20b/c.db?mode=ro`): every byte outside the
/// unreserved set is percent-encoded, a Windows verbatim prefix is dropped.
pub fn sqlite_read_only_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    // A Windows verbatim prefix (backslash backslash ? backslash) is not part of a SQLite file name.
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                uri.push(char::from(byte));
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri.push_str("?mode=ro");
    uri
}

/// Attaches the existing database file `path` to `connection` as `schema`, read-only by URI
/// (`mode=ro`), and checks SQLite agrees the schema is read-only.
pub fn attach_read_only(connection: &Connection, path: &Path, schema: &str) -> Result<()> {
    ensure!(
        schema
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "{schema} is not a plain schema name"
    );
    let absolute = std::path::absolute(path)?;
    ensure!(absolute.is_file(), "{} does not exist", absolute.display());
    connection
        .execute(
            &format!("ATTACH DATABASE ?1 AS {schema}"),
            [sqlite_read_only_uri(&absolute)],
        )
        .with_context(|| format!("attaching {} read-only", absolute.display()))?;
    ensure!(
        connection.is_readonly(DatabaseName::Attached(schema))?,
        "{} was not attached read-only",
        absolute.display()
    );
    Ok(())
}

/// Opens the base read-only and attaches the overlay's `clusters.db` read-only as
/// [`OVERLAY_SCHEMA`] (a `mode=ro` URI), after [`checked_overlay`] accepted the pair. Cluster
/// tables keep their unqualified names; a table of the same name in both databases is refused
/// where it is looked up (`extensions::locate_object`). The returned connection cannot write
/// either database.
pub fn open_overlay_database(base: &Path, overlay: &Path) -> Result<Connection> {
    OverlayBindingReceipt::check(base, overlay)?.connection()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::ExtensionOwns;
    use std::collections::BTreeMap;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "shared-cluster-overlay-{label}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();
            fs::write(
                root.join("OWNER.txt"),
                "shared cluster_overlay test fixture\n",
            )
            .unwrap();
            Self(root)
        }

        fn base(&self) -> PathBuf {
            self.0.join("base")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn make_base(scratch: &Scratch, identity: &str) -> PathBuf {
        let base = scratch.base();
        fs::create_dir_all(&base).unwrap();
        let connection = Connection::open(base.join(BASE_DATABASE_FILE)).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE lod_build(id INTEGER, build_identity TEXT);
                 INSERT INTO lod_build VALUES (1, '{identity}');
                 CREATE TABLE statics(id INTEGER);"
            ))
            .unwrap();
        fs::write(base.join(BASE_MANIFEST_FILE), br#"{"extensions":[]}"#).unwrap();
        base
    }

    /// Settings that differ only by `threshold`, so each threshold is a distinct overlay folder.
    fn settings(threshold: u32) -> ResolvedBuildSettings {
        ResolvedBuildSettings::new("0.0.0-test", "ab".repeat(32), threshold, false, false)
    }

    fn id(threshold: u32) -> String {
        settings(threshold).settings_id()
    }

    fn make_overlay(base: &Path, label: &str, threshold: u32) -> PathBuf {
        let build_settings = settings(threshold);
        let dir = overlay_dir(base, &build_settings.settings_id());
        fs::create_dir_all(&dir).unwrap();
        let identity = base_core_identity(base).unwrap();
        let metadata = OverlayMetadata {
            schema_version: OVERLAY_SCHEMA_VERSION,
            kind: OVERLAY_KIND.into(),
            label: label.into(),
            settings_id: build_settings.settings_id(),
            build_settings,
            terrain_sources: None,
            base_core_identity: identity.clone(),
            base_fingerprint: base_fingerprint(base).unwrap(),
            base_failure_count: 0,
            builder_identity: "ab".repeat(32),
            created: "2026-10-20T00:00:00Z".into(),
            owner: "test fixture".into(),
            extensions: vec![ExtensionEntry {
                id: CLUSTERS.into(),
                version: 1,
                owns: ExtensionOwns {
                    tables: vec!["cluster_build".into()],
                    ..Default::default()
                },
                sha256: "00".repeat(32),
                built_from: identity,
                stats: BTreeMap::new(),
            }],
        };
        fs::write(
            dir.join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        let connection = Connection::open(dir.join(OVERLAY_DATABASE_FILE)).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE cluster_build(id INTEGER); INSERT INTO cluster_build VALUES (1);",
            )
            .unwrap();
        dir
    }

    /// Rewrites `overlay.json` without validation, as a damaged or hand-edited file would be.
    fn edit_metadata(dir: &Path, edit: impl FnOnce(&mut OverlayMetadata)) {
        let mut metadata = read_metadata(dir).unwrap();
        edit(&mut metadata);
        fs::write(
            dir.join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn an_overlay_requires_the_cluster_geometry_certificate() {
        for terrain_only in [false, true] {
            let scratch = Scratch::new("missing-certificate");
            let base = make_base(&scratch, "core-a");
            let overlay = make_overlay(&base, "default", 1);
            edit_metadata(&overlay, |metadata| {
                if terrain_only {
                    metadata.extensions[0].id = CLUSTER_TERRAIN.into();
                } else {
                    metadata.extensions.clear();
                }
            });
            let error = format!("{:#}", open_overlay_database(&base, &overlay).unwrap_err());
            assert!(error.contains("requires mudcrab.clusters"), "{error}");
        }
    }

    #[test]
    fn overlay_certificates_reject_unsupported_wrapper_versions() {
        for extension in [CLUSTERS, CLUSTER_TERRAIN] {
            for version in [0, 2] {
                let scratch = Scratch::new("certificate-version");
                let base = make_base(&scratch, "core-a");
                let overlay = make_overlay(&base, "default", 1);
                edit_metadata(&overlay, |metadata| {
                    if extension == CLUSTER_TERRAIN {
                        let mut terrain = metadata.extensions[0].clone();
                        terrain.id = CLUSTER_TERRAIN.into();
                        terrain.owns = ExtensionOwns::default();
                        metadata.extensions.push(terrain);
                    }
                    metadata
                        .extensions
                        .iter_mut()
                        .find(|entry| entry.id == extension)
                        .unwrap()
                        .version = version;
                });
                let error = format!("{:#}", open_overlay_database(&base, &overlay).unwrap_err());
                assert!(
                    error.contains(extension) && error.contains("wrapper version"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn a_supported_terrain_certificate_can_accompany_cluster_geometry() {
        let scratch = Scratch::new("terrain-certificate");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        edit_metadata(&overlay, |metadata| {
            let mut terrain = metadata.extensions[0].clone();
            terrain.id = CLUSTER_TERRAIN.into();
            terrain.owns = ExtensionOwns::default();
            metadata.extensions.push(terrain);
            metadata.build_settings.terrain = true;
            metadata.settings_id = metadata.build_settings.settings_id();
        });
        let metadata: OverlayMetadata =
            serde_json::from_slice(&fs::read(overlay.join(OVERLAY_METADATA_FILE)).unwrap())
                .unwrap();
        let published = overlay_dir(&base, &metadata.settings_id);
        fs::rename(&overlay, &published).unwrap();
        open_overlay_database(&base, &published).unwrap();
    }

    #[test]
    fn terrain_settings_and_extension_declarations_must_agree() {
        for terrain in [false, true] {
            let scratch = Scratch::new("terrain-mismatch");
            let base = make_base(&scratch, "core-a");
            let overlay = make_overlay(&base, "default", 1);
            let mut metadata = read_metadata(&overlay).unwrap();
            metadata.build_settings.terrain = terrain;
            metadata.settings_id = metadata.build_settings.settings_id();
            if !terrain {
                let mut entry = metadata.extensions[0].clone();
                entry.id = CLUSTER_TERRAIN.into();
                entry.owns = ExtensionOwns::default();
                metadata.extensions.push(entry);
            }
            let renamed = overlay_dir(&base, &metadata.settings_id);
            fs::write(
                overlay.join(OVERLAY_METADATA_FILE),
                serde_json::to_vec(&metadata).unwrap(),
            )
            .unwrap();
            if overlay != renamed {
                fs::rename(&overlay, &renamed).unwrap();
            }
            let error = format!("{:#}", read_metadata(&renamed).unwrap_err());
            assert!(
                error.contains("terrain extension") && error.contains("terrain build setting"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_renamed_overlay_cannot_impersonate_another_settings_variant() {
        let scratch = Scratch::new("renamed-settings");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let renamed = overlay_dir(&base, &id(2));
        fs::rename(&overlay, &renamed).unwrap();
        let error = format!("{:#}", read_metadata(&renamed).unwrap_err());
        assert!(
            error.contains("folder name") && error.contains(&id(1)),
            "{error}"
        );
        assert!(resolve_overlay(&base, &id(2)).is_err());
        assert!(open_overlay_database(&base, &renamed).is_err());
    }

    #[test]
    fn staged_metadata_is_readable_only_after_publication_to_its_settings_folder() {
        let scratch = Scratch::new("staged-settings");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let staging = derived_dir(&base).join(format!("{OVERLAY_STAGING_PREFIX}123-456"));
        fs::rename(&overlay, &staging).unwrap();
        // Producers serialize metadata in staging without admitting it as a published overlay.
        let metadata: OverlayMetadata =
            serde_json::from_slice(&fs::read(staging.join(OVERLAY_METADATA_FILE)).unwrap())
                .unwrap();
        assert_eq!(metadata.settings_id, id(1));
        assert!(read_metadata(&staging).is_err());
        assert!(open_overlay_database(&base, &staging).is_err());
        fs::rename(&staging, &overlay).unwrap();
        open_overlay_database(&base, &overlay).unwrap();
    }

    #[test]
    fn the_settings_id_is_the_hash_of_the_exact_recorded_object() {
        let all = ResolvedBuildSettings::new("1.2.3", "ab".repeat(32), 1024, true, false);
        let expected = format!(
            "{{\"builder_identity\":\"{}\",\"contract\":\"mudcrab.cluster-overlay.settings.v1\",\
             \"converter_version\":\"1.2.3\",\"include_trees\":false,\"terrain\":true,\
             \"triangle_threshold\":1024}}",
            "ab".repeat(32)
        );
        assert_eq!(all.canonical_json(), expected.as_bytes());
        let digest = hex(&Sha256::digest(expected.as_bytes()));
        assert_eq!(all.settings_id(), digest[..SETTINGS_ID_LEN]);
        assert_eq!(all.settings_id().len(), 16);
        // Round trip through JSON keeps the id; the object has no scheduling or host field.
        let json = serde_json::to_value(&all).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "builder_identity",
                "contract",
                "converter_version",
                "include_trees",
                "terrain",
                "triangle_threshold"
            ]
        );
        let again: ResolvedBuildSettings = serde_json::from_value(json).unwrap();
        assert_eq!(again.settings_id(), all.settings_id());
        // Every field changes the id.
        let base = all.settings_id();
        let mut other = all.clone();
        other.triangle_threshold += 1;
        assert_ne!(other.settings_id(), base);
        let mut other = all.clone();
        other.terrain = false;
        assert_ne!(other.settings_id(), base);
        let mut other = all.clone();
        other.include_trees = true;
        assert_ne!(other.settings_id(), base);
        let mut other = all.clone();
        other.converter_version = "1.2.4".into();
        assert_ne!(other.settings_id(), base);
        let mut other = all;
        other.builder_identity = "cd".repeat(32);
        assert_ne!(other.settings_id(), base);
        // An unknown field is refused instead of silently dropped from the hash.
        let unknown = serde_json::json!({
            "contract": SETTINGS_CONTRACT, "converter_version": "1", "builder_identity": "ab".repeat(32),
            "triangle_threshold": 1, "terrain": false, "include_trees": false, "jobs": 2
        });
        assert!(serde_json::from_value::<ResolvedBuildSettings>(unknown).is_err());
    }

    #[test]
    fn the_rebuild_command_carries_every_flag_the_settings_resolve_from() {
        let base = Path::new("Q:\\Mods\\My Base");
        let plain = ResolvedBuildSettings::new("1", "ab".repeat(32), 1024, false, false);
        assert_eq!(
            plain.derive_command(base, "default", None),
            "converter derive-clusters 'Q:\\Mods\\My Base' --triangle-threshold 1024 \
             --label 'default' --replace"
        );
        let all = ResolvedBuildSettings::new("1", "ab".repeat(32), 512, true, true);
        assert_eq!(
            all.derive_command(base, "dense", Some(Path::new("E:\\raw dds"))),
            "converter derive-clusters 'Q:\\Mods\\My Base' --triangle-threshold 512 \
             --label 'dense' --replace --terrain --terrain-sources 'E:\\raw dds' --include-trees"
        );
        // Terrain sources are only meaningful for a terrain build; no scheduling flag is invented.
        assert!(
            !plain
                .derive_command(base, "x", Some(Path::new("E:\\raw")))
                .contains("--terrain")
        );
        assert!(
            !all.derive_command(base, "x", None)
                .contains("--terrain-sources")
        );
        assert!(!all.derive_command(base, "x", None).contains("--jobs"));
    }

    #[test]
    fn rebuild_command_arguments_are_powershell_single_quoted() {
        let build_settings = settings(1);
        let command = build_settings.derive_command(
            Path::new("Q:\\it's $base"),
            "a 'b' `c` \"d\" $e \u{2019}f",
            Some(Path::new("unused")),
        );
        assert!(command.contains("'Q:\\it''s $base'"), "{command}");
        assert!(
            command.contains("--label 'a ''b'' `c` \"d\" $e \u{2019}\u{2019}f'"),
            "{command}"
        );
        // Settings and label are independent of the quoting: the id never sees the label.
        assert_eq!(build_settings.settings_id(), id(1));
        assert_eq!(command_path(Path::new(r"\\?\Q:\base")), r"Q:\base");
        assert_eq!(
            command_path(Path::new(r"\\?\UNC\host\share\base")),
            r"\\host\share\base"
        );
    }

    #[test]
    fn portable_terrain_retry_requires_matching_sources_without_recording_a_host_path() {
        let scratch = Scratch::new("portable-terrain-retry");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "portable", 7);
        let mut metadata = read_metadata(&overlay).unwrap();
        metadata.build_settings.terrain = true;
        assert!(metadata.terrain_sources.is_none());
        let command = metadata.derive_command(&base);
        assert!(
            command.contains("--terrain-sources '<raw-dds-root>'"),
            "{command}"
        );
        let hint = metadata.rebuild_hint(&base);
        assert!(hint.contains("matching raw DDS sources"), "{hint}");
        fs::create_dir(base.join("vfs")).unwrap();
        assert!(!metadata.derive_command(&base).contains("--terrain-sources"));
    }

    #[test]
    fn metadata_records_settings_and_reconstructs_the_exact_retry() {
        let scratch = Scratch::new("retry");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "it's dense", 7);
        let mut build_settings = settings(7);
        build_settings.terrain = true;
        build_settings.include_trees = true;
        let published = overlay_dir(&base, &build_settings.settings_id());
        edit_metadata(&overlay, |metadata| {
            metadata.build_settings = build_settings;
            metadata.settings_id = metadata.build_settings.settings_id();
            metadata.terrain_sources = Some(PathBuf::from("E:\\raw dds"));
            let mut terrain = metadata.extensions[0].clone();
            terrain.id = CLUSTER_TERRAIN.into();
            terrain.owns = ExtensionOwns::default();
            metadata.extensions.push(terrain);
        });
        fs::rename(&overlay, &published).unwrap();
        let overlay = published;
        let metadata = read_metadata(&overlay).unwrap();
        assert_eq!(metadata.settings_id, metadata.build_settings.settings_id());
        assert_eq!(
            metadata.derive_command(&base),
            format!(
                "converter derive-clusters '{}' --triangle-threshold 7 --label 'it''s dense' \
                 --replace --terrain --terrain-sources 'E:\\raw dds' --include-trees",
                base.display()
            )
        );
        // The binding refusal ends with that exact command.
        fs::write(base.join(BASE_MANIFEST_FILE), br#"{"extensions":[],"x":1}"#).unwrap();
        let error = format!("{:#}", checked_overlay(&base, &overlay).unwrap_err());
        assert!(
            error.contains("another conversion of this base")
                && error.contains(&format!(
                    "rebuild it with {}",
                    metadata.derive_command(&base)
                ))
                && error.contains("raw sources from E:\\raw dds")
                && error.contains("restore or provide the matching raw DDS sources"),
            "{error}"
        );
        // Portable metadata requires a matching source root to be supplied explicitly.
        edit_metadata(&overlay, |metadata| metadata.terrain_sources = None);
        let error = format!("{:#}", checked_overlay(&base, &overlay).unwrap_err());
        let portable = read_metadata(&overlay).unwrap();
        assert!(error.ends_with(&portable.rebuild_hint(&base)), "{error}");
        assert!(
            error.contains("--terrain-sources '<raw-dds-root>'"),
            "{error}"
        );
        assert!(error.contains("matching raw DDS sources"), "{error}");
        // terrain_sources is optional on the wire and absent when unset.
        let text = fs::read_to_string(make_overlay(&base, "plain", 8).join(OVERLAY_METADATA_FILE))
            .unwrap();
        assert!(!text.contains("terrain_sources"), "{text}");
    }

    #[test]
    fn inconsistent_or_incomplete_settings_metadata_is_refused() {
        let scratch = Scratch::new("settings-metadata");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let refuse = |what: &str, edit: &dyn Fn(&mut OverlayMetadata), needle: &str| {
            let backup = fs::read(overlay.join(OVERLAY_METADATA_FILE)).unwrap();
            edit_metadata(&overlay, |metadata| edit(metadata));
            let error = format!("{:#}", read_metadata(&overlay).unwrap_err());
            assert!(error.contains(needle), "{what}: {error}");
            assert!(checked_overlay(&base, &overlay).is_err(), "{what}");
            fs::write(overlay.join(OVERLAY_METADATA_FILE), backup).unwrap();
            assert!(read_metadata(&overlay).is_ok(), "{what}: restore");
        };
        refuse(
            "settings id differs from the hashed settings",
            &|metadata| metadata.settings_id = "0123456789abcdef".into(),
            "settings id",
        );
        refuse(
            "settings changed without the id",
            &|metadata| metadata.build_settings.triangle_threshold = 2,
            "settings id",
        );
        refuse(
            "builder identity mirror differs",
            &|metadata| metadata.builder_identity = "cd".repeat(32),
            "builder identity",
        );
        refuse(
            "builder identity not lowercase hex",
            &|metadata| {
                metadata.build_settings.builder_identity = "AB".repeat(32);
                metadata.builder_identity = "AB".repeat(32);
                metadata.settings_id = metadata.build_settings.settings_id();
            },
            "builder_identity",
        );
        refuse(
            "zero threshold",
            &|metadata| {
                metadata.build_settings.triangle_threshold = 0;
                metadata.settings_id = metadata.build_settings.settings_id();
            },
            "triangle_threshold",
        );
        refuse(
            "other contract",
            &|metadata| {
                metadata.build_settings.contract = "other".into();
                metadata.settings_id = metadata.build_settings.settings_id();
            },
            "contract",
        );
        refuse(
            "empty converter version",
            &|metadata| {
                metadata.build_settings.converter_version = " ".into();
                metadata.settings_id = metadata.build_settings.settings_id();
            },
            "converter_version",
        );
        refuse(
            "terrain sources without terrain",
            &|metadata| metadata.terrain_sources = Some(PathBuf::from("E:\\raw")),
            "terrain sources",
        );
        // An overlay.json without the required build settings is not read.
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(overlay.join(OVERLAY_METADATA_FILE)).unwrap())
                .unwrap();
        value.as_object_mut().unwrap().remove("build_settings");
        fs::write(
            overlay.join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let error = format!("{:#}", read_metadata(&overlay).unwrap_err());
        assert!(error.contains("build_settings"), "{error}");
        assert!(list_overlays(&base).is_err());
    }

    #[test]
    fn the_overlay_folder_name_is_the_settings_id() {
        let scratch = Scratch::new("folder-id");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 5);
        let found = list_overlays(&base).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir, overlay);
        assert_eq!(
            overlay.file_name().unwrap().to_str().unwrap(),
            format!(
                "{OVERLAY_DIR_PREFIX}{}",
                found[0].metadata.build_settings.settings_id()
            )
        );
    }

    #[test]
    fn fingerprint_follows_core_identity_and_manifest_bytes_only() {
        let scratch = Scratch::new("fingerprint");
        let base = make_base(&scratch, "core-a");
        let first = base_fingerprint(&base).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(first, base_fingerprint(&base).unwrap());
        // The derived folder is outside the fingerprint.
        make_overlay(&base, "default", 1);
        assert_eq!(first, base_fingerprint(&base).unwrap());
        fs::write(base.join(BASE_MANIFEST_FILE), br#"{"extensions":[],"x":1}"#).unwrap();
        assert_ne!(first, base_fingerprint(&base).unwrap());
    }

    #[test]
    fn a_bound_overlay_opens_with_both_databases_read_only() {
        let scratch = Scratch::new("open");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let metadata = checked_overlay(&base, &overlay).unwrap();
        assert_eq!(metadata.label, "default");
        let connection = open_overlay_database(&base, &overlay).unwrap();
        // Unqualified names reach both databases.
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM cluster_build", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(
            core_build_identity(&connection).unwrap().as_deref(),
            Some("core-a")
        );
        // A write to either database fails.
        assert!(
            connection
                .execute("INSERT INTO clusters.cluster_build VALUES (2)", [])
                .is_err()
        );
        assert!(
            connection
                .execute("INSERT INTO statics VALUES (2)", [])
                .is_err()
        );
        assert!(
            connection
                .execute("CREATE TABLE clusters.extra(x)", [])
                .is_err()
        );
    }

    #[test]
    fn an_overlay_of_another_conversion_or_manifest_is_refused_with_the_rebuild_command() {
        let scratch = Scratch::new("tamper");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        // Manifest bytes changed: the fingerprint no longer matches.
        fs::write(base.join(BASE_MANIFEST_FILE), br#"{"extensions":[],"x":1}"#).unwrap();
        let error = format!("{:#}", checked_overlay(&base, &overlay).unwrap_err());
        assert!(
            error.contains("built for another conversion of this base")
                && error.contains("converter derive-clusters"),
            "{error}"
        );
        assert!(open_overlay_database(&base, &overlay).is_err());
        // Core identity changed (manifest restored): refused as well.
        fs::write(base.join(BASE_MANIFEST_FILE), br#"{"extensions":[]}"#).unwrap();
        assert!(checked_overlay(&base, &overlay).is_ok());
        let connection = Connection::open(base.join(BASE_DATABASE_FILE)).unwrap();
        connection
            .execute("UPDATE lod_build SET build_identity = 'core-b'", [])
            .unwrap();
        drop(connection);
        let error = format!("{:#}", checked_overlay(&base, &overlay).unwrap_err());
        assert!(error.contains("another conversion of this base"), "{error}");
        // Extension entries derived from another core are refused too.
        let other = Scratch::new("other");
        let other_base = make_base(&other, "core-b");
        let stale = make_overlay(&other_base, "default", 1);
        let mut metadata = read_metadata(&stale).unwrap();
        metadata.extensions[0].built_from = "core-a".into();
        fs::write(
            stale.join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        assert!(checked_overlay(&other_base, &stale).is_err());
    }

    #[test]
    fn an_overlay_copied_to_another_base_is_refused() {
        let scratch = Scratch::new("foreign");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let other = Scratch::new("foreign-other");
        let other_base = make_base(&other, "core-a");
        // Same core identity, different manifest bytes.
        fs::write(
            other_base.join(BASE_MANIFEST_FILE),
            br#"{"extensions":[],"y":2}"#,
        )
        .unwrap();
        assert!(checked_overlay(&other_base, &overlay).is_err());
    }

    #[test]
    fn an_old_full_copy_dataset_is_refused_and_names_the_rebuild_command() {
        let scratch = Scratch::new("full-copy");
        let base = make_base(&scratch, "core-a");
        let connection = Connection::open(base.join(BASE_DATABASE_FILE)).unwrap();
        connection
            .execute_batch("CREATE TABLE cluster_meshes(x INTEGER);")
            .unwrap();
        drop(connection);
        let error = format!("{:#}", resolve_overlay(&base, "").unwrap_err());
        assert!(
            error.contains("old full-copy cluster dataset")
                && error.contains("converter derive-clusters"),
            "{error}"
        );
        // Manifest claim only.
        let scratch = Scratch::new("full-copy-manifest");
        let base = make_base(&scratch, "core-a");
        fs::write(
            base.join(BASE_MANIFEST_FILE),
            br#"{"extensions":[{"id":"mudcrab.clusters"}]}"#,
        )
        .unwrap();
        assert!(refuse_full_copy(&base).is_err());
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

    #[test]
    fn datasets_in_folders_with_special_characters_open_read_only() {
        let scratch = Scratch::new("special #1 %20 é");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let connection = open_overlay_database(&base, &overlay).unwrap();
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM cluster_build", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        assert!(
            connection
                .execute("INSERT INTO clusters.cluster_build VALUES (2)", [])
                .is_err()
        );
    }

    #[test]
    fn live_sidecars_of_either_database_are_refused_before_any_open() {
        let scratch = Scratch::new("sidecars");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        assert!(open_overlay_database(&base, &overlay).is_ok());
        for (dir, database) in [
            (base.clone(), BASE_DATABASE_FILE),
            (overlay.clone(), OVERLAY_DATABASE_FILE),
        ] {
            for suffix in ["-wal", "-shm", "-journal"] {
                let sidecar = dir.join(format!("{database}{suffix}"));
                fs::write(&sidecar, b"live").unwrap();
                let error = format!("{:#}", open_overlay_database(&base, &overlay).unwrap_err());
                assert!(error.contains("not sealed"), "{error}");
                assert!(checked_overlay(&base, &overlay).is_err());
                // Refused untouched: the sidecar is neither read, replayed nor removed.
                assert_eq!(fs::read(&sidecar).unwrap(), b"live");
                fs::remove_file(&sidecar).unwrap();
            }
        }
        assert!(open_overlay_database(&base, &overlay).is_ok());
    }

    #[test]
    fn selectors_resolve_one_overlay_or_say_why_not() {
        let scratch = Scratch::new("resolve");
        let base = make_base(&scratch, "core-a");
        assert!(
            format!("{:#}", resolve_overlay(&base, "").unwrap_err()).contains("no cluster overlay")
        );
        let first = make_overlay(&base, "default", 1);
        assert_eq!(resolve_overlay(&base, "").unwrap(), first);
        let second = make_overlay(&base, "dense", 2);
        let error = format!("{:#}", resolve_overlay(&base, "").unwrap_err());
        assert!(
            error.contains("2 cluster overlays") && error.contains("--clusters"),
            "{error}"
        );
        assert_eq!(resolve_overlay(&base, "default").unwrap(), first);
        assert_eq!(resolve_overlay(&base, &id(2)).unwrap(), second);
        assert_eq!(
            resolve_overlay(&base, &format!("clusters-{}", id(2))).unwrap(),
            second
        );
        assert_eq!(
            resolve_overlay(&base, second.to_str().unwrap()).unwrap(),
            second
        );
        assert!(resolve_overlay(&base, "missing").is_err());
        // Two overlays with one label need the settings id.
        let third = make_overlay(&base, "default", 3);
        let error = format!("{:#}", resolve_overlay(&base, "default").unwrap_err());
        assert!(error.contains("matches 2 overlays"), "{error}");
        assert_eq!(resolve_overlay(&base, &id(3)).unwrap(), third);
        // Staging folders are not overlays.
        fs::create_dir_all(derived_dir(&base).join(".staging-1-2")).unwrap();
        assert_eq!(list_overlays(&base).unwrap().len(), 3);
    }

    #[test]
    fn an_unreadable_overlay_is_an_error_not_a_skipped_candidate() {
        let scratch = Scratch::new("unreadable");
        let base = make_base(&scratch, "core-a");
        let valid = make_overlay(&base, "default", 1);
        let broken = overlay_dir(&base, &id(2));
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join(OVERLAY_METADATA_FILE), b"{").unwrap();
        assert!(resolve_overlay(&base, "").is_err());
        for selector in [
            "default".to_owned(),
            id(1),
            format!("clusters-{}", id(1)),
            valid.display().to_string(),
        ] {
            assert_eq!(resolve_overlay(&base, &selector).unwrap(), valid);
        }
        assert!(resolve_overlay(&base, &id(2)).is_err());
    }

    #[test]
    fn a_label_keeps_precedence_over_an_existing_path_like_the_python_tool() {
        let scratch = Scratch::new("label-path-precedence");
        let base = make_base(&scratch, "core-a");
        let path = make_overlay(&base, "path-target", 1);
        let selector = path.display().to_string();
        let labelled = make_overlay(&base, &selector, 2);
        assert_eq!(resolve_overlay(&base, &selector).unwrap(), labelled);
    }

    #[test]
    fn foreign_corrupt_database_reports_binding_before_sqlite() {
        let scratch = Scratch::new("foreign-corrupt");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        fs::write(overlay.join(OVERLAY_DATABASE_FILE), b"not a database").unwrap();
        let database = Connection::open(base.join(BASE_DATABASE_FILE)).unwrap();
        database
            .execute("UPDATE lod_build SET build_identity = 'core-b'", [])
            .unwrap();
        drop(database);
        let error = format!("{:#}", open_overlay_database(&base, &overlay).unwrap_err());
        assert!(error.contains("another conversion of this base"), "{error}");
        assert!(!error.contains("not a database"), "{error}");
    }

    #[test]
    fn receipt_reuses_binding_but_rechecks_each_connections_core() {
        let scratch = Scratch::new("receipt");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let receipt = OverlayBindingReceipt::check(&base, &overlay).unwrap();
        // A receipt is retained under reader locks: no later manifest reparse is necessary.
        fs::write(base.join(BASE_MANIFEST_FILE), b"not JSON").unwrap();
        for _ in 0..3 {
            let connection = receipt.connection().unwrap();
            assert!(connection.is_readonly(DatabaseName::Main).unwrap());
            assert!(
                connection
                    .is_readonly(DatabaseName::Attached(OVERLAY_SCHEMA))
                    .unwrap()
            );
        }
        let database = Connection::open(base.join(BASE_DATABASE_FILE)).unwrap();
        database
            .execute("UPDATE lod_build SET build_identity = 'core-b'", [])
            .unwrap();
        drop(database);
        let error = format!("{:#}", receipt.connection().unwrap_err());
        assert!(error.contains("another conversion of this base"), "{error}");
    }

    #[test]
    fn receipt_preserves_base_sqlite_errors_instead_of_calling_them_foreign() {
        let scratch = Scratch::new("receipt-sql-error");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let receipt = OverlayBindingReceipt::check(&base, &overlay).unwrap();
        let database = Connection::open(base.join(BASE_DATABASE_FILE)).unwrap();
        database.execute("DROP TABLE lod_build", []).unwrap();
        drop(database);
        let error = format!("{:#}", receipt.connection().unwrap_err());
        assert!(error.contains("reading the base core identity"), "{error}");
        assert!(error.contains("no such table"), "{error}");
        assert!(!error.contains("another conversion"), "{error}");
    }

    #[test]
    fn an_object_in_two_attached_databases_is_refused() {
        let scratch = Scratch::new("ambiguous");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let connection = open_overlay_database(&base, &overlay).unwrap();
        assert_eq!(
            crate::extensions::locate_object(&connection, "table", "CLUSTER_BUILD")
                .unwrap()
                .as_deref(),
            Some(OVERLAY_SCHEMA)
        );
        assert_eq!(
            crate::extensions::locate_object(&connection, "table", "statics")
                .unwrap()
                .as_deref(),
            Some("main")
        );
        // Add a base table with an overlay table's name (through a plain connection).
        let writable = Connection::open(base.join(BASE_DATABASE_FILE)).unwrap();
        writable
            .execute_batch("CREATE TABLE cluster_build(id INTEGER);")
            .unwrap();
        drop(writable);
        assert!(refuse_full_copy(&base).is_err());
        // The base is main again and the overlay is attached: both hold `cluster_build`.
        let manual = Connection::open_with_flags(
            base.join(BASE_DATABASE_FILE),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        manual
            .execute(
                "ATTACH DATABASE ?1 AS clusters",
                [overlay.join(OVERLAY_DATABASE_FILE).to_str().unwrap()],
            )
            .unwrap();
        let error =
            crate::extensions::locate_object(&manual, "table", "cluster_build").unwrap_err();
        assert!(format!("{error:#}").contains("exists in both"), "{error:#}");
    }

    #[test]
    fn a_view_cannot_shadow_an_attached_table() {
        let scratch = Scratch::new("view-shadow");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let connection = open_overlay_database(&base, &overlay).unwrap();
        // A temp view named like the attached table answers unqualified queries.
        connection
            .execute_batch("CREATE TEMP VIEW cluster_build AS SELECT 7 AS id;")
            .unwrap();
        let error =
            crate::extensions::locate_object(&connection, "table", "cluster_build").unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("shadow"), "{text}");
        let mut hasher = crate::extensions::ExtensionHasher::new("x", 1);
        assert!(hasher.add_table(&connection, "cluster_build").is_err());
        // A view in the base database (main) with the overlay table's name is refused too.
        let scratch = Scratch::new("view-shadow-main");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let writable = Connection::open(base.join(BASE_DATABASE_FILE)).unwrap();
        writable
            .execute_batch("CREATE VIEW cluster_build AS SELECT 7 AS id;")
            .unwrap();
        drop(writable);
        let error = format!("{:#}", refuse_full_copy(&base).unwrap_err());
        assert!(error.contains("old full-copy cluster dataset"), "{error}");
        assert!(open_overlay_database(&base, &overlay).is_err());
        // Without the full-copy guard (a connection assembled by hand) the lookup still refuses.
        let manual = Connection::open_with_flags(
            base.join(BASE_DATABASE_FILE),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .unwrap();
        attach_read_only(
            &manual,
            &overlay.join(OVERLAY_DATABASE_FILE),
            OVERLAY_SCHEMA,
        )
        .unwrap();
        let error =
            crate::extensions::locate_object(&manual, "table", "cluster_build").unwrap_err();
        assert!(format!("{error:#}").contains("view"), "{error:#}");
    }

    #[test]
    fn the_extension_hasher_finds_attached_tables_and_indexes() {
        use crate::extensions::ExtensionHasher;
        // The stage's shape: the cluster database is main, the base is attached.
        let scratch = Scratch::new("hasher");
        let base = make_base(&scratch, "core-a");
        let overlay = make_overlay(&base, "default", 1);
        let stage = Connection::open(overlay.join(OVERLAY_DATABASE_FILE)).unwrap();
        stage
            .execute_batch("CREATE INDEX cluster_build_id ON cluster_build(id);")
            .unwrap();
        let expected = {
            let mut hasher = ExtensionHasher::new("x", 1);
            hasher.add_table(&stage, "cluster_build").unwrap();
            hasher.add_index(&stage, "cluster_build_id").unwrap();
            hasher.finish()
        };
        stage
            .execute(
                &format!("ATTACH DATABASE ?1 AS {BASE_SCHEMA}"),
                [base.join(BASE_DATABASE_FILE).to_str().unwrap()],
            )
            .unwrap();
        assert_eq!(
            core_build_identity(&stage).unwrap().as_deref(),
            Some("core-a")
        );
        let mut attached_view = ExtensionHasher::new("x", 1);
        attached_view.add_table(&stage, "cluster_build").unwrap();
        attached_view.add_index(&stage, "cluster_build_id").unwrap();
        assert_eq!(attached_view.finish(), expected);
        // The reader's shape: the base is main, the overlay is attached. Same bytes, same hash.
        stage.execute_batch("DETACH DATABASE base").unwrap();
        drop(stage);
        let reader = open_overlay_database(&base, &overlay).unwrap();
        let mut hasher = ExtensionHasher::new("x", 1);
        hasher.add_table(&reader, "cluster_build").unwrap();
        hasher.add_index(&reader, "cluster_build_id").unwrap();
        assert_eq!(hasher.finish(), expected);
        // A base table is hashed from the base, a missing one is reported.
        let mut hasher = ExtensionHasher::new("x", 1);
        assert!(hasher.add_table(&reader, "statics").is_ok());
        let error = hasher.add_table(&reader, "no_such_table").unwrap_err();
        assert!(format!("{error:#}").contains("no_such_table"));
    }
}
