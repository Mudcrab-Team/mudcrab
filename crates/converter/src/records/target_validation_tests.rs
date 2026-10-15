//! Synthetic tests for insertion as a child module of records::decoder.
//! Preparation only: root owns compilation and complete-export integration tests.
use super::*;

/// Build a checked empty schema for direct recursive validator probes.
fn empty_schema() -> Schema {
    Schema::parse(br#"{"version":1,"records":[]}"#).expect("synthetic empty schema")
}

/// Keep selector lookups independent from optional-link target declarations.
fn selection<'a>(kinds: &'a dyn Fn(u32) -> Option<[u8; 4]>) -> super::super::deciders::Context<'a> {
    super::super::deciders::Context {
        record_type: *b"TEST",
        form_version: 44,
        editor_id: "",
        preceding_subrecords: &[],
        parent_payload: None,
        link_record_type: Some(kinds),
    }
}

/// Supply native header/provenance fields independently of the value under test.
fn record(id: u32, signature: [u8; 4], fields: Vec<DecodedField>) -> DecodedRecord {
    DecodedRecord {
        form_id: id,
        source_form_id: 0x0200_1800,
        record_type: signature,
        flags: 0x20,
        version_control: 0x1234_5678,
        form_version: 44,
        header_unknown: 0x4321,
        load_order: 0,
        cell_form_id: None,
        worldspace_form_id: None,
        topic_form_id: None,
        source_topic_form_id: None,
        topic_group_offset: None,
        source_record_offset: 0x1_0000_0041,
        fields,
        deferred_fields: Vec::new(),
        rejected_fields: vec![*b"BAD_"],
        payload_complete: false,
        raw_payload: vec![0x51, 0x19, 0x80, 0x07],
        supported: true,
    }
}

/// Compare all source/header/provenance fields independently from runtime mutations.
fn preserved(before: &DecodedRecord, after: &DecodedRecord) {
    assert_eq!(before.form_id, after.form_id);
    assert_eq!(before.source_form_id, after.source_form_id);
    assert_eq!(before.record_type, after.record_type);
    assert_eq!(before.flags, after.flags);
    assert_eq!(before.version_control, after.version_control);
    assert_eq!(before.form_version, after.form_version);
    assert_eq!(before.header_unknown, after.header_unknown);
    assert_eq!(before.load_order, after.load_order);
    assert_eq!(before.cell_form_id, after.cell_form_id);
    assert_eq!(before.worldspace_form_id, after.worldspace_form_id);
    assert_eq!(before.source_topic_form_id, after.source_topic_form_id);
    assert_eq!(before.topic_group_offset, after.topic_group_offset);
    assert_eq!(before.source_record_offset, after.source_record_offset);
    assert_eq!(before.rejected_fields, after.rejected_fields);
    assert_eq!(before.payload_complete, after.payload_complete);
    assert_eq!(before.raw_payload, after.raw_payload);
    assert_eq!(before.supported, after.supported);
}

/// Nonlink integer/raw words must never be treated as links by a fast-path predicate.
#[test]
fn nonlink_leaves_and_nested_zero_keep_exact_bits_and_bytes() {
    let schema = empty_schema();
    let kinds = HashMap::from([(0x1800, *b"STAT")]);
    let lookup = |id| kinds.get(&id).copied();
    let context = selection(&lookup);
    let field = FieldSchema {
        kind: "bytes".into(),
        ..Default::default()
    };
    let mut diagnostics = PluginDiagnostics::default();
    let values = vec![
        Value::Unsigned(u64::MAX),
        Value::Signed(i64::MIN),
        Value::Float(-0.0),
        Value::Float(f32::from_bits(1)),
        Value::String("\0λ".into()),
        Value::LocalizedString(0x1800),
        Value::Bytes(0x1800_u32.to_le_bytes().to_vec()),
        Value::Deferred(0x1800_u32.to_le_bytes().to_vec()),
        Value::FormId(0),
        Value::Struct(vec![("zero".into(), Value::Array(vec![Value::FormId(0)]))]),
    ];
    for mut value in values {
        let expected = value.clone();
        let mut bytes = vec![0x19, 0x80, 0x51, 0x07];
        validate_value(
            &mut value,
            &mut bytes,
            &field,
            &schema,
            &kinds,
            &mut diagnostics,
            &context,
        );
        assert_eq!(value, expected);
        if let Value::Float(actual) = value {
            let Value::Float(original) = expected else {
                panic!("float fixture shape")
            };
            assert_eq!(actual.to_bits(), original.to_bits());
        }
        assert_eq!(bytes, [0x19, 0x80, 0x51, 0x07]);
    }
    assert_eq!(diagnostics.invalid_links, 0);
}

