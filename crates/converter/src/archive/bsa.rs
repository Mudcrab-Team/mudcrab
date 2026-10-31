use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure, eyre},
};
use flate2::read::ZlibDecoder;
use std::io::Read;

use super::bsa_name::{decode_name, file_hash, folder_hash};

const HEADER_SIZE: usize = 36;
const FILE_RECORD_SIZE: usize = 16;
const ARCHIVE_COMPRESSED: u32 = 0x0004;
const ARCHIVE_EMBED_FILE_NAMES: u32 = 0x0100;
const KNOWN_ARCHIVE_FLAGS: u32 = 0x03ff;
const FILE_COMPRESSION_TOGGLE: u32 = 0x4000_0000;
const FILE_SIZE_MASK: u32 = 0x3fff_ffff;

#[derive(Debug)]
struct FileRecord {
    folder: String,
    size_flags: u32,
    offset: u32,
    hash: u64,
}

#[derive(Debug)]
pub struct BsaRawEntry<'a> {
    pub name: String,
    pub payload: &'a [u8],
    pub version: u32,
    pub is_compressed: bool,
}

impl<'a> BsaRawEntry<'a> {
    pub fn decompress(&self) -> Result<Vec<u8>> {
        if self.is_compressed {
            decompress(self.payload, self.version)
                .wrap_err_with(|| format!("failed to decompress BSA entry {:?}", self.name))
        } else {
            Ok(self.payload.to_vec())
        }
    }
}

