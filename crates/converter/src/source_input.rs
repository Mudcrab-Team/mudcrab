//! Small input snapshots and checked direct mappings avoid unbounded temporary copies.

use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use memmap2::{Mmap, MmapMut};
use same_file::Handle;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Deref,
    path::Path,
    time::SystemTime,
};

/// Anonymous copies are reserved for external inputs at or below this size.
/// The 16 MiB cap bounds the extra memory per snapshot while larger inputs use direct mappings.
pub(crate) const MEMORY_SNAPSHOT_LIMIT: u64 = 16 * 1024 * 1024;

/// An input view and the source generation checked after its consumer finishes.
pub(crate) struct InputMap {
    bytes: Mmap,
    source: SourceStamp,
    snapshot: bool,
}

impl InputMap {
    /// Reject an observed source replacement or edit before publishing derived output.
    pub(crate) fn verify(&self, file: &File, path: &Path) -> Result<()> {
        self.source.verify(file, path)
    }

    /// Whether source writers are isolated from these anonymous bytes.
    pub(crate) fn is_snapshot(&self) -> bool {
        self.snapshot
    }
}

impl Deref for InputMap {
    type Target = [u8];

    /// Expose only the selected read-only bytes to existing framing consumers.
    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl std::fmt::Debug for InputMap {
    /// Describe view ownership and size without displaying source payload bytes.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InputMap")
            .field("length", &self.bytes.len())
            .field("snapshot", &self.snapshot)
            .finish()
    }
}

/// The opened source identity and metadata, independent of its current pathname.
pub(crate) struct SourceStamp {
    length: u64,
    modified: SystemTime,
    identity: Handle,
}

impl SourceStamp {
    /// Capture from the descriptor that will actually supply the bytes.
    pub(crate) fn capture(file: &File) -> Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            identity: Handle::from_file(file.try_clone()?)?,
        })
    }

    /// Detect changed metadata or a pathname replaced with a different file.
    pub(crate) fn verify(&self, file: &File, path: &Path) -> Result<()> {
        let unchanged = || -> std::io::Result<bool> {
            let metadata = file.metadata()?;
            Ok(metadata.len() == self.length
                && metadata.modified()? == self.modified
                && Handle::from_path(path)? == self.identity)
        };
        ensure!(
            unchanged().wrap_err_with(|| {
                format!(
                    "could not verify input during conversion: {}",
                    path.display()
                )
            })?,
            "input changed during conversion: {}",
            path.display()
        );
        Ok(())
    }
}

/// Copy small external inputs into anonymous pages; map larger inputs directly.
/// Callers verify the returned generation after use. Direct Unix mappings still
/// require the source not to be truncated while read; positioned reads are follow-up work.
pub(crate) fn map_input(file: &File, path: &Path) -> Result<InputMap> {
    let before =
        SourceStamp::capture(file).wrap_err_with(|| format!("inspect input {}", path.display()))?;
    ensure!(before.length != 0, "empty input: {}", path.display());
    let changed = || format!("input changed during conversion: {}", path.display());
    if before.length <= MEMORY_SNAPSHOT_LIMIT {
        let mut source = file.try_clone()?;
        source.seek(SeekFrom::Start(0))?;
        let mut snapshot = MmapMut::map_anon(before.length as usize)?;
        source.read_exact(&mut snapshot).wrap_err_with(changed)?;
        let mut extra = [0];
        ensure!(source.read(&mut extra)? == 0, "{}", changed());
        before.verify(file, path)?;
        return Ok(InputMap {
            bytes: snapshot.make_read_only()?,
            source: before,
            snapshot: true,
        });
    }
    direct_map(file, path, before)
}

/// Inspect an archive for reuse without copying it, even when it is small.
/// A cache miss may subsequently take the normal small anonymous snapshot.
pub(crate) fn map_for_reuse(file: &File, path: &Path) -> Result<InputMap> {
    let before =
        SourceStamp::capture(file).wrap_err_with(|| format!("inspect input {}", path.display()))?;
    ensure!(before.length != 0, "empty input: {}", path.display());
    direct_map(file, path, before)
}