/// Multiple known fields must not slice an omitted snapshot at a nonzero position.
#[test]
fn link_free_record_with_multiple_known_fields_keeps_all_values_and_bytes() {
    let schema = super::super::schema();
    let mut order = LoadOrder::read(&[]).expect("empty order");
    order.names.push("synthetic.esp".into());
    let item = record(
        0x1800,
        *b"ACTI",
        vec![
            DecodedField {
                opaque: false,
                localized_bank: None,
                signature: *b"EDID",
                name: "editor_id".into(),
                value: Value::String("Independent\0λ".into()),
                canonical_bytes: b"Independent\0".to_vec(),
            },
            DecodedField {
                opaque: false,
                localized_bank: None,
                signature: *b"FULL",
                name: "name".into(),
                value: Value::LocalizedString(0x1809),
                canonical_bytes: 0x1809_u32.to_le_bytes().to_vec(),
            },
        ],
    );
    assert!(
        prepared()
            .get(b"ACTI")
            .expect("prepared ACTI")
            .entries
            .iter()
            .any(|entry| entry.field.signature.as_deref() == Some("FULL")
                && entry.field.name == "name")
    );
    let mut result = ReadResult::default();
    result.records.insert(0x1800, item.clone());
    validate_targets(&mut result, schema, &order);
    let after = &result.records[&0x1800];
    preserved(&item, after);
    assert_eq!(after.fields.len(), 2);
    for (original, current) in item.fields.iter().zip(&after.fields) {
        assert_eq!(original.signature, current.signature);
        assert_eq!(original.name, current.name);
        assert_eq!(original.value, current.value);
        assert_eq!(original.canonical_bytes, current.canonical_bytes);
    }
    assert!(result.diagnostics.is_empty());
}

/// Existing wrong-kind low IDs differ from absent reserved IDs and valid siblings.
#[test]
fn missing_wrong_kind_and_reserved_links_have_distinct_exact_outcomes() {
    let schema = empty_schema();
    let kinds = HashMap::from([(0x61, *b"STAT"), (0x19, *b"RACE"), (0x1802, *b"RACE")]);
    let lookup = |id| kinds.get(&id).copied();
    let context = selection(&lookup);
    let field = FieldSchema {
        name: "race".into(),
        kind: "form_id".into(),
        targets: vec!["RACE".into()],
        ..Default::default()
    };
    let mut diagnostics = PluginDiagnostics::default();
    for (source, expected) in [
        (0, 0),
        (0x61, 0),
        (0x19, 0x19),
        (0x63, 0x63),
        (0x1809, 0),
        (0x1802, 0x1802),
    ] {
        let mut value = Value::FormId(source);
        let mut bytes = source.to_le_bytes();
        validate_value(
            &mut value,
            &mut bytes,
            &field,
            &schema,
            &kinds,
            &mut diagnostics,
            &context,
        );
        assert_eq!(value, Value::FormId(expected));
        assert_eq!(bytes, expected.to_le_bytes());
    }
    assert_eq!(diagnostics.invalid_links, 2);
    assert!(diagnostics.first_by_category["link"].contains("00000061"));
}

