//! Binary subrecord identities are four bytes independently of record identities.
use converter::records::schema_format::Schema;

/// Keep the native image-modifier control byte in its schema identity.
#[test]
fn binary_subrecord_signature_is_valid_and_preserved() {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "records": [{"signature": "IMAD", "fields": [{
            "signature": "\u{0000}IAD", "name": "blur_curve", "kind": "bytes"
        }]}]
    }))
    .unwrap();
    let result = Schema::parse(&bytes);
    assert!(
        result.is_ok(),
        "native binary signature must validate: {result:?}"
    );
    assert_eq!(
        result.unwrap().records[0].fields[0]
            .signature
            .as_ref()
            .unwrap()
            .as_bytes(),
        &[0, b'I', b'A', b'D']
    );
}

/// Record identities and link targets retain their textual four-byte contract.
#[test]
fn binary_subrecord_support_keeps_record_and_target_guards() {
    for record in [
        serde_json::json!({"signature":"\u{0000}IAD","fields":[]}),
        serde_json::json!({"signature":"IMAD","fields":[{
            "signature":"DNAM","name":"link","kind":"form_id",
            "targets":["\u{0000}IAD"]
        }]}),
        serde_json::json!({"signature":"IMAD","fields":[{
            "signature":"éIAD","name":"bad_width","kind":"bytes"
        }]}),
        serde_json::json!({"signature":"IMAD","fields":[{
            "signature":"IAD","name":"short","kind":"bytes"
        }]}),
    ] {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version":1,"records":[record]
        }))
        .unwrap();
        assert!(Schema::parse(&bytes).is_err());
    }
}
