//! Family assembly protects ownership while preserving shared, authored definitions.
use converter::records::schema_format::Schema;

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
