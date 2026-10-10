use color_eyre::{Result, eyre::WrapErr};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::SystemTime,
};

// Combined native-BC, lighting, LOD, TXST and XESP producer. Earlier numeric identities
// alone do not establish compatible output semantics.
pub const CONVERTER_SCHEMA_VERSION: u32 = shared::LOD_CONVERTER_SCHEMA_VERSION;

/// Provenance journal the converter keeps inside a staging directory.
///
/// A staging directory outlives the run that filled it, so a resumed run finds
/// outputs this process did not write. The journal records, per output, the
/// source hash, the converter schema and the configuration hash it was produced
/// under, together with the output's size and hash, appended as the output is
/// written. A resumed run reuses a staged output only while its record still
/// matches the current source, schema and configuration and the bytes on disk.
/// The name is dotted so it can never collide with a converted asset, and the
/// file is dropped from the output directory once the staging directory has
/// been published.
pub const STAGING_JOURNAL_FILE: &str = ".conversion-staging-journal.jsonl";

/// What a staged output was produced from, as recorded when it was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedOutput {
    pub schema_version: u32,
    pub configuration_hash: String,
    pub source_hash: String,
    pub output_size: u64,
    pub output_hash: String,
}

impl StagedOutput {
    /// True when the file at `path` is the output this record describes,
    /// produced from `source_hash` by the current converter schema and
    /// configuration.
    pub fn is_current(&self, path: &Path, source_hash: &str, configuration_hash: &str) -> bool {
        self.schema_version == CONVERTER_SCHEMA_VERSION
            && self.configuration_hash == configuration_hash
            && self.source_hash == source_hash
            && fs::metadata(path).is_ok_and(|metadata| metadata.len() == self.output_size)
            && hash_file(path).is_ok_and(|hash| hash == self.output_hash)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StagingJournalLine {
    key: String,
    #[serde(flatten)]
    output: StagedOutput,
}

#[cfg(test)]
thread_local! {
    /// Makes every journal write on this thread fail, for tests of the
    /// pipeline's error path. The batch loop records on the test's own thread.
    pub(crate) static FAIL_JOURNAL_WRITES: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

pub struct StagingJournal {
    path: PathBuf,
    file: fs::File,
}

impl StagingJournal {
    pub fn path_in(staging: &Path) -> PathBuf {
        staging.join(STAGING_JOURNAL_FILE)
    }

    /// Opens the journal belonging to `staging`, creating it when the directory
    /// has none yet. A run killed mid-append leaves a partial last line; it is
    /// ended here, so the next record starts a line of its own and only the
    /// partial one is dropped when the journal is read.
    pub fn open(staging: &Path) -> Result<Self> {
        let path = Self::path_in(staging);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .wrap_err_with(|| format!("failed to open staging journal {}", path.display()))?;
        let length = file.metadata()?.len();
        if length > 0 {
            let mut last = [0_u8];
            file.seek(SeekFrom::Start(length - 1))?;
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                // Append mode writes at the end whatever the read position.
                file.write_all(b"\n")
                    .wrap_err_with(|| format!("failed to repair {}", path.display()))?;
            }
        }
        Ok(Self { path, file })
    }

    /// Appends one output's provenance. Each record reaches the journal in a
    /// single write, so a run killed mid-append loses at most the last record,
    /// and the output it describes is converted again. The journal is not
    /// fsynced after each record, so that holds for a crashed or killed process;
    /// a power loss can lose more of the unsynced tail, and those outputs are
    /// converted again too.
    pub fn record(&mut self, key: &str, output: &StagedOutput) -> Result<()> {
        #[cfg(test)]
        if FAIL_JOURNAL_WRITES.with(std::cell::Cell::get) {
            color_eyre::eyre::bail!("injected journal write failure");
        }
        let mut line = serde_json::to_vec(&StagingJournalLine {
            key: key.to_owned(),
            output: output.clone(),
        })?;
        line.push(b'\n');
        self.file
            .write_all(&line)
            .wrap_err_with(|| format!("failed to append to {}", self.path.display()))
    }
}

/// Reads the records a previous run left in `staging`, keyed by canonical
/// source key. The last record for a key wins, so an output converted by this
/// run replaces the record of the run it resumes. A truncated or otherwise
/// unreadable line is dropped with a warning: the output it described has no
/// provenance and is converted again.
pub fn load_staged_outputs(staging: &Path) -> Result<BTreeMap<String, StagedOutput>> {
    let mut records = BTreeMap::new();
    let path = StagingJournal::path_in(staging);
    if !path.is_file() {
        return Ok(records);
    }
    let bytes = fs::read(&path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let mut dropped = 0u32;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        match serde_json::from_slice::<StagingJournalLine>(line) {
            Ok(parsed) => {
                records.insert(parsed.key, parsed.output);
            }
            Err(_) => dropped += 1,
        }
    }
    if dropped > 0 {
        eprintln!(
            "warning: dropped {dropped} unreadable record(s) from {}; those outputs are converted again",
            path.display()
        );
    }
    Ok(records)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheEntry {
    pub source_hash: String,
    pub output: String,
    pub output_size: u64,
    pub output_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestedFile {
    pub path: String,
    pub size: u64,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestionCacheEntry {
    pub source_hash: String,
    /// Extraction selection and checkpoint contract. Empty entries are legacy,
    /// fully extracted archives whose individual files were synced.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recipe: String,
    pub files: Vec<IngestedFile>,
}

const INGESTION_JOURNAL_FILE: &str = ".ingestion-archives.jsonl";

#[derive(Serialize, Deserialize)]
struct IngestionJournalLine {
    archive: String,
    entry: IngestionCacheEntry,
}

/// Completed archive inventories. Sealed batch packs carry the durable bytes;
/// this journal avoids decoding completed archives when resuming a staging run.
pub(crate) struct IngestionJournal(fs::File);

impl IngestionJournal {
    pub(crate) fn open(staging: &Path) -> Result<Self> {
        let path = staging.join(INGESTION_JOURNAL_FILE);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        let length = file.metadata()?.len();
        if length > 0 {
            let mut last = [0_u8];
            file.seek(SeekFrom::Start(length - 1))?;
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        Ok(Self(file))
    }

    pub(crate) fn record(&mut self, archive: &str, entry: &IngestionCacheEntry) -> Result<()> {
        let mut bytes = serde_json::to_vec(&IngestionJournalLine {
            archive: archive.to_owned(),
            entry: entry.clone(),
        })?;
        bytes.push(b'\n');
        self.0.write_all(&bytes)?;
        self.0.sync_all()?;
        Ok(())
    }

    pub(crate) fn load(staging: &Path) -> Result<BTreeMap<String, IngestionCacheEntry>> {
        let path = staging.join(INGESTION_JOURNAL_FILE);
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => return Err(error.into()),
        };
        let mut entries = BTreeMap::new();
        for line in bytes.split(|byte| *byte == b'\n') {
            if let Ok(record) = serde_json::from_slice::<IngestionJournalLine>(line) {
                entries.insert(record.archive, record.entry);
            }
        }
        Ok(entries)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversionManifest {
    pub schema_version: u32,
    /// Metadata-only rebuilds do not upgrade the retained mesh cache contract.
    /// Absent means the meshes follow `schema_version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_mesh_schema_version: Option<u32>,
    /// Producer settings of retained bytes, independent of rebuilt metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_asset_configuration_hash: Option<String>,
    pub complete: bool,
    #[serde(default)]
    pub configuration_hash: String,
    #[serde(default)]
    pub inputs_by_kind: BTreeMap<String, u64>,
    #[serde(default)]
    pub failures: BTreeMap<String, String>,
    /// Non-runtime inputs deliberately excluded, with an auditable reason.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub excluded_inputs: BTreeMap<String, String>,
    /// Texture references a published mesh omits because the game data does not
    /// contain that texture, keyed by the published `.glb` and holding the
    /// resolved texture paths it dropped. Kept out of `failures`: nothing failed
    /// to convert, so these do not make the conversion incomplete.
    ///
    /// This is an audit record for whoever reads the published manifest: the
    /// engine and launcher accept an asset set on `complete` plus the converter
    /// schema version, and nothing else in the workspace reads this list.
    #[serde(default)]
    pub pruned_texture_references: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub archives: BTreeMap<String, IngestionCacheEntry>,
    pub entries: BTreeMap<String, CacheEntry>,
}

/// These schema changes affect GLBs/textures/world data, leaving script/archive
/// bytes compatible. Configuration and source hashes still have to match.
pub(crate) fn can_reuse_scripts_and_archives(schema: u32) -> bool {
    matches!(schema, 12..=26)
}

/// Collision and material changes require meshes from the combined producer.
/// Retained older bytes do not authorize normal conversion cache reuse.
pub(crate) fn can_reuse_meshes(schema: u32) -> bool {
    schema == CONVERTER_SCHEMA_VERSION
}

impl ConversionManifest {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self {
                schema_version: CONVERTER_SCHEMA_VERSION,
                ..Self::default()
            });
        }
        let bytes =
            fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
        let mut manifest: Self =
            serde_json::from_slice(&bytes).wrap_err("invalid conversion manifest")?;
        let metadata_rebuild = path
            .parent()
            .is_some_and(|root| root.join("metadata-rebuild.json").exists());
        let retained_assets_are_stale = (manifest.schema_version == CONVERTER_SCHEMA_VERSION)
            && match manifest.retained_mesh_schema_version {
                Some(schema) => schema != CONVERTER_SCHEMA_VERSION,
                None => metadata_rebuild,
            };
        if (can_reuse_scripts_and_archives(manifest.schema_version)
            && manifest.schema_version != CONVERTER_SCHEMA_VERSION)
            || retained_assets_are_stale
        {
            let mesh_schema = manifest
                .retained_mesh_schema_version
                .unwrap_or(manifest.schema_version);
            let compatible_meshes = can_reuse_meshes(mesh_schema)
                && mesh_schema <= manifest.schema_version
                && (manifest.retained_mesh_schema_version.is_some() || !metadata_rebuild);
            // Older meshes and textures regenerate. Scripts remain candidates
            // for the usual per-asset verification.
            manifest.complete = false;
            manifest.entries.retain(|_, entry| {
                let output = entry.output.to_ascii_lowercase();
                output.ends_with(".luau") || (compatible_meshes && output.ends_with(".glb"))
            });
            return Ok(manifest);
        }
        if manifest.schema_version != CONVERTER_SCHEMA_VERSION {
            return Ok(Self {
                schema_version: CONVERTER_SCHEMA_VERSION,
                ..Self::default()
            });
        }
        Ok(manifest)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        let temporary = path.with_extension(format!("json.{}.partial", std::process::id()));
        let mut file = fs::File::create(&temporary)
            .wrap_err_with(|| format!("failed to create {}", temporary.display()))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
            .wrap_err_with(|| format!("failed to publish {}", path.display()))
    }
}

pub fn configuration_hash(config: &crate::config::PipelineConfig) -> Result<String> {
    // no_lod only selects regenerated outputs; it does not change converted asset
    // bytes. Resume invalidates database/LOD outputs before evaluating this proof.
    configuration_hash_for_schema(config, CONVERTER_SCHEMA_VERSION)
}

/// Hashes conversion settings and MO2 identity; winning inputs are hashed per source.
pub fn configuration_hash_for_schema(
    config: &crate::config::PipelineConfig,
    schema: u32,
) -> Result<String> {
    let mut relevant = serde_json::json!({
        "schema": schema,
        "texture_etc1s_quality": config.texture_fallback_quality,
        "texture_uastc_level": config.texture_uastc_level,
        "script_abi_version": config.script_abi_version,
    });
    if let Some(selection) = &config.mo2 {
        let instance = mo2::Instance::open(&selection.instance_path)?;
        let profile = instance.profile_dir(&selection.profile)?;

        relevant["mo2"] = serde_json::json!({
            "instance": instance.instance_path,
            "profile": profile,
            "mods": instance.mods_dir,
            "overwrite": instance.overwrite_dir,
            "data": std::fs::canonicalize(&config.data_dir)?,

        });
    }
    // The in-house frontend corrects texture roles as well as record projections.
    // Older legacy hashes stay byte-identical for existing pack compatibility.
    if config.record_reader == crate::config::RecordReader::Inhouse {
        relevant["record_reader"] = crate::esm::inhouse::reader_identity(config.record_reader);
    }
    if schema >= 16 {
        relevant["texture_zstd_level"] = serde_json::json!(config.texture_zstd_level);
    }
    if schema >= 22 {
        // CPU native-BC output and GPU UASTC output have different contracts;
        // quality changes bytes, while GPU batch size only changes scheduling.
        relevant["texture_encoder"] = match config.texture_encoder {
            crate::config::TextureEncoder::Cpu => serde_json::json!({"mode": "cpu"}),
            crate::config::TextureEncoder::Gpu { quality, .. } => {
                serde_json::json!({"mode": "gpu", "quality": quality})
            }
        };
    }
    Ok(hash_bytes(&serde_json::to_vec(&relevant)?))
}

/// Puts `from`'s bytes at `to` as a hard link where the filesystem allows one, else as a copy.
///
/// Every extracted archive entry is stored twice: once under `vfs` and once as the
/// content-addressed blob in `.ingestion-cache` (a cache hit repeats that split). Copying them
/// stored each asset twice, which is tens of gigabytes of game data for a full conversion.
/// Linking costs nothing where the filesystem supports it (NTFS, ext4 and APFS do) and is
/// impossible otherwise, so a cross-volume or linkless filesystem falls back to a copy.
///
/// Linking is safe because nothing writes a shared file in place: extraction and the loose-asset
/// overlay replace the path (unlink, then write or copy over the new, unshared file) and no cache
/// blob is ever written in place. An existing `to` is removed rather than written through, and is
/// left alone when it already names `from`. The destination's parent directory must exist.
pub(crate) fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    link_or_copy_with(from, to, |from, to| fs::hard_link(from, to))
}

/// `link_or_copy` with the link step injected, so a test can force the copy fallback on a
/// filesystem that has links.
fn link_or_copy_with(
    from: &Path,
    to: &Path,
    link: fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    if to.exists() {
        // Removing `to` would delete `from` itself when `to` is the same path as `from`, so that
        // state is success rather than a reason to touch anything.
        let names_one_file = match fs::canonicalize(from) {
            Ok(from) => fs::canonicalize(to).is_ok_and(|to| to == from),
            Err(_) => false,
        };
        if names_one_file {
            return Ok(());
        }
        fs::remove_file(to)?;
    }
    link(from, to).or_else(|_| fs::copy(from, to).map(|_| ()))
}

/// How many spill copies of one blob `link_or_copy_spilling` makes before it gives up and copies.
const MAX_SPILLS: u32 = 64;

/// Verification and spill cursors belong to one extraction. Only blobs that
/// reach the filesystem's link limit enter this bounded cache. A state retains
/// the base and active spill handles; saturated prior spills are discarded.
#[derive(Default)]
pub(crate) struct SpillCache {
    states: Mutex<BTreeMap<PathBuf, Arc<Mutex<SpillState>>>>,
    available: Condvar,
}

const MAX_SPILL_STATES: usize = 128;

#[derive(Default)]
struct SpillState {
    base: Option<(SpillProof, String)>,
    active: Option<SpillProof>,
    cursor: u32,
}

struct SpillLease<'a> {
    cache: &'a SpillCache,
    state: Option<Arc<Mutex<SpillState>>>,
}

impl SpillCache {
    fn lease(&self, blob: &Path) -> SpillLease<'_> {
        let mut states = self.states.lock().unwrap();
        let state = loop {
            if let Some(state) = states.get(blob) {
                break Arc::clone(state);
            }
            if states.len() >= MAX_SPILL_STATES {
                let idle = states
                    .iter()
                    .find(|(_, state)| Arc::strong_count(state) == 1)
                    .map(|(path, _)| path.clone());
                if let Some(idle) = idle {
                    states.remove(&idle);
                } else {
                    states = self.available.wait(states).unwrap();
                    continue;
                }
            }
            let state = Arc::new(Mutex::new(SpillState::default()));
            states.insert(blob.to_owned(), Arc::clone(&state));
            break state;
        };
        SpillLease {
            cache: self,
            state: Some(state),
        }
    }
}

impl Drop for SpillLease<'_> {
    fn drop(&mut self) {
        // Pair the state-release notification with the lease predicate's mutex.
        let _states = self.cache.states.lock().unwrap();
        drop(self.state.take());
        self.cache.available.notify_all();
    }
}

