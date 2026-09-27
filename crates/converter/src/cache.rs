use color_eyre::{Result, eyre::WrapErr};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufReader, Read, Write},
    path::Path,
};

pub const CONVERTER_SCHEMA_VERSION: u32 = 14;

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
    pub files: Vec<IngestedFile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversionManifest {
    pub schema_version: u32,
    pub complete: bool,
    #[serde(default)]
    pub configuration_hash: String,
    #[serde(default)]
    pub inputs_by_kind: BTreeMap<String, u64>,
    #[serde(default)]
    pub failures: BTreeMap<String, String>,
    #[serde(default)]
    pub archives: BTreeMap<String, IngestionCacheEntry>,
    pub entries: BTreeMap<String, CacheEntry>,
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
        if matches!(manifest.schema_version, 12 | 13) && CONVERTER_SCHEMA_VERSION == 14 {
            // Schemas 13/14 change only NIF material publication and LAND
            // normalization. Preserve verified archive ingestion, textures,
            // and scripts, but force every GLB plus the always-rebuilt world
            // database and cell cache through the new contracts.
            manifest.complete = false;
            manifest
                .entries
                .retain(|_, entry| !entry.output.to_ascii_lowercase().ends_with(".glb"));
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
    configuration_hash_for_schema(config, CONVERTER_SCHEMA_VERSION)
}

pub fn configuration_hash_for_schema(
    config: &crate::config::PipelineConfig,
    schema: u32,
) -> Result<String> {
    let relevant = serde_json::json!({
        "schema": schema,
        "texture_etc1s_quality": config.texture_fallback_quality,
        "texture_uastc_level": config.texture_uastc_level,
        "script_abi_version": config.script_abi_version,
    });
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

/// Like [`link_or_copy`], for a blob that many paths share.
///
/// A file can carry only so many names (1,024 on NTFS), and some game content is stored under
/// thousands of paths (terrain and face textures), more again during a reconversion, while the
/// previous output still holds its own names. When the blob is full, the destination is linked to
/// a spill copy beside it (`<blob>.1`, `<blob>.2`, ...), made once and shared by the next thousand
/// or so paths, instead of each path becoming its own copy. `blob` must be a file this run owns:
/// the spill copies are written beside it.
pub(crate) fn link_or_copy_spilling(blob: &Path, to: &Path) -> std::io::Result<()> {
    link_or_copy_spilling_with(blob, to, |from, to| fs::hard_link(from, to))
}

fn link_or_copy_spilling_with(
    blob: &Path,
    to: &Path,
    link: fn(&Path, &Path) -> std::io::Result<()>,
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
    for index in 1..=MAX_SPILLS {
        let mut name = blob.as_os_str().to_owned();
        name.push(format!(".{index}"));
        let spill = std::path::PathBuf::from(name);
        if !spill.is_file() {
            // Written under a temporary name and renamed, so a reader never sees half a spill.
            let mut partial = spill.as_os_str().to_owned();
            partial.push(format!(".partial-{}", std::process::id()));
            let partial = std::path::PathBuf::from(partial);
            fs::copy(blob, &partial)?;
            fs::rename(&partial, &spill)?;
        }
        match link(&spill, to) {
            Ok(()) => return Ok(()),
            Err(error) if is_too_many_links(&error) => continue,
            Err(_) => break,
        }
    }
    fs::copy(blob, to).map(|_| ())
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

pub fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn hash_file(path: &Path) -> Result<String> {
    let file = fs::File::open(path)
        .wrap_err_with(|| format!("failed to open {} for hashing", path.display()))?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .wrap_err_with(|| format!("failed to hash {}", path.display()))?;
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
    fn sha256_is_stable() {
        assert_eq!(
            hash_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
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

        link_or_copy_spilling_with(&blob, &first, blob_is_full).unwrap();
        link_or_copy_spilling_with(&blob, &second, blob_is_full).unwrap();

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

        link_or_copy_spilling_with(&blob, &vfs, |_, _| {
            Err(std::io::Error::other("cross-volume"))
        })
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
    fn recent_schema_migrations_reuse_only_unchanged_asset_kinds() {
        for schema_version in [12, 13] {
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
            assert!(migrated.entries.contains_key("textures/a.ktx2"));
            assert!(migrated.entries.contains_key("scripts/a.luau"));
        }
    }
}