/// Named definitions retain inherited target types and the occurrence's diagnostic name.
#[test]
fn definition_resolution_preserves_inherited_targets_and_occurrence_override() {
    let mut schema = empty_schema();
    schema.definitions.insert(
        "Target".into(),
        FieldSchema {
            name: "definition_base".into(),
            kind: "form_id".into(),
            targets: vec!["RACE".into()],
            size: Some(4),
            ..Default::default()
        },
    );
    let field = FieldSchema {
        definition: Some("Target".into()),
        name: "occurrence_override".into(),
        signature: Some("RNAM".into()),
        offset: 7,
        optional: true,
        repeat: true,
        string_table: Some("dlstrings".into()),
        ..Default::default()
    };
    let kinds = HashMap::from([(0x1802, *b"RACE")]);
    let lookup = |id| kinds.get(&id).copied();
    let context = selection(&lookup);
    let mut diagnostics = PluginDiagnostics::default();
    let mut valid = Value::FormId(0x1802);
    let mut valid_bytes = 0x1802_u32.to_le_bytes();
    validate_value(
        &mut valid,
        &mut valid_bytes,
        &field,
        &schema,
        &kinds,
        &mut diagnostics,
        &context,
    );
    assert_eq!(valid, Value::FormId(0x1802));
    assert_eq!(valid_bytes, 0x1802_u32.to_le_bytes());
    let mut missing = Value::FormId(0x1809);
    let mut missing_bytes = 0x1809_u32.to_le_bytes();
    validate_value(
        &mut missing,
        &mut missing_bytes,
        &field,
        &schema,
        &kinds,
        &mut diagnostics,
        &context,
    );
    assert_eq!(missing, Value::FormId(0));
    assert_eq!(missing_bytes, [0; 4]);
    assert_eq!(diagnostics.invalid_links, 1);
    assert!(diagnostics.first_by_category["link"].starts_with("occurrence_override link"));
}

