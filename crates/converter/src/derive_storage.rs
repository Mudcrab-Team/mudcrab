//! Own exactly one newly-created derive staging directory; never discover old ones.
use super::is_link;
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use shared::{asset_lock::AssetLock, cluster_overlay::OVERLAY_DIR_PREFIX};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) struct Staging {
    path: PathBuf,
    identity: fs::File,
    keep_failed: bool,
    published: bool,
    /// Held for the staging's whole life, so a base rebuild can tell a running derivation from a
    /// dead one (`derived_content_to_discard`).
    lock: Option<AssetLock>,
}

impl Staging {
    pub fn create(path: PathBuf, keep_failed: bool) -> Result<Self> {
        let lock = AssetLock::acquire_exclusive(&path)
            .wrap_err_with(|| format!("cannot lock derive staging {}", path.display()))?;
        // create_dir, never create_dir_all: an existing path is somebody else's.
        if let Err(error) = fs::create_dir(&path) {
            release(lock);
            return Err(error.into());
        }
        let identity = open_directory(&path).wrap_err_with(|| {
            format!(
                "cannot retain staging identity; new empty staging left at {}",
                path.display(),
            )
        })?;
        Ok(Self {
            path,
            identity,
            keep_failed,
            published: false,
            lock: Some(lock),
        })
    }

    pub fn published(&mut self) {
        self.published = true;
    }

    fn remove(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.path)?;
        ensure!(
            metadata.is_dir() && !is_link(&metadata),
            "staging path was replaced with a link or non-directory"
        );
        let current = open_directory(&self.path)?;
        ensure!(
            file_id(&self.identity)? == file_id(&current)?,
            "staging directory identity changed"
        );
        // Refuse reparse points before recursive deletion, including unexpected stage outputs.
        // This is a cooperative-process contract, not protection against a
        // hostile local actor swapping descendants between this walk and remove_dir_all.
        validate_tree(&self.path)?;
        fs::remove_dir_all(&self.path)?;
        Ok(())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        self.finish();
        if let Some(lock) = self.lock.take() {
            release(lock);
        }
    }
}

/// Release a staging lock, then remove its file: only an unlocked file may go.
fn release(lock: AssetLock) {
    let lock_file = lock.lock_path().to_owned();
    drop(lock);
    let _ = fs::remove_file(lock_file);
}

impl Staging {
    fn finish(&self) {
        if self.published {
            return;
        }
        if self.keep_failed {
            eprintln!(
                "derive-clusters: failed staging kept by --keep-failed-staging: {}",
                self.path.display()
            );
        } else {
            match self.remove() {
                Ok(()) => eprintln!(
                    "derive-clusters: removed own failed staging: {}",
                    self.path.display()
                ),
                Err(error) => eprintln!(
                    "derive-clusters: could not remove own failed staging {}; retained remainder: {error:#}",
                    self.path.display(),
                ),
            }
        }
    }
}

fn validate_tree(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            !is_link(&metadata),
            "refusing linked path in failed staging: {}",
            entry.path().display()
        );
        if metadata.is_dir() {
            validate_tree(&entry.path())?;
        } else {
            ensure!(metadata.is_file(), "special file in failed staging");
            // Staging holds cluster outputs only. Clear read-only destination
            // attributes as needed for Windows deletion; no base file is here.
            #[cfg(windows)]
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "Windows-only clearing of FILE_ATTRIBUTE_READONLY on owned staging files"
            )]
            if metadata.permissions().readonly() {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                fs::set_permissions(entry.path(), permissions)?;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn open_directory(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(windows))]
fn open_directory(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

#[cfg(windows)]
pub(super) fn file_id(file: &fs::File) -> Result<(u64, [u8; 16])> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };
    let mut info = FILE_ID_INFO::default();
    // SAFETY: the file is open and info is a correctly sized writable SDK structure.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}

#[cfg(unix)]
pub(super) fn file_id(file: &fs::File) -> Result<(u64, [u8; 16])> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    let mut identity = [0; 16];
    identity[..8].copy_from_slice(&metadata.ino().to_le_bytes());
    Ok((metadata.dev(), identity))
}

