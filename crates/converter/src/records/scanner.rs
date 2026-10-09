//! Memory-mapped framing with recoverable record errors and bounded inflation.
use super::PluginDiagnostics;
use color_eyre::{Result, eyre::WrapErr};
use flate2::{Decompress, FlushDecompress, Status};
use memmap2::Mmap;
use std::{fs::File, path::Path};

/// A framed, inflated record before any schema interpretation or ID resolution.
pub(crate) struct ScannedRecord {
    pub record_type: [u8; 4],
    pub flags: u32,
    pub version_control: u32,
    pub form_version: u16,
    pub header_unknown: u16,
    pub source_form_id: u32,
    pub cell_form_id: Option<u32>,
    pub worldspace_form_id: Option<u32>,
    /// Native label of the lexical GRUP7 enclosing an INFO record.
    pub source_topic_form_id: Option<u32>,
    /// Byte offset of that source GRUP7 header, preserving physical provenance.
    pub topic_group_offset: Option<u64>,
    /// Byte offset of this record header in its source plugin.
    pub source_record_offset: u64,
    pub payload: Vec<u8>,
}

/// Read a checked four-byte integer from framing bytes.
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("checked frame"))
}

/// Read source inputs only; ownership and slot assignment remain with LoadOrder.
pub(crate) fn scan_plugin(
    path: &Path,
    diagnostics: &mut PluginDiagnostics,
    mut receive: impl FnMut(ScannedRecord, &mut PluginDiagnostics),
) -> Result<()> {
    let file = File::open(path).wrap_err_with(|| format!("open {}", path.display()))?;
    if file.metadata()?.len() < 24 {
        diagnostics.note("record", || "input shorter than TES4 header".into());
        return Ok(());
    }
    // Inputs are read-only for the conversion; the map is never exposed to a writer.
    let map = unsafe { Mmap::map(&file)? };
    if map.len() < 24 || &map[..4] != b"TES4" {
        diagnostics.note("record", || "input has no complete TES4 header".into());
        return Ok(());
    }
    scan_bytes(&map, diagnostics, &mut receive);
    Ok(())
}

/// Traverse groups iteratively, retaining siblings when an inner boundary is corrupt.
fn scan_bytes(
    bytes: &[u8],
    diagnostics: &mut PluginDiagnostics,
    receive: &mut impl FnMut(ScannedRecord, &mut PluginDiagnostics),
) {
    let mut position = 0;
    let mut end = bytes.len();
    let mut cell = None;
    let mut world = None;
    let mut topic = None;
    let mut parents = Vec::new();
    loop {
        if position == end {
            let Some((parent_end, parent_cell, parent_world, parent_topic)) = parents.pop() else {
                break;
            };
            end = parent_end;
            cell = parent_cell;
            world = parent_world;
            topic = parent_topic;
            continue;
        }
        if end - position < 24 {
            diagnostics.note("record", || {
                format!("incomplete frame at byte {position}; skipping containing group remainder")
            });
            position = end;
            continue;
        }
        let header = &bytes[position..position + 24];
        let signature: [u8; 4] = header[..4].try_into().expect("fixed header");
        let size = u32_at(header, 4) as usize;
        if signature == *b"GRUP" {
            if size < 24 || size > end - position || parents.len() >= 4096 {
                diagnostics.note("record", || {
                    format!("invalid group boundary/nesting at byte {position}")
                });
                position = end;
                continue;
            }
            parents.push((end, cell, world, topic));
            end = position + size;
            let label = u32_at(header, 8);
            let kind = u32_at(header, 12) as i32;
            if kind == 1 {
                world = Some(label);
                cell = None;
            }
            if matches!(kind, 6 | 8 | 9 | 10) {
                cell = Some(label);
            }
            if kind == 7 {
                topic = Some((label, position as u64));
            }
            position += 24;
            continue;
        }
        if size > end - position - 24 {
            diagnostics.note("record", || {
                format!(
                    "truncated {} {:08X} at byte {position}; skipping containing group remainder",
                    String::from_utf8_lossy(&signature),
                    u32_at(header, 12)
                )
            });
            position = end;
            continue;
        }
        let source_record_offset = position as u64;
        let payload = &bytes[position + 24..position + 24 + size];
        position += 24 + size;
        let flags = u32_at(header, 8);
        let source_form_id = u32_at(header, 12);
        let inflated = if flags & 0x0004_0000 != 0 {
            match inflate(payload) {
                Ok(bytes) => bytes,
                Err(error) => {
                    diagnostics.note("record", || {
                        format!(
                            "{} {source_form_id:08X}: {error}",
                            String::from_utf8_lossy(&signature)
                        )
                    });
                    continue;
                }
            }
        } else {
            payload.to_vec()
        };
        receive(
            ScannedRecord {
                record_type: signature,
                flags,
                version_control: u32_at(header, 16),
                form_version: u16::from_le_bytes(
                    header[20..22].try_into().expect("checked header"),
                ),
                header_unknown: u16::from_le_bytes(
                    header[22..24].try_into().expect("checked header"),
                ),
                source_form_id,
                cell_form_id: cell,
                worldspace_form_id: world,
                source_topic_form_id: (signature == *b"INFO")
                    .then_some(topic)
                    .flatten()
                    .map(|(label, _)| label),
                topic_group_offset: (signature == *b"INFO")
                    .then_some(topic)
                    .flatten()
                    .map(|(_, offset)| offset),
                source_record_offset,
                payload: inflated,
            },
            diagnostics,
        );
    }
}