/// A sibling nulling must not alter the native parent used to choose a later union.
#[test]
fn inventory_selector_uses_original_parent_after_owner_sibling_is_nulled() {
    let schema = empty_schema();
    let kinds = HashMap::from([(0x1800, *b"NPC_"), (0x1801, *b"KYWD")]);
    let lookup = |id| kinds.get(&id).copied();
    let context = selection(&lookup);
    // Deliberately asymmetric supported schema: owner rejects NPC_, selector still
    // sees original NPC_ and picks the form_id condition rather than opaque bytes.
    let field = FieldSchema {
        kind: "struct".into(),
        size: Some(12),
        members: vec![
            FieldSchema {
                name: "owner".into(),
                kind: "form_id".into(),
                offset: 0,
                targets: vec!["FACT".into()],
                ..Default::default()
            },
            FieldSchema {
                name: "condition".into(),
                kind: "union".into(),
                size: Some(4),
                offset: 4,
                decider: Some("inventory_owner_condition".into()),
                fields: vec![
                    FieldSchema {
                        kind: "form_id".into(),
                        size: Some(4),
                        targets: vec!["KYWD".into()],
                        ..Default::default()
                    },
                    FieldSchema {
                        kind: "i32".into(),
                        size: Some(4),
                        ..Default::default()
                    },
                    FieldSchema {
                        kind: "bytes".into(),
                        size: Some(4),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            FieldSchema {
                name: "sentinel".into(),
                kind: "u32".into(),
                offset: 8,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let mut value = Value::Struct(vec![
        ("owner".into(), Value::FormId(0x1800)),
        ("condition".into(), Value::FormId(0x1801)),
        ("sentinel".into(), Value::Unsigned(0xa951_0719)),
    ]);
    let mut bytes = [
        0x1800_u32.to_le_bytes(),
        0x1801_u32.to_le_bytes(),
        0xa951_0719_u32.to_le_bytes(),
    ]
    .concat();
    validate_value(
        &mut value,
        &mut bytes,
        &field,
        &schema,
        &kinds,
        &mut PluginDiagnostics::default(),
        &context,
    );
    assert_eq!(
        value,
        Value::Struct(vec![
            ("owner".into(), Value::FormId(0)),
            ("condition".into(), Value::FormId(0x1801)),
            ("sentinel".into(), Value::Unsigned(0xa951_0719))
        ])
    );
    assert_eq!(
        bytes,
        [
            0_u32.to_le_bytes(),
            0x1801_u32.to_le_bytes(),
            0xa951_0719_u32.to_le_bytes()
        ]
        .concat()
    );
    // A deliberately mutated-parent control must choose the bytes branch and
    // therefore fail the independent retained-condition assertion above.
    let mutated_parent = [
        0_u32.to_le_bytes(),
        0x1801_u32.to_le_bytes(),
        0xa951_0719_u32.to_le_bytes(),
    ]
    .concat();
    let mutant_context = super::super::deciders::Context {
        parent_payload: Some(&mutated_parent),
        ..context
    };
    let mut mutant_value = Value::FormId(0x1801);
    let mut mutant_bytes = 0x1801_u32.to_le_bytes();
    let mut mutant_diagnostics = PluginDiagnostics::default();
    validate_value(
        &mut mutant_value,
        &mut mutant_bytes,
        &field.members[1],
        &schema,
        &kinds,
        &mut mutant_diagnostics,
        &mutant_context,
    );
    assert_eq!(mutant_value, Value::FormId(0));
    assert_eq!(mutant_diagnostics.invalid_links, 1);
}

/// Independently frame a single format2 object and preserve signed alias/padding.
fn vmad_object(id: u32) -> Vec<u8> {
    let mut bytes = [
        5_i16.to_le_bytes(),
        2_i16.to_le_bytes(),
        1_u16.to_le_bytes(),
    ]
    .concat();
    bytes.extend(11_u16.to_le_bytes());
    bytes.extend(b"Independent");
    bytes.push(0);
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(6_u16.to_le_bytes());
    bytes.extend(b"Object");
    bytes.extend([1, 1, 0xa1, 0xb2, 0x56, 0x34]);
    bytes.extend(id.to_le_bytes());
    bytes
}

/// The special VMAD re-decode must run even when the generic value appears link-free.
#[test]
fn vmad_special_branch_still_validates_canonical_object_and_preserves_source() {
    let schema = super::super::schema();
    let mut order = LoadOrder::read(&[]).expect("empty order");
    order.names.push("synthetic.esp".into());
    let mut result = ReadResult::default();
    // Deliberately inconsistent typed value zero vs canonical dangling ID makes
    // omission of the special canonical re-decode observable, independent of the guard.
    let (zero_value, _) =
        super::super::vmad::decode(&vmad_object(0), b"ACTI", Ok).expect("framed object");
    let original_canonical = vmad_object(0x1809);
    assert_eq!(original_canonical.len(), 40);
    let item = record(
        0x1800,
        *b"ACTI",
        vec![DecodedField {
            opaque: false,
            localized_bank: None,
            signature: *b"VMAD",
            name: "scripts".into(),
            value: zero_value,
            canonical_bytes: original_canonical.clone(),
        }],
    );
    result.records.insert(0x1800, item.clone());
    validate_targets(&mut result, schema, &order);
    let after = &result.records[&0x1800];
    preserved(&item, after);
    assert_eq!(
        &after.fields[0].canonical_bytes[..36],
        &original_canonical[..36]
    );
    assert_eq!(&after.fields[0].canonical_bytes[36..], &[0; 4]);
    assert_eq!(result.diagnostics["synthetic.esp"].invalid_links, 1);
}

/// INFO owner validation remains independent from all generic field-link work.
#[test]
fn link_free_info_still_checks_live_dial_wrong_kind_missing_and_null_owners() {
    let schema = super::super::schema();
    let mut order = LoadOrder::read(&[]).expect("empty order");
    order.names.push("synthetic.esp".into());
    let mut result = ReadResult::default();
    result
        .records
        .insert(0x1900, record(0x1900, *b"DIAL", vec![]));
    result
        .records
        .insert(0x1901, record(0x1901, *b"STAT", vec![]));
    let mut before = Vec::new();
    for (id, owner, raw) in [
        (0x1810, Some(0x1900), Some(0x0200_1900)),
        (0x1811, Some(0x1901), Some(0x0200_1901)),
        (0x1812, Some(0x1902), Some(0x0200_1902)),
        (0x1813, None, Some(0)),
        (0x1814, None, None),
    ] {
        let mut item = record(id, *b"INFO", vec![]);
        item.topic_form_id = owner;
        item.source_topic_form_id = raw;
        item.topic_group_offset = Some(0x1_0000_0031);
        before.push(item.clone());
        result.records.insert(id, item);
    }
    validate_targets(&mut result, schema, &order);
    for item in before {
        let after = &result.records[&item.form_id];
        preserved(&item, after);
        assert_eq!(
            after.topic_form_id,
            if item.form_id == 0x1810 {
                Some(0x1900)
            } else {
                None
            }
        );
    }
    assert_eq!(result.diagnostics["synthetic.esp"].invalid_links, 3);
}