struct SpillProof {
    handle: same_file::Handle,
    length: u64,
    modified: Option<SystemTime>,
}

impl SpillProof {
    fn open(path: &Path) -> std::io::Result<Self> {
        let handle = same_file::Handle::from_path(path)?;
        let metadata = handle.as_file().metadata()?;
        Ok(Self {
            handle,
            length: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }

    fn matches(&self, other: &Self) -> bool {
        self.handle == other.handle
            && self.length == other.length
            // Unsupported modification metadata requires hashing again.
            && self.modified.is_some()
            && self.modified == other.modified
    }

    fn hash(
        &self,
        hash: &impl Fn(&fs::File) -> std::io::Result<String>,
    ) -> std::io::Result<String> {
        let digest = hash(self.handle.as_file())?;
        let after = self.handle.as_file().metadata()?;
        if after.len() != self.length || after.modified().ok() != self.modified {
            return Err(std::io::Error::other("cache file changed while hashing"));
        }
        Ok(digest)
    }

    fn verify_link(&self, destination: &Path) -> std::io::Result<()> {
        let result = (|| {
            let linked = Self::open(destination)?;
            if self.handle != linked.handle
                || self.length != linked.length
                || self.modified != linked.modified
            {
                return Err(std::io::Error::other("cache file changed before linking"));
            }
            Ok(())
        })();
        result.map_err(|error| discard_unverified(destination, error))
    }
}

/// Link through verified spill instances, avoiding repeated reads and scans.
/// Owned cache inodes stay immutable during extraction; writers replace paths.
/// Replacement or observable size/mtime changes invalidate saved proofs, and
/// fresh extractions rehash restored spills. Different blobs have separate locks.
pub(crate) fn link_or_copy_spilling(
    blob: &Path,
    to: &Path,
    cache: &SpillCache,
) -> std::io::Result<()> {
    link_or_copy_spilling_with(
        blob,
        to,
        cache,
        &|from, to| fs::hard_link(from, to),
        &hash_open_file,
    )
}

fn link_or_copy_spilling_with(
    blob: &Path,
    to: &Path,
    cache: &SpillCache,
    link: &impl Fn(&Path, &Path) -> std::io::Result<()>,
    hash: &impl Fn(&fs::File) -> std::io::Result<String>,
) -> std::io::Result<()> {
    if same_path(blob, to) {
        return Ok(());
    }
    if to.exists() {
        fs::remove_file(to)?;
    }
    match link(blob, to) {
        Ok(()) => return Ok(()),
        Err(error) if !is_too_many_links(&error) => return fs::copy(blob, to).map(|_| ()),
        Err(_) => {}
    }
    let lease = cache.lease(blob);
    let mut state = lease.state.as_ref().unwrap().lock().unwrap();
    let base = SpillProof::open(blob)?;
    if !state
        .base
        .as_ref()
        .is_some_and(|(proof, _)| proof.matches(&base))
    {
        let digest = base.hash(hash)?;
        state.base = Some((base, digest));
        state.cursor = 1;
        state.active = None;
    }
    while state.cursor <= MAX_SPILLS {
        let mut name = blob.as_os_str().to_owned();
        name.push(format!(".{}", state.cursor));
        let spill = PathBuf::from(name);
        let candidate = SpillProof::open(&spill).ok();
        let unchanged = candidate.as_ref().is_some_and(|candidate| {
            state
                .active
                .as_ref()
                .is_some_and(|proof| proof.matches(candidate))
        });
        if !unchanged {
            let expected = &state.base.as_ref().unwrap().1;
            let valid = candidate.as_ref().is_some_and(|candidate| {
                candidate.hash(hash).is_ok_and(|digest| &digest == expected)
            });
            if valid {
                state.active = candidate;
            } else {
                // Same-blob writers share this lock; the temporary name also
                // protects creation from independent extractions.
                let temporary =
                    tempfile::NamedTempFile::new_in(blob.parent().unwrap_or(Path::new(".")))?;
                fs::copy(blob, temporary.path())?;
                let proof = SpillProof::open(temporary.path())?;
                if &proof.hash(hash)? != expected {
                    return Err(std::io::Error::other(
                        "cache file changed before spill copy",
                    ));
                }
                temporary.persist(&spill).map_err(|error| error.error)?;
                state.active = Some(proof);
            }
        }
        match link(&spill, to) {
            Ok(()) => return state.active.as_ref().unwrap().verify_link(to),
            Err(error) if is_too_many_links(&error) => {
                state.cursor += 1;
                state.active = None;
            }
            Err(_) => break,
        }
    }
    // Beyond the bounded spill count, verify this derived copy before use.
    let result = (|| {
        fs::copy(blob, to)?;
        let copied = SpillProof::open(to)?;
        if copied.hash(hash)? != state.base.as_ref().unwrap().1 {
            return Err(std::io::Error::other("cache file changed before copying"));
        }
        Ok(())
    })();
    result.map_err(|error| discard_unverified(to, error))
}

fn discard_unverified(path: &Path, error: std::io::Error) -> std::io::Error {
    match fs::remove_file(path) {
        Ok(()) => error,
        Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => error,
        Err(cleanup) => std::io::Error::other(format!(
            "{error}; failed to remove unverified destination {}: {cleanup}",
            path.display()
        )),
    }
}

/// A link refused because the file already has as many names as the filesystem allows.
fn is_too_many_links(error: &std::io::Error) -> bool {
    // ERROR_TOO_MANY_LINKS on Windows; EMLINK elsewhere.
    error.kind() == std::io::ErrorKind::TooManyLinks || error.raw_os_error() == Some(1142)
}

/// Whether `from` and `to` are the same path (removing `to` would then delete `from`).
fn same_path(from: &Path, to: &Path) -> bool {
    match fs::canonicalize(from) {
        Ok(from) => fs::canonicalize(to).is_ok_and(|to| to == from),
        Err(_) => false,
    }
}

/// Retained schema-15/16 producers also recorded their fixed Zstd level of 6.
/// The protected schema-15 source manifest proves this exact configuration
/// variant; quality and script ABI must still match its recorded hash.
/// This compatibility route verifies retained bytes, not normal cache reuse.
pub(crate) fn retained_configuration_matches(
    config: &crate::config::PipelineConfig,
    schema: u32,
    recorded: &str,
) -> Result<bool> {
    if recorded == configuration_hash_for_schema(config, schema)? {
        return Ok(true);
    }
    if !matches!(schema, 15 | 16) || config.texture_zstd_level != 6 {
        return Ok(false);
    }
    let native = serde_json::json!({
        "schema": schema,
        "texture_etc1s_quality": config.texture_fallback_quality,
        "texture_uastc_level": config.texture_uastc_level,
        "texture_zstd_level": 6,
        "script_abi_version": config.script_abi_version,
    });
    Ok(recorded == hash_bytes(&serde_json::to_vec(&native)?))
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn hash_file(path: &Path) -> Result<String> {
    let file = fs::File::open(path)
        .wrap_err_with(|| format!("failed to open {} for hashing", path.display()))?;
    hash_open_file(&file).wrap_err_with(|| format!("failed to hash {}", path.display()))
}

fn hash_open_file(mut file: &fs::File) -> std::io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_schema15_fixed_zstd_proof_preserves_quality_abi_and_cache_identity() {
        let config = crate::config::PipelineConfig::new("Data", "output");
        // Exact configuration hash in the protected original schema-15
        // manifest and its metadata rebuild's retained producer provenance.
        let recorded = "b23881a864cdcfe1609779a9a27fefab2de629b50ec339101d4e0ad592e7e435";
        assert!(retained_configuration_matches(&config, 15, recorded).unwrap());
        assert_eq!(
            configuration_hash_for_schema(&config, 15).unwrap(),
            "9a58fda00b27d0f2a8e46afb9334ea869602556393a35bffd7fcb39582a08a4f"
        );
        assert!(!retained_configuration_matches(&config, 16, recorded).unwrap());
        assert!(!retained_configuration_matches(&config, 15, "different-hash").unwrap());
        let mut changed = config.clone();
        changed.texture_fallback_quality = 191;
        assert!(!retained_configuration_matches(&changed, 15, recorded).unwrap());
        let mut changed = config.clone();
        changed.script_abi_version = 2;
        assert!(!retained_configuration_matches(&changed, 15, recorded).unwrap());
        let mut changed = config;
        changed.texture_zstd_level = 7;
        assert!(!retained_configuration_matches(&changed, 15, recorded).unwrap());
    }

    #[test]
    fn parallel_spill_creation_uses_unique_temporary_files() {
        let barrier = std::sync::Barrier::new(4);
        let full_blob = |from: &Path, to: &Path| -> std::io::Result<()> {
            if from.file_name().is_some_and(|name| name == "parallel_blob") {
                barrier.wait();
                Err(std::io::ErrorKind::TooManyLinks.into())
            } else {
                fs::hard_link(from, to)
            }
        };
        let directory = tempfile::tempdir().unwrap();
        let blob = directory.path().join("parallel_blob");
        fs::write(&blob, b"immutable bytes").unwrap();
        let cache = SpillCache::default();
        std::thread::scope(|scope| {
            for index in 0..4 {
                let blob = &blob;
                let cache = &cache;
                let full_blob = &full_blob;
                let destination = directory.path().join(format!("asset{index}"));
                scope.spawn(move || {
                    link_or_copy_spilling_with(
                        blob,
                        &destination,
                        cache,
                        full_blob,
                        &hash_open_file,
                    )
                    .unwrap();
                    assert_eq!(fs::read(destination).unwrap(), b"immutable bytes");
                });
            }
        });
    }

    #[test]
    fn corrupt_derived_spills_are_replaced_before_reuse() {
        fn full_blob(from: &Path, to: &Path) -> std::io::Result<()> {
            if from.file_name().is_some_and(|name| name == "blob") {
                Err(std::io::ErrorKind::TooManyLinks.into())
            } else {
                fs::hard_link(from, to)
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let blob = directory.path().join("blob");
        fs::write(&blob, b"valid bytes").unwrap();
        fs::write(directory.path().join("blob.1"), b"wrong bytes").unwrap();
        let destination = directory.path().join("asset");
        link_or_copy_spilling_with(
            &blob,
            &destination,
            &SpillCache::default(),
            &full_blob,
            &hash_open_file,
        )
        .unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"valid bytes");
    }

    fn full_base(from: &Path, to: &Path) -> std::io::Result<()> {
        if from.file_name().is_some_and(|name| name == "blob") {
            Err(std::io::ErrorKind::TooManyLinks.into())
        } else {
            fs::hard_link(from, to)
        }
    }

    #[test]
    fn spill_verification_reads_and_link_attempts_grow_linearly() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for count in [8, 16, 32] {
            let directory = tempfile::tempdir().unwrap();
            let blob = directory.path().join("blob");
            fs::write(&blob, [0x5a; 4096]).unwrap();
            let cache = SpillCache::default();
            let links = Mutex::new(BTreeMap::<PathBuf, usize>::new());
            let attempts = AtomicUsize::new(0);
            let reads = AtomicUsize::new(0);
            let link = |from: &Path, to: &Path| {
                attempts.fetch_add(1, Ordering::Relaxed);
                let mut links = links.lock().unwrap();
                let used = links.entry(from.to_owned()).or_default();
                if *used >= 2 {
                    return Err(std::io::ErrorKind::TooManyLinks.into());
                }
                fs::hard_link(from, to)?;
                *used += 1;
                Ok(())
            };
            let hash = |file: &fs::File| {
                reads.fetch_add(1, Ordering::Relaxed);
                hash_open_file(file)
            };
            for index in 0..count {
                let destination = directory.path().join(format!("asset{index}"));
                link_or_copy_spilling_with(&blob, &destination, &cache, &link, &hash).unwrap();
                assert_eq!(fs::read(destination).unwrap(), [0x5a; 4096]);
            }
            // One base hash and one per new spill. With the previous scan,
            // the same cases read 15, 63 and 255 full files.
            assert_eq!(reads.load(Ordering::Relaxed), count / 2);
            assert!(attempts.load(Ordering::Relaxed) < count * 3);
        }
    }

    #[test]
    fn cached_spill_revalidates_corruption_and_same_metadata_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let blob = directory.path().join("blob");
        let spill = directory.path().join("blob.1");
        fs::write(&blob, b"valid bytes").unwrap();
        let cache = SpillCache::default();
        let restore = |index| {
            let destination = directory.path().join(format!("asset{index}"));
            link_or_copy_spilling_with(&blob, &destination, &cache, &full_base, &hash_open_file)
                .unwrap();
            assert_eq!(fs::read(destination).unwrap(), b"valid bytes");
        };
        restore(0);
        fs::write(&spill, b"wrong bytes").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&spill)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
            .unwrap();
        restore(1);
        let modified = fs::metadata(&spill).unwrap().modified().unwrap();
        let replacement = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        fs::write(replacement.path(), b"wrong bytes").unwrap();
        replacement
            .as_file()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        replacement.persist(&spill).unwrap();
        assert_eq!(fs::metadata(&spill).unwrap().modified().unwrap(), modified);
        restore(2);
        // A new extraction must verify a restored spill even if the previous
        // extraction already trusted the path.
        let fresh = SpillCache::default();
        fs::write(&spill, b"wrong bytes").unwrap();
        let destination = directory.path().join("fresh");
        link_or_copy_spilling_with(&blob, &destination, &fresh, &full_base, &hash_open_file)
            .unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"valid bytes");
    }

    #[test]
    fn replacement_during_link_cannot_publish_unverified_spill_bytes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let directory = tempfile::tempdir().unwrap();
        let blob = directory.path().join("blob");
        fs::write(&blob, b"valid bytes").unwrap();
        let cache = SpillCache::default();
        let replace = AtomicBool::new(true);
        let link = |from: &Path, to: &Path| {
            if from == blob {
                return Err(std::io::ErrorKind::TooManyLinks.into());
            }
            if replace.swap(false, Ordering::Relaxed) {
                let modified = fs::metadata(from)?.modified()?;
                let replacement = tempfile::NamedTempFile::new_in(directory.path())?;
                fs::write(replacement.path(), b"wrong bytes")?;
                replacement
                    .as_file()
                    .set_times(fs::FileTimes::new().set_modified(modified))?;
                replacement.persist(from).map_err(|error| error.error)?;
            }
            fs::hard_link(from, to)
        };
        let destination = directory.path().join("asset");
        assert!(
            link_or_copy_spilling_with(&blob, &destination, &cache, &link, &hash_open_file)
                .is_err()
        );
        assert!(!destination.exists());
        link_or_copy_spilling_with(&blob, &destination, &cache, &link, &hash_open_file).unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"valid bytes");
    }

    #[test]
    fn failed_final_copy_verification_removes_only_the_new_destination() {
        let directory = tempfile::tempdir().unwrap();
        let blob = directory.path().join("blob");
        fs::write(&blob, b"valid bytes").unwrap();
        let cache = SpillCache::default();
        let verified = directory.path().join("verified");
        link_or_copy_spilling_with(&blob, &verified, &cache, &full_base, &hash_open_file).unwrap();
        {
            let states = cache.states.lock().unwrap();
            let mut state = states[&blob].lock().unwrap();
            state.cursor = MAX_SPILLS + 1;
            state.active = None;
        }
        let destination = directory.path().join("new");
        let fail_hash = |_: &fs::File| Err(std::io::Error::other("injected verification failure"));
        let error = link_or_copy_spilling_with(&blob, &destination, &cache, &full_base, &fail_hash)
            .unwrap_err();
        assert!(error.to_string().contains("injected verification failure"));
        assert!(!destination.exists());
        assert_eq!(fs::read(verified).unwrap(), b"valid bytes");
    }

    #[test]
    fn base_replacement_resets_the_spill_cursor_and_keeps_previous_links() {
        let directory = tempfile::tempdir().unwrap();
        let blob = directory.path().join("blob");
        fs::write(&blob, b"first bytes").unwrap();
        let cache = SpillCache::default();
        let link = |from: &Path, to: &Path| {
            if from == blob || (from.ends_with("blob.1") && to.ends_with("second")) {
                Err(std::io::ErrorKind::TooManyLinks.into())
            } else {
                fs::hard_link(from, to)
            }
        };
        let first = directory.path().join("first");
        link_or_copy_spilling_with(&blob, &first, &cache, &link, &hash_open_file).unwrap();
        link_or_copy_spilling_with(
            &blob,
            &directory.path().join("second"),
            &cache,
            &link,
            &hash_open_file,
        )
        .unwrap();
        let replacement = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        fs::write(replacement.path(), b"later bytes").unwrap();
        replacement.persist(&blob).unwrap();
        let later = directory.path().join("later");
        link_or_copy_spilling_with(&blob, &later, &cache, &link, &hash_open_file).unwrap();
        assert_eq!(fs::read(later).unwrap(), b"later bytes");
        assert_eq!(fs::read(first).unwrap(), b"first bytes");
        assert_eq!(
            cache.states.lock().unwrap()[&blob].lock().unwrap().cursor,
            1
        );
    }

    #[test]
    fn a_full_spill_cache_wakes_a_waiter_after_a_lease_is_released() {
        let cache = SpillCache::default();
        std::thread::scope(|scope| {
            let mut leases = (0..MAX_SPILL_STATES)
                .map(|index| cache.lease(Path::new(&format!("blob{index}"))))
                .collect::<Vec<_>>();
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let cache = &cache;
            scope.spawn(move || {
                started_tx.send(()).unwrap();
                let _lease = cache.lease(Path::new("extra"));
                done_tx.send(()).unwrap();
            });
            started_rx.recv().unwrap();
            assert!(matches!(
                done_rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ));
            drop(leases.pop());
            done_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert_eq!(cache.states.lock().unwrap().len(), MAX_SPILL_STATES);
        });
    }

    #[test]
    fn spill_cache_evicts_idle_states_and_releases_their_handles() {
        let directory = tempfile::tempdir().unwrap();
        let cache = SpillCache::default();
        for index in 0..MAX_SPILL_STATES + 2 {
            let blob = directory.path().join(format!("blob{index}"));
            fs::write(&blob, b"bytes").unwrap();
            let link = |from: &Path, to: &Path| {
                if from == blob {
                    Err(std::io::ErrorKind::TooManyLinks.into())
                } else {
                    fs::hard_link(from, to)
                }
            };
            link_or_copy_spilling_with(
                &blob,
                &directory.path().join(format!("asset{index}")),
                &cache,
                &link,
                &hash_open_file,
            )
            .unwrap();
        }
        let states = cache.states.lock().unwrap();
        assert_eq!(states.len(), MAX_SPILL_STATES);
        assert!(!states.contains_key(&directory.path().join("blob0")));
        assert!(states.values().all(|state| Arc::strong_count(state) == 1));
    }

    #[test]
    fn ingestion_journal_resumes_completed_inventories_and_drops_partial_tail() {
        let directory = tempfile::tempdir().unwrap();
        let entry = IngestionCacheEntry {
            source_hash: "ab".repeat(32),
            recipe: "converter-inputs-v1".to_owned(),
            files: vec![IngestedFile {
                path: "lodsettings/tamriel.lod".to_owned(),
                size: 4,
                hash: "cd".repeat(32),
            }],
        };
        let mut journal = IngestionJournal::open(directory.path()).unwrap();
        journal.record("base.bsa", &entry).unwrap();
        drop(journal);
        let path = directory.path().join(INGESTION_JOURNAL_FILE);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"archive\":\"truncated")
            .unwrap();
        assert_eq!(
            IngestionJournal::load(directory.path()).unwrap()["base.bsa"],
            entry
        );
        let mut journal = IngestionJournal::open(directory.path()).unwrap();
        journal.record("dlc.bsa", &entry).unwrap();
        let loaded = IngestionJournal::load(directory.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded["dlc.bsa"], entry);
    }

    #[test]
    fn prior_producers_cannot_reuse_staged_meshes_or_textures() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["old.glb", "old.ktx2"] {
            let path = directory.path().join(name);
            fs::write(&path, b"verified old bytes").unwrap();
            for schema_version in [17, 18, 19, 20, 21, 22, 23, 24, 25] {
                let record = StagedOutput {
                    schema_version,
                    configuration_hash: "matching-config".to_owned(),
                    source_hash: "matching-source".to_owned(),
                    output_size: 18,
                    output_hash: hash_file(&path).unwrap(),
                };
                assert!(
                    !record.is_current(&path, "matching-source", "matching-config"),
                    "legacy producer {schema_version} accepted for {name}"
                );
            }
        }
    }

    #[test]
    fn v73_encoder_contract_participates_in_current_configuration_proof() {
        use crate::config::{PipelineConfig, TextureEncoder};
        let mut config = PipelineConfig::new("Data", "output");
        let cpu = configuration_hash(&config).unwrap();
        let legacy = configuration_hash_for_schema(&config, 18).unwrap();
        config.texture_encoder = TextureEncoder::Gpu {
            quality: 0,
            batch_mb: 4,
        };
        let gpu = configuration_hash(&config).unwrap();
        assert_ne!(
            cpu, gpu,
            "native CPU and GPU texture contracts share configuration identity"
        );
        assert_eq!(legacy, configuration_hash_for_schema(&config, 18).unwrap());
        config.texture_encoder = TextureEncoder::Gpu {
            quality: 1,
            batch_mb: 4,
        };
        let refined = configuration_hash(&config).unwrap();
        assert_ne!(
            gpu, refined,
            "GPU quality changes must invalidate output provenance"
        );
        config.texture_encoder = TextureEncoder::Gpu {
            quality: 1,
            batch_mb: 8,
        };
        assert_eq!(
            refined,
            configuration_hash(&config).unwrap(),
            "batch scheduling does not change bytes"
        );
    }

    #[test]
    fn sha256_is_stable() {
        assert_eq!(
            hash_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn manifests_written_before_pruned_reference_tracking_still_load() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("conversion-manifest.json");
        // Every key but `pruned_texture_references`, exactly as manifests were
        // written before that field existed.
        fs::write(
            &path,
            format!(
                r#"{{
                    "schema_version": {CONVERTER_SCHEMA_VERSION},
                    "complete": true,
                    "configuration_hash": "configuration",
                    "inputs_by_kind": {{"nif": 4}},
                    "failures": {{}},
                    "archives": {{}},
                    "entries": {{}}
                }}"#
            ),
        )
        .unwrap();

        let manifest = ConversionManifest::load(&path).unwrap();

        assert_eq!(manifest.schema_version, CONVERTER_SCHEMA_VERSION);
        assert!(manifest.complete);
        assert_eq!(manifest.configuration_hash, "configuration");
        assert_eq!(manifest.inputs_by_kind.get("nif"), Some(&4));
        assert!(manifest.pruned_texture_references.is_empty());
    }

    #[test]
    fn journal_keeps_the_last_record_and_drops_a_truncated_tail() {
        let directory = tempfile::tempdir().unwrap();
        let staging = directory.path();
        let stale = StagedOutput {
            schema_version: CONVERTER_SCHEMA_VERSION,
            configuration_hash: "config".to_owned(),
            source_hash: "stale".to_owned(),
            output_size: 4,
            output_hash: "stale".to_owned(),
        };
        let current = StagedOutput {
            source_hash: "current".to_owned(),
            ..stale.clone()
        };
        let mut journal = StagingJournal::open(staging).unwrap();
        journal.record("scripts/one.pex", &stale).unwrap();
        journal.record("scripts/one.pex", &current).unwrap();
        journal.record("scripts/two.pex", &stale).unwrap();
        drop(journal);

        // A run killed mid-append leaves a partial line behind.
        let path = StagingJournal::path_in(staging);
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"key\":\"scripts/three.pex\",\"schema_ver");
        fs::write(&path, &bytes).unwrap();

        let records = load_staged_outputs(staging).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records["scripts/one.pex"], current);
        assert_eq!(records["scripts/two.pex"], stale);
        assert!(!records.contains_key("scripts/three.pex"));

        // A resumed run appends after the partial line without losing its record.
        let mut journal = StagingJournal::open(staging).unwrap();
        journal.record("scripts/four.pex", &current).unwrap();
        drop(journal);
        let records = load_staged_outputs(staging).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records["scripts/four.pex"], current);
        assert!(!records.contains_key("scripts/three.pex"));

        let missing = directory.path().join("absent");
        assert!(load_staged_outputs(&missing).unwrap().is_empty());
    }

    #[test]
    fn link_or_copy_leaves_a_file_that_already_names_its_source_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rock.dds");
        fs::write(&path, b"archive bytes").unwrap();

        // `from` and `to` are the same path: removing it first would delete the only copy.
        link_or_copy(&path, &path).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"archive bytes");
    }

    #[test]
    fn link_or_copy_accepts_a_destination_that_is_already_linked_to_its_source() {
        let directory = tempfile::tempdir().unwrap();
        let blob = directory.path().join("blob");
        fs::write(&blob, b"archive bytes").unwrap();
        let vfs = directory.path().join("rock.dds");
        fs::hard_link(&blob, &vfs).unwrap();

        link_or_copy(&blob, &vfs).unwrap();

        assert_eq!(fs::read(&blob).unwrap(), b"archive bytes");
        assert_eq!(fs::read(&vfs).unwrap(), b"archive bytes");
    }

    #[test]
    fn a_full_blob_shares_one_spill_copy_instead_of_copying_per_path() {
        // Stands in for ERROR_TOO_MANY_LINKS: the original blob takes no more names, a spill does.
        fn blob_is_full(from: &Path, to: &Path) -> std::io::Result<()> {
            if from.extension().is_none() {
                return Err(std::io::Error::from(std::io::ErrorKind::TooManyLinks));
            }
            fs::hard_link(from, to)
        }
        let directory = tempfile::tempdir().unwrap();
        let blob = directory.path().join("blob");
        fs::write(&blob, b"terrain").unwrap();
        let first = directory.path().join("first.dds");
        let second = directory.path().join("second.dds");

        let cache = SpillCache::default();
        link_or_copy_spilling_with(&blob, &first, &cache, &blob_is_full, &hash_open_file).unwrap();
        link_or_copy_spilling_with(&blob, &second, &cache, &blob_is_full, &hash_open_file).unwrap();

        // Both paths name the one spill copy: a write through one shows in the other, and the
        // blob itself is untouched.
        let spill = directory.path().join("blob.1");
        assert!(spill.is_file());
        fs::OpenOptions::new()
            .append(true)
            .open(&first)
            .unwrap()
            .write_all(b"+")
            .unwrap();
        assert_eq!(fs::read(&second).unwrap(), b"terrain+");
        assert_eq!(fs::read(&spill).unwrap(), b"terrain+");
        assert_eq!(fs::read(&blob).unwrap(), b"terrain");
    }

    #[test]
    fn a_link_refused_for_another_reason_still_copies_without_spilling() {
        let directory = tempfile::tempdir().unwrap();
        let blob = directory.path().join("blob");
        fs::write(&blob, b"archive bytes").unwrap();
        let vfs = directory.path().join("rock.dds");

        link_or_copy_spilling_with(
            &blob,
            &vfs,
            &SpillCache::default(),
            &|_, _| Err(std::io::Error::other("cross-volume")),
            &hash_open_file,
        )
        .unwrap();

        assert_eq!(fs::read(&vfs).unwrap(), b"archive bytes");
        assert!(!directory.path().join("blob.1").exists());
    }

    #[test]
    fn link_or_copy_copies_when_the_filesystem_refuses_a_link() {
        let directory = tempfile::tempdir().unwrap();
        let blob = directory.path().join("blob");
        fs::write(&blob, b"archive bytes").unwrap();
        let vfs = directory.path().join("rock.dds");

        link_or_copy_with(&blob, &vfs, |_, _| {
            Err(std::io::Error::other("no hard links here"))
        })
        .unwrap();

        assert_eq!(fs::read(&vfs).unwrap(), b"archive bytes");
        // The fallback is a copy, not a second name for the blob: writing through one must not
        // reach the other.
        fs::OpenOptions::new()
            .append(true)
            .open(&vfs)
            .unwrap()
            .write_all(b"+")
            .unwrap();
        assert_eq!(fs::read(&blob).unwrap(), b"archive bytes");
        assert_eq!(fs::read(&vfs).unwrap(), b"archive bytes+");
    }

    #[test]
    fn v91_lod_staging_identity_excludes_lighting_and_old_lod_schemas() {
        let directory = tempfile::tempdir().unwrap();
        let config =
            crate::config::PipelineConfig::new(directory.path(), directory.path().join("output"));
        let current_hash = configuration_hash(&config).unwrap();
        for schema in [16, 17, 18, 19, 20, 21, 22, 23] {
            assert_ne!(
                current_hash,
                configuration_hash_for_schema(&config, schema).unwrap()
            );
        }
        let path = directory.path().join("mesh.glb");
        fs::write(&path, b"verified mesh").unwrap();
        let record = StagedOutput {
            schema_version: CONVERTER_SCHEMA_VERSION,
            configuration_hash: "config".into(),
            source_hash: "source".into(),
            output_size: 13,
            output_hash: hash_file(&path).unwrap(),
        };
        assert!(record.is_current(&path, "source", "config"));
        for schema_version in [16, 17, 18, 19, 20, 21, 22, 23, 24, 25] {
            let stale = StagedOutput {
                schema_version,
                ..record.clone()
            };
            assert!(!stale.is_current(&path, "source", "config"));
        }
    }

    #[test]
    fn recent_schema_migrations_reuse_only_unchanged_asset_kinds() {
        for schema_version in [12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("conversion-manifest.json");
            let mut manifest = ConversionManifest {
                schema_version,
                complete: true,
                ..ConversionManifest::default()
            };
            for output in ["meshes/a.glb", "textures/a.ktx2", "scripts/a.luau"] {
                manifest.entries.insert(
                    output.to_owned(),
                    CacheEntry {
                        source_hash: "source".to_owned(),
                        output: output.to_owned(),
                        output_size: 1,
                        output_hash: "output".to_owned(),
                    },
                );
            }
            fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();

            let migrated = ConversionManifest::load(&path).unwrap();

            assert_eq!(migrated.schema_version, schema_version);
            assert!(!migrated.complete);
            assert!(!migrated.entries.contains_key("meshes/a.glb"));
            assert!(!migrated.entries.contains_key("textures/a.ktx2"));
            assert!(migrated.entries.contains_key("scripts/a.luau"));
        }
    }

    /// Compatible published meshes keep original provenance; texture migration
    /// still applies to metadata-only packs and impossible retained identities.
    #[test]
    fn database_only_migration_preserves_compatible_mesh_provenance() {
        for (producer, retained, compatible) in [
            (24, None, false),
            (CONVERTER_SCHEMA_VERSION, Some(24), false),
            (24, Some(CONVERTER_SCHEMA_VERSION), false),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("conversion-manifest.json");
            let mut manifest = ConversionManifest {
                schema_version: producer,
                retained_mesh_schema_version: retained,
                retained_asset_configuration_hash: retained.map(|_| "original-config".into()),
                configuration_hash: "producer-config".into(),
                complete: true,
                ..ConversionManifest::default()
            };
            for output in ["meshes/a.glb", "textures/a.ktx2", "scripts/a.luau"] {
                manifest.entries.insert(
                    output.into(),
                    CacheEntry {
                        source_hash: "source".into(),
                        output: output.into(),
                        output_size: 1,
                        output_hash: "output".into(),
                    },
                );
            }
            manifest.save(&path).unwrap();
            let migrated = ConversionManifest::load(&path).unwrap();
            assert!(!migrated.complete);
            assert_eq!(migrated.entries.contains_key("meshes/a.glb"), compatible);
            assert!(!migrated.entries.contains_key("textures/a.ktx2"));
            assert!(migrated.entries.contains_key("scripts/a.luau"));
            assert_eq!(migrated.retained_mesh_schema_version, retained);
            assert_eq!(migrated.configuration_hash, "producer-config");
            assert_eq!(
                migrated.retained_asset_configuration_hash,
                manifest.retained_asset_configuration_hash
            );
        }
    }

    /// Unproven older contracts never gain the current mesh identity.
    #[test]
    fn current_metadata_never_promotes_an_older_or_unknown_mesh_contract() {
        for mesh_schema in [
            15,
            16,
            17,
            18,
            19,
            20,
            21,
            22,
            23,
            24,
            CONVERTER_SCHEMA_VERSION + 1,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("conversion-manifest.json");
            let mut manifest = ConversionManifest {
                schema_version: CONVERTER_SCHEMA_VERSION,
                retained_mesh_schema_version: Some(mesh_schema),
                complete: true,
                ..ConversionManifest::default()
            };
            for output in ["meshes/a.GLB", "textures/a.ktx2", "scripts/a.luau"] {
                manifest.entries.insert(
                    output.to_owned(),
                    CacheEntry {
                        source_hash: "source".to_owned(),
                        output: output.to_owned(),
                        output_size: 1,
                        output_hash: "output".to_owned(),
                    },
                );
            }
            manifest.save(&path).unwrap();
            let eligible = ConversionManifest::load(&path).unwrap();
            assert!(!eligible.complete);
            assert!(!eligible.entries.contains_key("meshes/a.GLB"));
            assert!(!eligible.entries.contains_key("textures/a.ktx2"));
            assert!(eligible.entries.contains_key("scripts/a.luau"));
        }
    }
}
