//! The clone guard shared by the commands that add output to an APFS clone of a registered pack.
//!
//! Such a command must never write into a registered pack, and must prove that its target is an
//! exact copy of the pack it names. The proof is the manifest hashes in the pack registry
//! (`packs.json`): the target's `conversion-manifest.json` and `integration-report.json` must
//! hash the same, and must not be the same files (a hard link would share writes with the pack).
//! Every refusal names its reason.

use crate::{cache::hash_file, integration::require_private_file};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use serde::Deserialize;
use shared::asset_lock::{AssetLock, asset_lock_path};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Deserialize)]
struct RegisteredPack {
    path: PathBuf,
    #[serde(default)]
    manifests: BTreeMap<String, String>,
}

/// What the guard proved about the clone and the registered pack it copies.
#[derive(Debug, Clone)]
pub struct CloneFacts {
    /// Registry name of the pack the target copies.
    pub name: String,
    /// Where the registered pack lives.
    pub path: PathBuf,
    /// Manifest hashes the registry records, which the target matched at the start.
    pub manifests: BTreeMap<String, String>,
    /// The canonical path of the target.
    pub target: PathBuf,
}

/// Refuse a `target` that is not a clone of the registered pack `clone_of`, or that is, is inside
/// or contains a registered pack.
pub fn check_clone(registry_path: &Path, clone_of: &str, target: &Path) -> Result<CloneFacts> {
    let registry_bytes = fs::read(registry_path)
        .wrap_err_with(|| format!("cannot read pack registry {}", registry_path.display()))?;
    let registry: BTreeMap<String, RegisteredPack> = serde_json::from_slice(&registry_bytes)
        .wrap_err_with(|| format!("pack registry {} is not valid", registry_path.display()))?;
    let Some(source) = registry.get(clone_of) else {
        bail!(
            "refused: `{clone_of}` is not a registered pack in {}",
            registry_path.display()
        );
    };

    let metadata = fs::symlink_metadata(target)
        .wrap_err_with(|| format!("refused: target {} does not exist", target.display()))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "refused: target {} must be a real directory, not a file or a symlink",
        target.display()
    );
    let target = fs::canonicalize(target)?;
    for (name, registered) in &registry {
        // A registered pack that is missing on disk cannot contain the target.
        let path = match fs::canonicalize(&registered.path) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && name != clone_of => {
                continue;
            }
            Err(error) => {
                return Err(error)
                    .wrap_err_with(|| format!("refused: cannot resolve registered pack `{name}`"));
            }
        };
        ensure!(
            !same_file::is_same_file(&target, &path)?,
            "refused: target {} is the registered pack `{name}`; derive into an APFS clone, never into a registered pack",
            target.display()
        );
        ensure!(
            !target.starts_with(&path),
            "refused: target {} is inside the registered pack `{name}` ({})",
            target.display(),
            path.display()
        );
        ensure!(
            !path.starts_with(&target),
            "refused: target {} contains the registered pack `{name}` ({})",
            target.display(),
            path.display()
        );
    }

    ensure!(
        !source.manifests.is_empty(),
        "refused: the registry records no manifest hashes for `{clone_of}`, so the clone cannot be proven"
    );
    for (file, expected) in &source.manifests {
        require_private_output(&target, Path::new(file))?;
        let candidate = target.join(file);
        let original = source.path.join(file);
        ensure!(
            !same_file::is_same_file(&candidate, &original).wrap_err_with(|| format!(
                "refused: cannot compare target {file} with the registered pack"
            ))?,
            "refused: target {file} is the same file as the registered pack's"
        );
        let actual = hash_file(&candidate)?;
        ensure!(
            &actual == expected,
            "refused: target {file} hash {actual} differs from the registered `{clone_of}` hash {expected}; the target is not an exact clone"
        );
    }
    Ok(CloneFacts {
        name: clone_of.to_owned(),
        path: source.path.clone(),
        manifests: source.manifests.clone(),
        target,
    })
}

