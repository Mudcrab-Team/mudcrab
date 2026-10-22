//! Deterministic BSA archive fixtures.
//!
//! The generator targets the layouts accepted by the converter: legacy `v104`
//! archives with 16-byte folder records and SSE `v105` archives with 24-byte
//! folder records. Payloads are stored verbatim or as a `u32` uncompressed
//! size followed by a zlib stream.
//!
//! Folder and file name hashes are the real TES4 hashes, which the converter
//! verifies. This writer computes them on its own (not shared with the
//! converter), so the two implementations check each other. The payloads are
//! not meant for game clients.

use crate::Entry;
use crate::bytes::{push_u32, push_u64};
use crate::path::split_asset_name;
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure, eyre},
};
use std::collections::{HashMap, HashSet};

const HEADER_SIZE: u32 = 36;
const FILE_RECORD_SIZE: u32 = 16;
const ARCHIVE_INCLUDE_DIRECTORIES: u32 = 0x0001;
const ARCHIVE_INCLUDE_FILE_NAMES: u32 = 0x0002;
const ARCHIVE_COMPRESSED: u32 = 0x0004;
const FILE_SIZE_LIMIT: usize = 0x3fff_ffff;

/// Payload compression used when generating a BSA archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Stores every payload verbatim.
    None,
    /// Stores a `u32` uncompressed size followed by a zlib stream.
    Zlib,
    /// Stores a `u32` uncompressed size followed by an LZ4 block (`v105` only).
    Lz4,
}

