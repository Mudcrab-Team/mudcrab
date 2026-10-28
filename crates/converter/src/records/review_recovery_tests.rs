//! RV-A recovery regressions. Synthetic schema coverage is distinct from native-data reproduction.
use super::{
    DecodedRecord, ReadResult, Value, decoder, history::HistoryEntry,
    read_plugins_with_validation_and_observer_with_schema, schema_format::Schema,
};
use crate::esm::load_order::LoadOrder;
use std::{fs, path::PathBuf};

/// Check an independently authored mandatory deferred field while retaining ordinary neighbors.
/// The embedded COED schema is optional and complete; this deliberate test overlay is not a
/// claim that an immutable built-in-schema plugin can produce the whole-replay failure.
pub(crate) fn replay_failure_schema() -> Schema {
    let authored = Schema::parse(
        br#"{
        "version":1,
        "records":[
            {"signature":"TEST","fields":[
                {"signature":"EDID","name":"editor_id","kind":"zstring"},
                {"signature":"DATA","name":"safe_number","kind":"u32"},
                {"signature":"COED","name":"inventory_extra","kind":"struct","size":12,
                 "reject_record_on_error":true,"members":[
                    {"name":"owner","kind":"form_id","offset":0,"targets":["NPC_","FACT"]},
                    {"name":"global_or_rank","kind":"union","offset":4,"size":4,
                     "decider":"inventory_owner_condition","fields":[
                        {"name":"unused","kind":"bytes","size":4}
                     ]},
                    {"name":"condition","kind":"f32","offset":8}
                 ]}
            ]},
            {"signature":"PROB","fields":[
                {"signature":"EDID","name":"editor_id","kind":"zstring"},
                {"signature":"DATA","name":"safe_number","kind":"u32"},
                {"signature":"COED","name":"inventory_extra","kind":"struct","size":12,
                 "reject_record_on_error":true,"members":[
                    {"name":"owner","kind":"form_id","offset":0,"targets":["NPC_","FACT"]},
                    {"name":"global_or_rank","kind":"union","offset":4,"size":4,
                     "decider":"inventory_owner_condition","fields":[
                        {"name":"unused","kind":"bytes","size":4},
                        {"name":"global","kind":"form_id","targets":["GLOB"]},
                        {"name":"rank","kind":"i32"}
                     ]},
                    {"name":"condition","kind":"f32","offset":8}
                 ]}
            ]}
        ]
    }"#,
    )
    .expect("checked synthetic deferred layouts");
    let mut schema = super::schema().clone();
    schema.records.extend(authored.records);
    schema
        .validate()
        .expect("checked embedded neighbors and independent test overlay");
    schema
}

/// Frame a native subrecord independently of schema matching and canonical encoding.
fn subrecord(signature: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [
        signature.to_vec(),
        u16::try_from(bytes.len()).unwrap().to_le_bytes().to_vec(),
        bytes.to_vec(),
    ]
    .concat()
}

