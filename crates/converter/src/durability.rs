//! Durable writes used by conversion and derived-package publication.

use color_eyre::{
    Result,
    eyre::{WrapErr, ensure, eyre},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Atomically replace a derived file after flushing its complete new contents.
/// Readers observe the old file or the complete new one.
pub fn replace_file(destination: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = temporary_beside(destination);
    let result = (|| -> Result<()> {
        write_synced(&temporary, bytes)?;
        fs::rename(&temporary, destination)
            .wrap_err_with(|| format!("cannot replace {}", destination.display()))?;
        sync_parent(destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn temporary_beside(destination: &Path) -> PathBuf {
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    destination
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            ".mudcrab-publish-{}-{unique}.tmp",
            std::process::id()
        ))
}

pub(crate) fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("missing publication parent"))?;
    crate::archive::sync_directory(parent)
}

/// Flush every staged file, then its containing directories from leaves to root.
/// The caller retains its exclusive staging lock through the final rename.
/// Unix fsyncs directory handles. Windows has no portable directory fsync;
/// complete file contents are flushed, with directory-entry durability supplied
/// by the filesystem. A lost entry is refused by the reader's certificate.
pub(crate) fn sync_tree(root: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let linked = metadata.file_type().is_symlink();
    ensure!(
        !linked,
        "publication staging contains a link: {}",
        root.display()
    );
    if metadata.is_file() {
        fs::OpenOptions::new()
            .read(true)
            .write(cfg!(windows))
            .open(root)?
            .sync_all()
            .wrap_err_with(|| format!("cannot sync staged file {}", root.display()))?;
        return Ok(());
    }
    ensure!(
        metadata.is_dir(),
        "publication staging contains a special file"
    );
    for entry in fs::read_dir(root)? {
        sync_tree(&entry?.path())?;
    }
    crate::archive::sync_directory(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_staging_preserves_complete_nested_payloads_and_rejects_missing_sources() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("clusters/meshes");
        fs::create_dir_all(&nested).unwrap();
        let payload = nested.join("payload.mcl");
        fs::write(&payload, b"complete staged bytes").unwrap();
        sync_tree(root.path()).unwrap();
        assert_eq!(fs::read(payload).unwrap(), b"complete staged bytes");
        assert!(sync_tree(&root.path().join("absent")).is_err());
    }

    #[test]
    fn replacing_a_linked_staged_file_never_mutates_the_previous_payload() {
        let root = tempfile::tempdir().unwrap();
        let previous = root.path().join("previous.mcl");
        let staged = root.path().join("staged.mcl");
        fs::write(&previous, b"previous bytes").unwrap();
        fs::hard_link(&previous, &staged).unwrap();
        replace_file(&staged, b"new complete bytes").unwrap();
        assert_eq!(fs::read(previous).unwrap(), b"previous bytes");
        assert_eq!(fs::read(staged).unwrap(), b"new complete bytes");
    }
}
