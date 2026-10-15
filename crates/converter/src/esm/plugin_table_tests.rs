//! Plugin metadata controls for the shared insertion and streamed checksum refactor.

use super::{EsmParser, mmap_reader::EsmReader};
use crate::config::RecordReader;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

type PluginRow = (i64, String, i64, Vec<u8>, String);

/// Frame native subrecord bytes, using XXXX for payloads larger than u16.
fn native_subrecord(signature: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Ok(length) = u16::try_from(payload.len()) {
        bytes.extend_from_slice(signature);
        bytes.extend_from_slice(&length.to_le_bytes());
    } else {
        bytes.extend_from_slice(b"XXXX");
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(signature);
        bytes.extend_from_slice(&0u16.to_le_bytes());
    }
    bytes.extend_from_slice(payload);
    bytes
}

/// Build a TES4-only checksum fixture with distinct flags, author and optional ONAM bytes.
fn native_plugin(flags: u32, author: &[u8], override_ids: &[u8]) -> Vec<u8> {
    let mut hedr = Vec::new();
    hedr.extend_from_slice(&1.7f32.to_le_bytes());
    hedr.extend_from_slice(&0u32.to_le_bytes());
    hedr.extend_from_slice(&0x800u32.to_le_bytes());
    let mut payload = native_subrecord(b"HEDR", &hedr);
    payload.extend(native_subrecord(b"CNAM", author));
    if !override_ids.is_empty() {
        payload.extend(native_subrecord(b"ONAM", override_ids));
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"TES4");
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&44u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Read every plugin metadata column and its checksum SQL type in insertion-index order.
fn plugin_rows(database: &Path) -> Vec<PluginRow> {
    let conn = Connection::open(database).unwrap();
    let mut statement = conn
        .prepare("SELECT id,name,priority,checksum,typeof(checksum) FROM plugins ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

/// Derive an expected row from the known filename, input index and original fixture bytes.
fn expected_row(priority: i64, name: &str, bytes: &[u8]) -> PluginRow {
    (
        priority,
        name.to_owned(),
        priority,
        Sha256::digest(bytes).to_vec(),
        "blob".to_owned(),
    )
}

/// Check both real frontends preserve filenames, input indices and raw SHA-256 BLOBs.
#[test]
fn plugin_tables_match_frontends_and_preserve_raw_checksum_blobs() {
    let root = tempfile::tempdir().unwrap();
    let master = root.path().join("CaseSensitiveMaster.esm");
    let light = root.path().join("DifferentLight.esl");
    let master_bytes = native_plugin(1, b"asymmetric full author\0", &[]);
    let light_bytes = native_plugin(0x200, b"light\0", &[]);
    fs::write(&master, &master_bytes).unwrap();
    fs::write(&light, &light_bytes).unwrap();
    let paths = vec![master.clone(), light.clone()];
    let expected = vec![
        expected_row(0, "CaseSensitiveMaster.esm", &master_bytes),
        expected_row(1, "DifferentLight.esl", &light_bytes),
    ];
    for (reader, name) in [
        (RecordReader::Legacy, "legacy.db"),
        (RecordReader::Inhouse, "inhouse.db"),
    ] {
        let database = root.path().join(name);
        let records =
            EsmParser::convert_plugins_with_reader(&paths, &database, reader, root.path()).unwrap();
        assert!(records.is_empty());
        assert_eq!(plugin_rows(&database), expected, "reader {reader:?}");
    }
    assert_eq!(fs::read(&master).unwrap(), master_bytes);
    assert_eq!(fs::read(&light).unwrap(), light_bytes);
}

/// Check repeated publication replaces metadata indices while retaining source-associated hashes.
#[test]
fn plugin_table_priorities_and_checksums_follow_reordered_inputs() {
    let root = tempfile::tempdir().unwrap();
    let alpha = root.path().join("AlphaIndependent.esm");
    let beta = root.path().join("BetaIndependent.esp");
    let alpha_bytes = native_plugin(1, b"unequal first source\0", &[]);
    let beta_bytes = native_plugin(0, b"second\0", &[]);
    fs::write(&alpha, &alpha_bytes).unwrap();
    fs::write(&beta, &beta_bytes).unwrap();
    for (reader, name) in [
        (RecordReader::Legacy, "reorder-legacy.db"),
        (RecordReader::Inhouse, "reorder-inhouse.db"),
    ] {
        let database = root.path().join(name);
        EsmParser::convert_plugins_with_reader(
            &[alpha.clone(), beta.clone()],
            &database,
            reader,
            root.path(),
        )
        .unwrap();
        assert_eq!(
            plugin_rows(&database),
            vec![
                expected_row(0, "AlphaIndependent.esm", &alpha_bytes),
                expected_row(1, "BetaIndependent.esp", &beta_bytes),
            ],
            "initial reader {reader:?}"
        );
        EsmParser::convert_plugins_with_reader(
            &[beta.clone(), alpha.clone()],
            &database,
            reader,
            root.path(),
        )
        .unwrap();
        assert_eq!(
            plugin_rows(&database),
            vec![
                expected_row(0, "BetaIndependent.esp", &beta_bytes),
                expected_row(1, "AlphaIndependent.esm", &alpha_bytes),
            ],
            "reordered reader {reader:?}"
        );
    }
    assert_eq!(fs::read(&alpha).unwrap(), alpha_bytes);
    assert_eq!(fs::read(&beta).unwrap(), beta_bytes);
}

/// Compare streamed and mapped hashes across multiple chunks and a nonaligned final tail.
#[test]
fn streamed_plugin_hash_matches_mmap_and_original_source_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("MultiChunk.esm");
    let override_ids = (0..49_157u32)
        .flat_map(|index| (0x800 + index).to_le_bytes())
        .collect::<Vec<_>>();
    let bytes = native_plugin(1, b"multiple checksum chunks\0", &override_ids);
    assert!(bytes.len() > 3 * 64 * 1024);
    assert_ne!(bytes.len() % (64 * 1024), 0);
    fs::write(&path, &bytes).unwrap();
    let mapped = EsmReader::open(&path).unwrap();
    let expected = Sha256::digest(&bytes);
    let mapped_digest = Sha256::digest(mapped.as_slice());
    assert_eq!(mapped_digest, expected);
    let streamed_digest = super::plugin_checksum(&mut fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(streamed_digest.as_slice(), expected.as_slice());
    assert_eq!(
        crate::cache::hash_file(&path).unwrap(),
        format!("{expected:x}")
    );
    assert_eq!(mapped.as_slice(), bytes);
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

/// Check bounded checksum requests consume short reads and retry an interrupted read.
#[test]
fn plugin_checksum_uses_bounded_reads_and_retries_interruptions() {
    struct ShortInterruptedReader<'a> {
        bytes: &'a [u8],
        position: usize,
        calls: usize,
        max_requested: usize,
    }
    impl std::io::Read for ShortInterruptedReader<'_> {
        /// Interrupt once, then supply at most 4093 bytes while recording request sizes.
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.max_requested = self.max_requested.max(buffer.len());
            if self.calls == 1 {
                return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
            }
            let count = buffer
                .len()
                .min(4_093)
                .min(self.bytes.len() - self.position);
            buffer[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
            self.position += count;
            Ok(count)
        }
    }
    let bytes = (0..(3 * 64 * 1024 + 29))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let mut reader = ShortInterruptedReader {
        bytes: &bytes,
        position: 0,
        calls: 0,
        max_requested: 0,
    };
    let checksum = super::plugin_checksum(&mut reader).unwrap();
    assert_eq!(checksum.as_slice(), Sha256::digest(&bytes).as_slice());
    assert_eq!(reader.position, bytes.len());
    assert!(reader.calls > 3);
    assert!(reader.max_requested <= 64 * 1024);
}

/// Require a late read failure to propagate rather than authenticate the readable prefix.
#[test]
fn plugin_checksum_preserves_read_errors_instead_of_returning_a_partial_digest() {
    struct FailsAfterPrefix(bool);
    impl std::io::Read for FailsAfterPrefix {
        /// Supply one short prefix, then report a persistent synthetic I/O failure.
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                return Err(std::io::Error::other("synthetic checksum read failure"));
            }
            self.0 = true;
            buffer[..3].copy_from_slice(b"abc");
            Ok(3)
        }
    }
    let error = super::plugin_checksum(&mut FailsAfterPrefix(false)).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert_eq!(error.to_string(), "synthetic checksum read failure");
}

/// Compare raw streaming checksums with the standard SHA-256 empty and abc vectors.
#[test]
fn plugin_checksum_matches_standard_empty_and_short_vectors() {
    for (bytes, expected) in [
        (
            &b""[..],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            &b"abc"[..],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
    ] {
        let checksum = super::plugin_checksum(&mut std::io::Cursor::new(bytes)).unwrap();
        let hex = checksum
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(hex, expected);
    }
}