/// Keep version and unknown-header sentinels visible when a predecessor is restored.
fn record(signature: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    [
        signature.to_vec(),
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

/// Author an owner/rank payload; the second word deliberately differs from a global slot.
fn extra(owner: u32, global_or_rank: u32) -> Vec<u8> {
    [
        owner.to_le_bytes(),
        global_or_rank.to_le_bytes(),
        0.375f32.to_le_bytes(),
    ]
    .concat()
}

/// Give safe candidate fields asymmetric bytes unrelated to the deferred selection policy.
fn candidate(
    signature: &[u8; 4],
    id: u32,
    name: &str,
    number: u32,
    ownership: Option<Vec<u8>>,
) -> Vec<u8> {
    let mut payload = subrecord(b"EDID", format!("{name}\0").as_bytes());
    payload.extend(subrecord(b"DATA", &number.to_le_bytes()));
    if let Some(bytes) = ownership {
        payload.extend(subrecord(b"COED", &bytes));
    }
    record(signature, id, 0, &payload)
}

/// Hold physical files for a real scanner/load-order/reader invocation.
struct Fixture {
    directory: tempfile::TempDir,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Separate native master indices, normal slots, light slots and supplying priorities.
    fn new(base: Vec<u8>) -> Self {
        let mut fixture = Self {
            directory: tempfile::tempdir().unwrap(),
            paths: Vec::new(),
        };
        fixture.add("Spacer.esm", &[], 1, &[]);
        fixture.add("Owner.esm", &[], 1, &base);
        fixture.add("Unused.esm", &[], 1, &[]);
        fixture.add(
            "Light.esl",
            &[],
            0x201,
            &record(b"NPC_", 0xABC, 0, &subrecord(b"EDID", b"LightOwner\0")),
        );
        fixture
    }

    /// Write explicit masters and safely framed records; original fixture paths stay in scope.
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

    /// Read with the same checked schema through initial decoding, replay and target clearing.
    fn read(&self, schema: &Schema) -> ReadResult {
        let order = LoadOrder::read(&self.paths).unwrap();
        read_plugins_with_validation_and_observer_with_schema(
            &self.paths,
            &order,
            schema,
            |_| Ok(()),
            |_| {},
        )
        .unwrap()
    }
}

/// Provide a FACT owner whose native index zero maps to normal slot one.
fn owner() -> Vec<u8> {
    record(b"FACT", 0x1900, 0, &subrecord(b"EDID", b"RankOwner\0"))
}

/// Locate the exact supplying occurrence rather than conflating source and global FormIDs.
fn history(result: &ReadResult, id: u32, priority: u32) -> &HistoryEntry {
    result
        .history
        .iter()
        .find(|entry| entry.form_id == Some(id) && entry.plugin_index == priority)
        .unwrap()
}

/// Inspect every nested value so a hidden unresolved member cannot pass a recovery assertion.
fn contains_deferred(value: &Value) -> bool {
    match value {
        Value::Deferred(_) => true,
        Value::Struct(members) => members.iter().any(|(_, value)| contains_deferred(value)),
        Value::Array(values) => values.iter().any(contains_deferred),
        _ => false,
    }
}

/// Read an authored scalar without deriving the expected value from converter bytes.
fn number(record: &DecodedRecord) -> u64 {
    match &record
        .fields
        .iter()
        .find(|field| field.signature == *b"DATA")
        .unwrap()
        .value
    {
        Value::Unsigned(value) => *value,
        other => panic!("independent safe scalar, got {other:?}"),
    }
}

/// Prove the private checked schema creates a genuine decode Err only after catalogue replay.
#[test]
fn rv_a_checked_schema_defers_then_really_rejects_missing_rank_alternative() {
    let schema = replay_failure_schema();
    let fixture = Fixture::new(owner());
    let order = LoadOrder::read(&fixture.paths).unwrap();
    let payload = [
        subrecord(b"EDID", b"DirectReplay\0"),
        subrecord(b"COED", &extra(0x1900, 0x1379_2468)),
    ]
    .concat();
    let scanned = || super::scanner::ScannedRecord {
        record_type: *b"TEST",
        flags: 0,
        source_form_id: 0x1800,
        version_control: 0x1357_2468,
        form_version: 44,
        header_unknown: 0xA571,
        cell_form_id: None,
        worldspace_form_id: None,
        source_topic_form_id: None,
        topic_group_offset: None,
        source_record_offset: 0x51,
        payload: payload.clone(),
    };
    let prepared = decoder::prepare_schema(&schema);
    let context = decoder::Context {
        order: &order,
        plugin_index: 1,
        localized: false,
        winning_types: None,
    };
    let initial = decoder::decode_with_prepared(
        scanned(),
        &schema,
        &prepared,
        &context,
        &mut Default::default(),
    )
    .unwrap();
    assert!(
        initial
            .fields
            .iter()
            .any(|field| contains_deferred(&field.value))
    );
    let kinds = std::collections::HashMap::from([(0x0100_1900, *b"FACT")]);
    let replay = decoder::Context {
        winning_types: Some(&kinds),
        ..context
    };
    let failure = decoder::decode_with_prepared(
        scanned(),
        &schema,
        &prepared,
        &replay,
        &mut Default::default(),
    )
    .unwrap_err();
    assert!(
        failure.contains("mandatory inventory_extra: ownership lacks i32 alternative"),
        "{failure}"
    );
}

/// A whole replay failure cannot replace a complete earlier candidate or its provenance.
#[test]
fn rv_a_failed_deferred_override_restores_predecessor_and_skipped_history() {
    let schema = replay_failure_schema();
    let base = candidate(b"TEST", 0x1800, "EarlierVersion", 0x51A2_0719, None);
    let mut fixture = Fixture::new([owner(), base.clone()].concat());
    fixture.add(
        "Patch.esp",
        &["Owner.esm", "Light.esl"],
        0,
        &[
            candidate(
                b"TEST",
                0x1800,
                "UnusableLaterVersion",
                0x1379_2468,
                Some(extra(0x1900, 0xA5B6_C7D8)),
            ),
            candidate(b"TEST", 0x0200_1802, "UsableNeighbor", 29, None),
        ]
        .concat(),
    );
    let result = fixture.read(&schema);
    let winner = &result.records[&0x0100_1800];
    assert_eq!(
        winner.load_order, 1,
        "replay failure must restore the complete predecessor"
    );
    assert_eq!(number(winner), 0x51A2_0719);
    assert_eq!(winner.raw_payload, base[24..]);
    assert_eq!(winner.version_control, 0x1357_2468);
    assert_eq!(winner.header_unknown, 0xA571);
    assert!(
        winner
            .fields
            .iter()
            .all(|field| !contains_deferred(&field.value))
    );
    assert!(
        result.records.contains_key(&0x0300_1802),
        "usable neighboring record"
    );
    let rejected = history(&result, 0x0100_1800, 4);
    assert!(
        !rejected.accepted,
        "late failure must revoke the exact occurrence's admission"
    );
    assert!(
        rejected
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("deferred") && reason.contains("i32"))
    );
    assert!(history(&result, 0x0100_1800, 1).accepted);
}

/// A first definition retains safe fields and original source bytes, but no unresolved field.
#[test]
fn rv_a_failed_deferred_first_definition_retains_safe_prefix_and_accepted_reason() {
    let schema = replay_failure_schema();
    let mut fixture = Fixture::new(owner());
    let authored = candidate(
        b"TEST",
        0x0200_1804,
        "SafeFirstPrefix",
        0x07A1_5198,
        Some(extra(0x1900, 0x1379_2468)),
    );
    fixture.add("Patch.esp", &["Owner.esm", "Light.esl"], 0, &authored);
    let result = fixture.read(&schema);
    let winner = &result.records[&0x0300_1804];
    assert!(
        winner
            .fields
            .iter()
            .all(|field| field.signature != *b"COED"),
        "failed deferred field must not be published"
    );
    assert_eq!(number(winner), 0x07A1_5198);
    assert_eq!(winner.raw_payload, authored[24..]);
    assert!(winner.rejected_fields.contains(b"COED"));
    assert!(
        winner
            .fields
            .iter()
            .all(|field| !contains_deferred(&field.value))
    );
    let retained = history(&result, 0x0300_1804, 4);
    assert!(
        retained.accepted,
        "published safe prefix remains an admitted runtime winner"
    );
    assert!(
        retained
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("deferred") && reason.contains("COED"))
    );
    assert_eq!(winner.source_record_offset, retained.source_record_offset);
    assert!(result.diagnostics["patch.esp"].skipped_fields >= 1);
}

