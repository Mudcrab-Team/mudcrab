//! Complete exports separate runtime compatibility from strict original provenance.

use converter::esm::load_order::LoadOrder;
use converter::records::source_identity::SourceIdentityState;
use converter::records::{
    history_source::{SourceQuery, visit_source_history},
    read_plugins,
};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
    out.extend(bytes);
    out
}

fn frame(tag: &[u8; 4], raw: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(raw.to_le_bytes());
    out.extend(0x5566_7788u32.to_le_bytes());
    out.extend(44u16.to_le_bytes());
    out.extend(0x1234u16.to_le_bytes());
    out.extend(payload);
    out
}

fn plugin(data: &Path, name: &str, masters: &[&str], flags: u32, body: &[u8]) {
    let mut header = sub(b"HEDR", &[0; 12]);
    for master in masters {
        header.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(sub(b"DATA", &[0; 8]));
    }
    let mut bytes = frame(b"TES4", 0, flags, &header);
    bytes.extend(body);
    fs::write(data.join(name), bytes).unwrap();
}

/// Keep synthetic source files and exports alive until their owning test scope ends.
fn test_data() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir(&data).unwrap();
    (root, data)
}

/// Publish with the combined adapter while preserving each fixture's supplied order.
fn export(data: &Path, names: &[&str], label: &str, signatures: &[[u8; 4]]) -> PathBuf {
    let order = data.join(format!("{label}.plugins.txt"));
    fs::write(
        &order,
        names
            .iter()
            .map(|name| format!("*{name}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let output = data.parent().unwrap().join(label);
    converter::esm::inhouse::export_record_bundle_typed(data, &order, &output, signatures).unwrap();
    let identity: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("record-reader.json")).unwrap()).unwrap();
    // Adapter 12 keeps zero-ID GMST identities distinct without changing master-index compatibility.
    assert_eq!(identity["adapter_version"], 12);
    output
}

fn database(output: &Path) -> Connection {
    Connection::open(output.join("skyrim_world.db")).unwrap()
}

fn source_body(db: &Connection, id: u32) -> Vec<u8> {
    db.query_row(
        "SELECT payload FROM inhouse_source_records WHERE form_id=?1",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

fn runtime_count(db: &Connection, id: u32) -> u32 {
    db.query_row("SELECT count(*) FROM records WHERE form_id=?1", [id], |r| {
        r.get(0)
    })
    .unwrap()
}

fn supplier_ordinal(db: &Connection, name: &str) -> u32 {
    db.query_row(
        "SELECT id FROM plugins WHERE name=?1 COLLATE NOCASE",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

fn diagnostic(output: &Path, plugin: &str, key: &str) -> u64 {
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    json["decoder"][plugin][key].as_u64().unwrap()
}

fn unknown_source(
    output: &Path,
    supplier: u32,
    raw: u32,
    expected_runtime: Option<u32>,
    status: &str,
    payload: &[u8],
) {
    let db = database(output);
    let (runtime, offset, owner, local, state, actual_status): (Option<u32>, u64, Option<String>, Option<u32>, String, String) = db.query_row(
        "SELECT form_id,source_record_offset,original_owner,full_local,source_identity_state,status FROM record_overrides WHERE plugin_id=?1 AND source_form_id=?2 AND record_type!='TES4'",
        [supplier, raw], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ).unwrap();
    assert_eq!(runtime, expected_runtime);
    assert_eq!(owner, None);
    assert_eq!(local, Some(raw & 0x00ff_ffff));
    assert_eq!(state, "master_index_out_of_range");
    assert_eq!(actual_status, status);
    let summary = visit_source_history(
        output,
        None,
        SourceQuery::Physical {
            plugin_id: supplier,
            source_record_offset: offset,
        },
        |v| {
            assert_eq!(v.identity.state, SourceIdentityState::MasterIndexOutOfRange);
            assert_eq!(v.identity.original_owner, None);
            assert_eq!(v.identity.full_local, Some(raw & 0x00ff_ffff));
            assert_eq!(v.raw.unwrap().stored_payload.as_deref(), Some(payload));
            assert!(!v.latest_source_candidate);
            Ok(())
        },
    )
    .unwrap();
    assert!(summary.fully_raw_available);
}

#[test]
fn full_supplier_projection_preserves_source_and_optional_link_policy() {
    let (_fixture, data) = test_data();
    plugin(&data, "Base.esm", &[], 0, &[]);
    let mut first = sub(b"EDID", b"compatibility_one\0");
    let raw_link = 0x0200_0712u32;
    let mut dnam = 1.25f32.to_le_bytes().to_vec();
    dnam.extend(raw_link.to_le_bytes());
    first.extend(sub(b"DNAM", &dnam)); // Own STAT material role; beyond-MAST optional word.
    let second = sub(b"EDID", b"compatibility_two\0");
    let neighbor = sub(b"EDID", b"proven_self_neighbor\0");
    let mut body = frame(b"STAT", 0x0200_0711, 0, &first);
    body.extend(frame(b"STAT", 0x0200_0712, 0, &second));
    body.extend(frame(b"STAT", 0x0100_0722, 0, &neighbor));
    plugin(&data, "Patch.esp", &["Base.esm"], 0, &body);
    let names = ["Base.esm", "Patch.esp"];
    let paths = names.iter().map(|n| data.join(n)).collect::<Vec<_>>();
    let order = LoadOrder::read(&paths).unwrap();
    let read = read_plugins(&paths, &order).unwrap();
    let field = read.records[&0x0100_0711]
        .fields
        .iter()
        .find(|f| f.signature == *b"DNAM")
        .unwrap();
    assert_eq!(
        field.canonical_bytes,
        [1.25f32.to_le_bytes(), 0u32.to_le_bytes()].concat()
    );
    assert_eq!(read.records[&0x0100_0711].raw_payload, first);
    let output = export(&data, &names, "full", &[*b"STAT"]);
    let db = database(&output);
    assert_eq!(source_body(&db, 0x0100_0711), first);
    assert_eq!(source_body(&db, 0x0100_0712), second);
    assert_eq!(source_body(&db, 0x0100_0722), neighbor);
    assert_eq!(runtime_count(&db, 0x0100_0711), 1);
    assert_eq!(diagnostic(&output, "patch.esp", "compatibility_headers"), 2);
    assert_eq!(diagnostic(&output, "patch.esp", "invalid_links"), 1);
    assert_eq!(diagnostic(&output, "patch.esp", "skipped_records"), 0);
    unknown_source(
        &output,
        1,
        0x0200_0711,
        Some(0x0100_0711),
        "accepted",
        &first,
    );
    unknown_source(
        &output,
        1,
        0x0200_0712,
        Some(0x0100_0712),
        "accepted",
        &second,
    );
    let resolved: String = db
        .query_row(
            "SELECT original_owner FROM record_overrides WHERE source_form_id=?1",
            [0x0100_0722u32],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(resolved, "patch.esp");
}

#[test]
fn normal_full24_and_unknown_signature_preserve_generic_archive_without_typed_claim() {
    let (_fixture, data) = test_data();
    plugin(&data, "Base.esm", &[], 0, &[]);
    let wide = sub(b"EDID", b"normal_full24\0");
    let unknown = sub(b"ZZZZ", &[0, 255, 17, 0, 128]);
    let mut body = frame(b"STAT", 0x0201_2345, 0, &wide);
    body.extend(frame(b"QZZZ", 0x0201_2346, 0, &unknown));
    plugin(&data, "Patch.esp", &["Base.esm"], 0, &body);
    let names = ["Base.esm", "Patch.esp"];
    let paths = names.iter().map(|n| data.join(n)).collect::<Vec<_>>();
    let order = LoadOrder::read(&paths).unwrap();
    let read = read_plugins(&paths, &order).unwrap();
    assert_eq!(read.records[&0x0101_2345].raw_payload, wide);
    let record = &read.records[&0x0101_2346];
    assert_eq!(record.record_type, *b"QZZZ");
    assert!(!record.supported);
    assert_eq!(record.raw_payload, unknown);
    assert_eq!(record.fields.len(), 1);
    assert_eq!(record.fields[0].signature, *b"ZZZZ");
    assert_eq!(record.fields[0].canonical_bytes, [0, 255, 17, 0, 128]);
    let output = export(&data, &names, "unknown", &[*b"STAT"]);
    let db = database(&output);
    assert_eq!(source_body(&db, 0x0101_2345), wide);
    assert_eq!(source_body(&db, 0x0101_2346), unknown);
    assert_eq!(runtime_count(&db, 0x0101_2346), 1);
    assert_eq!(diagnostic(&output, "patch.esp", "compatibility_headers"), 2);
    unknown_source(
        &output,
        1,
        0x0201_2345,
        Some(0x0101_2345),
        "accepted",
        &wide,
    );
    unknown_source(
        &output,
        1,
        0x0201_2346,
        Some(0x0101_2346),
        "accepted",
        &unknown,
    );
    let typed = fs::read_to_string(output.join("typed-records.jsonl")).unwrap();
    let rows = typed
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1); // Authored typed selection is separate from unknown physical retention.
    assert_eq!(rows[0]["record_type"], serde_json::json!([83, 84, 65, 84]));
}

#[test]
fn narrow_light_projection_and_wide_failure_never_alias_adjacent_owner() {
    let (_fixture, data) = test_data();
    plugin(&data, "Base.esm", &[], 0, &[]);
    let narrow = sub(b"EDID", b"first_light\0");
    let wide = sub(b"EDID", b"wide_archived_only\0");
    let neighbor = sub(b"EDID", b"second_light\0");
    let mut a = frame(b"STAT", 0x0200_0234, 0, &narrow);
    a.extend(frame(b"STAT", 0x0200_1234, 0, &wide));
    plugin(&data, "A.esl", &["Base.esm"], 0x200, &a);
    plugin(
        &data,
        "B.esl",
        &["Base.esm"],
        0x200,
        &frame(b"STAT", 0x0100_0234, 0, &neighbor),
    );
    let output = export(&data, &["Base.esm", "A.esl", "B.esl"], "light", &[*b"STAT"]);
    let db = database(&output);
    assert_eq!(source_body(&db, 0xfe00_0234), narrow);
    assert_eq!(source_body(&db, 0xfe00_1234), neighbor);
    assert_eq!(diagnostic(&output, "a.esl", "compatibility_headers"), 1);
    assert_eq!(diagnostic(&output, "a.esl", "skipped_records"), 1);
    assert_eq!(diagnostic(&output, "a.esl", "invalid_links"), 0);
    unknown_source(
        &output,
        1,
        0x0200_0234,
        Some(0xfe00_0234),
        "accepted",
        &narrow,
    );
    unknown_source(&output, 1, 0x0200_1234, None, "skipped", &wide);
}

#[test]
fn duplicate_projection_both_orders_keeps_existing_first_admission_policy() {
    for compatibility_first in [false, true] {
        let (_fixture, data) = test_data();
        plugin(&data, "Base.esm", &[], 0, &[]);
        let compat = sub(b"EDID", b"compatibility_candidate\0");
        let valid = sub(b"EDID", b"proven_candidate\0");
        let a = frame(b"STAT", 0x0200_0711, 0, &compat);
        let b = frame(b"STAT", 0x0100_0711, 0, &valid);
        let body = if compatibility_first {
            [a, b].concat()
        } else {
            [b, a].concat()
        };
        plugin(&data, "Patch.esp", &["Base.esm"], 0, &body);
        let output = export(&data, &["Base.esm", "Patch.esp"], "duplicates", &[*b"STAT"]);
        let db = database(&output);
        assert_eq!(
            source_body(&db, 0x0100_0711),
            if compatibility_first {
                compat.clone()
            } else {
                valid
            }
        );
        assert_eq!(runtime_count(&db, 0x0100_0711), 1);
        assert_eq!(diagnostic(&output, "patch.esp", "compatibility_headers"), 1);
        assert_eq!(diagnostic(&output, "patch.esp", "skipped_records"), 1);
        assert_eq!(diagnostic(&output, "patch.esp", "invalid_links"), 0);
        let reason: String = db
            .query_row(
                "SELECT reason FROM record_overrides WHERE status='skipped' AND record_type='STAT'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(reason.contains("duplicate/colliding"));
        unknown_source(
            &output,
            1,
            0x0200_0711,
            Some(0x0100_0711),
            if compatibility_first {
                "accepted"
            } else {
                "skipped"
            },
            &compat,
        );
    }
}

#[test]
fn legal_master_override_deletion_and_restore_share_runtime_not_original_owner_proof() {
    let (_fixture, data) = test_data();
    let initial = sub(b"EDID", b"inferred_runtime\0");
    let replacement = sub(b"EDID", b"legal_override\0");
    let restore = sub(b"EDID", b"legal_restore\0");
    plugin(&data, "Base.esm", &[], 0, &[]);
    plugin(
        &data,
        "Patch.esp",
        &["Base.esm"],
        0,
        &frame(b"STAT", 0x0200_0711, 0, &initial),
    );
    plugin(
        &data,
        "Override.esp",
        &["Base.esm", "Patch.esp"],
        0,
        &frame(b"STAT", 0x0100_0711, 0, &replacement),
    );
    plugin(
        &data,
        "Delete.esp",
        &["Base.esm", "Patch.esp"],
        0,
        &frame(b"STAT", 0x0100_0711, 0x20, &[]),
    );
    plugin(
        &data,
        "Restore.esp",
        &["Base.esm", "Patch.esp"],
        0,
        &frame(b"STAT", 0x0100_0711, 0, &restore),
    );
    let names = [
        "Base.esm",
        "Patch.esp",
        "Override.esp",
        "Delete.esp",
        "Restore.esp",
    ];
    for (length, expected) in [
        (2, Some(&initial)),
        (3, Some(&replacement)),
        (4, None),
        (5, Some(&restore)),
    ] {
        let output = export(
            &data,
            &names[..length],
            &format!("stage{length}"),
            &[*b"STAT"],
        );
        let db = database(&output);
        assert_eq!(
            runtime_count(&db, 0x0100_0711),
            u32::from(expected.is_some())
        );
        if let Some(bytes) = expected {
            assert_eq!(source_body(&db, 0x0100_0711), *bytes);
        }
        let rows: Vec<(u32, bool, String)> = db.prepare("SELECT plugin_id,deleted,status FROM record_overrides WHERE form_id=?1 ORDER BY chain_position").unwrap()
            .query_map([0x0100_0711u32], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect();
        assert_eq!(rows.len(), length - 1);
        for (position, row) in rows.iter().enumerate() {
            assert_eq!(
                row,
                &((position + 1) as u32, position == 2, "accepted".into())
            );
        }
        unknown_source(
            &output,
            1,
            0x0200_0711,
            Some(0x0100_0711),
            "accepted",
            &initial,
        );
    }
}

#[test]
fn malformed_body_and_unusable_candidate_counts_do_not_masquerade_as_winners() {
    let (_fixture, data) = test_data();
    let compressed = [5, 0, 0, 0, 0x78, 0x9c, 0];
    let movt = sub(b"EDID", b"NpcDefault_MissingRequiredSpeeds\0");
    let neighbor = sub(b"EDID", b"usable_neighbor\0");
    let mut body = frame(b"STAT", 0x0100_1800, 0x40000, &compressed);
    body.extend(frame(b"MOVT", 0x0103_580d, 0, &movt)); // Projects to the actual own required MOVT validator key.
    body.extend(frame(b"STAT", 0x0100_1810, 0, &neighbor));
    plugin(&data, "Base.esm", &[], 0, &body);
    let output = export(&data, &["Base.esm"], "malformed", &[*b"STAT", *b"MOVT"]);
    let db = database(&output);
    assert_eq!(runtime_count(&db, 0x1800), 0);
    assert_eq!(runtime_count(&db, 0x3580d), 0);
    assert_eq!(source_body(&db, 0x1810), neighbor);
    assert_eq!(diagnostic(&output, "base.esm", "compatibility_headers"), 2); // MOVT remaps before candidate rejection; compressed body never reaches remap.
    assert_eq!(diagnostic(&output, "base.esm", "skipped_records"), 2);
    assert_eq!(diagnostic(&output, "base.esm", "invalid_links"), 0);
    unknown_source(&output, 0, 0x0100_1800, None, "skipped", &compressed);
    unknown_source(&output, 0, 0x0103_580d, Some(0x3580d), "skipped", &movt);
}

fn setting(raw: u32, name: &str, value: f32) -> Vec<u8> {
    let mut body = sub(b"EDID", format!("{name}\0").as_bytes());
    body.extend(sub(b"DATA", &value.to_le_bytes()));
    frame(b"GMST", raw, 0, &body)
}

#[test]
fn gmst_editor_identity_deletion_and_zero_restore_keep_existing_convention() {
    let (_fixture, data) = test_data();
    plugin(
        &data,
        "Base.esm",
        &[],
        0,
        &setting(0x0123_c00e, "fFixtureSetting", 17.5),
    );
    plugin(
        &data,
        "Patch.esp",
        &["Base.esm"],
        0,
        &setting(0x0200_0700, "FFIXTURESETTING", 22.5),
    );
    plugin(
        &data,
        "Delete.esp",
        &["Base.esm", "Patch.esp"],
        0,
        &frame(b"GMST", 0x0100_0700, 0x20, &[]),
    );
    // A light-flagged ESP stays in the ordinary-plugin ordering group: restore is genuinely last.
    plugin(
        &data,
        "Restore.esp",
        &["Base.esm"],
        0x200,
        &setting(0, "fFixtureSetting", 31.5),
    );
    let names = ["Base.esm", "Patch.esp", "Delete.esp", "Restore.esp"];
    for (length, present) in [(2, true), (3, false), (4, true)] {
        let output = export(
            &data,
            &names[..length],
            &format!("gmst{length}"),
            &[*b"GMST"],
        );
        let db = database(&output);
        if length == 4 {
            assert!(supplier_ordinal(&db, "Restore.esp") > supplier_ordinal(&db, "Delete.esp"));
        }
        assert_eq!(runtime_count(&db, 0x0023_c00e), u32::from(present));
        for name in &names[..length] {
            assert_eq!(
                diagnostic(&output, &name.to_ascii_lowercase(), "compatibility_headers"),
                0
            );
        }
        let state: String = db.query_row("SELECT source_identity_state FROM record_overrides WHERE plugin_id=0 AND record_type='GMST'", [], |r| r.get(0)).unwrap();
        assert_eq!(state, "gmst_editor_id_convention");
        let owner: Option<String> = db.query_row("SELECT original_owner FROM record_overrides WHERE plugin_id=0 AND record_type='GMST'", [], |r| r.get(0)).unwrap();
        assert_eq!(owner, None);
        if length == 4 {
            let source: u32 = db
                .query_row(
                    "SELECT source_form_id FROM inhouse_source_records WHERE form_id=?1",
                    [0x0023_c00eu32],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(source, 0);
        }
    }
}

/// Distinct EditorIDs remain independent when native zero headers override both settings.
#[test]
fn gmst_zero_id_overrides_in_one_plugin_keep_distinct_settings() {
    let (_fixture, data) = test_data();
    plugin(
        &data,
        "Base.esm",
        &[],
        0,
        &[
            setting(0x1800, "fFirstSetting", 11.5),
            setting(0x1801, "fSecondSetting", 22.5),
        ]
        .concat(),
    );
    let first = setting(0, "fFirstSetting", 31.5);
    let second = setting(0, "fSecondSetting", 42.5);
    plugin(
        &data,
        "Patch.esp",
        &["Base.esm"],
        0,
        &[first.clone(), second.clone()].concat(),
    );
    let output = export(
        &data,
        &["Base.esm", "Patch.esp"],
        "zero_overrides",
        &[*b"GMST"],
    );
    let db = database(&output);
    assert_eq!(source_body(&db, 0x1800), first[24..]);
    assert_eq!(source_body(&db, 0x1801), second[24..]);
    assert_eq!(runtime_count(&db, 0x1800), 1);
    assert_eq!(runtime_count(&db, 0x1801), 1);
    assert_eq!(diagnostic(&output, "patch.esp", "skipped_records"), 0);
}

/// A zero-header restoration in one plugin cannot claim another plugin's zero-header setting.
#[test]
fn gmst_zero_id_restorations_in_different_plugins_keep_distinct_settings() {
    let (_fixture, data) = test_data();
    plugin(
        &data,
        "Base.esm",
        &[],
        0,
        &[
            setting(0x1800, "fFirstSetting", 11.5),
            setting(0x1801, "fSecondSetting", 22.5),
        ]
        .concat(),
    );
    plugin(
        &data,
        "Delete.esp",
        &["Base.esm"],
        0,
        &[
            frame(b"GMST", 0x1800, 0x20, &[]),
            frame(b"GMST", 0x1801, 0x20, &[]),
        ]
        .concat(),
    );
    let first = setting(0, "fFirstSetting", 31.5);
    let second = setting(0, "fSecondSetting", 42.5);
    plugin(&data, "RestoreFirst.esp", &["Base.esm"], 0, &first);
    plugin(&data, "RestoreSecond.esp", &["Base.esm"], 0, &second);
    let output = export(
        &data,
        &[
            "Base.esm",
            "Delete.esp",
            "RestoreFirst.esp",
            "RestoreSecond.esp",
        ],
        "zero_restorations",
        &[*b"GMST"],
    );
    let db = database(&output);
    assert_eq!(runtime_count(&db, 0x1800), 1);
    assert_eq!(runtime_count(&db, 0x1801), 1);
    assert_eq!(source_body(&db, 0x1800), first[24..]);
    assert_eq!(source_body(&db, 0x1801), second[24..]);
    assert_eq!(
        diagnostic(&output, "restorefirst.esp", "skipped_records"),
        0
    );
    assert_eq!(
        diagnostic(&output, "restoresecond.esp", "skipped_records"),
        0
    );
}

/// Unidentified zero tombstones cannot borrow a setting's identity; EDID tombstones still work.
#[test]
fn gmst_zero_id_unidentified_deletion_preserves_editor_identified_setting() {
    let (_fixture, data) = test_data();
    plugin(
        &data,
        "Base.esm",
        &[],
        0,
        &[
            setting(0x1800, "fFirstSetting", 11.5),
            setting(0x1801, "fSecondSetting", 22.5),
        ]
        .concat(),
    );
    let restored = setting(0, "fFirstSetting", 31.5);
    plugin(&data, "Patch.esp", &["Base.esm"], 0, &restored);
    plugin(
        &data,
        "Delete.esp",
        &["Base.esm"],
        0,
        &[
            frame(b"GMST", 0, 0x20, &[]),
            frame(b"GMST", 0, 0x20, &sub(b"EDID", b"fSecondSetting\0")),
        ]
        .concat(),
    );
    let output = export(
        &data,
        &["Base.esm", "Patch.esp", "Delete.esp"],
        "zero_deletions",
        &[*b"GMST"],
    );
    let db = database(&output);
    assert_eq!(runtime_count(&db, 0x1800), 1);
    assert_eq!(source_body(&db, 0x1800), restored[24..]);
    assert_eq!(runtime_count(&db, 0x1801), 0);
    assert_eq!(diagnostic(&output, "delete.esp", "skipped_records"), 1);
    let statuses = db
        .prepare("SELECT status,reason FROM record_overrides WHERE plugin_id=?1 AND record_type='GMST' ORDER BY source_record_offset")
        .unwrap()
        .query_map([supplier_ordinal(&db, "Delete.esp")], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(statuses[0].0, "skipped");
    assert!(
        statuses[0]
            .1
            .as_ref()
            .unwrap()
            .contains("no known identity")
    );
    assert_eq!(statuses[1], ("accepted".into(), None));
}

/// Unknown zero-header definitions cannot poison an existing setting; duplicate EDIDs keep first.
#[test]
fn gmst_zero_id_unknown_definition_and_duplicate_do_not_replace_valid_override() {
    let (_fixture, data) = test_data();
    plugin(
        &data,
        "Base.esm",
        &[],
        0,
        &setting(0x1800, "fFirstSetting", 11.5),
    );
    let valid = setting(0, "fFirstSetting", 31.5);
    plugin(
        &data,
        "Patch.esp",
        &["Base.esm"],
        0,
        &[
            setting(0, "fUnknownSetting", 91.5),
            valid.clone(),
            setting(0, "fFirstSetting", 99.5),
        ]
        .concat(),
    );
    let output = export(
        &data,
        &["Base.esm", "Patch.esp"],
        "zero_duplicates",
        &[*b"GMST"],
    );
    let db = database(&output);
    assert_eq!(runtime_count(&db, 0), 0);
    assert_eq!(runtime_count(&db, 0x1800), 1);
    assert_eq!(source_body(&db, 0x1800), valid[24..]);
    assert_eq!(diagnostic(&output, "patch.esp", "skipped_records"), 2);
    let accepted: u32 = db
        .query_row(
            "SELECT count(*) FROM record_overrides WHERE plugin_id=?1 AND record_type='GMST' AND status='accepted'",
            [supplier_ordinal(&db, "Patch.esp")],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(accepted, 1);
}

/// A rejected native-ID alias cannot claim the duplicate key of a valid zero-header override.
#[test]
fn gmst_rejected_native_alias_does_not_claim_later_zero_setting() {
    let (_fixture, data) = test_data();
    plugin(
        &data,
        "Base.esm",
        &[],
        0,
        &[
            setting(0x1800, "fJumpHeightMin", 11.5),
            setting(0x1801, "fInAirFallingCharGravityMult", 22.5),
        ]
        .concat(),
    );
    let valid = setting(0, "fJumpHeightMin", 31.5);
    plugin(
        &data,
        "Patch.esp",
        &["Base.esm"],
        0,
        &[
            setting(0x1800, "fInAirFallingCharGravityMult", 99.5),
            valid.clone(),
            setting(0, "fJumpHeightMin", 42.5),
        ]
        .concat(),
    );
    let output = export(
        &data,
        &["Base.esm", "Patch.esp"],
        "rejected_native_alias",
        &[*b"GMST"],
    );
    let db = database(&output);
    assert_eq!(source_body(&db, 0x1800), valid[24..]);
    assert_eq!(
        source_body(&db, 0x1801),
        setting(0x1801, "fInAirFallingCharGravityMult", 22.5)[24..]
    );
    assert_eq!(runtime_count(&db, 0x1800), 1);
    assert_eq!(runtime_count(&db, 0x1801), 1);
    assert_eq!(diagnostic(&output, "patch.esp", "skipped_records"), 2);
    let statuses = db
        .prepare("SELECT status,reason FROM record_overrides WHERE plugin_id=?1 AND record_type='GMST' ORDER BY source_record_offset")
        .unwrap()
        .query_map([supplier_ordinal(&db, "Patch.esp")], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(statuses[0].0, "skipped");
    assert!(
        statuses[0]
            .1
            .as_ref()
            .unwrap()
            .contains("colliding identity")
    );
    assert_eq!(statuses[1], ("accepted".into(), None));
    assert_eq!(statuses[2].0, "skipped");
    assert!(
        statuses[2]
            .1
            .as_ref()
            .unwrap()
            .contains("duplicate/colliding")
    );
}

#[test]
fn actual_runtime_null_checks_do_not_invent_a_local_zero_rejection() {
    let (_fixture, data) = test_data();
    let body = sub(b"EDID", b"local_zero\0");
    let mut base = frame(b"STAT", 0, 0, &body);
    base.extend(frame(b"STAT", 0x0100_0000, 0, &body)); // Full slot0 required projection is runtime zero.
    plugin(&data, "Base.esm", &[], 0, &base);
    plugin(
        &data,
        "Patch.esp",
        &["Base.esm"],
        0,
        &frame(b"STAT", 0x0200_0000, 0, &body),
    );
    plugin(
        &data,
        "Light.esl",
        &["Base.esm"],
        0x200,
        &frame(b"STAT", 0x0200_0000, 0, &body),
    );
    let output = export(
        &data,
        &["Base.esm", "Patch.esp", "Light.esl"],
        "zero",
        &[*b"STAT"],
    );
    let db = database(&output);
    assert_eq!(runtime_count(&db, 0), 0);
    assert_eq!(source_body(&db, 0x0100_0000), body);
    assert_eq!(source_body(&db, 0xfe00_0000), body);
    assert_eq!(diagnostic(&output, "base.esm", "skipped_records"), 2);
    assert_eq!(diagnostic(&output, "base.esm", "compatibility_headers"), 0);
    // Resolved supplier ordinals and normal/light runtime slots are distinct namespaces.
    unknown_source(
        &output,
        supplier_ordinal(&db, "Patch.esp"),
        0x0200_0000,
        Some(0x0100_0000),
        "accepted",
        &body,
    );
    unknown_source(
        &output,
        supplier_ordinal(&db, "Light.esl"),
        0x0200_0000,
        Some(0xfe00_0000),
        "accepted",
        &body,
    );
}
