use crate::esm::{
    extractors::extract_subrecords,
    mmap_reader::EsmReader,
    records::RawRecord,
    types::{GroupHeader, RecordHeader, WorldReference},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use flate2::{Decompress, FlushDecompress, Status};
use nom::{
    IResult,
    bytes::complete::take,
    number::complete::{le_f32, le_i32, le_u16, le_u32},
};
use std::path::Path;

const FLAG_COMPRESSED: u32 = 0x00040000;

#[derive(Debug, Clone, Default)]
pub struct PluginMetadata {
    pub masters: Vec<String>,
    pub flags: u32,
}

/// Read TES4 flags and Windows-1252 master filenames for slot assignment.
pub fn parse_plugin_metadata(path: &Path) -> Result<PluginMetadata> {
    parse_plugin_metadata_inner(path)
        .wrap_err_with(|| format!("failed to read plugin header {}", path.display()))
}

/// Bound TES4 decoding before extracting the master dependency list.
fn parse_plugin_metadata_inner(path: &Path) -> Result<PluginMetadata> {
    let reader = EsmReader::open(path)?;
    let data = reader.as_slice();
    let (rest, header) =
        parse_record_header(data).map_err(|error| eyre!("failed to parse TES4 header: {error}"))?;
    if &header.type_tag != b"TES4" || header.data_size as usize > rest.len() {
        return Err(eyre!("invalid TES4 record in {}", path.display()));
    }
    let payload = &rest[..header.data_size as usize];
    let decoded = if header.flags & FLAG_COMPRESSED != 0 {
        if payload.len() < 4 {
            return Err(eyre!("truncated compressed TES4 record"));
        }
        let expected = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
        let output = read_bounded(&payload[4..], expected)?;
        if output.len() != expected {
            return Err(eyre!("TES4 decompressed size mismatch"));
        }
        output
    } else {
        payload.to_vec()
    };
    let masters = extract_subrecords(&decoded)?
        .into_iter()
        .filter_map(|(tag, data)| {
            (tag == b"MAST").then(|| {
                shared::plugin_selection::decode_windows_1252(&data)
                    .trim_end_matches('\0')
                    .to_string()
            })
        })
        .collect();
    Ok(PluginMetadata {
        masters,
        flags: header.flags,
    })
}

/// Cap decompressed output so a corrupt size cannot reserve gigabytes up front.
const MAX_RESERVATION: usize = 64 * 1024 * 1024;

/// The most a record may inflate to: deflate's best case is about 1032:1, plus
/// room for a tiny stream's fixed overhead, and never more than 64 MiB (the
/// largest record in the shipped plugins inflates to under 100 KB).
fn max_inflated_size(compressed_len: usize) -> usize {
    compressed_len
        .saturating_mul(1032)
        .saturating_add(4096)
        .min(MAX_RESERVATION)
}

/// Inflate into a bounded buffer and require exactly one complete zlib stream.
/// Producing the declared bytes alone does not validate a truncated checksum
/// trailer. Reserve one extra byte to detect output longer than declared, and
/// refuse impossible declared sizes before allocating.
fn read_bounded(compressed: &[u8], expected: usize) -> std::io::Result<Vec<u8>> {
    if expected > max_inflated_size(compressed.len()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "declared size {expected} is more than {} compressed bytes can hold",
                compressed.len()
            ),
        ));
    }
    let mut output = Vec::with_capacity(expected + 1);
    let mut inflater = Decompress::new(true);
    let status = inflater
        .decompress_vec(compressed, &mut output, FlushDecompress::Finish)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if output.len() != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "decompressed size mismatch: expected {expected}, got {}",
                output.len()
            ),
        ));
    }
    if status != Status::StreamEnd {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "incomplete zlib stream",
        ));
    }
    if inflater.total_in() != compressed.len() as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} trailing bytes after zlib stream",
                compressed.len() as u64 - inflater.total_in()
            ),
        ));
    }
    Ok(output)
}

/// For STAT / MSTT / FURN
pub struct StaticRecord {
    pub form_id: u32,
    pub editor_id: Option<String>,
    pub model_path: Option<String>,
}

