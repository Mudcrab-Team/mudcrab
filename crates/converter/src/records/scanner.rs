//! Memory-mapped framing with recoverable record errors and bounded inflation.
use super::{DiscardedFrameScope, PluginDiagnostics, TruncatedPlacement};
use crate::esm::types::CELL_CHILD_GROUP_KINDS;
use color_eyre::{Result, eyre::WrapErr};
use flate2::{Decompress, FlushDecompress, Status};
use memmap2::Mmap;
use std::{collections::HashSet, fs::File, path::Path};

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

/// Header-only observation also covers safely identified records whose payload cannot be read.
pub(crate) struct ObservedHeader {
    pub record_type: [u8; 4],
    pub source_form_id: u32,
    pub flags: u32,
    pub source_record_offset: u64,
    pub source_record_size: u32,
    /// Exclusive containing-group end (file end for an ungrouped record).
    pub source_group_end: u64,
    /// Declared stored body fits this group; siblings outside it cannot satisfy the frame.
    pub frame_complete: bool,
    pub cell_form_id: Option<u32>,
    pub worldspace_form_id: Option<u32>,
    pub source_topic_form_id: Option<u32>,
    pub topic_group_offset: Option<u64>,
    pub failure: Option<String>,
}

/// Read a checked four-byte integer from framing bytes.
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("checked frame"))
}

#[derive(Default)]
pub(crate) struct ScanDamage {
    pub frames: HashSet<DiscardedFrameScope>,
    pub truncated_placements: Vec<(u64, TruncatedPlacement)>,
}

impl ScanDamage {
    fn failed_record(&mut self, header: &ObservedHeader) {
        if header.record_type == *b"CELL" {
            self.frames
                .insert(DiscardedFrameScope::Cell(header.source_form_id));
        }
    }

    fn note_prefix(&mut self, bytes: &[u8], offset: usize) {
        if let Some(scope) = DiscardedFrameScope::from_group_prefix(bytes) {
            self.frames.insert(scope);
        } else if let Some(placement) = TruncatedPlacement::from_prefix(bytes) {
            self.truncated_placements.push((offset as u64, placement));
        } else if let Some(scope) = DiscardedFrameScope::from_cell_prefix(bytes) {
            self.frames.insert(scope);
        }
    }
}

/// Read source inputs only; ownership and slot assignment remain with LoadOrder.
pub(crate) fn scan_plugin(
    path: &Path,
    diagnostics: &mut PluginDiagnostics,
    mut observe: impl FnMut(ObservedHeader),
    mut receive: impl FnMut(ScannedRecord, &mut PluginDiagnostics),
) -> Result<ScanDamage> {
    diagnostics.set_record(0, None, None, None, "framing");
    let file = File::open(path).wrap_err_with(|| format!("open {}", path.display()))?;
    if file.metadata()?.len() < 24 {
        diagnostics.note("record", || "input shorter than TES4 header".into());
        return Ok(ScanDamage::default());
    }
    // SAFETY: inputs are read-only for the conversion and the map is never exposed to a writer.
    // An outside truncation or in-place rewrite would fault reads of the map; that risk is
    // accepted here (#633).
    let map = unsafe { Mmap::map(&file)? };
    if map.len() < 24 || &map[..4] != b"TES4" {
        diagnostics.note("record", || "input has no complete TES4 header".into());
        return Ok(ScanDamage::default());
    }
    Ok(scan_bytes_observed(
        &map,
        diagnostics,
        &mut receive,
        &mut observe,
    ))
}

/// Traverse groups iteratively, retaining siblings when an inner boundary is corrupt.
#[cfg(test)]
fn scan_bytes(
    bytes: &[u8],
    diagnostics: &mut PluginDiagnostics,
    receive: &mut impl FnMut(ScannedRecord, &mut PluginDiagnostics),
) {
    scan_bytes_observed(bytes, diagnostics, receive, &mut |_| {});
}

