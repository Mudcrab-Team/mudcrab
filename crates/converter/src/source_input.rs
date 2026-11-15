//! Private snapshots keep external input edits away from every mapped byte.

use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use memmap2::{Mmap, MmapMut};
use same_file::Handle;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::SystemTime,
};

/// Small inputs use anonymous pages; larger inputs spill to private temporary storage.
const MEMORY_SNAPSHOT_LIMIT: u64 = 16 * 1024 * 1024;

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
        let unchanged = || -> Result<bool> {
            let metadata = file.metadata()?;
            Ok(metadata.len() == self.length
                && metadata.modified()? == self.modified
                && Handle::from_path(path)? == self.identity)
        };
        ensure!(
            unchanged().unwrap_or(false),
            "input changed during conversion: {}",
            path.display()
        );
        Ok(())
    }
}

/// Read an externally mutable input into storage that no source writer can modify.
///
/// Metadata checks diagnose observed changes; private storage supplies memory safety
/// even when an edit escapes those checks. The copy is bounded to the initial size
/// plus one byte so a growing file cannot keep the snapshot running indefinitely.
pub(crate) fn map_input(file: &File, path: &Path) -> Result<Mmap> {
    let before =
        SourceStamp::capture(file).wrap_err_with(|| format!("inspect input {}", path.display()))?;
    ensure!(before.length != 0, "empty input: {}", path.display());
    let mut source = file.try_clone()?;
    source.seek(SeekFrom::Start(0))?;
    let changed = || format!("input changed during conversion: {}", path.display());
    if before.length <= MEMORY_SNAPSHOT_LIMIT {
        let mut snapshot = MmapMut::map_anon(before.length as usize)?;
        source.read_exact(&mut snapshot).wrap_err_with(changed)?;
        let mut extra = [0];
        ensure!(source.read(&mut extra)? == 0, "{}", changed());
        before.verify(file, path)?;
        return Ok(snapshot.make_read_only()?);
    }

    let mut snapshot = tempfile::tempfile()
        .wrap_err_with(|| format!("create private snapshot for {}", path.display()))?;
    let copied = std::io::copy(
        &mut source.take(before.length.saturating_add(1)),
        &mut snapshot,
    )
    .wrap_err_with(|| format!("copy input snapshot {}", path.display()))?;
    ensure!(copied == before.length, "{}", changed());
    before.verify(file, path)?;
    // SAFETY: only this function owns the private temporary file. It is fully
    // written before mapping, and its sole writable handle is dropped below.
    // No handle or pathname to this backing storage is exposed to input writers.
    let map = unsafe { Mmap::map(&snapshot) }?;
    drop(snapshot);
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// The spill branch uses private backing storage instead of a source-backed map.
    #[test]
    fn large_snapshot_survives_source_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.bin");
        let mut writer = File::create(&path).unwrap();
        writer.set_len(16 * 1024 * 1024 + 9).unwrap();
        writer.seek(SeekFrom::End(-4)).unwrap();
        writer.write_all(b"tail").unwrap();
        drop(writer);
        let source = File::open(&path).unwrap();
        let snapshot = map_input(&source, &path).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(0)
            .unwrap();
        assert_eq!(snapshot.len(), 16 * 1024 * 1024 + 9);
        assert_eq!(&snapshot[snapshot.len() - 4..], b"tail");
        assert_eq!(snapshot[0], 0);
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