/// Generates an SSE (`v105`) BSA archive.
pub fn v105(entries: &[Entry<'_>], compression: Compression) -> Result<Vec<u8>> {
    archive(105, entries, compression)
}

/// Generates a legacy (`v104`) BSA archive.
pub fn v104(entries: &[Entry<'_>], compression: Compression) -> Result<Vec<u8>> {
    archive(104, entries, compression)
}

struct PreparedEntry {
    file: String,
    payload: Vec<u8>,
}

fn archive(version: u32, entries: &[Entry<'_>], compression: Compression) -> Result<Vec<u8>> {
    ensure!(
        matches!(version, 104 | 105),
        "unsupported BSA version {version}"
    );
    ensure!(
        version >= 105 || compression != Compression::Lz4,
        "BSA v104 supports only zlib compression"
    );
    let folder_record_size = if version >= 105 { 24usize } else { 16usize };
    let mut seen = HashSet::with_capacity(entries.len());
    let mut folders: Vec<String> = Vec::new();
    let mut grouped: Vec<Vec<PreparedEntry>> = Vec::new();
    let mut folder_index: HashMap<&str, usize> = HashMap::new();
    for entry in entries {
        let (folder, file) = split_asset_name(entry.name, "BSA")
            .wrap_err_with(|| format!("invalid BSA entry {:?}", entry.name))?;
        ensure!(
            seen.insert(entry.name),
            "duplicate BSA entry {:?}",
            entry.name
        );
        ensure!(
            folder.len() < u8::MAX as usize,
            "BSA folder name exceeds 255 bytes: {folder:?}"
        );
        let index = match folder_index.get(folder) {
            Some(index) => *index,
            None => {
                let index = folders.len();
                folders.push(folder.to_string());
                grouped.push(Vec::new());
                folder_index.insert(folder, index);
                index
            }
        };
        grouped[index].push(PreparedEntry {
            file: file.to_string(),
            payload: encode_payload(entry.data, compression)?,
        });
    }

    let file_count = u32::try_from(entries.len()).map_err(|_| eyre!("BSA file count overflow"))?;
    let folder_count =
        u32::try_from(folders.len()).map_err(|_| eyre!("BSA folder count overflow"))?;
    let total_folder_name_length = folders.iter().try_fold(0usize, |total, folder| {
        total
            .checked_add(folder.len() + 1)
            .ok_or_else(|| eyre!("BSA folder name length overflow"))
    })?;
    let total_file_name_length = grouped.iter().flatten().try_fold(0usize, |total, entry| {
        total
            .checked_add(entry.file.len() + 1)
            .ok_or_else(|| eyre!("BSA file name length overflow"))
    })?;

    let mut cursor = HEADER_SIZE as usize;
    cursor = cursor
        .checked_add(
            folders
                .len()
                .checked_mul(folder_record_size)
                .ok_or_else(|| eyre!("BSA folder table overflow"))?,
        )
        .ok_or_else(|| eyre!("BSA folder table overflow"))?;
    for (index, folder) in folders.iter().enumerate() {
        cursor = cursor
            .checked_add(1 + folder.len() + 1)
            .and_then(|cursor| cursor.checked_add(grouped[index].len() * FILE_RECORD_SIZE as usize))
            .ok_or_else(|| eyre!("BSA metadata overflow"))?;
    }
    let filename_table_start = cursor;
    let payload_start = filename_table_start
        .checked_add(total_file_name_length)
        .ok_or_else(|| eyre!("BSA filename table overflow"))?;

    let mut payload_offsets = Vec::with_capacity(entries.len());
    let mut payload_cursor = payload_start;
    for entry in grouped.iter().flatten() {
        payload_offsets.push(
            u32::try_from(payload_cursor).map_err(|_| eyre!("BSA payload offset exceeds 4 GiB"))?,
        );
        payload_cursor = payload_cursor
            .checked_add(entry.payload.len())
            .ok_or_else(|| eyre!("BSA payload size overflow"))?;
    }
    let total_size = payload_cursor;
    u32::try_from(total_size).map_err(|_| eyre!("BSA archive exceeds 4 GiB"))?;

    let mut archive_flags = ARCHIVE_INCLUDE_DIRECTORIES | ARCHIVE_INCLUDE_FILE_NAMES;
    if compression != Compression::None {
        archive_flags |= ARCHIVE_COMPRESSED;
    }

    let mut bytes = Vec::with_capacity(total_size);
    bytes.extend_from_slice(b"BSA\0");
    push_u32(&mut bytes, version);
    push_u32(&mut bytes, HEADER_SIZE);
    push_u32(&mut bytes, archive_flags);
    push_u32(&mut bytes, folder_count);
    push_u32(&mut bytes, file_count);
    push_u32(
        &mut bytes,
        u32::try_from(total_folder_name_length)
            .map_err(|_| eyre!("BSA folder name table overflow"))?,
    );
    push_u32(
        &mut bytes,
        u32::try_from(total_file_name_length).map_err(|_| eyre!("BSA file name table overflow"))?,
    );
    push_u32(&mut bytes, 0);

    for (index, entries) in grouped.iter().enumerate() {
        push_u64(
            &mut bytes,
            name_hash(&folders[index].replace('/', "\\"), false),
        );
        push_u32(
            &mut bytes,
            u32::try_from(entries.len()).map_err(|_| eyre!("BSA folder entry count overflow"))?,
        );
        bytes.resize(bytes.len() + folder_record_size - 12, 0);
    }

    let mut file_index = 0usize;
    for (index, folder) in folders.iter().enumerate() {
        bytes.push(
            u8::try_from(folder.len() + 1)
                .map_err(|_| eyre!("BSA folder name exceeds 255 bytes"))?,
        );
        bytes.extend_from_slice(folder.as_bytes());
        bytes.push(0);
        for entry in &grouped[index] {
            push_u64(&mut bytes, name_hash(&entry.file, true));
            push_u32(
                &mut bytes,
                u32::try_from(entry.payload.len())
                    .map_err(|_| eyre!("BSA payload exceeds 4 GiB"))?,
            );
            push_u32(&mut bytes, payload_offsets[file_index]);
            file_index += 1;
        }
    }
    debug_assert_eq!(bytes.len(), filename_table_start);

    for entry in grouped.iter().flatten() {
        bytes.extend_from_slice(entry.file.as_bytes());
        bytes.push(0);
    }
    debug_assert_eq!(bytes.len(), payload_start);

    for entry in grouped.iter().flatten() {
        bytes.extend_from_slice(&entry.payload);
    }
    debug_assert_eq!(bytes.len(), total_size);
    Ok(bytes)
}

/// TES4 name hash of a lowercase name: the last, second-to-last, first byte and
/// length in the low word; a CRC of the middle (and extension) in the high word.
fn name_hash(name: &str, split_extension: bool) -> u64 {
    let name = name.to_ascii_lowercase();
    let bytes = name.as_bytes();
    let (root, extension) = match bytes.iter().rposition(|&byte| byte == b'.') {
        Some(dot) if split_extension => bytes.split_at(dot),
        _ => (bytes, &bytes[..0]),
    };
    let crc = |part: &[u8]| {
        part.iter().fold(0u32, |crc, &byte| {
            crc.wrapping_mul(0x1003f).wrapping_add(u32::from(byte))
        })
    };
    let mut low = 0u32;
    let mut middle = 0u32;
    if !root.is_empty() {
        let length = root.len();
        let second_last = if length > 2 { root[length - 2] } else { 0 };
        low = u32::from(root[length - 1])
            | u32::from(second_last) << 8
            | (length as u32 & 0xff) << 16
            | u32::from(root[0]) << 24;
        if length > 3 {
            middle = crc(&root[1..length - 2]);
        }
    }
    low |= match extension {
        b".kf" => 0x80,
        b".nif" => 0x8000,
        b".dds" => 0x8080,
        b".wav" => 0x8000_0000,
        b".adp" => 0x8000_8000,
        _ => 0,
    };
    u64::from(middle.wrapping_add(crc(extension))) << 32 | u64::from(low)
}

fn encode_payload(data: &[u8], compression: Compression) -> Result<Vec<u8>> {
    ensure!(
        data.len() <= FILE_SIZE_LIMIT,
        "BSA payload exceeds the 0x3fffffff byte limit"
    );
    match compression {
        Compression::None => Ok(data.to_vec()),
        Compression::Zlib => {
            let compressed = miniz_oxide::deflate::compress_to_vec_zlib(data, 9);
            ensure!(
                compressed.len() <= FILE_SIZE_LIMIT,
                "compressed BSA payload exceeds the 0x3fffffff byte limit"
            );
            let mut payload = Vec::with_capacity(4 + compressed.len());
            push_u32(
                &mut payload,
                u32::try_from(data.len()).map_err(|_| eyre!("BSA payload length overflow"))?,
            );
            payload.extend_from_slice(&compressed);
            ensure!(
                payload.len() <= FILE_SIZE_LIMIT,
                "compressed BSA payload exceeds the 0x3fffffff byte limit"
            );
            Ok(payload)
        }
        Compression::Lz4 => {
            let compressed = lz4_flex::block::compress(data);
            let mut payload = Vec::with_capacity(4 + compressed.len());
            push_u32(
                &mut payload,
                u32::try_from(data.len()).map_err(|_| eyre!("BSA payload length overflow"))?,
            );
            payload.extend_from_slice(&compressed);
            ensure!(
                payload.len() <= FILE_SIZE_LIMIT,
                "compressed BSA payload exceeds the 0x3fffffff byte limit"
            );
            Ok(payload)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn writes_v105_header_and_expected_size() {
        let entries = [Entry::new("scripts/hello.pex", b"PEX")];
        let bytes = v105(&entries, Compression::None).unwrap();
        assert_eq!(&bytes[..4], b"BSA\0");
        assert_eq!(u32_at(&bytes, 4), 105);
        assert_eq!(u32_at(&bytes, 12), 0x3, "uncompressed archive flags");
        assert_eq!(u32_at(&bytes, 16), 1, "folder count");
        assert_eq!(u32_at(&bytes, 20), 1, "file count");
        assert_eq!(
            bytes.len(),
            HEADER_SIZE as usize
                + 24
                + 1
                + "scripts".len()
                + 1
                + FILE_RECORD_SIZE as usize
                + "hello.pex".len()
                + 1
                + "PEX".len()
        );
    }

    #[test]
    fn writes_v104_with_sixteen_byte_folder_records() {
        let entries = [Entry::new("scripts/hello.pex", b"PEX")];
        let legacy = v104(&entries, Compression::None).unwrap();
        let modern = v105(&entries, Compression::None).unwrap();
        assert_eq!(u32_at(&legacy, 4), 104);
        assert_eq!(modern.len() - legacy.len(), 8);
    }

    #[test]
    fn compresses_payloads_with_uncompressed_size_prefix() {
        let data = b"fixture payload".repeat(4);
        let entries = [Entry::new("scripts/hello.pex", data.as_slice())];
        let bytes = v105(&entries, Compression::Zlib).unwrap();
        assert_eq!(u32_at(&bytes, 12), 0x7, "compressed archive flags");

        let file_record = HEADER_SIZE as usize + 24 + 1 + "scripts".len() + 1;
        let size_flags = u32_at(&bytes, file_record + 8) as usize & FILE_SIZE_LIMIT;
        let offset = u32_at(&bytes, file_record + 12) as usize;
        assert_eq!(u32_at(&bytes, offset) as usize, data.len());

        let decoded =
            miniz_oxide::inflate::decompress_to_vec_zlib(&bytes[offset + 4..offset + size_flags])
                .unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn groups_entries_by_folder_in_first_seen_order() {
        let entries = [
            Entry::new("textures/a.dds", b"A"),
            Entry::new("meshes/b.nif", b"B"),
            Entry::new("textures/c.dds", b"C"),
        ];
        let bytes = v105(&entries, Compression::None).unwrap();
        assert_eq!(u32_at(&bytes, 16), 2, "folder count");
        assert_eq!(u32_at(&bytes, 20), 3, "file count");

        let folder_table_end = HEADER_SIZE as usize + 2 * 24;
        assert_eq!(bytes[folder_table_end], 9, "first folder name length");
        assert_eq!(bytes[folder_table_end + 1], b't');
    }

    #[test]
    fn accepts_empty_archives() {
        let bytes = v105(&[], Compression::None).unwrap();
        assert_eq!(bytes.len(), HEADER_SIZE as usize);
        assert_eq!(u32_at(&bytes, 16), 0);
        assert_eq!(u32_at(&bytes, 20), 0);
    }

    #[test]
    fn rejects_unsafe_and_duplicate_names() {
        assert!(v105(&[Entry::new("../evil.pex", b"X")], Compression::None).is_err());
        assert!(v105(&[Entry::new("root.pex", b"X")], Compression::None).is_err());
        assert!(
            v105(
                &[
                    Entry::new("scripts/a.pex", b"X"),
                    Entry::new("scripts/a.pex", b"Y"),
                ],
                Compression::None
            )
            .is_err()
        );
    }

    #[test]
    fn stores_lz4_blocks_for_version_105() {
        let data = b"lz4 payload".repeat(4);
        let entries = [Entry::new("scripts/a.pex", data.as_slice())];
        let bytes = v105(&entries, Compression::Lz4).unwrap();
        assert_eq!(u32_at(&bytes, 12), 0x7, "compressed archive flags");

        let file_record = HEADER_SIZE as usize + 24 + 1 + "scripts".len() + 1;
        let size_flags = u32_at(&bytes, file_record + 8) as usize & FILE_SIZE_LIMIT;
        let offset = u32_at(&bytes, file_record + 12) as usize;
        assert_eq!(u32_at(&bytes, offset) as usize, data.len());
        assert_eq!(
            lz4_flex::block::decompress(&bytes[offset + 4..offset + size_flags], data.len())
                .unwrap(),
            data
        );
    }

    #[test]
    fn rejects_lz4_for_legacy_archives() {
        assert!(v104(&[Entry::new("scripts/a.pex", b"X")], Compression::Lz4).is_err());
    }

    #[test]
    fn compressed_output_is_deterministic() {
        let entries = [Entry::new("scripts/a.pex", b"payload")];
        assert_eq!(
            v105(&entries, Compression::Zlib).unwrap(),
            v105(&entries, Compression::Zlib).unwrap()
        );
    }
}