/// Every failing candidate in a retained chain is skipped before the usable predecessor wins.
#[test]
fn rv_a_failed_deferred_chain_rejects_each_later_occurrence() {
    let schema = replay_failure_schema();
    let mut fixture = Fixture::new(
        [
            owner(),
            candidate(b"TEST", 0x1800, "CompleteBase", 17, None),
        ]
        .concat(),
    );
    for (name, value) in [("FirstPatch.esp", 29), ("LastPatch.esp", 43)] {
        fixture.add(
            name,
            &["Owner.esm", "Light.esl"],
            0,
            &candidate(
                b"TEST",
                0x1800,
                name,
                value,
                Some(extra(0x1900, 0x1379_2468)),
            ),
        );
    }
    let result = fixture.read(&schema);
    assert_eq!(
        number(&result.records[&0x0100_1800]),
        17,
        "try all rejected later candidates before the complete base"
    );
    for priority in [4, 5] {
        let rejected = history(&result, 0x0100_1800, priority);
        assert!(!rejected.accepted);
        assert!(
            rejected
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("deferred"))
        );
    }
}

/// A deletion ends recovery ownership: a later first definition must not resurrect its base.
#[test]
fn rv_a_failed_deferred_definition_does_not_cross_a_deletion_barrier() {
    let schema = replay_failure_schema();
    let mut fixture = Fixture::new(
        [
            owner(),
            candidate(b"TEST", 0x1800, "BeforeDeletion", 17, None),
        ]
        .concat(),
    );
    fixture.add(
        "Delete.esp",
        &["Owner.esm", "Light.esl"],
        0,
        &record(b"TEST", 0x1800, 0x20, &[]),
    );
    fixture.add(
        "Redefine.esp",
        &["Owner.esm", "Light.esl"],
        0,
        &candidate(
            b"TEST",
            0x1800,
            "AfterDeletionPrefix",
            43,
            Some(extra(0x1900, 0x1379_2468)),
        ),
    );
    let result = fixture.read(&schema);
    let winner = &result.records[&0x0100_1800];
    assert!(
        winner
            .fields
            .iter()
            .all(|field| field.signature != *b"COED"),
        "the new prefix must omit the failed field"
    );
    assert_eq!(
        winner.load_order, 5,
        "never restore a version from before the deletion"
    );
    assert_eq!(number(winner), 43);
    let deletion = history(&result, 0x0100_1800, 4);
    assert!(deletion.accepted);
    assert_ne!(deletion.flags & 0x20, 0);
    assert!(history(&result, 0x0100_1800, 5).accepted);
}