#[cfg(not(any(windows, unix)))]
pub(super) fn file_id(_: &fs::File) -> Result<(u64, [u8; 16])> {
    color_eyre::eyre::bail!("staging identity not implemented on this platform")
}

use crate::derive_space::RETAINED_FREE_BYTES;

// Advisory estimate only, never an admission bound: the write-time guard is authoritative.
// Calibration: a same-core, threshold-1024 terrain derivation (2026-10-20).
// Base database: 2,173,202,432 bytes; cell cache: 488,986,104 bytes.
// Derived database: 2,736,652,288 bytes (growth 563,449,856); manifests under 1 MB.
// Mesh, object and terrain payload sizes remain explicit below. These measured ratios
// are advisory hints; the write-time guard determines actual available space.
const MEASURED_BASE_MESH_BYTES: u64 = 2_570_110_144;
const MEASURED_OBJECT_PAYLOAD_BYTES: u64 = 2_908_039_936;
const MEASURED_BASE_DATABASE_BYTES: u64 = 2_173_202_432;
const MEASURED_TABLE_GROWTH_BYTES: u64 = 563_449_856;
const MEASURED_CELL_CACHE_BYTES: u64 = 488_986_104;
const MEASURED_TERRAIN_PAYLOAD_BYTES: u64 = 165_344_400;
const MEASURED_MANIFEST_ALLOWANCE_BYTES: u64 = 1024 * 1024;

fn add(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right)
        .ok_or_else(|| color_eyre::eyre::eyre!("overlay space estimate overflow"))
}

#[cfg(any(unix, test))]
fn multiply(bytes: u64, factor: u64) -> Result<u64> {
    bytes
        .checked_mul(factor)
        .ok_or_else(|| color_eyre::eyre::eyre!("overlay space estimate overflow"))
}

fn measured_share(bytes: u64, output: u64, input: u64) -> Result<u64> {
    u64::try_from((u128::from(bytes) * u128::from(output)).div_ceil(u128::from(input)))
        .map_err(|_| color_eyre::eyre::eyre!("overlay space estimate overflow"))
}

fn tree_bytes(path: &Path) -> Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        !is_link(&metadata),
        "space inventory refuses linked path {}",
        path.display()
    );
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    ensure!(
        metadata.is_dir(),
        "space inventory refuses special path {}",
        path.display()
    );
    fs::read_dir(path)?.try_fold(0, |total, entry| add(total, tree_bytes(&entry?.path())?))
}

