//! Checked input framing with recoverable record errors and bounded inflation.
use super::PluginDiagnostics;
use crate::esm::binary::PluginMetadata;
use color_eyre::{Result, eyre::WrapErr};
use flate2::{Decompress, FlushDecompress, Status};
use sha2::{Digest, Sha256};
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

#[cfg(test)]
thread_local! {
    /// A deterministic source change made while the decode-view digest is computed.
    static DURING_SCAN_DIGEST: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Digest the exact view the records were decoded from. Tests may inject one source edit
/// partway through, so the stability check after the digest must still see it.
fn decode_view_digest(bytes: &[u8]) -> [u8; 32] {
    #[cfg(test)]
    if let Some(change) = DURING_SCAN_DIGEST.with(|slot| slot.borrow_mut().take()) {
        let mut hasher = Sha256::new();
        let split = bytes.len() / 2;
        hasher.update(&bytes[..split]);
        change();
        hasher.update(&bytes[split..]);
        return hasher.finalize().into();
    }
    Sha256::digest(bytes).into()
}

/// What a scanned input still proves after the source-stability check.
pub(crate) enum ScanOutcome {
    /// The decode view verified; the digest covers exactly those bytes.
    Verified([u8; 32]),
    /// The source changed after it was copied; records and digest cover that stable copy.
    RetainedSnapshot([u8; 32]),
    /// A direct mapping changed while decoding; retained records may mix generations.
    MixedRecords,
    /// No records usable for later proof stages.
    Skipped,
}

/// Read source inputs only; ownership and slot assignment remain with LoadOrder.
/// A non-`Verified` outcome means later proof stages must not reopen this changed source;
/// already decoded records can still be retained. A digest covers the bytes that were decoded.
/// `RetainedSnapshot` and `MixedRecords` mean records were delivered; with none, the input is
/// simply `Skipped`. Local record errors alone still return the digest.
pub(crate) fn scan_admitted_plugin(
    (path, admitted): (&Path, &PluginMetadata),
    diagnostics: &mut PluginDiagnostics,
    observe: impl FnMut(ObservedHeader),
    receive: impl FnMut(ScannedRecord, &mut PluginDiagnostics),
) -> Result<ScanOutcome> {
    scan_plugin_impl(path, Some(admitted), diagnostics, observe, receive)
}

/// Framing tests without a load-order admission retain the original scanner entry point.
#[cfg(test)]
pub(crate) fn scan_plugin(
    path: &Path,
    diagnostics: &mut PluginDiagnostics,
    observe: impl FnMut(ObservedHeader),
    receive: impl FnMut(ScannedRecord, &mut PluginDiagnostics),
) -> Result<ScanOutcome> {
    scan_plugin_impl(path, None, diagnostics, observe, receive)
}

/// Check an optional admission authority before exposing framed records to callbacks.
fn scan_plugin_impl(
    path: &Path,
    admitted: Option<&PluginMetadata>,
    diagnostics: &mut PluginDiagnostics,
    mut observe: impl FnMut(ObservedHeader),
    mut receive: impl FnMut(ScannedRecord, &mut PluginDiagnostics),
) -> Result<ScanOutcome> {
    diagnostics.set_record(0, None, None, None, "framing");
    let input = (|| -> Result<_> {
        let file = File::open(path).wrap_err_with(|| format!("open {}", path.display()))?;
        let map = crate::source_input::map_input(&file, path)?;
        Ok((file, map))
    })();
    let (file, map) = match input {
        Ok(input) => input,
        Err(error) => {
            diagnostics.note("record", || {
                format!("unusable input {}: {error:#}", path.display())
            });
            return Ok(ScanOutcome::Skipped);
        }
    };
    if map.len() < 24 || &map[..4] != b"TES4" {
        diagnostics.note("record", || "input has no complete TES4 header".into());
        return Ok(ScanOutcome::Skipped);
    }
    if let Some(admitted) = admitted
        && let Err(error) = crate::esm::binary::verify_admitted_metadata(&map, admitted)
    {
        diagnostics.set_record(0, Some(*b"TES4"), None, None, "admission");
        diagnostics.event_context.action = Some("skipped");
        diagnostics.note("record", || {
            format!("unusable input {}: {error:#}", path.display())
        });
        return Ok(ScanOutcome::Skipped);
    }
    let mut delivered = false;
    scan_bytes_observed(
        &map,
        diagnostics,
        |record, diagnostics| {
            delivered = true;
            receive(record, diagnostics);
        },
        &mut observe,
    );
    // Digest the view the records were decoded from before the final stability check, so a
    // change that lands while (or after) the digest is computed cannot be published as
    // verified. The legacy reader hashes and then rechecks its source stamp the same way.
    let digest = decode_view_digest(&map);
    if let Err(error) = map.verify(&file, path) {
        diagnostics.set_record(0, None, None, None, "input_stability");
        if map.is_snapshot() {
            // Anonymous bytes never change under the reader: one digest names both the
            // records that were delivered and the copy they came from.
            diagnostics.event_context.action = Some("kept_snapshot");
            diagnostics.note("input", || {
                format!("{error:#}; retained the stable captured snapshot")
            });
            return Ok(if delivered {
                ScanOutcome::RetainedSnapshot(digest)
            } else {
                ScanOutcome::Skipped
            });
        }
        diagnostics.event_context.action = Some("kept_decoded_records");
        diagnostics.note("input", || {
            format!("{error:#}; retained decoded records may mix versions of the large input and be inconsistent")
        });
        return Ok(if delivered {
            ScanOutcome::MixedRecords
        } else {
            ScanOutcome::Skipped
        });
    }
    Ok(ScanOutcome::Verified(digest))
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
        diagnostics.set_record(position as u64, None, None, None, "framing");
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
        diagnostics.set_record(
            position as u64,
            Some(signature),
            (signature != *b"GRUP").then(|| u32_at(header, 12)),
            None,
            "framing",
        );
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
        if size > end - position - 24 {
            observed.failure =
                Some("truncated record payload; containing group remainder skipped".into());
            observe(observed);
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
                // A bounded native deletion is authoritative even without a decodable body.
                // Keep usable bytes above so a GMST tombstone can still name its alias.
                Err(_) if flags & 0x20 != 0 => Vec::new(),
                Err(error) => {
                    observed.failure = Some(error.clone());
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

    /// Every semantic flag change is rejected before a header or record reaches a callback.
    #[test]
    fn admission_guard_rejects_changed_flags_before_callbacks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ChangedMod.esp");
        let admitted = PluginMetadata::default();
        for flags in [0x1, 0x80, 0x200] {
            let mut bytes = frame(b"TES4", 0, flags, &[]);
            bytes.extend(frame(b"STAT", 0x101, 0, b"EDID\x02\x00x\0"));
            std::fs::write(&path, bytes).unwrap();
            let mut diagnostics = PluginDiagnostics::default();
            let mut observed = 0;
            let mut received = 0;
            let digest = scan_admitted_plugin(
                (&path, &admitted),
                &mut diagnostics,
                |_| observed += 1,
                |_, _| received += 1,
            )
            .unwrap();
            assert!(matches!(digest, ScanOutcome::Skipped));
            assert_eq!((observed, received), (0, 0));
            assert_eq!(diagnostics.skipped_records, 1);
            assert_eq!(diagnostics.event_context.action, Some("skipped"));
            let warnings = diagnostics.warning_summary_lines("ChangedMod.esp");
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("changed after load order was read"));
        }
    }

    /// Case-equivalent master names pass admission while one corrupt record stays local.
    #[test]
    fn admission_guard_keeps_case_equivalent_masters_and_usable_record_neighbors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("UnchangedMod.esp");
        let mut masters = b"MAST".to_vec();
        masters.extend(9u16.to_le_bytes());
        masters.extend(b"BASE.ESM\0");
        let mut bytes = frame(b"TES4", 0, 0, &masters);
        bytes.extend(frame(b"STAT", 0x101, 0x40000, &[0, 0, 0, 0, 99]));
        bytes.extend(frame(b"STAT", 0x102, 0, b"EDID\x02\x00x\0"));
        std::fs::write(&path, bytes).unwrap();
        let admitted = PluginMetadata {
            flags: 0,
            masters: vec!["base.esm".into()],
        };
        let mut diagnostics = PluginDiagnostics::default();
        let mut observed = Vec::new();
        let mut received = Vec::new();
        let digest = scan_admitted_plugin(
            (&path, &admitted),
            &mut diagnostics,
            |header| observed.push(header.source_form_id),
            |record, _| received.push(record.source_form_id),
        )
        .unwrap();
        assert!(matches!(digest, ScanOutcome::Verified(_)));
        assert_eq!(observed, [0, 0x101, 0x102]);
        assert_eq!(received, [0, 0x102]);
        assert_eq!(diagnostics.skipped_records, 1);
        assert_eq!(diagnostics.input_changes, 0);
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

    /// Source edits report independently of malformed records while captured neighbors survive.
    #[test]
    #[cfg(windows)]
    fn scanner_snapshot_warns_for_source_truncation_and_keeps_neighbors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("MutableMod.esp");
        let mut bytes = frame(b"TES4", 0, 0, &[]);
        bytes.extend(frame(b"STAT", 0x101, 0, b"EDID\x02\x00a\0"));
        bytes.extend(frame(b"STAT", 0x102, 0x40000, &[0, 0, 0, 0, 99]));
        bytes.extend(frame(b"STAT", 0x103, 0, b"EDID\x02\x00b\0"));
        std::fs::write(&path, &bytes).unwrap();
        let mut diagnostics = PluginDiagnostics::default();
        let mut ids = Vec::new();
        let mut truncated = None;
        let outcome = scan_plugin(
            &path,
            &mut diagnostics,
            |_| {
                if truncated.is_none() {
                    truncated = Some(File::options().write(true).open(&path).unwrap().set_len(0));
                }
            },
            |record, _| ids.push(record.source_form_id),
        )
        .unwrap();
        assert!(truncated.unwrap().is_ok(), "source is still mapped");
        // The snapshot bytes are the delivered records, so their digest is the decode proof.
        let ScanOutcome::RetainedSnapshot(digest) = outcome else {
            panic!("a stable captured snapshot must keep its decode digest");
        };
        assert_eq!(digest, <[u8; 32]>::from(Sha256::digest(&bytes)));
        assert_eq!(ids, [0, 0x101, 0x103]);
        assert_eq!(diagnostics.skipped_records, 1);
        assert_eq!(diagnostics.skipped_fields, 0);
        let warnings = diagnostics.warning_summary_lines("MutableMod.esp");
        assert!(
            warnings
                .iter()
                .any(|line| line.contains("input changed during conversion"))
        );
        assert!(warnings.iter().any(|line| line.contains("MutableMod.esp")));
        assert!(!warnings.iter().any(|line| line.contains("inconsistent")));
    }

    /// A same-size edit between records can leave a direct view with two source generations.
    #[test]
    fn scanner_direct_view_warns_that_rewritten_records_may_be_inconsistent() {
        use std::io::{Seek, SeekFrom};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("MutableLargeMod.esp");
        let mut bytes = frame(b"TES4", 0, 0, &[]);
        bytes.extend(frame(b"STAT", 0x301, 0, b"EDID\x04\x00old\0"));
        let second_payload = bytes.len() as u64 + 24;
        bytes.extend(frame(b"STAT", 0x302, 0, b"EDID\x04\x00old\0"));
        bytes.extend(frame(
            b"STAT",
            0x303,
            0,
            &vec![0; crate::source_input::MEMORY_SNAPSHOT_LIMIT as usize],
        ));
        assert!(bytes.len() as u64 > crate::source_input::MEMORY_SNAPSHOT_LIMIT);
        std::fs::write(&path, bytes).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut diagnostics = PluginDiagnostics::default();
        let mut records = Vec::new();
        let outcome = scan_plugin(
            &path,
            &mut diagnostics,
            |_| {},
            |record, _| {
                if record.source_form_id == 0x301 {
                    let mut writer = File::options().write(true).open(&path).unwrap();
                    writer.seek(SeekFrom::Start(second_payload)).unwrap();
                    writer.write_all(b"EDID\x04\x00new\0").unwrap();
                    writer.flush().unwrap();
                    // Force an observable stamp change independently of filesystem clock precision.
                    writer
                        .set_times(
                            std::fs::FileTimes::new()
                                .set_modified(modified + std::time::Duration::from_secs(2)),
                        )
                        .unwrap();
                }
                if matches!(record.source_form_id, 0x301 | 0x302) {
                    records.push((record.source_form_id, record.payload));
                }
            },
        )
        .unwrap();
        // A changed direct mapping has no single decoded view to prove.
        assert!(matches!(outcome, ScanOutcome::MixedRecords));
        assert_eq!(
            records,
            [
                (0x301, b"EDID\x04\x00old\0".to_vec()),
                (0x302, b"EDID\x04\x00new\0".to_vec())
            ]
        );
        assert_eq!(diagnostics.input_changes, 1);
        assert_eq!(diagnostics.skipped_records, 0);
        assert_eq!(diagnostics.skipped_fields, 0);
        assert_eq!(
            diagnostics.event_context.action,
            Some("kept_decoded_records")
        );
        let warnings = diagnostics.warning_summary_lines("MutableLargeMod.esp");
        assert!(
            warnings
                .iter()
                .any(|line| line.contains("MutableLargeMod.esp")
                    && line.contains("may mix versions")
                    && line.contains("inconsistent")),
            "changed direct views must warn that retained records may mix versions and be inconsistent: {warnings:?}"
        );
    }

    /// A change made while the decode-view digest is computed must fail the source stamp.
    /// The old order (stamp, then digest) accepted the changed body as verified.
    #[test]
    fn a_source_change_during_the_decode_digest_is_rejected() {
        use std::io::{Seek, SeekFrom, Write};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Large.esp");
        let mut bytes = frame(b"TES4", 0, 0, &[]);
        bytes.extend(frame(b"STAT", 0x401, 0, b"EDID\x02\x00a\0"));
        let edited_offset = bytes.len() as u64;
        bytes.extend(frame(
            b"STAT",
            0x402,
            0,
            &vec![b'x'; crate::source_input::MEMORY_SNAPSHOT_LIMIT as usize],
        ));
        assert!(bytes.len() as u64 > crate::source_input::MEMORY_SNAPSHOT_LIMIT);
        std::fs::write(&path, &bytes).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let edited = path.clone();
        DURING_SCAN_DIGEST.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(move || {
                let mut writer = File::options().write(true).open(&edited).unwrap();
                writer.seek(SeekFrom::Start(edited_offset)).unwrap();
                writer.write_all(b"EDID\x02\x00z\0").unwrap();
                writer.flush().unwrap();
                // Force an observable stamp change independently of filesystem clock precision.
                writer
                    .set_times(
                        std::fs::FileTimes::new()
                            .set_modified(modified + std::time::Duration::from_secs(2)),
                    )
                    .unwrap();
            }));
        });
        let mut diagnostics = PluginDiagnostics::default();
        let outcome = scan_plugin(&path, &mut diagnostics, |_| {}, |_, _| {}).unwrap();
        assert!(
            !matches!(outcome, ScanOutcome::Verified(_)),
            "a change during the decode digest must never be accepted as verified"
        );
        assert_eq!(
            diagnostics.event_context.action,
            Some("kept_decoded_records")
        );
    }

    /// Atomic replacement leaves the old mapped inode untouched, so this red test is safe on Unix.
    #[test]
    #[cfg(unix)]
    fn scanner_snapshot_reports_source_replacement_and_keeps_captured_neighbors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("MutableMod.esp");
        let replacement = directory.path().join("replacement.esp");
        let mut bytes = frame(b"TES4", 0, 0, &[]);
        bytes.extend(frame(b"STAT", 0x201, 0, b"EDID\x02\x00a\0"));
        bytes.extend(frame(b"STAT", 0x202, 0, b"EDID\x02\x00b\0"));
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(&replacement, b"replacement source").unwrap();
        let mut diagnostics = PluginDiagnostics::default();
        let mut ids = Vec::new();
        let mut replaced = false;
        let outcome = scan_plugin(
            &path,
            &mut diagnostics,
            |_| {
                if !replaced {
                    std::fs::rename(&replacement, &path).unwrap();
                    replaced = true;
                }
            },
            |record, _| ids.push(record.source_form_id),
        )
        .unwrap();
        assert!(replaced);
        assert!(matches!(outcome, ScanOutcome::RetainedSnapshot(_)));
        assert_eq!(ids, [0, 0x201, 0x202]);
        assert_eq!(diagnostics.skipped_records, 0);
        assert_eq!(diagnostics.skipped_fields, 0);
        let warnings = diagnostics.warning_summary_lines("MutableMod.esp");
        assert!(
            warnings
                .iter()
                .any(|line| line.contains("input changed during conversion"))
        );
    }
}
