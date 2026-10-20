//! Bounded compatibility observations remain separate from failures and replay deltas.

use super::*;

/// Complete reason evaluation retains one bounded compatibility summary and no failures.
#[test]
fn compatibility_observation_is_bounded_and_separate_from_failures() {
    let mut diagnostics = PluginDiagnostics::default();
    let mut allocations = 0;
    for _ in 0..64 {
        diagnostics.note("compatibility", || {
            allocations += 1;
            "original owner unresolved; supplier runtime projection".to_owned()
        });
    }
    // R5 evaluates every diagnostic reason while retaining one bounded summary.
    assert_eq!(allocations, 64);
    assert_eq!(diagnostics.compatibility_headers, 64);
    assert_eq!(diagnostics.skipped_records, 0);
    assert_eq!(diagnostics.skipped_fields, 0);
    assert_eq!(diagnostics.invalid_links, 0);
    assert_eq!(diagnostics.first_by_category.len(), 1);
    assert_eq!(
        diagnostics.first_by_category["compatibility"],
        "original owner unresolved; supplier runtime projection"
    );
    assert_eq!(
        diagnostics.first_example.as_deref(),
        Some("original owner unresolved; supplier runtime projection")
    );
}

#[test]
fn contextual_replay_does_not_recount_a_header_or_turn_it_into_a_link_error() {
    let mut target = PluginDiagnostics::default();
    target.note("compatibility", || "initial physical header".into());
    let mut baseline = PluginDiagnostics::default();
    baseline.note("compatibility", || "baseline same header".into());
    let mut replay = PluginDiagnostics::default();
    replay.note("compatibility", || "replay same header".into());
    replay.note("link", || "new contextual owner link error".into());
    merge_new_diagnostics(&mut target, &baseline, replay);
    assert_eq!(target.compatibility_headers, 1);
    assert_eq!(target.invalid_links, 1);
    assert_eq!(target.skipped_records, 0);
    assert_eq!(
        target.first_by_category["compatibility"],
        "initial physical header"
    );
    assert_eq!(
        target.first_by_category["link"],
        "new contextual owner link error"
    );
}