pub(super) fn admit(
    base: &Path,
    derived: &Path,
    locked_output: &Path,
    terrain: bool,
    free_space: impl FnOnce(&Path) -> Result<u64>,
) -> Result<Vec<String>> {
    let mut notices = Vec::new();
    // The separately measured terrain share is charged only when requested, including a
    // first terrain build whose previous overlay held only objects.
    let terrain_budget = if terrain {
        measured_share(
            tree_bytes(&base.join("cell_cache.rkyv"))?,
            MEASURED_TERRAIN_PAYLOAD_BYTES,
            MEASURED_CELL_CACHE_BYTES,
        )?
    } else {
        0
    };
    let mut previous = None;
    let fingerprint = shared::cluster_overlay::base_fingerprint(base)?;
    for entry in fs::read_dir(derived)? {
        let path = entry?.path();
        if !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("clusters-"))
        {
            continue;
        }
        // A sibling is only a size hint. Corruption or an unavailable sibling lock
        // cannot prevent deriving another output. The target remains authoritative.
        let hint = (|| -> Result<Option<u64>> {
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(!is_link(&metadata), "overlay hint is linked");
            if !metadata.is_dir() {
                return Ok(None);
            }
            let _hint_lock = (path != locked_output)
                .then(|| AssetLock::acquire_shared(&path))
                .transpose()?;
            let overlay = shared::cluster_overlay::read_metadata(&path)?;
            if overlay.base_fingerprint != fingerprint {
                return Ok(None);
            }
            let has_terrain = overlay
                .extensions
                .iter()
                .any(|entry| entry.id == shared::extensions::CLUSTER_TERRAIN);
            Ok(Some(add(
                tree_bytes(&path)?,
                if terrain && !has_terrain {
                    terrain_budget
                } else {
                    0
                },
            )?))
        })();
        match hint {
            Ok(Some(growth)) => {
                previous = Some(previous.map_or(growth, |prior: u64| prior.max(growth)));
            }
            Ok(None) => {}
            Err(error) if path != locked_output => {
                skipped_sibling(&mut notices, &path, "space estimate", &error);
            }
            Err(error) => return Err(error),
        }
    }
    let estimate = match previous {
        Some(previous) => previous,
        None => add(
            add(
                measured_share(
                    tree_bytes(&base.join("meshes"))?,
                    MEASURED_OBJECT_PAYLOAD_BYTES,
                    MEASURED_BASE_MESH_BYTES,
                )?,
                terrain_budget,
            )?,
            add(
                measured_share(
                    tree_bytes(&base.join("skyrim_world.db"))?,
                    MEASURED_TABLE_GROWTH_BYTES,
                    MEASURED_BASE_DATABASE_BYTES,
                )?,
                MEASURED_MANIFEST_ALLOWANCE_BYTES,
            )?,
        )?,
    };
    let needed = add(estimate, RETAINED_FREE_BYTES)?;
    let free = free_space(derived)?;
    eprintln!(
        "derive-clusters admission: volume path {}, output estimate {estimate} bytes, retained floor {RETAINED_FREE_BYTES} bytes, needed {needed} bytes, free {free} bytes",
        derived.display()
    );
    ensure!(
        free >= RETAINED_FREE_BYTES,
        "insufficient free space on {}: needed at least {RETAINED_FREE_BYTES} bytes retained, free {free} bytes",
        derived.display()
    );
    if free < needed {
        let notice = format!(
            "advisory overlay space estimate on {}: estimated need {needed} bytes including retained floor, free {free} bytes; continuing under the authoritative write-time guard",
            derived.display()
        );
        eprintln!("derive-clusters: {notice}");
        notices.push(notice);
    }
    Ok(notices)
}

fn skipped_sibling(
    notices: &mut Vec<String>,
    path: &Path,
    purpose: &str,
    error: &color_eyre::Report,
) {
    let notice = format!(
        "skipped unrelated overlay {} while checking {purpose}: {error:#}; inspect or move this sibling aside if it should be usable",
        path.display()
    );
    eprintln!("derive-clusters: {notice}");
    notices.push(notice);
}

#[cfg(windows)]
pub(super) fn available_space(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    ensure!(
        !wide[..wide.len() - 1].contains(&0),
        "NUL in free-space path"
    );
    let mut available = 0;
    // SAFETY: terminated UTF-16 path and live writable output; unused outputs may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(available)
}

#[cfg(unix)]
pub(super) fn available_space(path: &Path) -> Result<u64> {
    let stat = rustix::fs::statvfs(path)?;
    multiply(stat.f_bavail, stat.f_frsize)
}

#[cfg(not(any(windows, unix)))]
pub(super) fn available_space(_: &Path) -> Result<u64> {
    color_eyre::eyre::bail!("overlay free-space admission unsupported on this platform")
}

/// Publish `staging` as `output`. `retired` is another overlay that held the same label under a
/// different settings id: it is set aside first and removed only after the new overlay is in
/// place, and put back if publication fails.
pub(super) fn publish(
    staging: &Path,
    output: &Path,
    replace: bool,
    retired: Option<&Path>,
) -> Result<()> {
    let Some(retired) = retired else {
        return publish_one(staging, output, replace);
    };
    let aside = SetAside::new(retired)?;
    match publish_one(staging, output, replace) {
        Ok(()) => {
            aside.discard();
            Ok(())
        }
        Err(error) => Err(aside.restore(error)),
    }
}

/// An overlay renamed out of the way under a `.replaced-` name, awaiting the fate of the
/// publication that displaced it.
struct SetAside {
    original: PathBuf,
    backup: PathBuf,
    identity: fs::File,
}