/// Refuse an `output` file inside a registered pack, for a command that creates one new file.
/// The output's folder must exist.
pub(crate) fn require_outside_registered_packs(registry_path: &Path, output: &Path) -> Result<()> {
    let registry_bytes = fs::read(registry_path)
        .wrap_err_with(|| format!("cannot read pack registry {}", registry_path.display()))?;
    let registry: BTreeMap<String, RegisteredPack> = serde_json::from_slice(&registry_bytes)
        .wrap_err_with(|| format!("pack registry {} is not valid", registry_path.display()))?;
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent)
        .wrap_err_with(|| format!("refused: the folder of {} does not exist", output.display()))?;
    for (name, registered) in &registry {
        let path = match fs::canonicalize(&registered.path) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .wrap_err_with(|| format!("refused: cannot resolve registered pack `{name}`"));
            }
        };
        ensure!(
            !parent.starts_with(&path),
            "refused: {} is inside the registered pack `{name}` ({})",
            output.display(),
            path.display()
        );
    }
    Ok(())
}

/// Hold the same exclusive pack lock across both LOD derive commands.
pub(crate) fn lock_for_derive(target: &Path) -> Result<AssetLock> {
    let path = asset_lock_path(target);
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        require_private_output(parent, Path::new(name))?;
    }
    Ok(AssetLock::acquire_exclusive(target)?)
}

/// Checks every existing component under a canonical clone root without following symlinks.
pub(crate) fn require_private_output(root: &Path, relative: &Path) -> Result<()> {
    let mut parts = relative.components().peekable();
    let mut path = root.to_path_buf();
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else {
            bail!("refused: invalid output path {}", relative.display());
        };
        path.push(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .wrap_err_with(|| format!("refused: cannot inspect {}", path.display()));
            }
        };
        if parts.peek().is_some() {
            ensure!(
                metadata.is_dir(),
                "refused: {} must be a real directory",
                path.display()
            );
        } else {
            require_private_file(&path, &metadata)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registered pack with two manifest files, and the registry that names it.
    fn registered(root: &Path) -> (PathBuf, PathBuf) {
        let pack = root.join("pack");
        fs::create_dir_all(&pack).unwrap();
        fs::write(pack.join("conversion-manifest.json"), b"{\"a\":1}").unwrap();
        fs::write(pack.join("integration-report.json"), b"{\"b\":2}").unwrap();
        let manifests: BTreeMap<_, _> = ["conversion-manifest.json", "integration-report.json"]
            .iter()
            .map(|name| (name.to_string(), hash_file(&pack.join(name)).unwrap()))
            .collect();
        let registry = root.join("packs.json");
        fs::write(
            &registry,
            serde_json::to_vec(&serde_json::json!({
                "base": { "path": pack, "manifests": manifests },
            }))
            .unwrap(),
        )
        .unwrap();
        (pack, registry)
    }

    fn copy(from: &Path, to: &Path) {
        let files: Vec<_> = fs::read_dir(from)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.path().is_file())
            .collect();
        fs::create_dir_all(to).unwrap();
        for entry in files {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }

    #[test]
    fn an_exact_copy_passes() {
        let root = tempfile::tempdir().unwrap();
        let (pack, registry) = registered(root.path());
        let clone = root.path().join("clone");
        copy(&pack, &clone);
        let facts = check_clone(&registry, "base", &clone).unwrap();
        assert_eq!(facts.name, "base");
        assert_eq!(facts.manifests.len(), 2);
    }

    #[test]
    fn the_registered_pack_itself_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let (pack, registry) = registered(root.path());
        let error = check_clone(&registry, "base", &pack).unwrap_err();
        assert!(format!("{error:#}").contains("is the registered pack"));
        // A path inside it and a path around it are refused too.
        let inside = pack.join("inner");
        copy(&pack, &inside);
        assert!(check_clone(&registry, "base", &inside).is_err());
        assert!(check_clone(&registry, "base", root.path()).is_err());
    }

    #[test]
    fn a_changed_or_unknown_target_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let (pack, registry) = registered(root.path());
        let clone = root.path().join("clone");
        copy(&pack, &clone);
        assert!(check_clone(&registry, "other", &clone).is_err());
        fs::write(clone.join("conversion-manifest.json"), b"{\"a\":2}").unwrap();
        let error = check_clone(&registry, "base", &clone).unwrap_err();
        assert!(format!("{error:#}").contains("not an exact clone"));
        assert!(check_clone(&registry, "base", &root.path().join("missing")).is_err());
    }

    #[test]
    fn a_hard_linked_target_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let (pack, registry) = registered(root.path());
        let clone = root.path().join("clone");
        fs::create_dir_all(&clone).unwrap();
        for name in ["conversion-manifest.json", "integration-report.json"] {
            fs::hard_link(pack.join(name), clone.join(name)).unwrap();
        }
        let error = check_clone(&registry, "base", &clone).unwrap_err();
        assert!(format!("{error:#}").contains("private copy"));
    }
}
