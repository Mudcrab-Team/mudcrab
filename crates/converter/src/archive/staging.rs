use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, TryLockError},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use color_eyre::{
    Result,
    eyre::{ContextCompat, WrapErr, bail, ensure},
};
use tempfile::{NamedTempFile, TempDir};

const RUN_PREFIX: &str = ".mudcrab-extract-run-";
const PAYLOAD_PREFIX: &str = ".mudcrab-extract-payload-";
const FILE_PREFIX: &str = ".mudcrab-staged-";
const OWNER: &str = "owner";
const SIGNATURE: &[u8] = b"Mudcrab archive staging v2\n";
const STATE_DIR: &str = ".mudcrab-extraction-state";
const FALLBACK_PREFIX: &str = ".mudcrab-extraction-state-";

pub(super) fn reserved(component: &std::ffi::OsStr) -> bool {
    let name = component.as_encoded_bytes();
    name.eq_ignore_ascii_case(STATE_DIR.as_bytes())
        || [PAYLOAD_PREFIX, FALLBACK_PREFIX].iter().any(|prefix| {
            name.get(..prefix.len())
                .is_some_and(|name| name.eq_ignore_ascii_case(prefix.as_bytes()))
        })
}

fn state_paths() -> impl Iterator<Item = PathBuf> {
    let preferred = std::env::var_os(if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "XDG_STATE_HOME"
    })
    .map(PathBuf::from)
    .filter(|path| path.is_absolute())
    .or_else(std::env::home_dir)
    .map(|base| base.join(STATE_DIR));
    #[cfg(unix)]
    let fallback = Some(std::env::temp_dir().join(format!(
        "{FALLBACK_PREFIX}{}",
        rustix::process::geteuid().as_raw()
    )));
    #[cfg(not(unix))]
    let fallback = None;
    [preferred, fallback].into_iter().flatten()
}

fn new_registry(root: &Path) -> Result<TempDir> {
    let prefix = run_prefix(root);
    let mut errors = Vec::new();
    for path in state_paths() {
        let result = (|| -> Result<TempDir> {
            check_state_path(root, &path)?;
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.recursive(true).create(&path)?;
            let state = check_state_directory(root, &path)?;
            Ok(tempfile::Builder::new()
                .prefix(&prefix)
                .rand_bytes(16)
                .tempdir_in(state)?)
        })();
        match result {
            Ok(registry) => return Ok(registry),
            Err(error) => errors.push(format!("{} ({error:#})", path.display())),
        }
    }
    if errors.is_empty() {
        bail!("archive staging needs a per-user state directory");
    }
    bail!(
        "archive staging needs a writable private state directory. Tried {}",
        errors.join(", ")
    );
}

fn check_state_path(root: &Path, path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute() && !path.starts_with(root),
        "archive staging state must be outside the extraction root"
    );
    let parent = path
        .parent()
        .wrap_err("archive staging state has no parent")?;
    if let Ok(parent) = parent.canonicalize() {
        ensure!(
            !parent.starts_with(root),
            "archive staging state must be outside the extraction root"
        );
    }
    #[cfg(unix)]
    if path.file_name().is_some_and(|name| {
        name.as_encoded_bytes()
            .starts_with(FALLBACK_PREFIX.as_bytes())
    }) {
        use std::os::unix::fs::MetadataExt;
        let metadata = parent.metadata()?;
        ensure!(
            (metadata.uid() == 0 || metadata.uid() == rustix::process::geteuid().as_raw())
                && (metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0),
            "the archive staging fallback requires a private or sticky temporary directory"
        );
    }
    Ok(())
}

fn check_state_directory(root: &Path, path: &Path) -> Result<PathBuf> {
    let metadata = path.symlink_metadata()?;
    ensure!(metadata.is_dir(), "invalid archive staging state directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.uid() == rustix::process::geteuid().as_raw() && metadata.mode() & 0o077 == 0,
            "archive staging state must be private to the current user"
        );
    }
    let path = path.canonicalize()?;
    ensure!(
        !path.starts_with(root),
        "archive staging state must be outside the extraction root"
    );
    Ok(path)
}