impl SetAside {
    fn new(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_dir() && !is_link(&metadata),
            "overlay to retire is not a regular directory"
        );
        let identity = open_directory(path)?;
        let backup = replaced_name(path, "-label")?;
        ensure!(
            !backup.try_exists()?,
            "overlay retirement backup already exists"
        );
        fs::rename(path, &backup)?;
        Ok(Self {
            original: path.to_owned(),
            backup,
            identity,
        })
    }

    fn restore(self, error: color_eyre::Report) -> color_eyre::Report {
        match fs::rename(&self.backup, &self.original) {
            Ok(()) => match crate::durability::sync_parent(&self.original) {
                Ok(()) => error,
                Err(sync) => error.wrap_err(format!(
                    "overlay restored at {} but its parent could not be synced ({sync:#})",
                    self.original.display()
                )),
            },
            Err(restore) => error.wrap_err(format!(
                "restoring the overlay that held the label failed ({restore}); it remains at {}",
                self.backup.display()
            )),
        }
    }

    fn discard(self) {
        remove_replaced(&self.identity, &self.backup);
    }
}

fn replaced_name(path: &Path, suffix: &str) -> Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    Ok(path.with_file_name(format!(".replaced-{}-{stamp}{suffix}", std::process::id())))
}

/// Publication has succeeded. Cleanup failures leave an explicitly named old overlay for
/// owner-reviewed cleanup, never roll back a valid new publication.
fn remove_replaced(identity: &fs::File, backup: &Path) {
    let cleanup = (|| -> Result<()> {
        ensure!(
            file_id(identity)? == file_id(&open_directory(backup)?)?,
            "old overlay identity changed"
        );
        validate_tree(backup)?;
        fs::remove_dir_all(backup)?;
        Ok(())
    })();
    match cleanup {
        Ok(()) => eprintln!(
            "derive-clusters: removed replaced overlay {}",
            backup.display()
        ),
        Err(error) => eprintln!(
            "derive-clusters: published new overlay; previous overlay retained at {}: {error:#}",
            backup.display()
        ),
    }
}

/// The exclusive claim on an overlay label within one base, held through publication.
pub(super) struct LabelClaim {
    /// The overlay that holds the label under another settings id, locked for retirement.
    retired: Option<(PathBuf, AssetLock)>,
    _label: AssetLock,
    pub notices: Vec<String>,
}

impl LabelClaim {
    pub fn retired(&self) -> Option<&Path> {
        self.retired.as_ref().map(|(path, _)| path.as_path())
    }
}

/// Labels are unique within a base. Another overlay (never `locked_output`, whose replacement
/// `--replace` already governs) holding `label` refuses without `--replace`; with it, that
/// overlay is locked so the publication can retire it. Concurrent derivations of one label
/// serialise on a label lock, so two settings ids cannot both take it.
pub(super) fn claim_label(
    derived: &Path,
    locked_output: &Path,
    label: &str,
    replace: bool,
) -> Result<LabelClaim> {
    let digest = crate::cache::hash_bytes(label.as_bytes());
    let claim = AssetLock::acquire_exclusive(derived.join(format!(".label-{}", &digest[..16])))
        .wrap_err_with(|| format!("another derivation is claiming overlay label {label:?}"))?;
    let mut holders = Vec::new();
    let mut notices = Vec::new();
    for entry in fs::read_dir(derived)? {
        let path = entry?.path();
        let named = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(OVERLAY_DIR_PREFIX));
        if !named || path == locked_output {
            continue;
        }
        let plain_directory = (|| -> Result<bool> {
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(!is_link(&metadata), "overlay is linked");
            Ok(metadata.is_dir())
        })();
        match plain_directory {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                skipped_sibling(&mut notices, &path, "label uniqueness", &error);
                continue;
            }
        }
        // A busy reader/writer is not corruption. Preserve label serialization:
        // never skip a potentially valid holder just because its lock is busy.
        let _reading = AssetLock::acquire_shared(&path)?;
        let existing = read_label(&path);
        let existing = match existing {
            Ok(existing) => existing,
            Err(error) => {
                skipped_sibling(&mut notices, &path, "label uniqueness", &error);
                continue;
            }
        };
        if existing.0 == label {
            holders.push((path, existing.1));
        }
    }
    let retired = match holders.pop() {
        None => None,
        Some((path, settings_id)) => {
            ensure!(
                holders.is_empty(),
                "several overlays already carry label {label:?}; remove all but one before deriving"
            );
            ensure!(
                replace,
                "overlay label {label:?} is already used by {} (settings {settings_id}); use --replace to replace it or choose another --label",
                path.display()
            );
            let lock = AssetLock::acquire_exclusive(&path)
                .wrap_err_with(|| format!("overlay {} is in use", path.display()))?;
            if read_label(&path)?.0 != label {
                bail!(
                    "overlay {} changed while its label was being claimed; retry",
                    path.display()
                );
            }
            Some((path, lock))
        }
    };
    Ok(LabelClaim {
        retired,
        _label: claim,
        notices,
    })
}