/// A stable later winner ends a tentative predecessor chain without replaying the loser.
#[test]
fn rv_a_non_deferred_override_ends_tentative_recovery_chain() {
    let schema = replay_failure_schema();
    let mut fixture = Fixture::new(
        [
            owner(),
            candidate(b"TEST", 0x1800, "CompleteBase", 17, None),
        ]
        .concat(),
    );
    fixture.add(
        "Tentative.esp",
        &["Owner.esm", "Light.esl"],
        0,
        &candidate(
            b"TEST",
            0x1800,
            "Tentative",
            29,
            Some(extra(0x1900, 0x1379_2468)),
        ),
    );
    fixture.add(
        "Complete.esp",
        &["Owner.esm", "Light.esl"],
        0,
        &candidate(b"TEST", 0x1800, "CompleteFinal", 43, None),
    );
    let result = fixture.read(&schema);
    assert_eq!(result.records[&0x0100_1800].load_order, 5);
    assert_eq!(number(&result.records[&0x0100_1800]), 43);
    assert!(history(&result, 0x0100_1800, 5).accepted);
    assert_eq!(result.diagnostics["tentative.esp"].skipped_records, 0);
}

/// Restore the kind authority before finalizing even a dependent that replayed successfully.
#[test]
fn rv_a_deferred_fallback_rebuilds_catalogue_for_previously_resolved_dependents() {
    let schema = replay_failure_schema();
    let mut fixture = Fixture::new(
        [
            owner(),
            record(
                b"NPC_",
                0x1800,
                0,
                &subrecord(b"EDID", b"EarlierNPCOwner\0"),
            ),
        ]
        .concat(),
    );
    fixture.add(
        "Patch.esp",
        &["Owner.esm", "Light.esl"],
        0,
        &[
            candidate(
                b"TEST",
                0x1800,
                "RejectedKindReplacement",
                29,
                Some(extra(0x1900, 0x1379_2468)),
            ),
            // Master-owned 1700 sorts before the rejected owner 1800, so the probe
            // first succeeds as bytes and must then replay after NPC_ restoration.
            candidate(
                b"PROB",
                0x1700,
                "DependentOwnerProbe",
                43,
                Some(extra(0x1800, 0x0200_1806)),
            ),
            record(
                b"GLOB",
                0x0200_1806,
                0,
                &subrecord(b"EDID", b"GlobalForRestoredNPC\0"),
            ),
        ]
        .concat(),
    );
    let result = fixture.read(&schema);
    assert_eq!(
        result.records[&0x0100_1800].record_type, *b"NPC_",
        "the failed replacement must not own the final selector catalogue"
    );
    let probe = &result.records[&0x0100_1700];
    let coed = probe
        .fields
        .iter()
        .find(|field| field.signature == *b"COED")
        .unwrap();
    let Value::Struct(members) = &coed.value else {
        panic!("native ownership struct")
    };
    assert_eq!(
        members
            .iter()
            .find(|(name, _)| name == "global_or_rank")
            .unwrap()
            .1,
        Value::FormId(0x0300_1806)
    );
    assert_eq!(&coed.canonical_bytes[4..8], &0x0300_1806u32.to_le_bytes());
    assert!(
        probe
            .fields
            .iter()
            .all(|field| !contains_deferred(&field.value))
    );
    assert!(!history(&result, 0x0100_1800, 4).accepted);
}

