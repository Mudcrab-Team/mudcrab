//! Fault-injected schema walks retain fallback and publish actionable recovery events.
use super::*;

/// A schema failure after successful value decoding is injected directly because
/// validated production schemas traverse the same resolver during value decoding.
#[test]
fn bank_selection_errors_report_field_and_keep_default_fallback() {
    let schema = Schema::parse(br#"{"version":1,"records":[]}"#).unwrap();
    let selection = super::super::deciders::Context {
        record_type: *b"ZLOC",
        form_version: 44,
        editor_id: "BankFailure",
        preceding_subrecords: &[],
        parent_payload: None,
        link_record_type: None,
    };
    for (field, step) in [
        (
            FieldSchema {
                name: "bad_alias".into(),
                definition: Some("AbsentDefinition".into()),
                ..Default::default()
            },
            "resolve step 0",
        ),
        (
            FieldSchema {
                name: "bad_union".into(),
                kind: "union".into(),
                ..Default::default()
            },
            "decide step 0",
        ),
    ] {
        let mut diagnostics = PluginDiagnostics::default();
        diagnostics.set_record(
            24,
            Some(*b"ZLOC"),
            Some(0x01001811),
            Some(0x01001811),
            "decode",
        );
        diagnostics.set_field("ZTXT[source:1]".into());
        let bank = selected_localized_bank(
            &field,
            &73u32.to_le_bytes(),
            &schema,
            &selection,
            &Value::LocalizedString(73),
            &mut diagnostics,
        );
        assert_eq!(
            bank, None,
            "existing signature default-bank fallback changes no values"
        );
        assert_eq!(
            diagnostics.skipped_fields, 1,
            "a failed bank walk needs one warning"
        );
        let message = diagnostics.first_example.as_deref().unwrap();
        assert!(message.contains("01001811"), "{message}");
        assert!(message.contains(&field.name), "{message}");
        assert!(message.contains(step), "{message}");
        assert_eq!(
            diagnostics.event_context.field.as_deref(),
            Some("ZTXT[source:1]")
        );
        assert_eq!(diagnostics.event_context.action, None);
    }
    let field = FieldSchema {
        name: "valid_text".into(),
        kind: "lstring".into(),
        string_table: Some("ilstrings".into()),
        ..Default::default()
    };
    let mut diagnostics = PluginDiagnostics::default();
    assert_eq!(
        selected_localized_bank(
            &field,
            &73u32.to_le_bytes(),
            &schema,
            &selection,
            &Value::LocalizedString(73),
            &mut diagnostics
        )
        .as_deref(),
        Some("ilstrings")
    );
    assert_eq!(diagnostics.skipped_fields, 0);
}