/// Binary Nom Parser for 24-byte Record Header
pub fn parse_record_header(input: &[u8]) -> IResult<&[u8], RecordHeader> {
    let (input, type_tag_bytes) = take(4usize)(input)?;
    let (input, data_size) = le_u32(input)?;
    let (input, flags) = le_u32(input)?;
    let (input, form_id) = le_u32(input)?;
    let (input, version_control) = le_u32(input)?;
    let (input, version) = le_u16(input)?;
    let (input, unknown) = le_u16(input)?;

    let mut type_tag = [0u8; 4];
    type_tag.copy_from_slice(type_tag_bytes);

    Ok((
        input,
        RecordHeader {
            type_tag,
            data_size,
            flags,
            form_id,
            version_control,
            version,
            unknown,
        },
    ))
}

pub fn parse_group_header(input: &[u8]) -> IResult<&[u8], GroupHeader> {
    let (input, _type_tag) = take(4usize)(input)?;
    let (input, data_size) = le_u32(input)?;
    let (input, label) = le_u32(input)?;
    let (input, group_type) = le_i32(input)?;
    let (input, _) = take(8usize)(input)?;

    Ok((
        input,
        GroupHeader {
            data_size,
            label,
            group_type,
        },
    ))
}

/// Count corrupt candidates without retaining or printing every malformed payload.
#[derive(Default)]
struct ParseWarnings {
    count: u64,
    first: Option<String>,
}

impl ParseWarnings {
    fn skipped(&mut self, reason: String) {
        self.count += 1;
        if self.first.is_none() {
            self.first = Some(reason);
        }
    }

    /// Print and collect the same bounded summary for terminal and report consumers.
    fn report(&self, source: &str) -> Vec<String> {
        let Some(first) = &self.first else {
            return Vec::new();
        };
        let summary = format!(
            "warning: {source}: skipped {} malformed record/frame occurrence(s); first: {first}; complete neighboring records retained",
            self.count
        );
        eprintln!("{summary}");
        vec![summary]
    }
}

/// Parse nested groups in file order using an explicit stack. Recover only at
/// trusted record/group boundaries; never search payload bytes for a new header.
pub fn parse_group(
    input: &[u8],
    current_cell: Option<u32>,
    current_worldspace: Option<u32>,
    records: &mut Vec<RawRecord>,
) -> Result<()> {
    let mut warnings = ParseWarnings::default();
    parse_group_inner(
        input,
        current_cell,
        current_worldspace,
        records,
        &mut warnings,
    );
    let _ = warnings.report("plugin record stream");
    Ok(())
}

fn parse_group_inner(
    input: &[u8],
    current_cell: Option<u32>,
    current_worldspace: Option<u32>,
    records: &mut Vec<RawRecord>,
    warnings: &mut ParseWarnings,
) {
    let mut curr = input;
    let mut current_cell = current_cell;
    let mut current_worldspace = current_worldspace;
    let mut parents = Vec::new();

    loop {
        if curr.is_empty() {
            let Some((rest, cell, worldspace)) = parents.pop() else {
                return;
            };
            curr = rest;
            current_cell = cell;
            current_worldspace = worldspace;
            continue;
        }
        if curr.len() < 24 {
            warnings.skipped(format!(
                "trailing {} bytes in bounded record frame",
                curr.len(),
            ));
            curr = &[];
            continue;
        }
        let peek_tag = &curr[..4];
        if peek_tag == b"GRUP" {
            let Ok((rest, group)) = parse_group_header(curr) else {
                warnings.skipped("invalid GRUP header".into());
                curr = &[];
                continue;
            };
            let group_data_size = group.data_size as usize;
            if group_data_size < 24 {
                warnings.skipped(format!("invalid GRUP size {}", group.data_size));
                curr = &[];
                continue;
            }
            let content_size = group_data_size - 24;
            if content_size > rest.len() {
                warnings.skipped(format!(
                    "truncated GRUP payload: expected {content_size}, got {}",
                    rest.len()
                ));
                curr = &[];
                continue;
            }
            let group_content = &rest[..content_size];

            let next_cell = match group.group_type {
                6 | 8 | 9 | 10 => Some(group.label),
                _ => current_cell,
            };
            let next_worldspace = match group.group_type {
                1 => Some(group.label),
                _ => current_worldspace,
            };

            parents.push((&rest[content_size..], current_cell, current_worldspace));
            curr = group_content;
            current_cell = next_cell;
            current_worldspace = next_worldspace;
        } else {
            let Ok((rest, header)) = parse_record_header(curr) else {
                warnings.skipped("invalid record header".into());
                curr = &[];
                continue;
            };
            let record_len = header.data_size as usize;
            if record_len > rest.len() {
                warnings.skipped(format!(
                    "truncated {:?} record {:08x}: expected {record_len}, got {}",
                    header.type_tag,
                    header.form_id,
                    rest.len()
                ));
                curr = &[];
                continue;
            }
            let raw_payload = &rest[..record_len];
            curr = &rest[record_len..];
            let subrecords = match decode_record_subrecords(&header, raw_payload) {
                Ok(subrecords) => subrecords,
                Err(error) => {
                    warnings.skipped(format!(
                        "{} {:08X}: {error}",
                        String::from_utf8_lossy(&header.type_tag),
                        header.form_id
                    ));
                    continue;
                }
            };

            records.push(RawRecord {
                form_id: header.form_id,
                record_type: header.type_tag,
                flags: header.flags,
                subrecords,
                cell_form_id: current_cell,
                worldspace_form_id: current_worldspace,
                load_order: 0,
            });
        }
    }
}

