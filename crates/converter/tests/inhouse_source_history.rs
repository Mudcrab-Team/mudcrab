//! Complete-export regressions for native source identity and guarded raw access.
use converter::records::history_source::{SourceQuery, visit_source_history};
use converter::records::source_identity::SourceIdentityState;
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Independent minimal subrecord framing for authored bytes.
fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend((bytes.len() as u16).to_le_bytes());
    out.extend(bytes);
    out
}

/// Independent native main-header framing with unequal sentinel metadata.
fn frame(tag: &[u8; 4], raw: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend((payload.len() as u32).to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(raw.to_le_bytes());
    out.extend(0x55667788u32.to_le_bytes());
    out.extend(44u16.to_le_bytes());
    out.extend(0x1234u16.to_le_bytes());
    out.extend(payload);
    out
}

/// Write one synthetic original and an explicit original-master header.
fn plugin(root: &Path, name: &str, masters: &[&str], flags: u32, records: &[u8]) -> PathBuf {
    let mut tes4 = sub(b"HEDR", &[0; 12]);
    for master in masters {
        tes4.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
        tes4.extend(sub(b"DATA", &[0; 8]));
    }
    let mut bytes = frame(b"TES4", 0, flags, &tes4);
    bytes.extend(records);
    let path = root.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// Keep original files and exported replay context alive for the complete test scope.
fn test_root() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir(&data).unwrap();
    (root, data)
}

/// Exercise the completed public records-only export API, including real SQLite publication.
fn export(root: &Path, names: &[&str], signatures: &[[u8; 4]]) -> PathBuf {
    let order = root.join("plugins.txt");
    fs::write(
        &order,
        names.iter().map(|n| format!("*{n}\n")).collect::<String>(),
    )
    .unwrap();
    let output = root.parent().unwrap().join("output");
    converter::esm::inhouse::export_record_bundle_typed(root, &order, &output, signatures).unwrap();
    output
}

#[test]
/// Unknown signatures remain explicit unsupported typed payloads and exact stored raw material.
fn unknown_new_signature_and_header_bits_export_without_typed_invention() {
    let (_fixture, root) = test_root();
    let payload = sub(b"NEWX", &[0, 0xff, 0x12, 0x99]);
    let original = frame(b"ZZZZ", 0x1800, 0x8000_0000, &payload);
    plugin(&root, "Base.esm", &[], 0, &original);
    let output = export(&root, &["Base.esm"], &[*b"ZZZZ"]);
    let typed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(output.join("typed-records.jsonl")).unwrap())
            .unwrap();
    assert_eq!(typed["supported"], false);
    assert_eq!(typed["payload_complete"], true);
    assert_eq!(
        typed["source_payload_sha256"],
        converter::cache::hash_bytes(&payload)
    );
    assert_eq!(
        typed["fields"][0]["value"]["Bytes"],
        serde_json::json!([0, 255, 18, 153])
    );
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let source: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=6144",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(source, payload);
    let mut count = 0;
    let summary = visit_source_history(
        &output,
        None,
        SourceQuery::Source {
            original_owner: "base.esm".into(),
            full_local: 0x1800,
        },
        |version| {
            assert_eq!(version.flags, 0x8000_0000);
            let raw = version.raw.as_ref().unwrap();
            assert_eq!(raw.header.as_slice(), &original[..24]);
            assert_eq!(raw.stored_payload.as_deref(), Some(payload.as_slice()));
            assert_eq!(version.record_signature, *b"ZZZZ");
            count += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(count, 1);
    assert!(summary.fully_raw_available);
    assert!(!summary.typed_runtime_decoding_attempted);
}

#[test]
/// A nullable runtime ID retains guarded converter-readable bytes and original source identity.
fn wide_light_body_is_raw_queryable_without_aliasing_adjacent_owner() {
    let (_fixture, root) = test_root();
    plugin(&root, "Base.esm", &[], 0, &[]);
    let wide_payload = sub(b"EDID", b"wide_original\0");
    let neighbor_payload = sub(b"EDID", b"different_neighbor\0");
    let wide = frame(b"STAT", 0x0100_1234, 0, &wide_payload);
    plugin(&root, "WideA.esl", &["Base.esm"], 0x200, &wide);
    plugin(
        &root,
        "NeighborB.esl",
        &["Base.esm"],
        0x200,
        &frame(b"STAT", 0x0100_0234, 0, &neighbor_payload),
    );
    let output = export(
        &root,
        &["Base.esm", "WideA.esl", "NeighborB.esl"],
        &[*b"STAT"],
    );
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let neighbor: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=4261417524",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(neighbor, neighbor_payload); // 0xFE001234 belongs only to the adjacent narrow owner.
    let (runtime, offset): (Option<u32>, u64) = db.query_row(
        "SELECT form_id,source_record_offset FROM record_overrides WHERE original_owner='widea.esl' AND full_local=4660", [],
        |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(runtime, None);
    let summary = visit_source_history(
        &output,
        None,
        SourceQuery::Source {
            original_owner: "WideA.esl".into(),
            full_local: 0x1234,
        },
        |v| {
            assert_eq!(v.identity.state, SourceIdentityState::Resolved);
            assert_eq!(v.identity.full_local, Some(0x1234));
            assert_eq!(v.runtime_form_id, None);
            assert!(v.latest_source_candidate);
            assert!(!v.accepted_source_winner && !v.runtime_record_present);
            assert_eq!(v.raw.unwrap().stored_payload, Some(wide_payload.clone()));
            Ok(())
        },
    )
    .unwrap();
    assert!(summary.fully_raw_available);
    let summary = visit_source_history(
        &output,
        None,
        SourceQuery::Physical {
            plugin_id: 1,
            source_record_offset: offset,
        },
        |v| {
            assert_eq!(v.source_form_id, 0x0100_1234);
            assert_eq!(v.raw.unwrap().header.as_slice(), &wide[..24]);
            Ok(())
        },
    )
    .unwrap();
    assert!(summary.fully_raw_available);
    // Changed synthetic original is a local unavailable result, never newly trusted material.
    let path = root.join("WideA.esl");
    let mut changed = fs::read(&path).unwrap();
    changed.push(0x55);
    fs::write(path, changed).unwrap();
    let rejected = visit_source_history(
        &output,
        None,
        SourceQuery::Physical {
            plugin_id: 1,
            source_record_offset: offset,
        },
        |v| {
            assert!(v.raw.is_none() && v.raw_access_failure.is_some());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(rejected.unavailable, 1);
    assert!(!rejected.fully_raw_available);
}

#[test]
/// A malformed compressed record does not hide a valid later unknown-signature neighbor.
fn malformed_compressed_neighbor_preserves_header_and_later_complete_unknown_record() {
    let (_fixture, root) = test_root();
    let compressed_bad = frame(b"STAT", 0x1800, 0x40000, &[5, 0, 0, 0, 0x78, 0x9c, 0]);
    let later_payload = sub(b"NEWX", &[9, 8, 7]);
    let mut records = compressed_bad;
    records.extend(frame(b"ZZZZ", 0x1810, 0, &later_payload));
    plugin(&root, "Base.esm", &[], 0, &records);
    let output = export(&root, &["Base.esm"], &[*b"STAT", *b"ZZZZ"]);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let state: String = db
        .query_row(
            "SELECT status FROM record_overrides WHERE source_form_id=6144",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "skipped");
    let later: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=6160",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(later, later_payload);
    let summary = visit_source_history(
        &output,
        None,
        SourceQuery::Source {
            original_owner: "base.esm".into(),
            full_local: 0x1800,
        },
        |v| {
            assert_eq!(v.status, "skipped");
            assert!(v.rejection_reason.is_some());
            assert_eq!(
                v.raw.as_ref().unwrap().stored_payload.as_deref(),
                Some(&[5, 0, 0, 0, 0x78, 0x9c, 0][..])
            );
            assert!(!v.runtime_record_present);
            Ok(())
        },
    )
    .unwrap();
    assert!(summary.fully_raw_available); // Exact stored material, not a decoded compressed body.
}

#[test]
/// A malformed later TES4 retains its scanner reason without hiding a usable neighbor.
fn malformed_later_header_preserves_failure_and_usable_neighbor() {
    let (_fixture, root) = test_root();
    let bad = frame(b"TES4", 0, 0x40000, &[5, 0, 0, 0, 0x78, 0x9c, 0]);
    let payload = sub(b"NEWX", &[0x91, 0x42, 0xde]);
    let mut records = bad.clone();
    records.extend(frame(b"ZZZZ", 0x1810, 0, &payload));
    plugin(&root, "Base.esm", &[], 0, &records);
    let output = export(&root, &["Base.esm"], &[*b"ZZZZ"]);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let (offset, status, reason): (u64, String, Option<String>) = db.query_row(
        "SELECT source_record_offset,status,reason FROM record_overrides WHERE record_type='TES4' AND source_record_offset>0",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(status, "header");
    assert!(reason.is_some_and(|text| !text.is_empty()));
    let neighbor: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=6160",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(neighbor, payload);
    let summary = visit_source_history(
        &output,
        None,
        SourceQuery::Physical {
            plugin_id: 0,
            source_record_offset: offset,
        },
        |version| {
            assert!(version.rejection_reason.is_some());
            let raw = version.raw.unwrap();
            assert_eq!(raw.header.as_slice(), &bad[..24]);
            assert_eq!(raw.stored_payload.as_deref(), Some(&bad[24..]));
            Ok(())
        },
    )
    .unwrap();
    assert!(summary.fully_raw_available);
}

/// A complete main header inside a truncated group must never consume an outer sibling body.
#[test]
fn containing_group_limit_returns_header_only_and_keeps_outer_neighbor() {
    let (_fixture, root) = test_root();
    let mut bad_header = frame(b"STAT", 0x1800, 0, &[]);
    bad_header[4..8].copy_from_slice(&1000u32.to_le_bytes());
    let mut group = vec![0; 24];
    group[..4].copy_from_slice(b"GRUP");
    group[4..8].copy_from_slice(&48u32.to_le_bytes());
    group[8..12].copy_from_slice(b"STAT");
    group.extend(&bad_header);
    let later_payload = sub(b"NEWX", &[0xde, 0x42, 0x91]);
    group.extend(frame(b"ZZZZ", 0x1810, 0, &later_payload));
    plugin(&root, "Base.esm", &[], 0, &group);
    let output = export(&root, &["Base.esm"], &[*b"STAT", *b"ZZZZ"]);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let later: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=6160",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(later, later_payload);
    let summary = visit_source_history(
        &output,
        None,
        SourceQuery::Source {
            original_owner: "base.esm".into(),
            full_local: 0x1800,
        },
        |v| {
            assert!(!v.frame_complete);
            assert_eq!(v.status, "skipped");
            let raw = v.raw.unwrap();
            assert_eq!(raw.header.as_slice(), bad_header);
            assert_eq!(raw.stored_payload, None);
            assert_eq!(raw.access_status, "header_only_incomplete_frame");
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(summary.header_only, 1);
    assert_eq!(summary.unavailable, 0);
    assert!(!summary.fully_raw_available);
}

/// Runtime supplier compatibility does not invent original ownership; a self neighbor stays exact.
#[test]
fn generic_master_compatibility_keeps_original_owner_unresolved_and_valid_neighbor() {
    let (_fixture, root) = test_root();
    plugin(&root, "Base.esm", &[], 0, &[]);
    let bad_payload = sub(b"EDID", b"unproven_owner\0");
    let good_payload = sub(b"EDID", b"valid_owner\0");
    let mut records = frame(b"STAT", 0x0200_0711, 0, &bad_payload);
    records.extend(frame(b"STAT", 0x0100_0722, 0, &good_payload));
    plugin(&root, "Patch.esp", &["Base.esm"], 0, &records);
    let output = export(&root, &["Base.esm", "Patch.esp"], &[*b"STAT"]);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let count: u32 = db
        .query_row(
            "SELECT count(*) FROM inhouse_source_records WHERE source_form_id=33556241",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1); // Full raw 0x02000711 is preserved alongside runtime compatibility.
    let restored: Vec<u8> = db.query_row(
        "SELECT payload FROM inhouse_source_records WHERE form_id=16779025 AND source_form_id=33556241",
        [], |r| r.get(0),
    ).unwrap();
    assert_eq!(restored, bad_payload);
    let good: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE source_form_id=16779042",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(good, good_payload); // Full raw 0x01000722.
    let (runtime, offset): (Option<u32>, u64) = db.query_row("SELECT form_id,source_record_offset FROM record_overrides WHERE source_form_id=33556241", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(runtime, Some(0x0100_0711));
    let summary = visit_source_history(
        &output,
        None,
        SourceQuery::Physical {
            plugin_id: 1,
            source_record_offset: offset,
        },
        |v| {
            assert_eq!(v.identity.state, SourceIdentityState::MasterIndexOutOfRange);
            assert_eq!(v.identity.original_owner, None);
            assert_eq!(v.identity.full_local, Some(0x711));
            assert_eq!(v.raw.unwrap().stored_payload, Some(bad_payload.clone()));
            assert_eq!(v.status, "accepted");
            assert!(v.rejection_reason.is_none());
            assert!(v.runtime_record_present);
            assert!(!v.latest_source_candidate); // No proven original-owner chain is manufactured.
            Ok(())
        },
    )
    .unwrap();
    assert!(summary.fully_raw_available);
}

/// Raw latest, reader-admitted latest, surviving fallback and accepted deletion stay distinct.
#[test]
fn source_candidate_admission_fallback_and_deletion_are_independent() {
    let (_fixture, root) = test_root();
    let a = sub(b"EDID", b"older_usable\0");
    let b = sub(b"EDID", b"later_deleted\0");
    let mut base = frame(b"STAT", 0x1800, 0, &a);
    base.extend(frame(b"STAT", 0x1810, 0, &b));
    plugin(&root, "Base.esm", &[], 0, &base);
    let mut patch = frame(b"STAT", 0x1800, 0x40000, &[5, 0, 0, 0, 0x78, 0x9c, 0]);
    patch.extend(frame(b"STAT", 0x1810, 0x20, &[]));
    plugin(&root, "Patch.esp", &["Base.esm"], 0, &patch);
    let output = export(&root, &["Base.esm", "Patch.esp"], &[*b"STAT"]);
    let mut admission = Vec::new();
    let summary = visit_source_history(
        &output,
        None,
        SourceQuery::Source {
            original_owner: "base.esm".into(),
            full_local: 0x1800,
        },
        |v| {
            admission.push((
                v.plugin_id,
                v.latest_source_candidate,
                v.latest_reader_admitted_candidate,
                v.accepted_source_winner,
                v.runtime_record_present,
                v.runtime_fallback_before_latest_source,
            ));
            Ok(())
        },
    )
    .unwrap();
    assert!(summary.fully_raw_available);
    assert_eq!(
        admission,
        [
            (0, false, true, true, true, true),
            (1, true, false, false, false, false)
        ]
    );
    let mut deleted = Vec::new();
    visit_source_history(
        &output,
        None,
        SourceQuery::Source {
            original_owner: "base.esm".into(),
            full_local: 0x1810,
        },
        |v| {
            deleted.push((
                v.plugin_id,
                v.latest_source_candidate,
                v.latest_reader_admitted_candidate,
                v.accepted_source_winner,
                v.runtime_record_present,
                v.runtime_fallback_before_latest_source,
            ));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        deleted,
        [
            (0, false, false, false, false, false),
            (1, true, true, false, false, false)
        ]
    );
}