/// Require exactly one complete zlib stream, with a 64 MiB per-record ceiling.
fn inflate(payload: &[u8]) -> std::result::Result<Vec<u8>, String> {
    if payload.len() < 4 {
        return Err("compressed record lacks size".into());
    }
    let size = u32_at(payload, 0) as usize;
    let compressed = &payload[4..];
    let bound = compressed
        .len()
        .saturating_mul(1032)
        .saturating_add(4096)
        .min(64 * 1024 * 1024);
    if size > bound {
        return Err(format!("inflated size {size} exceeds bound {bound}"));
    }
    let mut output = Vec::with_capacity(size + 1);
    let mut inflater = Decompress::new(true);
    let status = inflater
        .decompress_vec(compressed, &mut output, FlushDecompress::Finish)
        .map_err(|error| format!("zlib: {error}"))?;
    if output.len() != size
        || status != Status::StreamEnd
        || inflater.total_in() != compressed.len() as u64
    {
        return Err("incomplete zlib stream, trailing bytes or size mismatch".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Frame a synthetic record independently of the production scanner.
    fn frame(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 24];
        bytes[..4].copy_from_slice(tag);
        bytes[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes[8..12].copy_from_slice(&flags.to_le_bytes());
        bytes[12..16].copy_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    /// A corrupt compressed neighbor must not hide a later usable record.
    #[test]
    fn corrupt_record_keeps_neighbors() {
        let mut bytes = frame(b"TES4", 0, 0, &[]);
        bytes.extend(frame(b"STAT", 1, 0x40000, &[0, 0, 0, 0, 99]));
        bytes.extend(frame(b"STAT", 2, 0, b"EDID\x02\x00x\0"));
        let mut diagnostics = PluginDiagnostics::default();
        let mut ids = Vec::new();
        scan_bytes(&bytes, &mut diagnostics, &mut |record, _| {
            ids.push(record.source_form_id)
        });
        assert_eq!(ids, [0, 2]);
        assert_eq!(diagnostics.skipped_records, 1);
    }

    /// Deflate validation checks its trailer as well as declared output length.
    #[test]
    fn compression_requires_complete_single_stream() {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"data").unwrap();
        let compressed = encoder.finish().unwrap();
        let mut payload = 4u32.to_le_bytes().to_vec();
        payload.extend(&compressed);
        assert_eq!(inflate(&payload).unwrap(), b"data");
        payload.pop();
        assert!(inflate(&payload).is_err());
        payload = 4u32.to_le_bytes().to_vec();
        payload.extend(&compressed);
        payload.push(0);
        assert!(inflate(&payload).is_err());
        payload[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(inflate(&payload).is_err());
    }
}