fn decode_record_subrecords(
    header: &RecordHeader,
    raw_payload: &[u8],
) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let subrecords = decode_record_subrecords_inner(header, raw_payload);
    // A deletion's header remains authoritative when its discarded body is torn.
    // Preserve a complete body: legacy named GMST deletions can resolve by EDID.
    if header.flags & 0x20 != 0 {
        return Ok(subrecords.unwrap_or_default());
    }
    subrecords
}

fn decode_record_subrecords_inner(
    header: &RecordHeader,
    raw_payload: &[u8],
) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    if header.flags & FLAG_COMPRESSED == 0 {
        return extract_subrecords(raw_payload);
    }
    color_eyre::eyre::ensure!(raw_payload.len() >= 4, "truncated compressed record");
    let expected = u32::from_le_bytes(raw_payload[..4].try_into().unwrap()) as usize;
    let decoded = read_bounded(&raw_payload[4..], expected)?;
    extract_subrecords(&decoded)
}

/// Parse usable records once, retaining the existing terminal skip summaries.
pub fn parse_plugin_file(path: &Path) -> Result<Vec<RawRecord>> {
    parse_plugin_file_with_warnings(path).map(|(records, _)| records)
}

/// Return the same per-plugin skip line printed while retaining usable neighboring records.
pub(crate) fn parse_plugin_file_with_warnings(
    path: &Path,
) -> Result<(Vec<RawRecord>, Vec<String>)> {
    parse_plugin_file_inner(path)
        .wrap_err_with(|| format!("failed to parse plugin {}", path.display()))
}

/// Keep header checks and local recovery unchanged while returning their bounded summary.
fn parse_plugin_file_inner(path: &Path) -> Result<(Vec<RawRecord>, Vec<String>)> {
    let reader = EsmReader::open(path)?;
    let data = reader.as_slice();

    let mut records = Vec::new();

    let (after_header, header) =
        parse_record_header(data).map_err(|e| eyre!("Failed to parse record header: {e}"))?;
    if &header.type_tag != b"TES4" {
        return Err(eyre!("plugin does not start with TES4"));
    }
    let header_payload = header.data_size as usize;
    if header_payload > after_header.len() {
        return Err(eyre!("truncated TES4 payload"));
    }
    let payload = &after_header[..header_payload];
    if header.flags & FLAG_COMPRESSED != 0 {
        if payload.len() < 4 {
            return Err(eyre!("truncated compressed TES4 record"));
        }
        let expected = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
        let decoded = read_bounded(&payload[4..], expected)?;
        if decoded.len() != expected {
            return Err(eyre!("TES4 decompressed size mismatch"));
        }
        extract_subrecords(&decoded)?;
    } else {
        extract_subrecords(payload)?;
    }
    let mut warnings = ParseWarnings::default();
    parse_group_inner(
        &after_header[header_payload..],
        None,
        None,
        &mut records,
        &mut warnings,
    );
    let summaries = warnings.report(&path.file_name().unwrap_or_default().to_string_lossy());

    Ok((records, summaries))
}