fn run_prefix(root: &Path) -> String {
    format!(
        "{RUN_PREFIX}{}-",
        crate::cache::hash_bytes(root.as_os_str().as_encoded_bytes())
    )
}

fn payload_relative<'a>(root: &Path, destination: &'a Path) -> Result<&'a Path> {
    let relative = destination.strip_prefix(root).wrap_err_with(|| {
        format!(
            "archive staging directory escapes extraction root: {}",
            destination.display()
        )
    })?;
    ensure!(
        !relative.iter().any(reserved),
        "reserved archive staging directory"
    );
    Ok(relative)
}

fn create_payload_parent(root: &Path, parent: &Path) -> Result<PathBuf> {
    let requested = root.join(parent);
    let mut ancestor = requested.as_path();
    loop {
        match ancestor.symlink_metadata() {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .wrap_err("missing archive extraction root")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let destination = ancestor
        .canonicalize()?
        .join(requested.strip_prefix(ancestor)?);
    payload_relative(root, &destination)?;
    fs::create_dir_all(&destination)?;
    Ok(destination.canonicalize()?)
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
        ensure!(
            !root.iter().any(reserved),
            "cannot extract into archive staging state"
        );
        let registry = new_registry(&root)?;
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
            let destination = create_payload_parent(&root, parent)?;
            let relative = payload_relative(&root, &destination)?;
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

    fn check_staging_space<K: Ord>(
        &self,
        required: &BTreeMap<&Path, u64>,
        space: impl Fn(&Path) -> io::Result<(K, u64)>,
    ) -> Result<()> {
        let mut volumes = BTreeMap::new();
        for (parent, bytes) in required {
            let directory = self.directories[*parent].path();
            let (device, available) = space(directory)?;
            let (total, free, _) = volumes
                .entry(device)
                .or_insert((0u64, available, directory));
            *free = (*free).min(available);
            *total = total
                .checked_add(*bytes)
                .wrap_err("archive staging size overflow")?;
        }
        for (total, available, directory) in volumes.into_values() {
            ensure!(
                total <= available,
                "insufficient archive staging space at {}: need at least {} bytes, {} available",
                directory.display(),
                total,
                available
            );
        }
        Ok(())
    }

    pub(super) fn check_space(&self, required: &BTreeMap<&Path, u64>) -> Result<()> {
        self.check_staging_space(required, filesystem_space)
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

#[cfg(unix)]
fn filesystem_space(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let stats = rustix::fs::statvfs(path)?;
    Ok((
        path.metadata()?.dev(),
        stats.f_bavail.saturating_mul(stats.f_frsize),
    ))
}

#[cfg(windows)]
fn filesystem_space(path: &Path) -> io::Result<(Vec<u16>, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW};
    let path: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut volume = vec![0u16; 32768];
    let mut available = 0;
    // SAFETY: path is NUL-terminated and both APIs receive writable buffers of the stated size.
    unsafe {
        if GetVolumePathNameW(path.as_ptr(), volume.as_mut_ptr(), 32768) == 0
            || GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    volume.truncate(volume.iter().position(|&c| c == 0).unwrap_or(volume.len()));
    Ok((volume, available))
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
    if let Some(entries) = staging_entries(&root)? {
        for entry in entries {
            let entry = entry?;
            if entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(RUN_PREFIX.as_bytes())
            {
                // Names under the output root are not proof of ownership.
                report_skip(&entry.path(), "matches the legacy run registry prefix");
            }
        }
    }
    let prefix = run_prefix(&root);
    for path in state_paths() {
        // A retry can use a different state location if home becomes writable again.
        match check_state_path(&root, &path).and_then(|()| check_state_directory(&root, &path)) {
            Ok(state) => reclaim_state(&root, &state, &prefix)?,
            Err(error)
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) => {}
            Err(error) => report_skip(&path, format_args!("{error:#}")),
        }
    }
    Ok(())
}

fn report_skip(path: &Path, reason: impl std::fmt::Display) {
    eprintln!(
        "warning: archive staging cleanup skipped {} ({reason})",
        path.display()
    );
}

fn state_unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::ReadOnlyFilesystem
    )
}

fn staging_entries(path: &Path) -> io::Result<Option<fs::ReadDir>> {
    match fs::read_dir(path) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if state_unavailable(&error) => {
            report_skip(path, error);
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn reclaim_state(root: &Path, state: &Path, prefix: &str) -> Result<()> {
    let Some(entries) = staging_entries(state)? else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(prefix) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => reclaim(root, &entry.path(), &name)?,
            Ok(_) => report_skip(&entry.path(), "run registry is not a directory"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                report_skip(&entry.path(), error)
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn reclaim(root: &Path, registry: &Path, run_name: &str) -> Result<()> {
    let marker = registry.join(OWNER);
    let mut owner = match open_marker(&marker) {
        Ok(Some(owner)) => owner,
        Ok(None) => {
            report_skip(&marker, "owner marker is not a regular file");
            return Ok(());
        }
        Err(error) if state_unavailable(&error) => {
            report_skip(&marker, format_args!("owner marker unavailable: {error}"));
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    match owner.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            report_skip(registry, "run is active");
            return Ok(());
        }
        Err(TryLockError::Error(error)) => return Err(error.into()),
    }
    let mut signature = [0; SIGNATURE.len()];
    let recognized = match owner.read_exact(&mut signature) {
        Ok(()) => signature == SIGNATURE,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => false,
        Err(error) => return Err(error.into()),
    };
    if !recognized {
        report_skip(&marker, "unrecognized owner signature");
        return Ok(());
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
        reclaim_payload(&root.join(directory), &witness)?;
    }
    // Only the marker belongs to the registry. Preserve files added after a crash.
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if state_unavailable(&error) => {
            report_skip(&marker, error);
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    match fs::remove_dir(registry) {
        Err(error)
            if state_unavailable(&error) || error.kind() == io::ErrorKind::DirectoryNotEmpty =>
        {
            report_skip(registry, error);
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

fn reclaim_payload(path: &Path, witness: &str) -> io::Result<()> {
    let verified = (|| -> io::Result<bool> {
        Ok(path.symlink_metadata()?.is_dir() && path.canonicalize()? == path)
    })();
    match verified {
        Ok(true) => {}
        Ok(false) => {
            report_skip(path, "payload is a symlink or has been replaced");
            return Ok(());
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            report_skip(path, error);
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    match path.join(witness).symlink_metadata() {
        Ok(metadata) if metadata.is_file() => remove_payload(path, witness),
        Ok(_) => {
            report_skip(path, "payload witness is not a regular file");
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            report_skip(path, "payload witness is missing");
            Ok(())
        }
        Err(error) => Err(error),
    }
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
        Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {
            report_skip(path, error);
            Ok(())
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::ArchiveExtractor;

    #[test]
    fn staging_space_is_shared_only_within_a_volume() {
        let root = tempfile::tempdir().unwrap();
        let a = Path::new("a");
        let b = Path::new("b");
        let run = Run::new(root.path(), BTreeSet::from([a, b])).unwrap();
        let required = BTreeMap::from([(a, 60), (b, 60)]);
        assert!(
            run.check_staging_space(&required, |_| Ok((1, 100)))
                .is_err()
        );
        run.check_staging_space(&required, |dir| Ok((dir.parent().unwrap().to_owned(), 100)))
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn creates_nested_output_with_private_staging() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().canonicalize().unwrap().join("out");
        let parent = Path::new("meshes/architecture/interior");
        let run = Run::new(&output, BTreeSet::from([parent])).unwrap();
        let staged = run.new_file(parent).unwrap();
        let payload = staged.path().parent().unwrap().to_owned();
        assert_eq!(payload.parent(), Some(output.join(parent).as_path()));
        let registry = run._registry.path().to_owned();
        for path in [payload.as_path(), registry.parent().unwrap()] {
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert!(!registry.starts_with(&output));
        staged
            .persist(output.join(parent).join("model.nif"))
            .unwrap();
        drop(run);
        assert!(!payload.exists());
        assert!(!registry.exists());
        assert!(output.join(parent).join("model.nif").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn read_only_home_allows_extraction_and_retry() {
        use std::{os::unix::fs::PermissionsExt, process::Command};

        let uid = rustix::process::geteuid();
        if uid.is_root() {
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("home");
        let scratch = fixture.path().join("scratch");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&scratch).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o500)).unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "archive::staging::tests::extraction_retry_reclaims_abandoned_payloads_but_keeps_a_live_run",
            ])
            .env("HOME", &home)
            .env("TMPDIR", &scratch)
            .env_remove("XDG_STATE_HOME")
            .output()
            .unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!home.join(STATE_DIR).exists());
        let state = scratch.join(format!("{FALLBACK_PREFIX}{}", uid.as_raw()));
        assert_eq!(
            state.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn recovery_preserves_legacy_registries_and_symlink_payloads() {
        use std::os::unix::fs::symlink;

        let output = tempfile::tempdir().unwrap();
        let legacy = output.path().join(format!("{RUN_PREFIX}legacy"));
        let v1 = b"Mudcrab archive staging v1\n";
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join(OWNER), v1).unwrap();

        let Run {
            directories,
            _registry,
            _owner,
        } = Run::new(output.path(), BTreeSet::from([Path::new("")])).unwrap();
        let registry = _registry.keep();
        let payload = directories.into_values().next().unwrap().keep();
        let unrelated = tempfile::tempdir().unwrap();
        let preserved = unrelated.path().join(format!("{FILE_PREFIX}independent"));
        fs::write(&preserved, b"preserve").unwrap();
        let witness = fs::read_dir(&payload).unwrap().next().unwrap().unwrap();
        fs::write(unrelated.path().join(witness.file_name()), b"").unwrap();
        fs::remove_file(witness.path()).unwrap();
        fs::remove_dir(&payload).unwrap();
        symlink(unrelated.path(), &payload).unwrap();
        drop(_owner);

        reclaim_abandoned(output.path()).unwrap();

        assert!(!registry.exists());
        assert!(payload.is_symlink());
        assert_eq!(fs::read(&preserved).unwrap(), b"preserve");
        assert_eq!(fs::read(legacy.join(OWNER)).unwrap(), v1);
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

        let state = tempfile::tempdir().unwrap();
        let prefix = run_prefix(&output.canonicalize().unwrap());
        let unrelated = state.path().join(format!("{prefix}unrelated"));
        fs::create_dir(&unrelated).unwrap();
        fs::write(unrelated.join(OWNER), b"unrelated data").unwrap();
        let v1 = state.path().join(format!("{prefix}v1"));
        fs::create_dir(&v1).unwrap();
        let v1_signature = b"Mudcrab archive staging v1\n";
        fs::write(v1.join(OWNER), v1_signature).unwrap();
        let unmarked = state.path().join(format!("{prefix}unmarked"));
        fs::create_dir(&unmarked).unwrap();
        fs::write(unmarked.join("retained"), b"independent file").unwrap();
        let interrupted = state.path().join(format!("{prefix}interrupted"));
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

        reclaim_state(&output.canonicalize().unwrap(), state.path(), &prefix).unwrap();
        let empty = ArchiveExtractor::extract_matching(&archive, &output, |_| false).unwrap();
        assert!(empty.is_empty());
        assert!(!stale_registry.exists());
        assert!(!output.join("meshes/test.nif").exists());
        assert!(
            stale_payloads
                .iter()
                .all(|path| !path.parent().unwrap().exists())
        );
        assert_eq!(fs::read(live_file.path()).unwrap(), b"staged by live run");

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
        assert_eq!(fs::read(v1.join(OWNER)).unwrap(), v1_signature);
        assert_eq!(
            fs::read(unmarked.join("retained")).unwrap(),
            b"independent file"
        );
        assert!(!interrupted.join(OWNER).exists());
        assert_eq!(
            fs::read(interrupted.join("retained")).unwrap(),
            b"independent file"
        );
    }
}
