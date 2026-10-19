//! Regression probes for bounded summaries under different observation and merge orders.
use super::{merge_new_diagnostics, merge_replay_observations};
use crate::records::{DiagnosticSummaryKey, PluginDiagnostics, diagnostics::EventContext};

/// Keep source coordinates distinct from canonical IDs and message text order.
fn observe(diagnostics: &mut PluginDiagnostics, offset: u64, category: &str, reason: &str) {
    diagnostics.set_record(
        offset,
        Some(*b"REFR"),
        Some(0x0100_1823),
        Some(0x0700_1823),
        "validate_links",
    );
    diagnostics.set_field("NAME[source:2]".into());
    if category == "link" {
        diagnostics.link(0x0800_1987);
    }
    diagnostics.note(category, || reason.to_owned());
}

/// Compare the public JSON contract, including every scalar counter and category example.
fn summary(diagnostics: &PluginDiagnostics) -> Vec<u8> {
    serde_json::to_vec_pretty(diagnostics).unwrap()
}

/// Every category chooses the earliest physical source rather than the first visit.
#[test]
fn summaries_ignore_observation_order_and_remain_bounded() {
    let long_reason = format!("{} late source", "\u{e9}".repeat(1500));
    let events = [
        (96, "record", long_reason.as_str()),
        (80, "field", "A later malformed field"),
        (88, "link", "A later invalid reference"),
        (72, "unexpected", "A later unrecognized field"),
        (64, "compatibility", "A later compatibility remap"),
        (24, "record", "Z earliest damaged record"),
        (32, "field", "Z earliest malformed field"),
        (40, "link", "Z earliest invalid reference"),
        (48, "unexpected", "Z earliest unrecognized field"),
        (56, "compatibility", "Z earliest compatibility remap"),
    ];
    let mut forward = PluginDiagnostics::default();
    let mut reverse = PluginDiagnostics::default();
    for &(offset, category, reason) in &events {
        observe(&mut forward, offset, category, reason);
    }
    for &(offset, category, reason) in events.iter().rev() {
        observe(&mut reverse, offset, category, reason);
    }
    assert_eq!(summary(&forward), summary(&reverse));
    assert_eq!(
        forward.first_example.as_deref(),
        Some("Z earliest damaged record")
    );
    assert_eq!(forward.first_by_category.len(), 5);
    assert!(
        forward
            .first_by_category
            .values()
            .all(|reason| reason.len() <= 2048)
    );
    assert_eq!(forward.skipped_records, 2);
    assert_eq!(forward.skipped_fields, 2);
    assert_eq!(forward.invalid_links, 2);
    assert_eq!(forward.unexpected_subrecords, 2);
    assert_eq!(forward.compatibility_headers, 2);
}

/// Equal record coordinates still select the same bounded category and field examples.
#[test]
fn summaries_ignore_same_record_tie_observation_order() {
    let shared_prefix = "\u{e9}".repeat(1100);
    let long_first = format!("{shared_prefix}A beyond the displayed prefix");
    let long_second = format!("{shared_prefix}Z beyond the displayed prefix");
    let events = [
        ("link", "NAME[source:2]", "A later field"),
        ("field", "NAME[source:1]", "Z earlier field"),
        ("link", "NAME[source:1]", long_first.as_str()),
        ("link", "NAME[source:1]", long_second.as_str()),
    ];
    let apply = |diagnostics: &mut PluginDiagnostics,
                 (category, field, reason): (&str, &str, &str)| {
        diagnostics.set_record(
            24,
            Some(*b"REFR"),
            Some(0x0100_1823),
            Some(0x0700_1823),
            "validate_links",
        );
        diagnostics.set_field(field.into());
        diagnostics.note(category, || reason.into());
    };
    let mut forward = PluginDiagnostics::default();
    let mut reverse = PluginDiagnostics::default();
    for &event in &events {
        apply(&mut forward, event);
    }
    for &event in events.iter().rev() {
        apply(&mut reverse, event);
    }
    assert_eq!(summary(&forward), summary(&reverse));
    assert_eq!(forward.first_by_category["field"], "Z earlier field");
    assert!(forward.first_by_category["link"].len() <= 2048);
    assert_eq!(forward.invalid_links, 3);
    assert_eq!(forward.skipped_fields, 1);
}