/// Keep history observation separate from payload allocation and existing recovery policy.
fn scan_bytes_observed(
    bytes: &[u8],
    diagnostics: &mut PluginDiagnostics,
    receive: &mut impl FnMut(ScannedRecord, &mut PluginDiagnostics),
    observe: &mut impl FnMut(ObservedHeader),
) -> ScanDamage {
    let mut position = 0;
    let mut end = bytes.len();
    let mut cell = None;
    let mut world = None;
    let mut topic = None;
    let mut parents = Vec::new();
    let mut damage = ScanDamage::default();
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
        diagnostics.set_record(position as u64, None, None, None, "framing");
        if end - position < 24 {
            diagnostics.note("record", || {
                format!("incomplete frame at byte {position}; skipping containing group remainder")
            });
            let prefix = &bytes[position..end];
            damage.note_prefix(prefix, position);
            damage.frames.insert(cell.into());
            position = end;
            continue;
        }
        let header = &bytes[position..position + 24];
        let signature: [u8; 4] = header[..4].try_into().expect("fixed header");
        let size = u32_at(header, 4) as usize;
        diagnostics.set_record(
            position as u64,
            Some(signature),
            (signature != *b"GRUP").then(|| u32_at(header, 12)),
            None,
            "framing",
        );
        if signature == *b"GRUP" {
            let label = u32_at(header, 8);
            let kind = u32_at(header, 12) as i32;
            let cell_child = CELL_CHILD_GROUP_KINDS.contains(&kind).then_some(label);
            if size < 24 || size > end - position || parents.len() >= 4096 {
                diagnostics.note("record", || {
                    format!("invalid group boundary/nesting at byte {position}")
                });
                let child_offset = position + 24;
                let child_prefix = &bytes[child_offset..end];
                damage.note_prefix(child_prefix, child_offset);
                damage
                    .frames
                    .insert(DiscardedFrameScope::unread_tail(cell, child_prefix.len()));
                damage
                    .frames
                    .extend(cell_child.map(DiscardedFrameScope::Cell));
                position = end;
                continue;
            }
            parents.push((end, cell, world, topic));
            end = position + size;
            if kind == 1 {
                world = Some(label);
                cell = None;
            }
            if cell_child.is_some() {
                cell = cell_child;
            }
            if kind == 7 {
                topic = Some((label, position as u64));
            }
            position += 24;
            continue;
        }
        let mut observed = ObservedHeader {
            record_type: signature,
            source_form_id: u32_at(header, 12),
            flags: u32_at(header, 8),
            source_record_offset: position as u64,
            source_record_size: size as u32,
            source_group_end: end as u64,
            frame_complete: size <= end - position - 24,
            cell_form_id: cell,
            worldspace_form_id: world,
            source_topic_form_id: (signature == *b"INFO")
                .then_some(topic)
                .flatten()
                .map(|(id, _)| id),
            topic_group_offset: (signature == *b"INFO")
                .then_some(topic)
                .flatten()
                .map(|(_, offset)| offset),
            failure: None,
        };
        let source_record_offset = position as u64;
        let flags = u32_at(header, 8);
        let payload = if observed.frame_complete {
            let start = position + 24;
            position = start + size;
            &bytes[start..position]
        } else {
            let start = position + 24;
            damage
                .frames
                .insert(DiscardedFrameScope::unread_tail(cell, end - start));
            diagnostics.note("record", || {
                format!(
                    "truncated {} {:08X} at byte {position}; skipping containing group remainder",
                    String::from_utf8_lossy(&signature),
                    u32_at(header, 12)
                )
            });
            position = end;
            if flags & 0x20 == 0 {
                observed.failure =
                    Some("truncated record payload; containing group remainder skipped".into());
                damage.failed_record(&observed);
                observe(observed);
                continue;
            }
            &[]
        };
        let source_form_id = u32_at(header, 12);
        let inflated = if flags & 0x0004_0000 != 0 {
            match inflate(payload) {
                Ok(bytes) => bytes,
                // A native deletion is authoritative even without a decodable body.
                // Keep usable bytes above so a GMST tombstone can still name its alias.
                Err(_) if flags & 0x20 != 0 => Vec::new(),
                Err(error) => {
                    observed.failure = Some(error.clone());
                    damage.failed_record(&observed);
                    observe(observed);
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
        observe(observed);
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
    damage
}

/// Fuzz entry point for a compressed record payload.
///
/// The fuzz targets call this through `converter::records`; ordinary builds do not
/// compile it. Any input may return `Err`, but it must not panic or abort.
#[cfg(fuzzing)]
pub fn fuzz_inflate(payload: &[u8]) -> std::result::Result<Vec<u8>, String> {
    inflate(payload)
}

/// Fuzz entry point for record framing and recovery over in-memory plugin bytes.
///
/// The diagnostics sink keeps counters and bounded summaries but no disk journal, so
/// each input is independent. Record contents are still fully framed and inflated.
#[cfg(fuzzing)]
pub fn fuzz_scan(bytes: &[u8]) {
    let mut diagnostics = PluginDiagnostics::fuzz_summary_only();
    scan_bytes_observed(bytes, &mut diagnostics, &mut |_, _| {}, &mut |_| {});
}

/// Require exactly one complete zlib stream, with a 64 MiB per-record ceiling.
pub(crate) fn inflate(payload: &[u8]) -> std::result::Result<Vec<u8>, String> {
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
    use crate::test_strategies::{arbitrary_bytes, config};
    use proptest::prelude::*;
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

    proptest! {
        // Each case opens the diagnostic journal once, so keep the sweep short.
        #![proptest_config(config(64))]

        /// One damaged compressed record is skipped with a reported error, and the
        /// valid records before and after it are still delivered. This is the
        /// recovery class of the #744 import abort.
        #[test]
        fn damaged_compressed_record_keeps_neighbors(content in arbitrary_bytes(256)) {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&content).expect("encode synthetic content");
            let stream = encoder.finish().expect("finish synthetic stream");
            // Declare one byte more than the stream holds: a valid stream with a
            // wrong size is always a damaged record.
            let mut damaged = (content.len() as u32 + 1).to_le_bytes().to_vec();
            damaged.extend(stream);

            let mut bytes = frame(b"TES4", 0, 0, &[]);
            bytes.extend(frame(b"STAT", 1, 0, b"EDID\x02\x00a\0"));
            bytes.extend(frame(b"STAT", 2, 0x0004_0000, &damaged));
            bytes.extend(frame(b"STAT", 3, 0, b"EDID\x02\x00b\0"));

            let mut diagnostics = PluginDiagnostics::default();
            let mut ids = Vec::new();
            scan_bytes(&bytes, &mut diagnostics, &mut |record, _| {
                ids.push(record.source_form_id);
            });

            prop_assert_eq!(ids, [0, 1, 3]);
            prop_assert_eq!(diagnostics.skipped_records, 1);
        }
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
