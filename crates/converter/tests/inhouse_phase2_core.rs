//! Shared family assembly and deferred diagnostics preserve their authored contracts.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::load_order::LoadOrder,
    records::{Value, read_plugins, schema_format::Schema},
};
use dummy_content::{inhouse_magic as magic, layout};
use std::fs;

/// An overlay replaces an assigned partial record and leaves unrelated records intact.
#[test]
fn overlays_replace_only_their_owned_family() {
    let base = br#"{"version":1,"sources":{},"definitions":{},"records":[{"signature":"WEAP","fields":[]},{"signature":"LAND","fields":[]}]}"#;
    let items = br#"{"version":1,"records":[{"signature":"WEAP","fields":[{"signature":"DATA","name":"weight","kind":"f32"}]}]}"#;
    let combined = Schema::assemble(base, &[("items", items)]).unwrap();
    let schema = Schema::parse(&combined).unwrap();
    assert_eq!(schema.records.len(), 2);
    assert_eq!(schema.records[0].fields[0].name, "weight");
    assert_eq!(schema.records[1].signature, "LAND");
    let unauthorized = br#"{"version":1,"records":[{"signature":"LAND","fields":[]}]}"#;
    assert!(Schema::assemble(base, &[("items", unauthorized)]).is_err());
    let shared_override =
        br#"{"version":1,"common_fields":[{"signature":"VMAD","kind":"bytes"}],"records":[]}"#;
    assert!(Schema::assemble(base, &[("actors", shared_override)]).is_err());
}

/// Identical shared definitions are safe; conflicting meanings cannot silently win by order.
#[test]
fn conflicting_shared_definitions_are_rejected() {
    let base = br#"{"version":1,"sources":{},"definitions":{"count":{"kind":"u32"}},"records":[]}"#;
    let identical = br#"{"version":1,"definitions":{"count":{"kind":"u32"}},"records":[]}"#;
    assert!(Schema::assemble(base, &[("magic", identical)]).is_ok());
    let conflict = br#"{"version":1,"definitions":{"count":{"kind":"i32"}},"records":[]}"#;
    assert!(Schema::assemble(base, &[("magic", conflict)]).is_err());
}

/// Empty native prefixes need wholly optional members; wildcard links have one clear policy.
#[test]
fn optional_empty_structs_and_wildcards_are_explicit() {
    let optional = br#"{"version":1,"records":[{"signature":"EFSH","fields":[{"signature":"DATA","kind":"struct","sizes":[0,4],"members":[{"name":"tail","kind":"f32","optional":true}]}]}]}"#;
    assert!(Schema::parse(optional).is_ok());
    let mandatory = br#"{"version":1,"records":[{"signature":"EFSH","fields":[{"signature":"DATA","kind":"struct","sizes":[0,4],"members":[{"name":"tail","kind":"f32"}]}]}]}"#;
    assert!(Schema::parse(mandatory).is_err());
    let any_link = br#"{"version":1,"records":[{"signature":"FLST","fields":[{"signature":"LNAM","kind":"form_id","targets":["*"]}]}]}"#;
    assert!(Schema::parse(any_link).is_ok());
    let ambiguous = br#"{"version":1,"records":[{"signature":"FLST","fields":[{"signature":"LNAM","kind":"form_id","targets":["*","NPC_"]}]}]}"#;
    assert!(Schema::parse(ambiguous).is_err());
}

/// Replaying NPC ownership keeps initial issues counted once and retains new link warnings.
#[tokio::test]
async fn deferred_npc_ownership_counts_source_issues_once_through_publication() {
    let temporary = tempfile::tempdir().unwrap();
    let data = temporary.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let plugin = data.join("Skyrim.esm");
    let mut bytes = fs::read(&plugin).unwrap();
    bytes.extend(magic::records());
    bytes.extend(magic::record(
        b"NPC_",
        0x19C0,
        0,
        &magic::subrecord(b"EDID", b"OwnershipTarget\0"),
    ));
    bytes.extend(magic::spell(0x19D1, 47));
    let mut payload = magic::subrecord(b"EDID", b"DeferredDiagnosticsNPC\0");
    // Bounds are optional, and this damaged present field is recoverably omitted.
    payload.extend(magic::subrecord(b"OBND", &[0; 11]));
    // This unrelated bad master index is diagnosed in the original decoding pass.
    payload.extend(magic::subrecord(b"INAM", &0x0200_0077u32.to_le_bytes()));
    payload.extend(magic::subrecord(b"COCT", &2u32.to_le_bytes()));
    for (count, global, condition) in [
        (2i32, 0x0300_0088u32, 0.375f32),
        (3, magic::FIRST_ID + 10, 0.8125),
    ] {
        let mut item = 0x18F1u32.to_le_bytes().to_vec();
        item.extend(count.to_le_bytes());
        payload.extend(magic::subrecord(b"CNTO", &item));
        let mut extra = 0x19C0u32.to_le_bytes().to_vec();
        extra.extend(global.to_le_bytes());
        extra.extend(condition.to_le_bytes());
        payload.extend(magic::subrecord(b"COED", &extra));
    }
    // Framing is valid; one unsupported occurrence must produce exactly one note.
    payload.extend(magic::subrecord(b"ZZZZ", &[0xA5]));
    bytes.extend(magic::record(b"NPC_", 0x19D0, 0, &payload));
    bytes.extend(magic::spell(0x19D2, 83));
    fs::write(&plugin, bytes).unwrap();
    let paths = [plugin];
    let result = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
    let diagnostics = &result.diagnostics["skyrim.esm"];
    assert_eq!(diagnostics.skipped_records, 0);
    assert_eq!(diagnostics.skipped_fields, 1, "optional OBND counted twice");
    assert_eq!(
        diagnostics.unexpected_subrecords, 1,
        "unknown ZZZZ counted twice"
    );
    assert_eq!(
        diagnostics.invalid_links, 2,
        "initial INAM and newly selected GLOB"
    );
    for id in [0x19C0, 0x19D0, 0x19D1, 0x19D2] {
        assert!(result.records.contains_key(&id));
    }
    let actor = &result.records[&0x19D0];
    assert!(
        actor
            .fields
            .iter()
            .all(|field| !contains_deferred(&field.value))
    );
    let extras = actor
        .fields
        .iter()
        .filter(|field| field.signature == *b"COED")
        .collect::<Vec<_>>();
    assert_eq!(extras.len(), 2);
    for (field, expected) in extras.iter().zip([0, magic::FIRST_ID + 10]) {
        let Value::Struct(members) = &field.value else {
            panic!("native ownership struct")
        };
        let global = &members
            .iter()
            .find(|(name, _)| name == "global_or_rank")
            .unwrap()
            .1;
        assert_eq!(global, &Value::FormId(expected));
    }
    let output = temporary.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let database = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in [0x19C0, 0x19D0, 0x19D1, 0x19D2] {
        let count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    let published: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    let counts = &published["decoder"]["skyrim.esm"];
    assert_eq!(counts["skipped_records"], 0);
    assert_eq!(counts["skipped_fields"], 1);
    assert_eq!(counts["unexpected_subrecords"], 1);
    assert_eq!(counts["invalid_links"], 2);
}

/// Detect deferred ownership anywhere in a nested typed result.
fn contains_deferred(value: &Value) -> bool {
    match value {
        Value::Deferred(_) => true,
        Value::Struct(members) => members.iter().any(|(_, value)| contains_deferred(value)),
        Value::Array(values) => values.iter().any(contains_deferred),
        _ => false,
    }
}
