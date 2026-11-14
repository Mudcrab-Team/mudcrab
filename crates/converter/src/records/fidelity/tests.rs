use super::*;
use crate::esm::types::ArchivedSubRecord;

fn schema() -> Schema {
    Schema::parse(super::super::SCHEMA_BYTES).unwrap()
}

#[test]
fn every_declared_field_has_a_review_denominator() {
    let expected: BTreeMap<String, BaselineEntry> =
        serde_json::from_str(include_str!("schema-baseline.json")).unwrap();
    let actual = baseline(&schema()).unwrap();
    let changed: Vec<_> = actual
        .keys()
        .chain(expected.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|key| actual.get(*key) != expected.get(*key))
        .collect();
    assert!(
        changed.is_empty(),
        "unaccounted schema changes: {changed:?}; review each field and regenerate with record-fidelity baseline; this is not semantic acceptance"
    );
}

#[test]
fn denominator_detects_new_removed_nested_and_reinterpreted_fields() {
    let original = schema();
    let before = baseline(&original).unwrap();
    let mut changed = original.clone();
    changed.records[0].fields.push(FieldSchema {
        signature: Some("ZNEW".into()),
        name: "new_source_field".into(),
        kind: "u32".into(),
        ..Default::default()
    });
    changed.validate().unwrap();
    assert_ne!(baseline(&changed).unwrap(), before);
    let mut changed = original.clone();
    changed.records[0].fields.remove(0);
    assert_ne!(baseline(&changed).unwrap(), before);
    let mut changed = original.clone();
    changed.definitions.get_mut("transform").unwrap().members[0].offset += 1;
    assert_ne!(baseline(&changed).unwrap(), before);
    let mut changed = original;
    changed.records[0].allow_unordered = !changed.records[0].allow_unordered;
    assert_ne!(baseline(&changed).unwrap(), before);
}

#[test]
fn schema_placed_types_cannot_silently_fall_out_of_publication() {
    let schema = schema();
    let placed: BTreeSet<_> = schema
        .records
        .iter()
        .filter(|record| {
            record.fields.iter().any(|field| {
                field.signature.as_deref() == Some("DATA")
                    && field.definition.as_deref() == Some("transform")
            })
        })
        .map(|record| record.signature.as_bytes().try_into().unwrap())
        .collect();
    assert_eq!(
        placed.len(),
        11,
        "review a new placement layout and its runtime projection"
    );
    for tag in placed {
        assert!(
            crate::esm::is_placed_record_type(&tag),
            "schema-declared placement {} is not published",
            String::from_utf8_lossy(&tag)
        );
    }
}

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

#[test]
fn source_framing_keeps_duplicate_zero_extended_and_malformed_occurrences() {
    let mut bytes = [sub(b"ZERO", &[]), sub(b"SAME", &[0]), sub(b"SAME", &[1])].concat();
    bytes.extend(sub(b"XXXX", &3u32.to_le_bytes()));
    bytes.extend(b"LONG\0\0abc");
    let tail_offset = bytes.len();
    bytes.extend(b"BAD\xFF\x08\0xx");
    let (fields, tail) = frame_source(&bytes);
    assert_eq!(tail, Some(tail_offset));
    assert_eq!(fields.len(), 4);
    assert!(fields[0].bytes.is_empty());
    assert_eq!(fields[1].bytes, &[0]);
    assert_eq!(fields[2].bytes, &[1]);
    assert_eq!(fields[3].tag, *b"LONG");
    assert_eq!(fields[3].bytes, b"abc");
    for invalid in [
        b"XXXX\x04\0\x01\0\0\0".as_slice(),
        b"XXXX\x04\0\0\0\0\0XXXX\0\0",
        b"JUNK\xFF\xFFx",
    ] {
        assert_eq!(frame_source(invalid).1, Some(0));
    }
}

fn database() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE inhouse_source_records(form_id INTEGER PRIMARY KEY,record_type TEXT,payload BLOB); CREATE TABLE records(form_id INTEGER PRIMARY KEY,record_type TEXT,data BLOB);").unwrap();
    conn
}

fn source(conn: &Connection, id: u32, payload: &[u8]) {
    conn.execute(
        "INSERT INTO inhouse_source_records VALUES(?1,'KYWD',?2)",
        rusqlite::params![id, payload],
    )
    .unwrap();
}

fn canonical(conn: &Connection, id: u32, fields: &[([u8; 4], &[u8])]) {
    let record = ArchivedRecordData {
        subrecords: fields
            .iter()
            .map(|(tag, data)| ArchivedSubRecord {
                tag: *tag,
                data: data.to_vec(),
            })
            .collect(),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&record).unwrap();
    conn.execute(
        "INSERT INTO records VALUES(?1,'KYWD',?2)",
        rusqlite::params![id, bytes.as_slice()],
    )
    .unwrap();
}

