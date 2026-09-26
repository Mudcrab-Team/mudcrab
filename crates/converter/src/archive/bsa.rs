use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure, eyre},
};
use flate2::read::ZlibDecoder;
use std::io::Read;

const HEADER_SIZE: usize = 36;
const FILE_RECORD_SIZE: usize = 16;
const ARCHIVE_COMPRESSED: u32 = 0x0004;
const ARCHIVE_EMBED_FILE_NAMES: u32 = 0x0100;
const KNOWN_ARCHIVE_FLAGS: u32 = 0x03ff;
const STANDARD_FILE_FLAGS: u32 = 0x01ff;
// Some distributed Skyrim SE animation archives set these otherwise undocumented
// classifier bits together with all standard file-type bits. File flags are
// advisory (entry names determine how files are handled), so accepting this exact
// observed extension does not change offsets, compression, or allocation behavior.
const SKYRIM_ANIMATION_FILE_FLAGS: u32 = 0x0052_0000;
// Some distributed Creation Club archives set additional classifier bits
// (observed: 0x01000000, 0x02000000, 0x86110000 across the 93 shipped BSAs).
// Like the animation flags above, these are advisory only.
const CREATION_CLUB_FILE_FLAGS: u32 = 0x8711_0000;
const KNOWN_FILE_FLAGS: u32 =
    STANDARD_FILE_FLAGS | SKYRIM_ANIMATION_FILE_FLAGS | CREATION_CLUB_FILE_FLAGS;
const FILE_COMPRESSION_TOGGLE: u32 = 0x4000_0000;
const FILE_SIZE_MASK: u32 = 0x3fff_ffff;

#[derive(Debug)]
struct FileRecord {
    folder: String,
    size_flags: u32,
    offset: u32,
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
    let file_flags = u32_at(bytes, 32)?;
    ensure!(
        file_flags & !KNOWN_FILE_FLAGS == 0,
        "unsupported BSA file flags: {file_flags:#010x}"
    );
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

    let mut folder_file_counts = Vec::with_capacity(folder_count);
    for index in 0..folder_count {
        let base = folder_record_offset + index * folder_record_size;
        folder_file_counts.push(u32_at(bytes, base + 8)? as usize);
    }

    let mut cursor = folder_table_end;
    let mut records = Vec::with_capacity(file_count);
    let mut folder_names_length = 0usize;
    for count in folder_file_counts {
        let name_len = slice_at(bytes, cursor, 1, "folder name length")?[0] as usize;
        cursor = checked_end(cursor, 1, "folder name length")?;
        let folder_bytes = slice_at(bytes, cursor, name_len, "folder name")?;
        ensure!(
            folder_bytes.is_empty() || folder_bytes.last() == Some(&0),
            "BSA folder name is not null-terminated"
        );
        let folder = String::from_utf8_lossy(folder_bytes)
            .trim_end_matches('\0')
            .to_string();
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
        names.push(String::from_utf8_lossy(&tail[..end]).to_string());
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
        .zip(names)
        .map(|(record, file_name)| {
            let full_name = if record.folder.is_empty() {
                file_name
            } else {
                format!("{}/{}", record.folder, file_name)
            };
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

fn decompress(payload: &[u8], version: u32) -> Result<Vec<u8>> {
    ensure!(payload.len() >= 4, "compressed BSA payload is truncated");
    let expected = u32_at(payload, 0)? as usize;
    let compressed = &payload[4..];
    let decoded = if version >= 105 {
        if compressed.starts_with(&[0x04, 0x22, 0x4d, 0x18]) {
            let mut output = Vec::with_capacity(expected);
            lz4_flex::frame::FrameDecoder::new(compressed)
                .read_to_end(&mut output)
                .wrap_err("LZ4 frame decoding failed")?;
            output
        } else if let Ok(output) = lz4_flex::block::decompress(compressed, expected) {
            output
        } else {
            let mut output = Vec::with_capacity(expected);
            ZlibDecoder::new(compressed)
                .read_to_end(&mut output)
                .wrap_err("LZ4 block and zlib decoding failed")?;
            output
        }
    } else {
        let mut output = Vec::with_capacity(expected);
        ZlibDecoder::new(compressed).read_to_end(&mut output)?;
        output
    };
    if decoded.len() != expected {
        bail!(
            "decompressed size mismatch: expected {expected}, got {}",
            decoded.len()
        );
    }
    Ok(decoded)
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let raw: [u8; 4] = slice_at(bytes, offset, 4, "u32")?
        .try_into()
        .map_err(|_| eyre!("invalid u32 at offset {offset}"))?;
    Ok(u32::from_le_bytes(raw))
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

    #[test]
    fn rejects_unsupported_versions_offsets_and_flags() {
        for (range, value, expected) in [
            (4..8, 103u32, "unsupported BSA version"),
            (8..12, 40u32, "unsupported BSA folder record offset"),
            (12..16, 0x8000_0000u32, "unsupported BSA archive flags"),
            (32..36, 0x4000_0000u32, "unsupported BSA file flags"),
        ] {
            let mut bytes = uncompressed_fixture();
            bytes[range].copy_from_slice(&value.to_le_bytes());
            let error = iter_raw_entries(&bytes).unwrap_err().to_string();
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    #[ignore = "requires OPENSKYRIM_SKYRIM_DATA with locally installed game assets"]
    fn accepts_installed_skyrim_bsa_headers() {
        let data_dir = std::env::var_os("OPENSKYRIM_SKYRIM_DATA")
            .map(std::path::PathBuf::from)
            .expect("set OPENSKYRIM_SKYRIM_DATA to the Skyrim Data directory");
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