// Reads BSA metadata table only
// Payloads remain lightweight slices into the mmap
pub(crate) fn iter_raw_entries<'a>(bytes: &'a [u8]) -> Result<Vec<BsaRawEntry<'a>>> {
    ensure!(bytes.len() >= HEADER_SIZE, "truncated BSA header");
    ensure!(&bytes[..4] == b"BSA\0", "invalid BSA magic");
    let version = u32_at(bytes, 4)?;
    ensure!(
        matches!(version, 104 | 105),
        "unsupported BSA version {version}"
    );
    let folder_record_offset = u32_at(bytes, 8)? as usize;
    ensure!(
        folder_record_offset == HEADER_SIZE,
        "unsupported BSA folder record offset {folder_record_offset}"
    );
    let archive_flags = u32_at(bytes, 12)?;
    ensure!(
        archive_flags & !KNOWN_ARCHIVE_FLAGS == 0,
        "unsupported BSA archive flags: {archive_flags:#010x}"
    );
    let folder_count = u32_at(bytes, 16)? as usize;
    let file_count = u32_at(bytes, 20)? as usize;
    let total_folder_name_length = u32_at(bytes, 24)? as usize;
    let total_file_name_length = u32_at(bytes, 28)? as usize;
    // MonoPT's earlier allow-list admitted observed animation, Creation Club and
    // UIExtensions classifier bits. Treat all file-type bits as advisory instead:
    // filenames determine handling, and these bits do not control offsets,
    // compression or allocation. Archive flags and structural checks remain strict.
    let _file_type_flags = u32_at(bytes, 32)?;
    let folder_record_size = if version >= 105 { 24 } else { 16 };
    let folder_table_size = folder_count
        .checked_mul(folder_record_size)
        .ok_or_else(|| eyre!("BSA folder count overflow"))?;
    let folder_table_end = checked_end(folder_record_offset, folder_table_size, "folder table")?;

    // Validate all count-derived metadata before allocating. Besides detecting corrupt
    // headers early, this prevents attacker-controlled capacities from aborting the process.
    let file_records_size = file_count
        .checked_mul(FILE_RECORD_SIZE)
        .ok_or_else(|| eyre!("BSA file count overflow"))?;
    let minimum_metadata_end = checked_end(folder_table_end, folder_count, "folder names")
        .and_then(|end| checked_end(end, total_folder_name_length, "folder names"))
        .and_then(|end| checked_end(end, file_records_size, "file records"))
        .and_then(|end| checked_end(end, total_file_name_length, "filename table"))?;
    ensure!(
        minimum_metadata_end <= bytes.len(),
        "BSA metadata exceeds archive size"
    );

    let mut folder_records = Vec::with_capacity(folder_count);
    for index in 0..folder_count {
        let base = folder_record_offset + index * folder_record_size;
        folder_records.push((u64_at(bytes, base)?, u32_at(bytes, base + 8)? as usize));
    }

    let mut cursor = folder_table_end;
    let mut records = Vec::with_capacity(file_count);
    let mut folder_names_length = 0usize;
    for (stored_folder_hash, count) in folder_records {
        let name_len = slice_at(bytes, cursor, 1, "folder name length")?[0] as usize;
        cursor = checked_end(cursor, 1, "folder name length")?;
        let folder_bytes = slice_at(bytes, cursor, name_len, "folder name")?;
        ensure!(
            folder_bytes.is_empty() || folder_bytes.last() == Some(&0),
            "BSA folder name is not null-terminated"
        );
        let folder_name = folder_bytes.strip_suffix(&[0]).unwrap_or(folder_bytes);
        let folder = decode_name(folder_name);
        ensure!(
            folder_hash(folder_name) == stored_folder_hash,
            "BSA folder hash mismatch for {folder:?}: stored {stored_folder_hash:#018x}, calculated {:#018x}",
            folder_hash(folder_name)
        );
        cursor = checked_end(cursor, name_len, "folder name")?;
        folder_names_length = folder_names_length
            .checked_add(name_len)
            .ok_or_else(|| eyre!("BSA folder name length overflow"))?;
        ensure!(
            records
                .len()
                .checked_add(count)
                .is_some_and(|sum| sum <= file_count),
            "BSA folder records exceed declared file count"
        );
        for _ in 0..count {
            slice_at(bytes, cursor, FILE_RECORD_SIZE, "file record")?;
            records.push(FileRecord {
                folder: folder.clone(),
                size_flags: u32_at(bytes, cursor + 8)?,
                offset: u32_at(bytes, cursor + 12)?,
                hash: u64_at(bytes, cursor)?,
            });
            cursor = checked_end(cursor, FILE_RECORD_SIZE, "file record")?;
        }
    }
    ensure!(
        folder_names_length == total_folder_name_length,
        "BSA folder name length mismatch: header={total_folder_name_length}, records={folder_names_length}"
    );
    ensure!(
        records.len() == file_count,
        "BSA file count mismatch: header={file_count}, records={}",
        records.len()
    );

    let filename_table_end = checked_end(cursor, total_file_name_length, "filename table")?;
    let filename_table = slice_at(bytes, cursor, total_file_name_length, "filename table")?;
    let mut names = Vec::with_capacity(file_count);
    let mut raw_names = Vec::with_capacity(file_count);
    let mut name_cursor = 0usize;
    for _ in 0..file_count {
        let tail = filename_table
            .get(name_cursor..)
            .ok_or_else(|| eyre!("missing BSA filename table"))?;
        let end = tail
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| eyre!("unterminated BSA filename"))?;
        ensure!(end > 0, "BSA filename is empty");
        raw_names.push(&tail[..end]);
        names.push(decode_name(&tail[..end]));
        name_cursor = checked_end(name_cursor, end + 1, "filename")?;
    }
    // Some distributed archives (e.g. MarketplaceTextures.bsa) pad the filename
    // table with trailing zero bytes beyond the declared file names. The header
    // length remains the table bound, so only all-zero slack is accepted here.
    ensure!(
        name_cursor <= filename_table.len(),
        "BSA filename table length mismatch: header={}, records={name_cursor}",
        filename_table.len()
    );
    ensure!(
        filename_table[name_cursor..].iter().all(|byte| *byte == 0),
        "BSA filename table has {} trailing bytes after {} file names",
        filename_table.len() - name_cursor,
        names.len()
    );

    records
        .into_iter()
        .zip(names.into_iter().zip(raw_names))
        .map(|(record, (file_name, raw_name))| {
            let full_name = if record.folder.is_empty() {
                file_name
            } else {
                format!("{}/{}", record.folder, file_name)
            };
            let calculated = file_hash(raw_name);
            ensure!(
                calculated == record.hash,
                "BSA file hash mismatch for {full_name:?}: stored {:#018x}, calculated {calculated:#018x}",
                record.hash
            );
            let stored_size = (record.size_flags & FILE_SIZE_MASK) as usize;
            let start = record.offset as usize;
            ensure!(
                start >= filename_table_end,
                "BSA file payload overlaps metadata: {full_name}"
            );
            let mut payload = slice_at(bytes, start, stored_size, "file payload")
                .wrap_err_with(|| format!("invalid BSA file payload: {full_name}"))?;
            if archive_flags & ARCHIVE_EMBED_FILE_NAMES != 0 {
                let embedded_len = *payload
                    .first()
                    .ok_or_else(|| eyre!("missing embedded BSA filename"))?
                    as usize;
                let payload_start = checked_end(1, embedded_len, "embedded filename")?;
                payload = payload
                    .get(payload_start..)
                    .ok_or_else(|| eyre!("truncated embedded BSA filename"))?;
            }

            let is_compressed = (archive_flags & ARCHIVE_COMPRESSED != 0)
                ^ (record.size_flags & FILE_COMPRESSION_TOGGLE != 0);

            Ok(BsaRawEntry {
                name: full_name,
                payload,
                version,
                is_compressed,
            })
        })
        .collect()
}

