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
        "texture_etc1s_quality": config.texture_etc1s_quality,
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