/// Keep the original file backing for bounded inspection and large input use.
fn direct_map(file: &File, path: &Path, before: SourceStamp) -> Result<InputMap> {
    // SAFETY: framing consumers bounds-check every access. On Windows the live
    // mapping refuses truncation; on Unix the source must not shrink during use.
    // Stamps diagnose observed changes but do not make Unix truncation fault-safe.
    let bytes = unsafe { Mmap::map(file) }?;
    before.verify(file, path)?;
    ensure!(
        bytes.len() as u64 == before.length,
        "input changed during conversion: {}",
        path.display()
    );
    Ok(InputMap {
        bytes,
        source: before,
        snapshot: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::io::Write;

    /// Anonymous storage stays readable after a second handle truncates the source.
    #[test]
    fn small_snapshot_survives_source_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("small.bin");
        std::fs::write(&path, b"snapshot bytes").unwrap();
        let source = File::open(&path).unwrap();
        let stamp = SourceStamp::capture(&source).unwrap();
        let snapshot = map_input(&source, &path).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(0)
            .unwrap();
        assert_eq!(&snapshot[..], b"snapshot bytes");
        assert!(
            format!("{:#}", stamp.verify(&source, &path).unwrap_err())
                .contains("input changed during conversion")
        );
    }

    /// A large Windows input keeps the OS truncation protection without a temporary copy.
    #[test]
    #[cfg(windows)]
    fn large_mapping_refuses_source_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.bin");
        let mut writer = File::create(&path).unwrap();
        writer.set_len(16 * 1024 * 1024 + 9).unwrap();
        writer.seek(SeekFrom::End(-4)).unwrap();
        writer.write_all(b"tail").unwrap();
        drop(writer);
        let source = File::open(&path).unwrap();
        let snapshot = map_input(&source, &path).unwrap();
        assert!(!snapshot.is_snapshot());
        let truncated = File::options().write(true).open(&path).unwrap().set_len(0);
        assert!(
            truncated.is_err(),
            "a large source-backed Windows view must refuse truncation"
        );
        assert_eq!(snapshot.len(), 16 * 1024 * 1024 + 9);
        assert_eq!(&snapshot[snapshot.len() - 4..], b"tail");
        assert_eq!(snapshot[0], 0);
        snapshot.verify(&source, &path).unwrap();
    }

    /// A large Unix mapping detects replacement without truncating a live mapped inode.
    #[test]
    #[cfg(unix)]
    fn large_mapping_reports_source_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.bin");
        let file = File::create(&path).unwrap();
        file.set_len(MEMORY_SNAPSHOT_LIMIT + 1).unwrap();
        drop(file);
        let source = File::open(&path).unwrap();
        let mapped = map_input(&source, &path).unwrap();
        assert!(!mapped.is_snapshot());
        std::fs::rename(&path, directory.path().join("old.bin")).unwrap();
        let replacement = File::create(&path).unwrap();
        replacement.set_len(MEMORY_SNAPSHOT_LIMIT + 1).unwrap();
        assert!(mapped.verify(&source, &path).is_err());
        assert_eq!(mapped.len() as u64, MEMORY_SNAPSHOT_LIMIT + 1);
    }

    /// Replacing a pathname is rejected even if its replacement has identical bytes.
    #[test]
    fn snapshot_rejects_replaced_source_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.bin");
        std::fs::write(&path, b"same").unwrap();
        let source = File::open(&path).unwrap();
        std::fs::rename(&path, directory.path().join("previous.bin")).unwrap();
        std::fs::write(&path, b"same").unwrap();
        let error = map_input(&source, &path).unwrap_err();
        assert!(format!("{error:#}").contains("input changed during conversion"));
    }

    /// A missing pathname retains its I/O cause in the verification diagnostic.
    #[test]
    fn verification_reports_missing_path_io_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.bin");
        std::fs::write(&path, b"source").unwrap();
        let source = File::open(&path).unwrap();
        let stamp = SourceStamp::capture(&source).unwrap();
        let missing = directory.path().join("missing.bin");
        let error = stamp.verify(&source, &missing).unwrap_err();
        assert!(format!("{error:#}").contains("missing.bin"));
        assert!(
            error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            }),
            "verification must report the missing-path I/O cause: {error:#}"
        );
    }

    /// An empty input returns a named error rather than requesting an empty OS mapping.
    #[test]
    fn empty_snapshot_is_a_named_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.bin");
        std::fs::write(&path, b"").unwrap();
        let source = File::open(&path).unwrap();
        let error = map_input(&source, &path).unwrap_err();
        assert!(format!("{error:#}").contains("empty.bin"));
    }
}
