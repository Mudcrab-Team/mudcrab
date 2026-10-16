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
    assert_eq!(forward.summary.counts, reverse.summary.counts);
    assert_eq!(forward.warnings.counts, reverse.warnings.counts);
    assert_eq!(forward.summary.counts, forward.warnings.counts);
    assert!(forward.notices.counts.is_empty());
    assert!(reverse.notices.counts.is_empty());
    assert_eq!(
        forward.summary.counts["synthetic.esm"],
        (2, "Z earliest terrain issue".into())
    );
}
