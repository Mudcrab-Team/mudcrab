use std::{fs::File, path::Path};

use color_eyre::eyre::{Result, ensure};
use memmap2::Mmap;

/// The smallest thing that can be a plugin: one TES4 record header.
const MIN_PLUGIN_LENGTH: u64 = 24;

/// A checked plugin snapshot with a stable read-only byte view.
pub struct EsmReader {
    _file: File,
    mmap: Mmap,
}

impl EsmReader {
    /// Open and validate captured bytes without mapping the source inode.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        ensure!(
            length >= MIN_PLUGIN_LENGTH,
            "{} is too small to be a plugin: {length} bytes, below the {MIN_PLUGIN_LENGTH} byte TES4 header",
            path.display()
        );
        // Snapshot external bytes before exposing a stable read-only mapping.
        let mmap = crate::source_input::map_input(&file, path)?;
        // Validate the captured bytes as well as the initial source metadata.
        ensure!(
            mmap.len() as u64 >= MIN_PLUGIN_LENGTH,
            "{} shrank to {} bytes while it was being opened",
            path.display(),
            mmap.len()
        );
        ensure!(
            &mmap[..4] == b"TES4",
            "{} does not start with a TES4 header",
            path.display()
        );
        Ok(Self { _file: file, mmap })
    }

    /// Borrow bytes whose backing storage cannot be changed by source-file edits.
    pub fn as_slice(&self) -> &[u8] {
        &self.mmap[..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_plugin(name: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        (directory, path)
    }

    fn open_error(path: &std::path::Path) -> String {
        match EsmReader::open(path) {
            Ok(_) => panic!("{} was accepted as a plugin", path.display()),
            Err(error) => format!("{error:#}"),
        }
    }

    #[test]
    fn rejects_empty_and_non_plugin_files() {
        let (_empty, path) = temp_plugin("empty.esm", b"");
        assert!(
            open_error(&path).contains("too small"),
            "{}",
            open_error(&path)
        );

        let (_short, path) = temp_plugin("short.esm", &[0; 12]);
        assert!(
            open_error(&path).contains("too small"),
            "{}",
            open_error(&path)
        );

        let (_text, path) = temp_plugin("text.esm", &[b'X'; 64]);
        assert!(open_error(&path).contains("TES4"), "{}", open_error(&path));
    }

    #[test]
    fn accepts_a_minimal_tes4_header() {
        let mut bytes = vec![0; 24];
        bytes[..4].copy_from_slice(b"TES4");
        let (_guard, path) = temp_plugin("plugin.esm", &bytes);

        let reader = EsmReader::open(&path).unwrap();
        assert_eq!(reader.as_slice().len(), 24);
    }

    /// Windows rejects truncation of source-backed maps; snapshot readers release that coupling.
    #[test]
    #[cfg(windows)]
    fn reader_snapshot_survives_external_truncation() {
        let mut bytes = vec![0; 24];
        bytes[..4].copy_from_slice(b"TES4");
        let (_guard, path) = temp_plugin("mutable.esm", &bytes);
        let reader = EsmReader::open(&path).unwrap();
        let truncated = File::options().write(true).open(&path).unwrap().set_len(0);
        assert!(truncated.is_ok(), "source is still mapped: {truncated:?}");
        assert_eq!(reader.as_slice(), bytes);
    }
}