/// The largest decompressed size any BSA entry may declare. Retail textures,
/// meshes and sounds stay well under this; a larger declared size is a corrupt
/// or crafted entry, not a payload the game itself ships.
const MAX_ENTRY_SIZE: usize = 1024 * 1024 * 1024;
/// Deflate cannot expand a stream by more than 1032:1 (the loosest ratio of the
/// three codecs a BSA payload may use).
const MAX_ZLIB_EXPANSION: usize = 1032;
/// Headroom for the framing of a tiny stream, whose own block headers cost
/// more than the data it carries.
const DECOMPRESSION_SLACK: usize = 4096;

/// The largest decompressed size a payload of `packed_len` bytes can honestly
/// declare. The absolute `MAX_ENTRY_SIZE` ceiling keeps a large packed payload
/// from licensing a multi-gigabyte reservation through the ratio alone.
fn expansion_limit(packed_len: usize) -> usize {
    packed_len
        .saturating_mul(MAX_ZLIB_EXPANSION)
        .saturating_add(DECOMPRESSION_SLACK)
        .min(MAX_ENTRY_SIZE)
}

fn decompress(payload: &[u8], version: u32) -> Result<Vec<u8>> {
    ensure!(payload.len() >= 4, "compressed BSA payload is truncated");
    let expected = u32_at(payload, 0)? as usize;
    let compressed = &payload[4..];
    // One choke point for every decoder below, so no codec arm can reserve the
    // declared size before its own length check runs.
    let limit = expansion_limit(compressed.len());
    ensure!(
        expected <= limit,
        "compressed BSA entry declares {expected} decompressed bytes from a {} byte payload, over the {limit} byte safety limit",
        compressed.len()
    );
    let decoded = if version >= 105 {
        if compressed.starts_with(&[0x04, 0x22, 0x4d, 0x18]) {
            read_bounded(lz4_flex::frame::FrameDecoder::new(compressed), expected)
                .wrap_err("LZ4 frame decoding failed")?
        } else if expected <= max_lz4_block_output(compressed.len())
            && let Ok(output) = lz4_flex::block::decompress(compressed, expected)
        {
            output
        } else {
            read_bounded(ZlibDecoder::new(compressed), expected)
                .wrap_err("LZ4 block and zlib decoding failed")?
        }
    } else {
        read_bounded(ZlibDecoder::new(compressed), expected)?
    };
    if decoded.len() != expected {
        bail!(
            "decompressed size mismatch: expected {expected}, got {}",
            decoded.len()
        );
    }
    Ok(decoded)
}

/// Decompressed output is reserved up to this size; anything larger grows as
/// it is read, so a corrupt declared size cannot reserve gigabytes up front.
const MAX_RESERVATION: usize = 64 * 1024 * 1024;