#[test]
fn coverage_distinguishes_empty_matched_source_only_and_missing_source() {
    let conn = database();
    assert_eq!(
        audit_storage(&conn, &schema()).unwrap().coverage().status,
        "empty"
    );
    source(&conn, 1, &[]);
    let audit = audit_storage(&conn, &schema()).unwrap();
    assert_eq!(
        audit.coverage(),
        StorageCoverage {
            status: "source_only_records",
            matched_records: 0,
            source_only_records: 1,
            canonical_only_records: 0,
        }
    );
    canonical(&conn, 1, &[]);
    assert_eq!(
        audit_storage(&conn, &schema()).unwrap().coverage(),
        StorageCoverage {
            status: "matched_input_rows",
            matched_records: 1,
            source_only_records: 0,
            canonical_only_records: 0,
        }
    );
    canonical(&conn, 2, &[]);
    assert_eq!(
        audit_storage(&conn, &schema()).unwrap().coverage(),
        StorageCoverage {
            status: "incomplete_source_evidence",
            matched_records: 1,
            source_only_records: 0,
            canonical_only_records: 1,
        }
    );
}

#[test]
fn dropped_publication_is_counted_and_other_records_cannot_cancel_it() {
    let conn = database();
    source(&conn, 1, &sub(b"EDID", b"First\0"));
    canonical(&conn, 1, &[(*b"EDID", b"First\0")]);
    source(&conn, 2, &sub(b"EDID", b"Dropped\0"));
    canonical(&conn, 2, &[]);
    source(&conn, 3, &[]);
    canonical(&conn, 3, &[(*b"EDID", b"Extra\0")]);
    let audit = audit_storage(&conn, &schema()).unwrap();
    let edid = &audit.occurrences["4B595744/45444944"];
    assert_eq!(edid.source_occurrences, 2);
    assert_eq!(edid.canonical_occurrences, 2);
    assert_eq!(edid.canonical_occurrence_deficit, 1);
    assert_eq!(edid.exact_byte_matches, 1);
    assert_eq!(audit.canonical_sequence_equal_records, 1);
    assert_eq!(audit.canonical_sequence_changed_records, 2);
    assert!(edid.declared_signature);
}

#[test]
fn unknowns_missing_zero_malformed_and_reordered_duplicates_stay_distinct() {
    let conn = database();
    let mut payload = [
        sub(b"ZERO", &[]),
        sub(b"EDID", b"One\0"),
        sub(b"EDID", b"Two\0"),
    ]
    .concat();
    let offset = payload.len();
    payload.extend(b"broken");
    source(&conn, 1, &payload);
    canonical(
        &conn,
        1,
        &[(*b"ZERO", b""), (*b"EDID", b"Two\0"), (*b"EDID", b"One\0")],
    );
    source(&conn, 2, &[]);
    canonical(&conn, 3, &[]);
    let audit = audit_storage(&conn, &schema()).unwrap();
    let zero = &audit.occurrences["4B595744/5A45524F"];
    assert_eq!((zero.source_occurrences, zero.source_bytes), (1, 0));
    assert_eq!(zero.exact_byte_matches, 1);
    assert!(!zero.declared_signature);
    assert_eq!(audit.source_records_without_canonical, 1);
    assert_eq!(audit.canonical_records_without_source, 1);
    assert_eq!(audit.malformed_payloads, 1);
    assert_eq!(audit.unparsed_bytes, 6);
    assert_eq!(audit.malformed_samples[0].payload_offset, offset);
    assert_eq!(audit.malformed_samples[0].sha256, hash_bytes(b"broken"));
    assert_eq!(audit.canonical_sequence_changed_records, 1);
    assert_eq!(audit.occurrences["4B595744/45444944"].exact_byte_matches, 2);
}

#[test]
fn independent_extension_declarations_and_storage_do_not_change_builtin_order() {
    let bytes = br#"{"version":1,"name":"audit-extension","records":[{"signature":"KYWD","extends":true,"source":"synthetic-test","fields":[{"signature":"XVAL","name":"mod_value","kind":"u32","repeat":true}]}]}"#;
    let schemas = crate::records::addons::RuntimeSchemas::from_addon_bytes(&[bytes]).unwrap();
    let conn = database();
    let zero = 0u32.to_le_bytes();
    let changed = 13u32.to_le_bytes();
    source(
        &conn,
        1,
        &[
            sub(b"XVAL", &zero),
            sub(b"EDID", b"Control\0"),
            sub(b"XVAL", &changed),
        ]
        .concat(),
    );
    canonical(
        &conn,
        1,
        &[
            (*b"XVAL", &zero),
            (*b"EDID", b"Control\0"),
            (*b"XVAL", &changed),
        ],
    );
    let audit = audit_runtime_storage(&conn, &schemas).unwrap();
    let values = &audit.occurrences["4B595744/5856414C"];
    assert!(values.declared_signature);
    assert_eq!(values.source_occurrences, 2);
    assert_eq!(values.exact_byte_matches, 2);
    assert_eq!(audit.canonical_sequence_equal_records, 1);
    let fields = addon_declarations(&schema(), &schemas).unwrap();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].id, "extension/KYWD/0:XVAL:mod_value");
    assert_eq!(fields[0].source, "synthetic-test");
    assert_eq!(fields[0].semantic_support, "unverified");
    assert!(
        !declarations(schemas.schema())
            .unwrap()
            .iter()
            .any(|field| field.id.ends_with(":mod_value"))
    );
}
