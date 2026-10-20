//! Source-authored full-audit regressions; these have not been executed.
use super::super::{addons::RuntimeSchemas, read_plugins_with_runtime_schemas};
use super::*;
use serde_json::json;
use std::{fs, path::PathBuf};

/// Frame synthetic native bytes independently of the schema decoder.
fn subrecord(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [
        tag.to_vec(),
        u16::try_from(bytes.len()).unwrap().to_le_bytes().to_vec(),
        bytes.to_vec(),
    ]
    .concat()
}

/// Keep asymmetric header facts visible alongside source and canonical payloads.
fn record(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    [
        tag.to_vec(),
        u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec(),
        flags.to_le_bytes().to_vec(),
        id.to_le_bytes().to_vec(),
        0x1357_2468u32.to_le_bytes().to_vec(),
        44u16.to_le_bytes().to_vec(),
        0xA571u16.to_le_bytes().to_vec(),
        payload.to_vec(),
    ]
    .concat()
}

/// Drive actual header reading, scanning, winner admission and target validation.
struct Fixture {
    directory: tempfile::TempDir,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Keep every synthetic input alive until the reader has released its maps.
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
            paths: Vec::new(),
        }
    }

    /// Write a framed TES4 header and explicit native master indices.
    fn add(&mut self, name: &str, masters: &[&str], flags: u32, records: &[u8]) {
        let mut body = subrecord(
            b"HEDR",
            &[
                1.7f32.to_le_bytes(),
                20u32.to_le_bytes(),
                0x800u32.to_le_bytes(),
            ]
            .concat(),
        );
        for master in masters {
            body.extend(subrecord(b"MAST", format!("{master}\0").as_bytes()));
            body.extend(subrecord(b"DATA", &[0xA5; 8]));
        }
        let mut bytes = record(b"TES4", 0, flags, &body);
        bytes.extend(records);
        let path = self.directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        self.paths.push(path);
    }

    /// Complete the same recovery and target passes as ordinary runtime-schema readers.
    fn read(&self, schemas: &RuntimeSchemas) -> ReadResult {
        let order = LoadOrder::read(&self.paths).unwrap();
        read_plugins_with_runtime_schemas(&self.paths, &order, schemas, |_| Ok(()), |_| {}).unwrap()
    }
}

/// Make admission consume the exact authored add-on envelope, including definitions.
fn addon(definitions: serde_json::Value, records: serde_json::Value) -> RuntimeSchemas {
    let bytes = serde_json::to_vec(&json!({
        "version":1,"name":"core-audit-witness","definitions":definitions,"records":records
    }))
    .unwrap();
    RuntimeSchemas::from_addon_bytes(&[&bytes]).unwrap()
}

/// Select a physical field by its authored binary tag, independent of its display label.
fn field<'a>(record: &'a DecodedRecord, tag: &[u8; 4]) -> &'a DecodedField {
    record
        .fields
        .iter()
        .find(|field| &field.signature == tag)
        .unwrap()
}