/// Safe-prefix recovery must honor the consumer's admission check after removing its field.
#[test]
fn rv_a_recovered_first_definition_is_revalidated_before_admission() {
    let schema = replay_failure_schema();
    let mut fixture = Fixture::new(owner());
    fixture.add(
        "Patch.esp",
        &["Owner.esm", "Light.esl"],
        0,
        &candidate(
            b"TEST",
            0x0200_1804,
            "NeedsRecoveredOwnership",
            29,
            Some(extra(0x1900, 0x1379_2468)),
        ),
    );
    let order = LoadOrder::read(&fixture.paths).unwrap();
    let result = read_plugins_with_validation_and_observer_with_schema(
        &fixture.paths,
        &order,
        &schema,
        |candidate| {
            if candidate.record_type == *b"TEST"
                && !candidate
                    .fields
                    .iter()
                    .any(|field| field.signature == *b"COED")
            {
                Err("this consumer requires the ownership field".into())
            } else {
                Ok(())
            }
        },
        |_| {},
    )
    .unwrap();
    assert!(
        !result.records.contains_key(&0x0300_1804),
        "an unusable recovered prefix must remain a local rejected version"
    );
    let rejected = history(&result, 0x0300_1804, 4);
    assert!(!rejected.accepted);
    assert!(
        rejected
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("ownership"))
    );
    assert!(
        result.records.contains_key(&0x0100_1900),
        "usable neighboring owner survives"
    );
}