/// Reads at most one byte past `expected`, so a stream longer than declared
/// shows up as a size mismatch instead of being read to its end.
fn read_bounded(reader: impl Read, expected: usize) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(expected.min(MAX_RESERVATION));
    reader
        .take((expected as u64).saturating_add(1))
        .read_to_end(&mut output)?;
    Ok(output)
}

/// An LZ4 block writes at most 255 bytes per input byte (a match costs at
/// least three bytes and each further length byte adds 255), so a declared
/// size above this cannot come from `compressed_len` bytes and is not
/// allocated: `lz4_flex::block::decompress` zero-fills the whole declared size.
fn max_lz4_block_output(compressed_len: usize) -> usize {
    compressed_len.saturating_mul(255)
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let raw: [u8; 4] = slice_at(bytes, offset, 4, "u32")?
        .try_into()
        .map_err(|_| eyre!("invalid u32 at offset {offset}"))?;
    Ok(u32::from_le_bytes(raw))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    let raw: [u8; 8] = slice_at(bytes, offset, 8, "u64")?
        .try_into()
        .map_err(|_| eyre!("invalid u64 at offset {offset}"))?;
    Ok(u64::from_le_bytes(raw))
}

fn checked_end(start: usize, length: usize, field: &str) -> Result<usize> {
    start
        .checked_add(length)
        .ok_or_else(|| eyre!("BSA {field} range overflow"))
}

