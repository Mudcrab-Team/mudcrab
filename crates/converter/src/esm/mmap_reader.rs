use std::{fs::File, path::Path};

use color_eyre::eyre::{Result, ensure};
use memmap2::Mmap;

/// The smallest thing that can be a plugin: one TES4 record header.
const MIN_PLUGIN_LENGTH: u64 = 24;

pub struct EsmReader {
    _file: File,
    mmap: Mmap,
}

impl EsmReader {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        ensure!(
            length >= MIN_PLUGIN_LENGTH,
            "{} is too small to be a plugin: {length} bytes, below the {MIN_PLUGIN_LENGTH} byte TES4 header",
            path.display()
        );
        let mmap = unsafe { Mmap::map(&file)? };
        ensure!(
            &mmap[..4] == b"TES4",
            "{} does not start with a TES4 header",
            path.display()
        );
        Ok(Self { _file: file, mmap })
    }

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
}