/// A nonzero singleton offset, a definition width and equal labels each select their own slot.
#[test]
fn accepted_addon_links_clear_exact_canonical_occurrences_and_preserve_source() {
    let schemas = addon(
        json!({"AuditTarget":{"kind":"form_id","targets":["STAT"]}}),
        json!([
            {"signature":"ZREC","fields":[
                {"signature":"ZARR","name":"offset_array","kind":"array","count":{"kind":"remaining","stride":8},
                 "members":[{"name":"link","kind":"form_id","offset":4,"targets":["STAT"]}]},
                {"signature":"ZDEF","name":"definition_struct","kind":"struct","size":8,"members":[
                    {"name":"link","definition":"AuditTarget","offset":0},
                    {"name":"padding","kind":"bytes","offset":4,"size":4}]},
                {"signature":"ZDUP","name":"duplicate_struct","kind":"struct","size":8,"members":[
                    {"name":"link","kind":"form_id","offset":0,"targets":["STAT"]},
                    {"name":"link","kind":"form_id","offset":4,"targets":["STAT"]}]},
                {"signature":"ZOPT","name":"omitted_duplicate","kind":"struct","size":4,"members":[
                    {"name":"link","kind":"form_id","offset":4,"optional":true,"targets":["STAT"]},
                    {"name":"link","kind":"form_id","offset":0,"targets":["STAT"]}]},
                {"signature":"ZVOK","name":"valid_self_link","kind":"form_id","targets":["STAT"]}
            ]}
        ]),
    );
    let missing = 0x0100_1999u32.to_le_bytes();
    let mut payload = subrecord(
        b"ZARR",
        &[vec![0xA1, 0xA2, 0xA3, 0xA4], missing.to_vec()].concat(),
    );
    payload.extend(subrecord(
        b"ZDEF",
        &[missing.to_vec(), vec![0xD1, 0xD2, 0xD3, 0xD4]].concat(),
    ));
    payload.extend(subrecord(
        b"ZDUP",
        &[0x1900u32.to_le_bytes(), missing].concat(),
    ));
    payload.extend(subrecord(b"ZOPT", &missing));
    payload.extend(subrecord(b"ZVOK", &0x0100_1910u32.to_le_bytes()));
    let mut fixture = Fixture::new();
    fixture.add(
        "Base.esm",
        &[],
        1,
        &record(b"STAT", 0x1900, 0, &subrecord(b"EDID", b"Target\0")),
    );
    fixture.add("Spacer.esm", &[], 1, &[]);
    let mut supplier = record(b"ZREC", 0x0100_1901, 0, &payload);
    supplier.extend(record(
        b"STAT",
        0x0100_1910,
        0,
        &subrecord(b"EDID", b"SelfTarget\0"),
    ));
    fixture.add("Supplier.esp", &["Base.esm"], 0, &supplier);
    let result = fixture.read(&schemas);
    let winner = &result.records[&0x0200_1901];
    assert_eq!(winner.raw_payload, payload);
    assert_eq!(winner.source_form_id, 0x0100_1901);
    assert_eq!(winner.load_order, 2);
    assert_eq!(winner.version_control, 0x1357_2468);
    assert_eq!(winner.header_unknown, 0xA571);
    let array = field(winner, b"ZARR");
    assert_eq!(
        array.value,
        Value::Array(vec![Value::Struct(vec![("link".into(), Value::FormId(0))])])
    );
    assert_eq!(array.canonical_bytes, [0xA1, 0xA2, 0xA3, 0xA4, 0, 0, 0, 0]);
    let definition = field(winner, b"ZDEF");
    assert_eq!(
        definition.value,
        Value::Struct(vec![
            ("link".into(), Value::FormId(0)),
            ("padding".into(), Value::Bytes(vec![0xD1, 0xD2, 0xD3, 0xD4]))
        ])
    );
    assert_eq!(
        definition.canonical_bytes,
        [0, 0, 0, 0, 0xD1, 0xD2, 0xD3, 0xD4]
    );
    let duplicate = field(winner, b"ZDUP");
    assert_eq!(
        duplicate.value,
        Value::Struct(vec![
            ("link".into(), Value::FormId(0x1900)),
            ("link".into(), Value::FormId(0))
        ])
    );
    assert_eq!(duplicate.canonical_bytes, [0x00, 0x19, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        field(winner, b"ZOPT").value,
        Value::Struct(vec![("link".into(), Value::FormId(0))])
    );
    assert_eq!(field(winner, b"ZOPT").canonical_bytes, [0; 4]);
    assert_eq!(field(winner, b"ZVOK").value, Value::FormId(0x0200_1910));
    assert_eq!(field(winner, b"ZVOK").canonical_bytes, [0x10, 0x19, 0, 2]);
    assert!(result.records.contains_key(&0x1900));
    assert_eq!(result.diagnostics["supplier.esp"].invalid_links, 4);
    assert_eq!(result.diagnostics["supplier.esp"].skipped_fields, 0);
    assert_eq!(result.diagnostics["supplier.esp"].skipped_records, 0);
}

/// Admission rejects raw overlays of link bytes, including definitions and nested links.
#[test]
fn overlapping_link_spans_are_rejected_but_nonlink_overlays_remain_valid() {
    for link in [
        json!({"name":"link","kind":"form_id","offset":0,"targets":["STAT"]}),
        json!({"name":"link","definition":"AuditTarget","offset":0}),
        json!({"name":"nested","kind":"struct","offset":0,"size":4,
            "members":[{"name":"link","kind":"form_id","targets":["STAT"]}]}),
    ] {
        let bytes = serde_json::to_vec(&json!({"version":1,"name":"overlap-audit",
            "definitions":{"AuditTarget":{"kind":"form_id","targets":["STAT"]}},
            "records":[{"signature":"ZREC","fields":[{"signature":"ZOVR","name":"overlapping_link",
                "kind":"struct","size":4,"members":[link,{"name":"raw","kind":"bytes","offset":0,"size":4}]}]}]
        })).unwrap();
        let error = RuntimeSchemas::from_addon_bytes(&[&bytes]).unwrap_err();
        assert!(error.contains("overlapping link member spans"), "{error}");
    }
    let schemas = addon(
        json!({}),
        json!([{"signature":"ZREC","fields":[
        {"signature":"ZOVR","name":"nonlink_overlay","kind":"struct","size":4,"members":[
            {"name":"number","kind":"u32","offset":0},
            {"name":"raw","kind":"bytes","offset":0,"size":4}]}]}]),
    );
    let mut fixture = Fixture::new();
    fixture.add(
        "Overlay.esp",
        &[],
        0,
        &record(
            b"ZREC",
            0x1901,
            0,
            &subrecord(b"ZOVR", &[0xA1, 0xB2, 0xC3, 0xD4]),
        ),
    );
    let result = fixture.read(&schemas);
    let overlay = field(&result.records[&0x1901], b"ZOVR");
    assert_eq!(overlay.canonical_bytes, [0xA1, 0xB2, 0xC3, 0xD4]);
    assert_eq!(
        overlay.value,
        Value::Struct(vec![
            ("number".into(), Value::Unsigned(0xD4C3_B2A1)),
            ("raw".into(), Value::Bytes(vec![0xA1, 0xB2, 0xC3, 0xD4]))
        ])
    );
}

/// A variable tail's overlap is checked against actual bytes and remains a local field error.
#[test]
fn payload_dependent_link_overlap_keeps_record_and_neighbor_fields() {
    let schemas = addon(
        json!({}),
        json!([{"signature":"ZREC","fields":[
        {"signature":"ZOVR","name":"variable_overlap","kind":"struct","members":[
            {"name":"raw","kind":"bytes","offset":0},
            {"name":"link","kind":"form_id","offset":0,"targets":["STAT"]}]},
        {"signature":"SAFE","name":"number","kind":"u32"}]}]),
    );
    let mut payload = subrecord(b"ZOVR", &0x1900u32.to_le_bytes());
    payload.extend(subrecord(b"SAFE", &0xA1B2_C3D4u32.to_le_bytes()));
    let mut fixture = Fixture::new();
    fixture.add(
        "Variable.esp",
        &[],
        0,
        &record(b"ZREC", 0x1901, 0, &payload),
    );
    let result = fixture.read(&schemas);
    let winner = &result.records[&0x1901];
    assert_eq!(winner.raw_payload, payload);
    assert_eq!(winner.rejected_fields, [*b"ZOVR"]);
    assert_eq!(winner.fields.len(), 1);
    assert_eq!(field(winner, b"SAFE").value, Value::Unsigned(0xA1B2_C3D4));
    assert_eq!(result.diagnostics["variable.esp"].skipped_fields, 1);
    assert_eq!(result.diagnostics["variable.esp"].skipped_records, 0);
}

/// Complete EditorID aliases retain their winner when the next header has a torn tail.
#[test]
fn torn_gmst_alias_preserves_complete_winner_and_does_not_publish_rejected_alias() {
    let schemas = RuntimeSchemas::from_addon_bytes(&[]).unwrap();
    let mut complete = subrecord(b"EDID", b"iAuditValue\0");
    complete.extend(subrecord(b"DATA", &17i32.to_le_bytes()));
    let mut torn = subrecord(b"EDID", b"IAUDITVALUE\0");
    torn.extend(b"DATA\x04\x00\x22");
    let mut bytes = record(b"GMST", 0x1800, 0, &complete);
    bytes.extend(record(b"GMST", 0x1801, 0, &torn));
    bytes.extend(record(
        b"STAT",
        0x1801,
        0,
        &subrecord(b"EDID", b"IndependentNeighbor\0"),
    ));
    let mut fixture = Fixture::new();
    fixture.add("Settings.esp", &[], 0, &bytes);
    let result = fixture.read(&schemas);
    let winner = &result.records[&0x1800];
    assert_eq!(winner.source_form_id, 0x1800);
    assert_eq!(winner.raw_payload, complete);
    assert!(winner.payload_complete);
    assert_eq!(field(winner, b"DATA").value, Value::Signed(17));
    assert_eq!(field(winner, b"DATA").canonical_bytes, 17i32.to_le_bytes());
    assert_eq!(result.records[&0x1801].record_type, *b"STAT");
    assert_eq!(result.history.len(), 4); // TES4 plus all three physical record headers.
    let rejected = result
        .history
        .iter()
        .find(|entry| entry.record_type == *b"GMST" && entry.source_form_id == 0x1801)
        .unwrap();
    assert!(!rejected.accepted);
    assert!(
        rejected
            .reason
            .as_deref()
            .unwrap()
            .contains("incomplete override")
    );
    assert!(!result.overrides.contains_key(&0x1800));
    assert_eq!(result.diagnostics["settings.esp"].skipped_records, 1);
}

/// Definitions chained through groups retain their ordered repeated leaves and end markers.
#[test]
fn chained_group_definitions_decode_every_occurrence_and_terminated_boundary() {
    let definitions = json!({
        "AuditAlias":{"definition":"AuditGroup"},
        "AuditGroup":{"kind":"group","repeat":true,"repeat_terminated":true,"fields":[
            {"signature":"ZVAL","name":"number","kind":"u32"},
            {"signature":"ZEND","name":"end","kind":"bytes","size":0}]}
    });
    let schemas = addon(
        definitions.clone(),
        json!([{"signature":"ZREC","fields":[{"definition":"AuditAlias"}]}]),
    );
    let prepared = prepare_runtime_schema(&schemas);
    assert_eq!(prepared[b"ZREC"].entries.len(), 2);
    assert!(prepared[b"ZREC"].entries[0].repeat_ranges[0].terminated);
    let mut payload = subrecord(b"ZVAL", &0xA1B2_C3D4u32.to_le_bytes());
    payload.extend(subrecord(b"ZEND", &[]));
    payload.extend(subrecord(b"ZVAL", &0x1928_3746u32.to_le_bytes()));
    payload.extend(subrecord(b"ZEND", &[]));
    let mut fixture = Fixture::new();
    fixture.add("Groups.esp", &[], 0, &record(b"ZREC", 0x1901, 0, &payload));
    let result = fixture.read(&schemas);
    let winner = &result.records[&0x1901];
    assert_eq!(winner.raw_payload, payload);
    assert_eq!(
        winner
            .fields
            .iter()
            .map(|field| field.value.clone())
            .collect::<Vec<_>>(),
        [
            Value::Unsigned(0xA1B2_C3D4),
            Value::Bytes(vec![]),
            Value::Unsigned(0x1928_3746),
            Value::Bytes(vec![])
        ]
    );
    assert_eq!(result.diagnostics["groups.esp"].unexpected_subrecords, 0);
    assert_eq!(result.diagnostics["groups.esp"].skipped_fields, 0);
    let invalid = serde_json::to_vec(&json!({"version":1,"definitions":definitions,
        "records":[{"signature":"ZREC","allow_unordered":true,"fields":[{"definition":"AuditAlias"}]}]})).unwrap();
    assert!(
        Schema::parse(&invalid)
            .unwrap_err()
            .contains("terminated repeats require ordered matching")
    );
    let cycle = Schema::parse(br#"{"version":1,"definitions":{"A":{"definition":"B"},"B":{"definition":"A"}},"records":[]}"#).unwrap_err();
    assert!(cycle.contains("32 levels"));
}

/// The selected leaf overrides a parent bank, while an omitted leaf bank inherits it.
#[test]
fn localized_bank_metadata_follows_selected_union_and_definition_leaf_without_json_changes() {
    let schemas = addon(
        json!({
            "AuditLeaf":{"kind":"lstring","size":4,"string_table":"ilstrings"},
            "AuditLeafAlias":{"definition":"AuditLeaf"}
        }),
        json!([{"signature":"ZREC","fields":[
        {"signature":"ZLFA","name":"leaf_bank","kind":"union","decider":"size","string_table":"strings",
         "fields":[{"kind":"lstring","size":4,"string_table":"dlstrings"}]},
        {"signature":"ZPAR","name":"parent_bank","kind":"union","decider":"size","string_table":"dlstrings",
         "fields":[{"kind":"lstring","size":4}]},
        {"signature":"ZDEF","name":"definition_bank","kind":"union","decider":"size","string_table":"dlstrings",
         "fields":[{"definition":"AuditLeafAlias"}]},
        {"signature":"ZLOC","name":"localized_definition_bank","kind":"union","decider":"localized_string",
         "fields":[{"definition":"AuditLeafAlias"}]},
        {"signature":"ZSTD","name":"default_bank","kind":"lstring"},
        {"signature":"ZRAW","name":"raw_number","kind":"u32"}]}]),
    );
    let mut payload = Vec::new();
    for tag in [b"ZLFA", b"ZPAR", b"ZDEF", b"ZLOC", b"ZSTD", b"ZRAW"] {
        payload.extend(subrecord(tag, &0x1928_3746u32.to_le_bytes()));
    }
    let mut fixture = Fixture::new();
    fixture.add(
        "Localized.esp",
        &[],
        0x80,
        &record(b"ZREC", 0x1901, 0, &payload),
    );
    let result = fixture.read(&schemas);
    let winner = &result.records[&0x1901];
    assert_eq!(winner.raw_payload, payload);
    for (tag, bank) in [
        (b"ZLFA", "dlstrings"),
        (b"ZPAR", "dlstrings"),
        (b"ZDEF", "ilstrings"),
        (b"ZLOC", "ilstrings"),
        (b"ZSTD", "strings"),
    ] {
        let item = field(winner, tag);
        assert_eq!(item.localized_bank.as_deref(), Some(bank));
        assert_eq!(item.value, Value::LocalizedString(0x1928_3746));
        assert_eq!(item.canonical_bytes, 0x1928_3746u32.to_le_bytes());
        assert!(
            serde_json::to_value(item)
                .unwrap()
                .get("localized_bank")
                .is_none()
        );
    }
    assert_eq!(field(winner, b"ZRAW").localized_bank, None);
    assert_eq!(field(winner, b"ZRAW").value, Value::Unsigned(0x1928_3746));
    assert_eq!(result.diagnostics["localized.esp"].skipped_fields, 0);
}