/// Inject inconsistent retained source to cover defensive deferred-replay recovery.
/// Real plugin bytes are immutable during conversion; this is explicitly not a native repro.
#[test]
fn rv_a_injected_baseline_failure_omits_unresolved_first_definition_fields() {
    let schema = replay_failure_schema();
    let mut fixture = Fixture::new(owner());
    fixture.add(
        "Patch.esp",
        &["Owner.esm", "Light.esl"],
        0,
        &candidate(
            b"TEST",
            0x0200_1804,
            "InjectedBaselinePrefix",
            29,
            Some(extra(0x1900, 0x1379_2468)),
        ),
    );
    let order = LoadOrder::read(&fixture.paths).unwrap();
    let prepared = decoder::prepare_schema(&schema);
    let mut first_pass = None;
    let context = decoder::Context {
        order: &order,
        plugin_index: 4,
        localized: false,
        winning_types: None,
    };
    super::scanner::scan_plugin(
        &fixture.paths[4],
        &mut Default::default(),
        |_| {},
        |scanned, diagnostics| {
            if scanned.record_type == *b"TEST" {
                first_pass = Some(
                    decoder::decode_with_prepared(
                        scanned,
                        &schema,
                        &prepared,
                        &context,
                        diagnostics,
                    )
                    .unwrap(),
                );
            }
        },
    )
    .unwrap();
    let mut result = fixture.read(&schema);
    let mut retained = first_pass.unwrap();
    retained.raw_payload = [
        subrecord(b"EDID", b"InjectedBaselinePrefix\0"),
        subrecord(b"DATA", &29u32.to_le_bytes()),
        subrecord(b"COED", &[0xA5; 11]),
    ]
    .concat();
    result.records.insert(retained.form_id, retained);
    decoder::resolve_deferred(
        &mut result,
        &schema,
        &prepared,
        &order,
        &mut Default::default(),
        &mut |_| Ok(()),
    );
    let recovered = &result.records[&0x0300_1804];
    assert!(
        recovered
            .fields
            .iter()
            .all(|field| !contains_deferred(&field.value)),
        "inconsistent retained source must not retain unresolved canonical bytes"
    );
    assert!(
        recovered
            .fields
            .iter()
            .all(|field| field.signature != *b"COED")
    );
    assert_eq!(number(recovered), 29);
}

/// Author distinct layer slots without sharing the decoder's struct offsets as expectations.
fn layer(texture: u32, slot: u16) -> Vec<u8> {
    [
        texture.to_le_bytes().to_vec(),
        vec![0, 0xA5],
        slot.to_le_bytes().to_vec(),
    ]
    .concat()
}

/// Author sentinel padding and asymmetric vertex/opacity pairs directly in native bytes.
fn weight(vertex: u16, opacity: f32) -> Vec<u8> {
    [
        vertex.to_le_bytes().to_vec(),
        vec![0xA5, 0x71],
        opacity.to_le_bytes().to_vec(),
    ]
    .concat()
}

/// Rejected physical anchors own their following weights until the next valid anchor.
#[test]
fn rv_a_rejected_land_anchor_owns_following_weights_and_recovers_next_anchor() {
    for rejected_anchor in [*b"ATXT", *b"BTXT"] {
        let payload = [
            subrecord(b"ATXT", &layer(0x41, 0)),
            subrecord(b"VTXT", &weight(2, 0.25)),
            subrecord(&rejected_anchor, &[0xB6; 7]),
            subrecord(b"VTXT", &weight(9, 0.75)),
            subrecord(b"ATXT", &layer(0x42, 1)),
            subrecord(b"VTXT", &weight(5, 0.5)),
        ]
        .concat();
        let fixture = Fixture::new(record(b"LAND", 0x1A00, 0, &payload));
        let result = fixture.read(super::schema());
        let land = &result.records[&0x0100_1A00];
        let kept = land
            .fields
            .iter()
            .filter(|field| field.signature == *b"VTXT")
            .map(|field| field.canonical_bytes.as_slice())
            .collect::<Vec<_>>();
        assert_eq!(
            kept,
            vec![weight(2, 0.25).as_slice(), weight(5, 0.5).as_slice()],
            "a rejected physical anchor must not donate vertex 9 to the previous surviving layer"
        );
        assert_eq!(land.raw_payload, payload);
        assert!(land.rejected_fields.contains(&rejected_anchor));
        assert!(land.rejected_fields.contains(b"VTXT"));
        assert_eq!(result.diagnostics["owner.esm"].skipped_fields, 2);
    }
}