/// Replay unions preserve the same selected source when local batches arrive in reverse.
#[test]
fn summaries_ignore_replay_merge_order() {
    let batch = |offset, reason: &str| {
        let mut diagnostics = PluginDiagnostics::default();
        observe(&mut diagnostics, offset, "link", reason);
        diagnostics
    };
    let mut forward = PluginDiagnostics::default();
    merge_replay_observations(&mut forward, batch(80, "A later invalid link"));
    merge_replay_observations(&mut forward, batch(24, "Z earliest invalid link"));
    let mut reverse = PluginDiagnostics::default();
    merge_replay_observations(&mut reverse, batch(24, "Z earliest invalid link"));
    merge_replay_observations(&mut reverse, batch(80, "A later invalid link"));
    assert_eq!(summary(&forward), summary(&reverse));
    assert_eq!(
        forward.first_example.as_deref(),
        Some("Z earliest invalid link")
    );
    // This helper merges observations only; the replay planner installs scalar totals.
    assert_eq!(forward.invalid_links, 0);
}

/// New replay categories carry their provenance through scalar-difference merging.
#[test]
fn summaries_ignore_new_diagnostic_merge_order() {
    let batch = |offset, reason: &str| {
        let mut diagnostics = PluginDiagnostics::default();
        observe(&mut diagnostics, offset, "field", reason);
        diagnostics
    };
    let baseline = PluginDiagnostics::default();
    let mut forward = PluginDiagnostics::default();
    merge_new_diagnostics(&mut forward, &baseline, batch(80, "A later bad field"));
    merge_new_diagnostics(&mut forward, &baseline, batch(24, "Z earliest bad field"));
    let mut reverse = PluginDiagnostics::default();
    merge_new_diagnostics(&mut reverse, &baseline, batch(24, "Z earliest bad field"));
    merge_new_diagnostics(&mut reverse, &baseline, batch(80, "A later bad field"));
    assert_eq!(summary(&forward), summary(&reverse));
    assert_eq!(
        forward.first_example.as_deref(),
        Some("Z earliest bad field")
    );
    assert_eq!(forward.skipped_fields, 2);
}

/// Authored summaries without internal provenance still have a stable text fallback.
#[test]
fn summaries_merge_authored_examples_deterministically() {
    let authored = |reason: &str| PluginDiagnostics {
        first_example: Some(reason.into()),
        first_by_category: [("link".into(), reason.into())].into(),
        ..Default::default()
    };
    let mut forward = PluginDiagnostics::default();
    merge_replay_observations(&mut forward, authored("Z authored invalid link"));
    merge_replay_observations(&mut forward, authored("A authored invalid link"));
    let mut reverse = PluginDiagnostics::default();
    merge_replay_observations(&mut reverse, authored("A authored invalid link"));
    merge_replay_observations(&mut reverse, authored("Z authored invalid link"));
    assert_eq!(summary(&forward), summary(&reverse));
    assert_eq!(
        forward.first_example.as_deref(),
        Some("A authored invalid link")
    );
}

/// Copied examples retain physical provenance when a later observed source competes.
#[test]
fn copied_summaries_keep_their_source_order() {
    let mut source = PluginDiagnostics::default();
    observe(&mut source, 24, "link", "Z earliest invalid link");
    let mut copied = PluginDiagnostics::default();
    copied.merge_summaries(&source);
    observe(&mut copied, 80, "link", "A later invalid link");
    assert_eq!(copied.first_example, source.first_example);
    assert_eq!(copied.first_by_category, source.first_by_category);
    assert_eq!(copied.invalid_links, 1);
    let serialized = serde_json::to_value(&copied).unwrap();
    assert_eq!(serialized.as_object().unwrap().len(), 7);
}

/// Full-text ties remain distinct even when both field and reason display prefixes match.
#[test]
fn summary_keys_distinguish_truncated_field_and_reason_suffixes() {
    let reason_prefix = "\u{e9}".repeat(1100);
    let field_prefix = "\u{e9}".repeat(300);
    let context = |suffix| EventContext {
        source_record_offset: Some(24),
        field: Some(format!("{field_prefix}{suffix}")),
        ..Default::default()
    };
    let first = DiagnosticSummaryKey::new(&context("A"), "link", &format!("{reason_prefix}A"));
    let other_reason =
        DiagnosticSummaryKey::new(&context("A"), "link", &format!("{reason_prefix}Z"));
    let other_field =
        DiagnosticSummaryKey::new(&context("Z"), "link", &format!("{reason_prefix}A"));
    assert_ne!(first, other_reason);
    assert_ne!(first, other_field);
}