fn slice_at<'a>(bytes: &'a [u8], start: usize, length: usize, field: &str) -> Result<&'a [u8]> {
    let end = checked_end(start, length, field)?;
    bytes
        .get(start..end)
        .ok_or_else(|| eyre!("truncated BSA {field} at offset {start}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_strategies::{arbitrary_bytes, config, corrupted};
    use proptest::prelude::*;
    use std::io::Write;

    fn uncompressed_fixture() -> Vec<u8> {
        dummy_content::bsa::v104(
            &[dummy_content::Entry::new("scripts/hello.pex", b"PEX")],
            dummy_content::bsa::Compression::None,
        )
        .unwrap()
    }

    /// A one-file v104 archive with raw name bytes and the given stored hashes,
    /// so names outside ASCII and wrong hashes can be written.
    fn raw_archive(folder: &[u8], file: &[u8], folder_hash: u64, file_hash: u64) -> Vec<u8> {
        let folder_len = folder.len() + 1;
        let names_start = HEADER_SIZE + 16 + 1 + folder_len + FILE_RECORD_SIZE;
        let payload_start = names_start + file.len() + 1;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BSA\0");
        for value in [104u32, HEADER_SIZE as u32, 0x3, 1, 1, folder_len as u32] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&(file.len() as u32 + 1).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&folder_hash.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.push(folder_len as u8);
        bytes.extend_from_slice(folder);
        bytes.push(0);
        bytes.extend_from_slice(&file_hash.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&(payload_start as u32).to_le_bytes());
        bytes.extend_from_slice(file);
        bytes.push(0);
        bytes.extend_from_slice(b"PEX");
        bytes
    }

    #[test]
    fn decodes_accented_windows_1252_names() {
        // "textures, backslash, caf, e-acute" and "cr\xe8me br\xfbl\xe9e.dds" as Windows-1252 bytes.
        let folder = b"textures\x5ccaf\xe9";
        let file = b"cr\xe8me br\xfbl\xe9e.dds";
        let bytes = raw_archive(
            folder,
            file,
            super::super::bsa_name::folder_hash(folder),
            super::super::bsa_name::file_hash(file),
        );
        let entries = iter_raw_entries(&bytes).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].name,
            "textures\u{5c}caf\u{e9}/cr\u{e8}me br\u{fb}l\u{e9}e.dds"
        );
        assert!(!entries[0].name.contains('\u{fffd}'));
    }

    #[test]
    fn rejects_a_wrong_file_hash_naming_the_entry() {
        let (folder, file) = (b"scripts".as_slice(), b"hello.pex".as_slice());
        let good_folder = super::super::bsa_name::folder_hash(folder);
        let good_file = super::super::bsa_name::file_hash(file);
        assert!(iter_raw_entries(&raw_archive(folder, file, good_folder, good_file)).is_ok());

        let error = iter_raw_entries(&raw_archive(folder, file, good_folder, good_file ^ 1))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("file hash mismatch"),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("scripts/hello.pex"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_a_wrong_folder_hash_naming_the_folder() {
        let (folder, file) = (b"scripts".as_slice(), b"hello.pex".as_slice());
        let good_file = super::super::bsa_name::file_hash(file);
        let bad_folder = super::super::bsa_name::folder_hash(folder) ^ (1 << 40);
        let error = iter_raw_entries(&raw_archive(folder, file, bad_folder, good_file))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("folder hash mismatch"),
            "unexpected error: {error}"
        );
        assert!(error.contains("scripts"), "unexpected error: {error}");
    }

    #[test]
    fn fixture_writer_hashes_agree_with_the_reader() {
        for compression in [
            dummy_content::bsa::Compression::None,
            dummy_content::bsa::Compression::Zlib,
        ] {
            let bytes = dummy_content::bsa::v104(
                &[
                    dummy_content::Entry::new("meshes/Actors/Dragon/Dragon.NIF", b"a"),
                    dummy_content::Entry::new("textures/x/y.dds", b"b"),
                    dummy_content::Entry::new("sound/fx/hit.wav", b"c"),
                    dummy_content::Entry::new("a/ab.kf", b"d"),
                    dummy_content::Entry::new("sound/fx/voice.adp", b"e"),
                ],
                compression,
            )
            .unwrap();
            assert_eq!(iter_raw_entries(&bytes).unwrap().len(), 5);
        }
    }

    #[test]
    fn extracts_uncompressed_skyrim_bsa_fixture() {
        let bytes = uncompressed_fixture();
        let entries = iter_raw_entries(&bytes).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "scripts/hello.pex");
        assert_eq!(entries[0].payload, b"PEX");
        assert_eq!(entries[0].decompress().unwrap(), b"PEX");
    }

    #[test]
    fn accepts_distributed_skyrim_animation_file_flags() {
        let mut bytes = uncompressed_fixture();
        bytes[32..36].copy_from_slice(&0x0052_01ffu32.to_le_bytes());

        let entries = iter_raw_entries(&bytes).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "scripts/hello.pex");
    }

    #[test]
    fn accepts_distributed_creation_club_file_flags() {
        for flags in [0x0100_0000u32, 0x0200_0000u32, 0x8611_0000u32] {
            let mut bytes = uncompressed_fixture();
            bytes[32..36].copy_from_slice(&flags.to_le_bytes());

            let entries = iter_raw_entries(&bytes).unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "scripts/hello.pex");
        }
    }

    /// Unobserved classifier bits cannot alter payload handling or admit corrupt metadata.
    #[test]
    fn unknown_file_type_flags_are_advisory_but_counts_stay_strict() {
        for flags in [0x8000_0000u32, u32::MAX] {
            let mut bytes = uncompressed_fixture();
            bytes[32..36].copy_from_slice(&flags.to_le_bytes());
            let entries = iter_raw_entries(&bytes).unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "scripts/hello.pex");
            assert_eq!(entries[0].decompress().unwrap(), b"PEX");

            bytes[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(iter_raw_entries(&bytes).is_err());
        }
    }

    #[test]
    fn accepts_zero_padded_filename_table_and_rejects_trailing_garbage() {
        let name = b"hello.pex\0";
        let mut padded = uncompressed_fixture();
        let names_start =
            HEADER_SIZE + FILE_RECORD_SIZE + 1 + b"scripts\0".len() + FILE_RECORD_SIZE;
        padded.splice(names_start + name.len()..names_start + name.len(), [0u8; 9]);
        let padded_len = (name.len() + 9) as u32;
        padded[28..32].copy_from_slice(&padded_len.to_le_bytes());
        // Payload offsets shift by the inserted padding.
        let payload_offset =
            u32::from_le_bytes(padded[names_start - 4..names_start].try_into().unwrap()) + 9;
        padded[names_start - 4..names_start].copy_from_slice(&payload_offset.to_le_bytes());

        let entries = iter_raw_entries(&padded).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "scripts/hello.pex");

        padded[names_start + name.len()] = b'x';
        assert!(
            iter_raw_entries(&padded)
                .unwrap_err()
                .to_string()
                .contains("trailing bytes")
        );
    }

    #[test]
    fn every_truncated_fixture_returns_an_error_without_panicking() {
        let bytes = uncompressed_fixture();
        for length in 0..bytes.len() {
            let result = std::panic::catch_unwind(|| iter_raw_entries(&bytes[..length]));
            assert!(result.is_ok(), "parser panicked for length {length}");
            assert!(
                result.unwrap().is_err(),
                "truncated fixture was accepted at length {length}"
            );
        }
    }

    /// UIExtensions classifier bits are advisory while archive header controls stay strict.
    #[test]
    fn rejects_unsupported_versions_offsets_and_archive_flags() {
        let mut ui_extensions = uncompressed_fixture();
        ui_extensions[32..36].copy_from_slice(&0x0067_0104u32.to_le_bytes());
        assert!(iter_raw_entries(&ui_extensions).is_ok());
        for (range, value, expected) in [
            (4..8, 103u32, "unsupported BSA version"),
            (8..12, 40u32, "unsupported BSA folder record offset"),
            (12..16, 0x8000_0000u32, "unsupported BSA archive flags"),
        ] {
            let mut bytes = uncompressed_fixture();
            bytes[range].copy_from_slice(&value.to_le_bytes());
            let error = iter_raw_entries(&bytes).unwrap_err().to_string();
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    #[ignore = "requires MUDCRAB_SKYRIM_DATA with locally installed game assets"]
    fn accepts_installed_skyrim_bsa_headers() {
        let data_dir = std::env::var_os("MUDCRAB_SKYRIM_DATA")
            .or_else(|| std::env::var_os("OPENSKYRIM_SKYRIM_DATA"))
            .map(std::path::PathBuf::from)
            .expect("set MUDCRAB_SKYRIM_DATA to the Skyrim Data directory");
        let mut archives: Vec<_> = std::fs::read_dir(&data_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("bsa"))
            })
            .collect();
        archives.sort();
        assert!(
            !archives.is_empty(),
            "no BSA archives found in {}",
            data_dir.display()
        );

        for archive in archives {
            let file = std::fs::File::open(&archive).unwrap();
            // SAFETY: the mapping is read-only and the file stays open while parsed.
            let bytes = unsafe { memmap2::Mmap::map(&file) }.unwrap();
            iter_raw_entries(&bytes)
                .unwrap_or_else(|error| panic!("failed to parse {}: {error:#}", archive.display()));
        }
    }

    #[test]
    fn generated_archives_never_panic_under_truncation_or_mutation() {
        let entries = [
            dummy_content::Entry::new("scripts/one.pex", b"PEX"),
            dummy_content::Entry::new("textures/two.dds", b"DDS DATA"),
        ];
        let fixtures = [
            dummy_content::bsa::v104(&entries, dummy_content::bsa::Compression::None).unwrap(),
            dummy_content::bsa::v104(&entries, dummy_content::bsa::Compression::Zlib).unwrap(),
            dummy_content::bsa::v105(&entries, dummy_content::bsa::Compression::None).unwrap(),
            dummy_content::bsa::v105(&entries, dummy_content::bsa::Compression::Zlib).unwrap(),
            dummy_content::bsa::v105(&entries, dummy_content::bsa::Compression::Lz4).unwrap(),
        ];
        let mut rng = dummy_content::rng::Rng::new(42);
        for fixture in fixtures {
            for length in 0..fixture.len() {
                let result = std::panic::catch_unwind(|| iter_raw_entries(&fixture[..length]));
                assert!(result.is_ok(), "BSA parser panicked at length {length}");
            }
            for _ in 0..256 {
                let mut mutated = fixture.clone();
                let index = rng.next_u64() as usize % mutated.len();
                mutated[index] ^= 0xff;
                let result = std::panic::catch_unwind(|| iter_raw_entries(&mutated));
                assert!(result.is_ok(), "BSA parser panicked on mutation at {index}");
            }
        }
    }

    #[test]
    fn rejects_count_and_name_length_mismatches() {
        let mut excessive_folder_count = uncompressed_fixture();
        excessive_folder_count[44..48].copy_from_slice(&2u32.to_le_bytes());
        assert!(
            iter_raw_entries(&excessive_folder_count)
                .unwrap_err()
                .to_string()
                .contains("folder records exceed declared file count")
        );

        let mut wrong_folder_name_length = uncompressed_fixture();
        wrong_folder_name_length[24..28].copy_from_slice(&7u32.to_le_bytes());
        assert!(iter_raw_entries(&wrong_folder_name_length).is_err());

        let mut unterminated_filename = uncompressed_fixture();
        let filename_terminator = unterminated_filename.len() - b"PEX".len() - 1;
        unterminated_filename[filename_terminator] = b'x';
        assert!(
            iter_raw_entries(&unterminated_filename)
                .unwrap_err()
                .to_string()
                .contains("unterminated BSA filename")
        );
    }

    #[test]
    fn rejects_huge_counts_and_overlapping_payloads_without_panicking() {
        let mut huge_count = uncompressed_fixture();
        huge_count[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        let result = std::panic::catch_unwind(|| iter_raw_entries(&huge_count));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());

        let mut overlapping_payload = uncompressed_fixture();
        overlapping_payload[73..77].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        assert!(
            iter_raw_entries(&overlapping_payload)
                .unwrap_err()
                .to_string()
                .contains("payload overlaps metadata")
        );

        assert!(u32_at(&[], usize::MAX).is_err());
    }

    #[test]
    fn rejects_truncated_and_size_mismatched_compressed_payloads() {
        assert!(decompress(&[0, 0, 0], 104).is_err());

        let mut payload = 10u32.to_le_bytes().to_vec();
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(b"short").unwrap();
        payload.extend_from_slice(&encoder.finish().unwrap());
        assert!(
            decompress(&payload, 104)
                .unwrap_err()
                .to_string()
                .contains("decompressed size mismatch")
        );
    }

    #[test]
    fn decompresses_special_edition_lz4_frame() {
        let source = b"Gamebryo File Format, Version 20.2.0.7\n";
        let mut compressed = Vec::new();
        {
            let mut encoder = lz4_flex::frame::FrameEncoder::new(&mut compressed);
            encoder.write_all(source).unwrap();
            encoder.finish().unwrap();
        }
        assert_eq!(&compressed[..4], &[0x04, 0x22, 0x4d, 0x18]);

        let mut payload = Vec::with_capacity(4 + compressed.len());
        payload.extend_from_slice(&(source.len() as u32).to_le_bytes());
        payload.extend_from_slice(&compressed);

        assert_eq!(decompress(&payload, 105).unwrap(), source);
    }

    fn fixtures() -> Vec<Vec<u8>> {
        let entries = [
            dummy_content::Entry::new("scripts/one.pex", b"PEX"),
            dummy_content::Entry::new("textures/two.dds", b"DDS DATA"),
        ];
        vec![
            dummy_content::bsa::v104(&entries, dummy_content::bsa::Compression::Zlib).unwrap(),
            dummy_content::bsa::v105(&entries, dummy_content::bsa::Compression::None).unwrap(),
            dummy_content::bsa::v105(&entries, dummy_content::bsa::Compression::Lz4).unwrap(),
        ]
    }

    fn read_all(bytes: &[u8]) {
        if let Ok(entries) = iter_raw_entries(bytes) {
            for entry in entries {
                let _ = entry.decompress();
            }
        }
    }

    fn with_declared_size(declared: u32, compressed: &[u8]) -> Vec<u8> {
        let mut payload = declared.to_le_bytes().to_vec();
        payload.extend_from_slice(compressed);
        payload
    }

    #[test]
    fn every_codec_rejects_a_corrupt_declared_size() {
        let source = b"Gamebryo File Format, Version 20.2.0.7\n".repeat(64);
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        zlib.write_all(&source).unwrap();
        let zlib = zlib.finish().unwrap();
        let mut frame = Vec::new();
        {
            let mut encoder = lz4_flex::frame::FrameEncoder::new(&mut frame);
            encoder.write_all(&source).unwrap();
            encoder.finish().unwrap();
        }
        let block = lz4_flex::block::compress(&source);
        let cases = [(104, &zlib), (105, &zlib), (105, &frame), (105, &block)];

        for (version, compressed) in cases {
            let valid = with_declared_size(source.len() as u32, compressed);
            assert_eq!(decompress(&valid, version).unwrap(), source);
            for declared in [u32::MAX, source.len() as u32 - 1] {
                let corrupt = with_declared_size(declared, compressed);
                assert!(
                    decompress(&corrupt, version).is_err(),
                    "version {version} accepted declared size {declared}"
                );
            }
        }
    }

    #[test]
    fn a_declared_size_over_the_absolute_cap_errors_instead_of_allocating() {
        // Sixteen payload bytes (four of declared size plus twelve of stream) that
        // declare a gigabyte: the 255x ratio once admitted this to the LZ4 block
        // decoder, which zero-fills the whole declared size before checking it.
        let payload = with_declared_size(MAX_ENTRY_SIZE as u32, &[0u8; 12]);
        for version in [104, 105] {
            let error = decompress(&payload, version)
                .expect_err("a gigabyte from sixteen bytes is rejected")
                .to_string();
            assert!(error.contains("safety limit"), "unexpected error: {error}");
            assert!(
                error.contains(&MAX_ENTRY_SIZE.to_string()),
                "unexpected error: {error}"
            );
        }

        let entry = BsaRawEntry {
            name: "textures/landscape/foo.dds".to_owned(),
            payload: &payload,
            version: 105,
            is_compressed: true,
        };
        let error = entry.decompress().unwrap_err().to_string();
        assert!(
            error.contains("textures/landscape/foo.dds"),
            "the entry name is missing from: {error}"
        );
    }

    #[test]
    fn the_ratio_bound_never_exceeds_the_absolute_entry_cap() {
        assert_eq!(expansion_limit(usize::MAX), MAX_ENTRY_SIZE);
        assert_eq!(
            expansion_limit(1024),
            1024 * MAX_ZLIB_EXPANSION + DECOMPRESSION_SLACK
        );
        assert!(expansion_limit(0) >= DECOMPRESSION_SLACK);
    }

    #[test]
    fn lz4_blocks_at_the_highest_ratio_still_decompress() {
        let source = vec![0; 1024 * 1024];
        let block = lz4_flex::block::compress(&source);
        assert!(source.len() <= max_lz4_block_output(block.len()));
        let payload = with_declared_size(source.len() as u32, &block);
        assert_eq!(decompress(&payload, 105).unwrap(), source);
    }

    proptest! {
        #![proptest_config(config(256))]

        #[test]
        fn headers_never_panic_on_arbitrary_bytes(
            version in prop_oneof![Just(104u32), Just(105u32), any::<u32>()],
            tail in arbitrary_bytes(512),
        ) {
            let mut bytes = b"BSA\0".to_vec();
            bytes.extend_from_slice(&version.to_le_bytes());
            bytes.extend_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
            bytes.extend_from_slice(&tail);
            read_all(&bytes);
        }

        #[test]
        fn corrupted_archives_never_panic(
            bytes in prop::sample::select(fixtures()).prop_flat_map(corrupted),
        ) {
            read_all(&bytes);
        }

        #[test]
        fn corrupted_payloads_never_panic(
            version in prop_oneof![Just(104u32), Just(105u32)],
            expected in prop_oneof![Just(0u32), 0u32..4096],
            compressed in arbitrary_bytes(256),
        ) {
            let mut payload = expected.to_le_bytes().to_vec();
            payload.extend_from_slice(&compressed);
            let _ = decompress(&payload, version);
        }
    }
}
