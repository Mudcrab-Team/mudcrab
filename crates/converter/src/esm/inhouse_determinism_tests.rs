//! Adapter summary selection must be independent of observer and projection visit order.
use super::*;

/// Supply asymmetric physical order and text order to distinguish selection rules.
fn context(offset: u64) -> records::diagnostics::EventContext {
    records::diagnostics::EventContext {
        source_record_offset: Some(offset),
        source_form_id: Some(0x0100_1800),
        form_id: Some(0x0700_1800),
        record_type: Some(*b"LAND"),
        stage: "terrain_provenance",
        field: Some("texture_layers".into()),
        action: Some("skipped"),
        ..Default::default()
    }
}

/// Counts and the earliest physical example survive reversed adapter observation order.
#[test]
fn adapter_summary_ignores_visit_order() {
    let mut forward = Warnings::default();
    let mut reverse = Warnings::default();
    for (offset, message) in [
        (80, "A later terrain issue"),
        (24, "Z earliest terrain issue"),
    ] {
        forward.skipped("synthetic.esm", context(offset), message.into(), false);
    }
    for (offset, message) in [
        (24, "Z earliest terrain issue"),
        (80, "A later terrain issue"),
    ] {
        reverse.skipped("synthetic.esm", context(offset), message.into(), false);
    }
    assert_eq!(forward.summary, reverse.summary);
    assert_eq!(forward.summary_keys, reverse.summary_keys);
    assert!(forward.notices.is_empty());
    assert!(reverse.notices.is_empty());
    assert_eq!(
        forward.summary["synthetic.esm"],
        (2, "Z earliest terrain issue".into())
    );
    let expected = vec![
        "warning: synthetic.esm: inhouse adapter reported 2 local decode/lookup issues (first: Z earliest terrain issue)".to_owned(),
    ];
    assert_eq!(forward.report(), expected);
    assert_eq!(reverse.report(), expected);
    assert!(forward.report_notices().is_empty());
    assert!(reverse.report_notices().is_empty());
}