fn read_label(overlay: &Path) -> Result<(String, String)> {
    let metadata = shared::cluster_overlay::read_metadata(overlay).wrap_err_with(|| {
        format!(
            "cannot read {} to check overlay label uniqueness",
            overlay.display()
        )
    })?;
    Ok((metadata.label, metadata.settings_id))
}

fn publish_one(staging: &Path, output: &Path, replace: bool) -> Result<()> {
    publish_one_with_sync(staging, output, replace, crate::durability::sync_parent)
}

// The injected operation observes the real publication boundary in filesystem tests.
fn publish_one_with_sync(
    staging: &Path,
    output: &Path,
    replace: bool,
    sync_parent: impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    // Data and directory contents must reach storage before their staging name
    // can become the published package. Do this before moving any previous output.
    crate::durability::sync_tree(staging)?;
    if !output.try_exists()? {
        fs::rename(staging, output)?;
        return sync_parent(output).wrap_err_with(|| format!(
            "overlay installed at {} but its publication parent could not be synced; installed bytes are retained",
            output.display()));
    }
    ensure!(replace, "overlay exists; use --replace");
    let metadata = fs::symlink_metadata(output)?;
    ensure!(
        metadata.is_dir() && !is_link(&metadata),
        "existing overlay is not a regular directory"
    );
    let identity = open_directory(output)?;
    let backup = replaced_name(output, "")?;
    ensure!(
        !backup.try_exists()?,
        "overlay replacement backup already exists"
    );
    fs::rename(output, &backup)?;
    if let Err(error) = fs::rename(staging, output) {
        fs::rename(&backup, output).wrap_err_with(|| format!(
            "publishing overlay failed ({error}); restoring previous overlay failed; previous overlay remains {}", backup.display()))?;
        sync_parent(output).wrap_err_with(|| format!(
            "publishing overlay failed ({error}); previous overlay restored at {} but its parent could not be synced",
            output.display()))?;
        return Err(error.into());
    }
    // Persist the installed name and backup name before deleting recovery data.
    // A failed sync retains both complete directories and reports their paths.
    sync_parent(output).wrap_err_with(|| format!(
        "overlay installed at {} but its publication parent could not be synced; previous overlay retained at {}",
        output.display(), backup.display()))?;
    remove_replaced(&identity, &backup);
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn publication_syncs_the_installed_name_before_discarding_previous_bytes() {
        use std::cell::Cell;
        for replace in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let output = root.path().join("output");
            let staging = root.path().join("staging");
            fs::create_dir(&staging).unwrap();
            fs::write(staging.join("payload"), b"new").unwrap();
            if replace {
                fs::create_dir(&output).unwrap();
                fs::write(output.join("payload"), b"old").unwrap();
            }
            let calls = Cell::new(0);
            let error = publish_one_with_sync(&staging, &output, replace, |path| {
                assert_eq!(path, output);
                assert_eq!(fs::read(path.join("payload")).unwrap(), b"new");
                if replace {
                    assert_eq!(replaced_leftovers(root.path()), 1);
                }
                calls.set(calls.get() + 1);
                bail!("injected publication parent sync failure")
            });
            assert!(
                error.is_err(),
                "publication must not acknowledge an unsynced installed name"
            );
            assert_eq!(calls.get(), 1);
            assert_eq!(fs::read(output.join("payload")).unwrap(), b"new");
            if replace {
                let backup = fs::read_dir(root.path())
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| {
                        path.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with(".replaced-")
                    })
                    .expect("previous bytes retained after failed publication sync");
                assert_eq!(fs::read(backup.join("payload")).unwrap(), b"old");
            }
        }
    }

    use super::*;

    #[test]
    fn failed_replace_restores_old_overlay() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("old");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("keep"), b"old bytes").unwrap();
        assert!(publish(&root.path().join("missing-stage"), &output, true, None).is_err());
        assert_eq!(fs::read(output.join("keep")).unwrap(), b"old bytes");
    }

    fn replaced_leftovers(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".replaced-")
            })
            .count()
    }

    #[test]
    fn failed_publication_puts_back_the_overlay_that_held_the_label() {
        let root = tempfile::tempdir().unwrap();
        let retired = root.path().join("clusters-old");
        fs::create_dir(&retired).unwrap();
        fs::write(retired.join("keep"), b"old bytes").unwrap();
        let output = root.path().join("clusters-new");
        assert!(
            publish(
                &root.path().join("missing-stage"),
                &output,
                false,
                Some(&retired)
            )
            .is_err()
        );
        assert_eq!(fs::read(retired.join("keep")).unwrap(), b"old bytes");
        assert!(!output.exists());
        assert_eq!(replaced_leftovers(root.path()), 0);
    }

    #[test]
    fn successful_publication_retires_the_overlay_that_held_the_label() {
        let root = tempfile::tempdir().unwrap();
        let retired = root.path().join("clusters-old");
        fs::create_dir(&retired).unwrap();
        fs::write(retired.join("keep"), b"old bytes").unwrap();
        let stage = root.path().join("stage");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("new"), b"new bytes").unwrap();
        let output = root.path().join("clusters-new");
        publish(&stage, &output, false, Some(&retired)).unwrap();
        assert!(!retired.exists());
        assert_eq!(fs::read(output.join("new")).unwrap(), b"new bytes");
        assert_eq!(replaced_leftovers(root.path()), 0);
    }

    #[test]
    fn staging_lock_guards_a_live_derivation_and_leaves_nothing_after_a_refusal() {
        let root = tempfile::tempdir().unwrap();
        let existing = root.path().join("stage");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("sentinel"), b"somebody else's").unwrap();
        assert!(Staging::create(existing.clone(), false).is_err());
        assert_eq!(
            fs::read(existing.join("sentinel")).unwrap(),
            b"somebody else's"
        );
        assert!(!shared::asset_lock::asset_lock_path(&existing).exists());
        let fresh = root.path().join("fresh");
        let live = Staging::create(fresh.clone(), false).unwrap();
        assert!(shared::asset_lock::asset_lock_path(&fresh).is_file());
        assert!(AssetLock::acquire_exclusive(&fresh).is_err());
        drop(live);
        assert!(!fresh.exists());
        assert!(!shared::asset_lock::asset_lock_path(&fresh).exists());
    }

    #[test]
    fn staging_never_adopts_existing_or_replaced_paths_and_disarms_after_publish() {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("stage");
        let moved = root.path().join("moved");
        let guard = Staging::create(stage.clone(), false).unwrap();
        assert!(Staging::create(stage.clone(), false).is_err());
        fs::rename(&stage, &moved).unwrap();
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("sentinel"), b"replacement").unwrap();
        drop(guard);
        assert_eq!(fs::read(stage.join("sentinel")).unwrap(), b"replacement");
        let original = root.path().join("own");
        let mut guard = Staging::create(original.clone(), false).unwrap();
        publish(&original, &root.path().join("published"), false, None).unwrap();
        guard.published();
        fs::create_dir(&original).unwrap();
        drop(guard);
        assert!(original.is_dir());
        assert!(root.path().join("published").is_dir());
    }

    #[test]
    fn space_arithmetic_refuses_overflow() {
        assert!(add(u64::MAX, 1).is_err());
        assert!(multiply(u64::MAX, 2).is_err());
        assert!(
            measured_share(
                u64::MAX,
                MEASURED_OBJECT_PAYLOAD_BYTES,
                MEASURED_BASE_MESH_BYTES
            )
            .is_err()
        );
    }
}
