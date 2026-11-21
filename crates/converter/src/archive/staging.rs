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
const FILE_PREFIX: &str = ".mudcrab-staged-";
const OWNER: &str = "owner";
const SIGNATURE: &[u8] = b"Mudcrab archive staging v2\n";
const STATE_DIR: &str = ".mudcrab-extraction-state";

pub(super) fn reserved(component: &std::ffi::OsStr) -> bool {
    let name = component.as_encoded_bytes();
    name.eq_ignore_ascii_case(STATE_DIR.as_bytes())
        || name
            .get(..PAYLOAD_PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PAYLOAD_PREFIX.as_bytes()))
}

fn state_directory() -> Result<PathBuf> {
    let base = std::env::var_os(if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "XDG_STATE_HOME"
    })
    .map(PathBuf::from)
    .filter(|path| path.is_absolute())
    .or_else(std::env::home_dir)
    .wrap_err("archive staging needs a per-user state directory")?;
    let path = base.join(STATE_DIR);
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.recursive(true).create(&path)?;
    let metadata = path.symlink_metadata()?;
    ensure!(metadata.is_dir(), "invalid archive staging state directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "archive staging state must be private to the current user"
        );
    }
    Ok(path)
}

fn run_prefix(root: &Path) -> String {
    format!(
        "{RUN_PREFIX}{}-",
        crate::cache::hash_bytes(root.as_os_str().as_encoded_bytes())
    )
}

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
        let root = root.canonicalize()?;
        let state = state_directory()?;
        ensure!(
            !root.iter().any(reserved),
            "cannot extract into archive staging state"
        );
        let registry = tempfile::Builder::new()
            .prefix(&run_prefix(&root))
            .rand_bytes(16)
            .tempdir_in(&state)?;
        let run_name = registry
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .wrap_err("invalid archive staging run name")?;
        let mut owner = tempfile::Builder::new()
            .rand_bytes(16)
            .tempfile_in(registry.path())?;
        let witness = owner
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .wrap_err("invalid archive staging witness")?
            .to_owned();
        owner.write_all(SIGNATURE)?;
        owner.as_file().lock()?;
        let mut owner = owner.persist(registry.path().join(OWNER))?;

        let mut paths = Vec::with_capacity(parents.len());
        let mut manifest = Vec::with_capacity(parents.len());
        for (index, parent) in parents.into_iter().enumerate() {
            let destination = root.join(parent);
            fs::create_dir_all(&destination)?;
            let destination = destination.canonicalize()?;
            let relative = destination.strip_prefix(&root)?;
            ensure!(
                !relative.iter().any(reserved),
                "reserved archive staging directory"
            );
            let name = payload_name(run_name, index);
            manifest.push(relative.join(&name));
            paths.push((parent, destination, name));
        }
        // The locked, private manifest is complete before any payload can be created.
        serde_json::to_writer(&mut owner, &(&witness, &manifest))?;

        let directories = paths
            .into_iter()
            .map(|(parent, destination, name)| {
                let mut builder = tempfile::Builder::new();
                builder.prefix(&name).rand_bytes(0);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    builder.permissions(fs::Permissions::from_mode(0o700));
                }
                let directory = builder.tempdir_in(&destination)?;
                File::create_new(directory.path().join(&witness))?;
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
        builder.prefix(FILE_PREFIX);
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
    let root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let prefix = run_prefix(&root);
    let entries = fs::read_dir(state_directory()?)?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => reclaim(&root, &entry.path(), &name)?,
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

    let (witness, directories): (String, Vec<PathBuf>) = match serde_json::from_reader(&owner) {
        Ok(directories) => directories,
        // Payload directories are only created after the entire list has been written.
        Err(error) if error.is_eof() => (String::new(), Vec::new()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        directories.is_empty()
            || Path::new(&witness).components().count() == 1
                && matches!(
                    Path::new(&witness).components().next(),
                    Some(Component::Normal(_))
                ),
        "invalid staging witness"
    );
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
        let directory = root.join(directory);
        match fs::symlink_metadata(directory.join(&witness)) {
            Ok(marker)
                if marker.is_file()
                    && directory.symlink_metadata()?.is_dir()
                    && directory.canonicalize()? == directory =>
            {
                remove_payload(&directory, &witness)?
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(error.into()),
        }
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

fn remove_payload(path: &Path, witness: &str) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(FILE_PREFIX)
            && entry.file_type()?.is_file()
        {
            fs::remove_file(entry.path())?;
        }
    }
    fs::remove_file(path.join(witness))?;
    match fs::remove_dir(path) {
        Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::ArchiveExtractor;

    #[cfg(unix)]
    #[test]
    fn stages_in_writable_child_of_read_only_root() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("out");
        fs::create_dir_all(output.join("meshes")).unwrap();
        let archive = temp.path().join("mesh.bsa");
        fs::write(
            &archive,
            dummy_content::bsa::v105(
                &[dummy_content::Entry::new("meshes/test.nif", b"mesh")],
                dummy_content::bsa::Compression::None,
            )
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&output, fs::Permissions::from_mode(0o555)).unwrap();
        let result = ArchiveExtractor::extract(&archive, &output);
        fs::set_permissions(&output, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(result.unwrap().len(), 1);
        assert_eq!(fs::read(output.join("meshes/test.nif")).unwrap(), b"mesh");
    }

    #[test]
    fn recovery_preserves_replacements_and_skips_absent_directories() {
        let output = tempfile::tempdir().unwrap();
        let first = Path::new("first");
        let second = Path::new("second");
        let Run {
            directories,
            _registry,
            _owner,
        } = Run::new(output.path(), BTreeSet::from([first, second])).unwrap();
        let registry = _registry.keep();
        for (parent, directory) in directories {
            let path = directory.path().to_owned();
            directory.close().unwrap();
            if parent == first {
                fs::create_dir(&path).unwrap();
                fs::write(path.join("independent"), b"retained").unwrap();
            } else {
                fs::remove_dir(output.path().join(&parent)).unwrap();
                fs::write(output.path().join(parent), b"replacement").unwrap();
            }
        }
        drop(_owner);
        reclaim_abandoned(output.path()).unwrap();
        assert!(!registry.exists());
        let replacement = fs::read_dir(output.path().join(first))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::read(replacement.join("independent")).unwrap(),
            b"retained"
        );
        assert_eq!(
            fs::read(output.path().join(second)).unwrap(),
            b"replacement"
        );
    }

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
        assert_eq!(fs::read(interrupted.join(OWNER)).unwrap(), SIGNATURE);
        assert_eq!(
            fs::read(interrupted.join("retained")).unwrap(),
            b"independent file"
        );
    }
}