/// Parses REFR payload for Position and Base Form ID pointers
pub fn parse_refr_record(input: &[u8], form_id: u32) -> IResult<&[u8], WorldReference> {
    let mut base_form_id = 0u32;
    let mut pos_x = 0.0f32;
    let mut pos_y = 0.0f32;
    let mut pos_z = 0.0f32;
    let mut rot_x = 0.0f32;
    let mut rot_y = 0.0f32;
    let mut rot_z = 0.0f32;

    let mut curr = input;
    while curr.len() >= 6 {
        let (next, sub_tag) = take(4usize)(curr)?;
        let (next, sub_len) = le_u16(next)?;
        let sub_tag_str = std::str::from_utf8(sub_tag).unwrap_or("");

        if sub_tag_str == "NAME" && sub_len == 4 {
            let (_, id) = le_u32(next)?;
            base_form_id = id;
        } else if sub_tag_str == "DATA" && sub_len >= 24 {
            let (rem, px) = le_f32(next)?;
            let (rem, py) = le_f32(rem)?;
            let (rem, pz) = le_f32(rem)?;
            let (rem, rx) = le_f32(rem)?;
            let (rem, ry) = le_f32(rem)?;
            let (_, rz) = le_f32(rem)?;
            pos_x = px;
            pos_y = py;
            pos_z = pz;
            rot_x = rx;
            rot_y = ry;
            rot_z = rz;
        }

        let advance = (sub_len as usize).min(next.len());
        curr = &next[advance..];
    }

    Ok((
        curr,
        WorldReference {
            form_id,
            base_form_id,
            pos_x,
            pos_y,
            pos_z,
            rot_x,
            rot_y,
            rot_z,
            cell_form_id: 0,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_strategies::{arbitrary_bytes, config, corrupted};
    use proptest::prelude::*;

    fn plugin_bytes() -> Vec<u8> {
        let cells = [
            dummy_content::esm::Cell {
                grid_x: -1,
                grid_y: -1,
            },
            dummy_content::esm::Cell {
                grid_x: 0,
                grid_y: 0,
            },
            dummy_content::esm::Cell {
                grid_x: 1,
                grid_y: 1,
            },
        ];
        dummy_content::esm::plugin(&dummy_content::esm::Plugin {
            author: "Mudcrab dummy-content",
            worldspace: "GeneratedWorld",
            cells: &cells,
            model_path: "meshes/generated.nif",
            diffuse: "textures/generated_color.dds",
            normal_texture: "textures/generated_normal.dds",
        })
        .unwrap()
    }

    /// Mirrors `parse_plugin_file` without file IO so prefixes can be swept.
    fn parse_prefix(bytes: &[u8]) -> Result<()> {
        let (rest, header) = parse_record_header(bytes).map_err(|error| eyre!("{error}"))?;
        if &header.type_tag != b"TES4" || header.data_size as usize > rest.len() {
            return Err(eyre!("invalid TES4 record"));
        }
        let mut records = Vec::new();
        parse_group(&rest[header.data_size as usize..], None, None, &mut records)
    }

    #[test]
    fn record_parser_skips_partial_subrecords_and_compressed_headers() {
        for compressed in [false, true] {
            let payload = b"EDID\x02\x00x\x00DATA\x04\x00x";
            let bytes = if compressed {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                std::io::Write::write_all(&mut encoder, payload).unwrap();
                [
                    (payload.len() as u32).to_le_bytes().as_slice(),
                    &encoder.finish().unwrap(),
                ]
                .concat()
            } else {
                payload.to_vec()
            };
            let flags = if compressed { FLAG_COMPRESSED } else { 0 };
            let record = [
                b"STAT".as_slice(),
                &(bytes.len() as u32).to_le_bytes(),
                &flags.to_le_bytes(),
                &0x123u32.to_le_bytes(),
                &[0; 8],
                &bytes,
            ]
            .concat();
            let mut records = Vec::new();
            parse_group(&record, None, None, &mut records).unwrap();
            assert!(records.is_empty());
        }
        let record = [
            b"STAT".as_slice(),
            &3u32.to_le_bytes(),
            &FLAG_COMPRESSED.to_le_bytes(),
            &0x123u32.to_le_bytes(),
            &[0; 8],
            &[0; 3],
        ]
        .concat();
        let mut records = Vec::new();
        parse_group(&record, None, None, &mut records).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn a_declared_size_beyond_what_the_stream_can_hold_is_refused_before_reading() {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, b"small").unwrap();
        let compressed = encoder.finish().unwrap();

        let error = read_bounded(&compressed, u32::MAX as usize).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("compressed bytes can hold"));

        assert_eq!(read_bounded(&compressed, 5).unwrap(), b"small");
        // A highly compressible payload within deflate's ratio still inflates.
        let zeros = vec![0u8; 1 << 20];
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut encoder, &zeros).unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(read_bounded(&compressed, zeros.len()).unwrap(), zeros);
    }

    #[test]
    fn generated_plugins_never_panic_under_truncation_or_mutation() {
        let bytes = plugin_bytes();
        for length in 0..bytes.len() {
            let result = std::panic::catch_unwind(|| parse_prefix(&bytes[..length]));
            assert!(result.is_ok(), "ESM parser panicked at length {length}");
        }
        let mut rng = dummy_content::rng::Rng::new(44);
        for _ in 0..256 {
            let mut mutated = bytes.clone();
            let index = rng.next_u64() as usize % mutated.len();
            mutated[index] ^= 0xff;
            let result = std::panic::catch_unwind(|| parse_prefix(&mutated));
            assert!(result.is_ok(), "ESM parser panicked on mutation at {index}");
        }
    }

    #[test]
    fn empty_and_foreign_files_are_rejected_as_plugins() {
        let directory = tempfile::tempdir().unwrap();
        let group_first = [b"GRUP".as_slice(), &24u32.to_le_bytes(), &[0; 16]].concat();
        let cases: [(&str, &[u8]); 4] = [
            ("empty.esm", b""),
            ("text.esp", b"not a plugin"),
            ("tag-only.esm", b"TES4"),
            ("group-first.esm", &group_first),
        ];
        for (name, bytes) in cases {
            let path = directory.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            assert!(
                parse_plugin_file(&path).is_err(),
                "{name} parsed as a plugin"
            );
            assert!(
                parse_plugin_metadata(&path).is_err(),
                "{name} parsed as a plugin"
            );
        }
        assert!(EsmReader::open(directory.path().join("missing.esm")).is_err());
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn compressed_record(type_tag: &[u8; 4], declared: u32, data: &[u8]) -> Vec<u8> {
        let mut payload = declared.to_le_bytes().to_vec();
        payload.extend_from_slice(&zlib(data));
        let mut bytes = type_tag.to_vec();
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&FLAG_COMPRESSED.to_le_bytes());
        bytes.extend_from_slice(&[0; 12]);
        bytes.extend_from_slice(&payload);
        bytes
    }

    #[test]
    fn compressed_records_require_a_complete_zlib_stream_even_when_output_matches() {
        let directory = tempfile::tempdir().unwrap();
        for data in [
            Vec::new(),
            [b"EDID".as_slice(), &4u16.to_le_bytes(), b"Test"].concat(),
        ] {
            let valid = compressed_record(b"STAT", data.len() as u32, &data);
            let mut records = Vec::new();
            parse_group(&valid, None, None, &mut records).unwrap();
            assert_eq!(records.len(), 1);
            let path = directory.path().join("valid.esp");
            std::fs::write(&path, compressed_record(b"TES4", data.len() as u32, &data)).unwrap();
            parse_plugin_metadata(&path).unwrap();
            parse_plugin_file(&path).unwrap();
            for missing in 1..=4 {
                // The subrecords can inflate completely before the zlib
                // checksum trailer arrives. Matching output length is not
                // sufficient evidence that the compressed record is intact.
                let mut bytes = compressed_record(b"STAT", data.len() as u32, &data);
                bytes.truncate(bytes.len() - missing);
                let payload_length = (bytes.len() - 24) as u32;
                bytes[4..8].copy_from_slice(&payload_length.to_le_bytes());
                let mut records = Vec::new();
                parse_group(&bytes, None, None, &mut records).unwrap();
                assert!(
                    records.is_empty(),
                    "accepted a stream missing {missing} trailer bytes"
                );

                bytes[..4].copy_from_slice(b"TES4");
                let path = directory.path().join("truncated.esp");
                std::fs::write(&path, &bytes).unwrap();
                assert!(parse_plugin_metadata(&path).is_err());
                assert!(parse_plugin_file(&path).is_err());
            }
        }
    }

    #[test]
    fn compressed_records_reject_trailing_junk_and_second_zlib_streams() {
        let directory = tempfile::tempdir().unwrap();
        for data in [
            Vec::new(),
            [b"EDID".as_slice(), &4u16.to_le_bytes(), b"Test"].concat(),
        ] {
            for suffix in [b"junk".to_vec(), zlib(b"second stream")] {
                let mut bytes = compressed_record(b"STAT", data.len() as u32, &data);
                let mut records = Vec::new();
                parse_group(&bytes, None, None, &mut records).unwrap();
                assert_eq!(records.len(), 1);
                bytes.extend_from_slice(&suffix);
                let payload_length = (bytes.len() - 24) as u32;
                bytes[4..8].copy_from_slice(&payload_length.to_le_bytes());
                let mut records = Vec::new();
                parse_group(&bytes, None, None, &mut records).unwrap();
                assert!(records.is_empty());

                // Compressed TES4 headers use the same exact-input rule.
                bytes[..4].copy_from_slice(b"TES4");
                let path = directory.path().join("trailing.esp");
                std::fs::write(&path, bytes).unwrap();
                for error in [
                    parse_plugin_metadata(&path).unwrap_err(),
                    parse_plugin_file(&path).unwrap_err(),
                ] {
                    let message = format!("{error:?}");
                    assert!(
                        message.contains("trailing.esp")
                            && message.contains("trailing bytes after zlib stream"),
                        "{message}"
                    );
                }
            }
        }
    }

    #[test]
    fn compressed_records_reject_corrupt_declared_sizes() {
        let subrecord = [b"EDID".as_slice(), &4u16.to_le_bytes(), b"Test"].concat();
        let mut records = Vec::new();
        let valid = compressed_record(b"STAT", subrecord.len() as u32, &subrecord);
        parse_group(&valid, None, None, &mut records).unwrap();
        assert_eq!(records[0].subrecords[0].1, b"Test");

        for declared in [u32::MAX, subrecord.len() as u32 - 1] {
            let corrupt = compressed_record(b"STAT", declared, &subrecord);
            let mut records = Vec::new();
            parse_group(&corrupt, None, None, &mut records).unwrap();
            assert!(records.is_empty());
            let error = read_bounded(&corrupt[28..], declared as usize).unwrap_err();
            // A size no stream this short could reach is refused before inflating.
            let message = error.to_string();
            assert!(
                message.contains("decompressed size mismatch")
                    || message.contains("compressed bytes can hold"),
                "{message}"
            );
        }
    }

    #[test]
    fn compressed_plugin_headers_reject_corrupt_declared_sizes() {
        let directory = tempfile::tempdir().unwrap();
        let master = [b"MAST".as_slice(), &9u16.to_le_bytes(), b"Base.esm\0"].concat();
        for (declared, valid) in [
            (master.len() as u32, true),
            (u32::MAX, false),
            (master.len() as u32 - 1, false),
        ] {
            let path = directory.path().join("compressed.esp");
            std::fs::write(&path, compressed_record(b"TES4", declared, &master)).unwrap();
            let metadata = parse_plugin_metadata(&path);
            assert_eq!(metadata.is_ok(), valid, "declared size {declared}");
            assert_eq!(parse_plugin_file(&path).is_ok(), valid);
            if valid {
                assert_eq!(metadata.unwrap().masters, ["Base.esm"]);
            }
        }
    }

    proptest! {
        #![proptest_config(config(256))]

        #[test]
        fn record_parsers_never_panic_on_arbitrary_bytes(bytes in arbitrary_bytes(1024)) {
            let _ = parse_record_header(&bytes);
            let _ = parse_group_header(&bytes);
            let _ = parse_refr_record(&bytes, 0);
            let _ = extract_subrecords(&bytes);
            let _ = parse_group(&bytes, None, None, &mut Vec::new());
        }

        #[test]
        fn corrupted_plugins_never_panic(bytes in corrupted(plugin_bytes())) {
            let _ = parse_prefix(&bytes);
        }
    }

    proptest! {
        // Each case writes a file, so fewer cases keep the suite quick.
        #![proptest_config(config(64))]

        #[test]
        fn plugin_files_never_panic_on_arbitrary_contents(bytes in arbitrary_bytes(256)) {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("arbitrary.esp");
            std::fs::write(&path, &bytes).unwrap();
            let _ = parse_plugin_file(&path);
            let _ = parse_plugin_metadata(&path);
        }
    }
}
