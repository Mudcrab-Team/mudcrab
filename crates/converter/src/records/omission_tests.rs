//! Intended omissions are listed apart from reader failures (#811).
use super::*;
use crate::{cache::IntendedOmission, cache::merge_intended_omissions};

fn context(diagnostics: &mut PluginDiagnostics, id: u32) {
    diagnostics.set_record(24, Some(*b"LAND"), Some(id), Some(id), "admission");
}

/// Only the height-less LAND is an intended omission; every other rejection stays a failure.
#[test]
fn only_a_missing_vhgt_is_an_intended_omission() {
    let missing = format!(
        "LAND 000B2797: unusable runtime candidate: {}",
        crate::esm::cell_cache::LAND_WITHOUT_HEIGHTS
    );
    assert_eq!(
        intended_omission_class(&missing),
        Some("land_missing_heights")
    );
    for failure in [
        "incomplete or non-finite authored VHGT",
        "incomplete VNML",
        "incomplete VCLR",
        "rejected placed base NAME",
        "LAND 00000001: truncated payload",
    ] {
        assert_eq!(intended_omission_class(failure), None, "{failure}");
    }
}

/// An omission is counted and journaled, but it adds no warning and no skipped record.
#[test]
fn an_omission_is_counted_apart_from_failures() {
    let mut diagnostics = PluginDiagnostics::default();
    context(&mut diagnostics, 0x20);
    diagnostics.note_omission("land_missing_heights", || "LAND 00000020: no VHGT".into());
    context(&mut diagnostics, 0x10);
    diagnostics.note_omission("land_missing_heights", || "LAND 00000010: no VHGT".into());

    assert_eq!(diagnostics.omitted_records, 2);
    assert_eq!(diagnostics.skipped_records, 0);
    assert!(diagnostics.warning_summary_lines("a.esm").is_empty());
    let stat = &diagnostics.intended_omissions["LAND"]["land_missing_heights"];
    assert_eq!(stat.count, 2);
    assert_eq!(
        stat.first_example, "LAND 00000010: no VHGT",
        "least example"
    );
    let notices = diagnostics.notice_summary_lines("a.esm");
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("2 LAND record(s)") && notices[0].contains("not a read failure"));
    let rows = diagnostics.intended_omission_rows("a.esm");
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].source.as_str(), rows[0].count), ("a.esm", 2));

    // A real failure on the same plugin still warns.
    context(&mut diagnostics, 0x30);
    diagnostics.note("record", || "LAND 00000030: truncated payload".into());
    assert_eq!(diagnostics.skipped_records, 1);
    assert_eq!(diagnostics.warning_summary_lines("a.esm").len(), 1);
}

#[test]
fn equal_rows_merge_by_summing_counts_and_keeping_the_least_example() {
    let row = |source: &str, count: u64, example: &str| IntendedOmission {
        source: source.into(),
        record_type: "LAND".into(),
        class: "land_missing_heights".into(),
        count,
        first_example: example.into(),
    };
    let mut rows = vec![
        row("b.esm", 1, "z"),
        row("a.esm", 5, "m"),
        row("a.esm", 2, "c"),
    ];
    merge_intended_omissions(&mut rows);
    assert_eq!(rows, vec![row("a.esm", 7, "c"), row("b.esm", 1, "z")]);
}
