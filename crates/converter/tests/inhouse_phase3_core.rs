//! Shared condition assembly keeps family ownership and prior pending consumers explicit.
use converter::records::schema_format::Schema;

/// The shared component replaces only the documented pending leaves after all overlays.
#[test]
fn condition_component_activates_existing_family_consumers() {
    let base = br#"{"version":1,"sources":{},"definitions":{"pending_conditions":{"kind":"group","fields":[{"signature":"CTDA","name":"pending_condition","kind":"bytes","repeat":true}]}},"records":[{"signature":"COBJ","fields":[{"definition":"pending_conditions"}]},{"signature":"LAND","fields":[{"signature":"CTDA","name":"opaque_other_role","kind":"bytes"}]}]}"#;
    let items = br#"{"version":1,"definitions":{"effects":{"kind":"group","repeat":true,"fields":[{"signature":"CTDA","name":"pending_condition","kind":"bytes","repeat":true,"source":"pending-component:D"},{"signature":"CIS1","name":"pending_condition_string_a","kind":"bytes","repeat":true,"source":"pending-component:D"}]}},"records":[{"signature":"INGR","fields":[{"definition":"effects"}]}]}"#;
    let conditions = br#"{"version":1,"definitions":{"condition":{"kind":"struct","size":32,"members":[]},"conditions":{"kind":"group","repeat":true,"fields":[{"signature":"CTDA","definition":"condition","repeat":true},{"signature":"CIS1","name":"condition_string_1","kind":"zstring","repeat":true},{"signature":"CIS2","name":"condition_string_2","kind":"zstring","repeat":true}]}},"records":[]}"#;
    for modules in [
        [
            ("conditions", conditions.as_slice()),
            ("items", items.as_slice()),
        ],
        [
            ("items", items.as_slice()),
            ("conditions", conditions.as_slice()),
        ],
    ] {
        let assembled = Schema::assemble(base, &modules)
            .expect("the conditions owner can activate its consumers");
        let schema = Schema::parse(&assembled).unwrap();
        assert!(!schema.definitions.contains_key("pending_conditions"));
        let craft = schema
            .records
            .iter()
            .find(|record| record.signature == "COBJ")
            .unwrap();
        assert_eq!(craft.fields[0].definition.as_deref(), Some("conditions"));
        let effects = &schema.definitions["effects"];
        assert_eq!(effects.fields[0].definition.as_deref(), Some("condition"));
        assert_eq!(schema.resolve(&effects.fields[0]).unwrap().kind, "struct");
        assert_eq!(effects.fields[1].kind, "zstring");
        let land = schema
            .records
            .iter()
            .find(|record| record.signature == "LAND")
            .unwrap();
        assert_eq!(
            land.fields[0].kind, "bytes",
            "unassigned opaque roles remain intact"
        );
    }
}

/// A definitions-only shared owner cannot replace unrelated records or definitions.
#[test]
fn condition_component_rejects_unowned_definitions_and_records() {
    let base = br#"{"version":1,"sources":{},"definitions":{},"records":[]}"#;
    let unowned_definition =
        br#"{"version":1,"definitions":{"bounds":{"kind":"bytes"}},"records":[]}"#;
    let unowned_record =
        br#"{"version":1,"definitions":{},"records":[{"signature":"QUST","fields":[]}]}"#;
    assert!(Schema::assemble(base, &[("conditions", unowned_definition)]).is_err());
    assert!(Schema::assemble(base, &[("conditions", unowned_record)]).is_err());
}

/// Section D's record ownership cannot replace an actor or world entry.
#[test]
fn dialogue_overlay_replaces_only_section_d_records() {
    let base = br#"{"version":1,"sources":{},"definitions":{},"records":[{"signature":"QUST","fields":[]},{"signature":"NPC_","fields":[]}]}"#;
    let dialogue = br#"{"version":1,"records":[{"signature":"QUST","fields":[{"signature":"EDID","name":"editor_id","kind":"zstring"}]}]}"#;
    let schema =
        Schema::parse(&Schema::assemble(base, &[("dialogue", dialogue)]).unwrap()).unwrap();
    assert_eq!(schema.records[0].fields[0].name, "editor_id");
    assert_eq!(schema.records[1].signature, "NPC_");
    let unauthorized = br#"{"version":1,"records":[{"signature":"NPC_","fields":[]}]}"#;
    assert!(Schema::assemble(base, &[("dialogue", unauthorized)]).is_err());
}

/// Explicit boundaries require actual markers rather than a guessed group end.
#[test]
fn terminated_repeats_require_an_explicit_empty_marker() {
    let valid = serde_json::json!({"version":1,"records":[{"signature":"SCEN","fields":[{
        "name":"actions","kind":"group","repeat":true,"repeat_terminated":true,
        "fields":[{"signature":"ANAM","name":"action_type","kind":"u16"},
                  {"signature":"ANAM","name":"action_end","kind":"bytes","size":0}]
    }]}]});
    assert!(Schema::parse(&serde_json::to_vec(&valid).unwrap()).is_ok());
    let mut repeated_start = valid.clone();
    repeated_start["records"][0]["fields"][0]["fields"][0]["repeat"] = serde_json::json!(true);
    assert!(Schema::parse(&serde_json::to_vec(&repeated_start).unwrap()).is_err());
    let mut unordered = valid.clone();
    unordered["records"][0]["allow_unordered"] = serde_json::json!(true);
    assert!(Schema::parse(&serde_json::to_vec(&unordered).unwrap()).is_err());
    let mut nested_start = valid.clone();
    nested_start["records"][0]["fields"][0]["fields"][0] =
        serde_json::json!({"signature":"ANAM","kind":"group","fields":[]});
    assert!(Schema::parse(&serde_json::to_vec(&nested_start).unwrap()).is_err());
    for (key, value) in [
        ("repeat", serde_json::json!(false)),
        ("kind", serde_json::json!("struct")),
    ] {
        let mut invalid = valid.clone();
        invalid["records"][0]["fields"][0][key] = value;
        assert!(Schema::parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
    let mut invalid = valid.clone();
    invalid["records"][0]["fields"][0]["fields"][1]["size"] = serde_json::json!(4);
    assert!(Schema::parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
    let mut invalid = valid;
    invalid["records"][0]["fields"][0]["fields"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    assert!(Schema::parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
}
