use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, TryLockError},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use color_eyre::{
    Result,
    eyre::{ContextCompat, ensure},
};
use tempfile::{NamedTempFile, TempDir};

const RUN_PREFIX: &str = ".mudcrab-extract-run-";
const PAYLOAD_PREFIX: &str = ".mudcrab-extract-payload-";
const OWNER: &str = "owner";
const SIGNATURE: &[u8] = b"Mudcrab archive staging v1\n";

/// Holds temporary files beside their destinations while one lock protects the whole run.
pub(super) struct Run {
    // Drop the payload directories and their registry before closing the ownership lock.
    directories: BTreeMap<PathBuf, TempDir>,
    _registry: TempDir,
    _owner: File,
}

impl Run {
    pub(super) fn new(root: &Path, parents: BTreeSet<&Path>) -> Result<Self> {
        let root = std::env::current_dir()?.join(root);
        fs::create_dir_all(&root)?;
        let registry = tempfile::Builder::new()
            .prefix(RUN_PREFIX)
            .rand_bytes(16)
            .tempdir_in(&root)?;
        let run_name = registry
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .wrap_err("invalid archive staging run name")?;
        let mut owner = NamedTempFile::new_in(registry.path())?;
        owner.write_all(SIGNATURE)?;
        owner.as_file().lock()?;
        // A retry can see the marker only after its lock is held.
        let mut owner = owner.persist(registry.path().join(OWNER))?;
        let paths: Vec<_> = parents
            .into_iter()
            .enumerate()
            .map(|(index, parent)| (parent, payload_name(run_name, index)))
            .collect();
        let manifest: Vec<_> = paths
            .iter()
            .map(|(parent, name)| parent.join(name))
            .collect();
        serde_json::to_writer(&mut owner, &manifest)?;

        let directories = paths
            .into_iter()
            .map(|(parent, name)| {
                let destination = root.join(parent);
                fs::create_dir_all(&destination)?;
                let directory = tempfile::Builder::new()
                    .prefix(&name)
                    .rand_bytes(0)
                    .tempdir_in(&destination)?;
                Ok((parent.to_owned(), directory))
            })
            .collect::<io::Result<_>>()?;
        Ok(Self {
            directories,
            _registry: registry,
            _owner: owner,
        })
    }

    pub(super) fn new_file(&self, parent: &Path) -> io::Result<NamedTempFile> {
        let directory = self
            .directories
            .get(parent)
            .ok_or_else(|| io::Error::other("missing archive staging directory"))?;
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o666));
        }
        builder.tempfile_in(directory.path())
    }
}

fn payload_name(run_name: &str, index: usize) -> String {
    format!("{PAYLOAD_PREFIX}{}-{index}", &run_name[RUN_PREFIX.len()..])
}

pub(super) fn reclaim_abandoned(root: &Path) -> Result<()> {
    let root = if root.as_os_str().is_empty() {
        Path::new(".")
    } else {
        root
    };
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(RUN_PREFIX) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => reclaim(root, &entry.path(), &name)?,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn reclaim(root: &Path, registry: &Path, run_name: &str) -> Result<()> {
    let marker = registry.join(OWNER);
    let mut owner = match open_marker(&marker) {
        Ok(Some(owner)) => owner,
        Ok(None) => return Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    match owner.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(error)) => return Err(error.into()),
    }
    let mut signature = [0; SIGNATURE.len()];
    match owner.read_exact(&mut signature) {
        Ok(()) if signature == SIGNATURE => {}
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(error) => return Err(error.into()),
    }

    let directories: Vec<PathBuf> = match serde_json::from_reader(&owner) {
        Ok(directories) => directories,
        // Payload directories are only created after the entire list has been written.
        Err(error) if error.is_eof() => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    for (index, directory) in directories.iter().enumerate() {
        let expected = payload_name(run_name, index);
        ensure!(
            directory
                .file_name()
                .is_some_and(|name| name == expected.as_str())
                && directory
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "invalid archive staging registry at {}",
            registry.display()
        );
    }
    for directory in directories {
        remove_if_present(&root.join(directory))?;
    }
    // Only the marker belongs to the registry. Preserve files added after a crash.
    match fs::remove_file(marker) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    match fs::remove_dir(registry) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        result => Ok(result?),
    }
}

fn open_marker(path: &Path) -> io::Result<Option<File>> {
    if !path.symlink_metadata()?.is_file() {
        return Ok(None);
    }
    File::options().read(true).write(true).open(path).map(Some)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::ArchiveExtractor;

    #[test]
    fn extraction_retry_reclaims_abandoned_payloads_but_keeps_a_live_run() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("test.bsa");
        let output = directory.path().join("out");
        let meshes = Path::new("meshes");
        let strings = Path::new("strings");
        let parents = BTreeSet::from([meshes, strings]);
        let live = Run::new(&output, parents.clone()).unwrap();
        let mut live_file = live.new_file(meshes).unwrap();
        live_file.write_all(b"staged by live run").unwrap();

        let abandoned = Run::new(&output, parents).unwrap();
        let stale_payloads: Vec<_> = [meshes, strings]
            .into_iter()
            .map(|parent| {
                let mut staged = abandoned.new_file(parent).unwrap();
                staged.write_all(b"abandoned extracted bytes").unwrap();
                staged.into_temp_path().keep().unwrap()
            })
            .collect();
        let Run {
            directories,
            _registry,
            _owner,
        } = abandoned;
        for path in directories.into_values() {
            assert!(path.keep().is_dir());
        }
        let stale_registry = _registry.keep();
        drop(_owner);

        let unrelated = output.join(format!("{RUN_PREFIX}unrelated"));
        fs::create_dir(&unrelated).unwrap();
        fs::write(unrelated.join(OWNER), b"unrelated data").unwrap();
        let interrupted = output.join(format!("{RUN_PREFIX}interrupted"));
        fs::create_dir(&interrupted).unwrap();
        fs::write(interrupted.join(OWNER), SIGNATURE).unwrap();
        fs::write(interrupted.join("retained"), b"independent file").unwrap();
        fs::write(
            &archive,
            dummy_content::bsa::v105(
                &[dummy_content::Entry::new("meshes/test.nif", b"runtime")],
                dummy_content::bsa::Compression::None,
            )
            .unwrap(),
        )
        .unwrap();

        let extracted = ArchiveExtractor::extract(&archive, &output).unwrap();
        assert_eq!(extracted.len(), 1);
        assert_eq!(
            fs::read(output.join("meshes/test.nif")).unwrap(),
            b"runtime"
        );
        assert!(!stale_registry.exists());
        assert!(
            stale_payloads
                .iter()
                .all(|path| !path.parent().unwrap().exists())
        );
        assert_eq!(fs::read(live_file.path()).unwrap(), b"staged by live run");
        assert_eq!(fs::read(unrelated.join(OWNER)).unwrap(), b"unrelated data");
        assert!(!interrupted.join(OWNER).exists());
        assert_eq!(
            fs::read(interrupted.join("retained")).unwrap(),
            b"independent file"
        );
    }
}
