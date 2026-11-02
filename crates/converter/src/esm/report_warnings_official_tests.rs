//! Opt-in legacy-reader report control over authenticated official inputs, without assets.
use super::{EsmParser, exporter::validate_database, load_order::LoadOrder, plugins_txt};
use crate::{
    config::RecordReader,
    pipeline::{PipelineReport, conversion_is_complete},
};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Require explicit input/selection/output authorities rather than inspecting an ambient install.
fn configured_path(name: &str) -> PathBuf {
    std::env::var_os(format!("MUDCRAB_REPORT_{name}"))
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set MUDCRAB_REPORT_{name}"))
}

/// Resolve a fresh retained destination disjoint from the read-only source inputs.
fn fresh_output(data: &Path, list: &Path) -> PathBuf {
    let requested = configured_path("OUTPUT");
    assert!(requested.is_absolute(), "retained output must be absolute");
    let output = fs::canonicalize(requested.parent().expect("output parent"))
        .unwrap()
        .join(requested.file_name().expect("output name"));
    assert!(
        !output.exists(),
        "retain prior output; select a fresh directory"
    );
    assert!(!output.starts_with(data) && !data.starts_with(&output));
    assert!(!list.starts_with(&output));
    output
}

/// Classify retained warning and notice lines for a compact receipt without dropping either.
fn reader_summary_kind(message: &str) -> &'static str {
    if message.contains("malformed record/frame") {
        "malformed_record_frame"
    } else if message.contains("light-plugin ID occurrences") {
        "truncated_light_ids"
    } else if message.contains("master index past") {
        "out_of_range_master_indices"
    } else if message.contains("invalid optional FormID links") {
        "invalid_optional_links"
    } else if message.contains("unusable record candidate(s)") {
        "unusable_record_candidates"
    } else {
        "other"
    }
}

/// Preserve actual DB/report observations before checking compatibility and missing-key notices.
/// Input hashing and retained-output ownership are enforced by the owning execution receipt.
/// This exercises reader export plus the shared completeness rule, not a full asset pipeline.
/// Optional MUDCRAB_REPORT_STRINGS points to banks extracted outside the read-only Data folder.
#[test]
#[ignore = "requires explicit MUDCRAB_REPORT_DATA, PLUGINS and fresh retained OUTPUT"]
fn official_legacy_reader_report_has_no_warnings_and_is_complete() {
    let data = fs::canonicalize(configured_path("DATA")).unwrap();
    let list = fs::canonicalize(configured_path("PLUGINS")).unwrap();
    let output = fresh_output(&data, &list);
    let plugins = plugins_txt::read_plugins_txt(&list, &data).unwrap();
    assert!(
        !plugins.is_empty(),
        "this control requires selected plugins"
    );
    let order = LoadOrder::read(&plugins).unwrap();
    let strings = std::env::var_os("MUDCRAB_REPORT_STRINGS")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.clone());
    fs::create_dir(&output).unwrap();
    let database_path = output.join("skyrim_world.db");
    let conversion = EsmParser::convert_plugins_with_reader_and_schemas(
        &plugins,
        &database_path,
        RecordReader::Legacy,
        &strings,
        crate::config::DEFAULT_LANGUAGE,
        None,
    )
    .unwrap();
    let winning_records = conversion.records.len();
    drop(conversion.records);
    let database = Connection::open(&database_path).unwrap();
    validate_database(&database).unwrap();
    let database_plugins: u64 = database
        .query_row("SELECT count(*) FROM plugins", [], |row| row.get(0))
        .unwrap();
    let database_records: u64 = database
        .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
        .unwrap();
    let mut report = PipelineReport {
        warnings: conversion.warnings,
        notices: conversion.notices,
        artifacts: vec![PathBuf::from("skyrim_world.db")],
        ..Default::default()
    };
    report.complete = conversion_is_complete(&report);
    let load_order: Vec<_> = order
        .names
        .iter()
        .enumerate()
        .map(|(priority, name)| {
            serde_json::json!({
                "priority": priority,
                "name": name,
                "flags": order.metadata[priority].flags,
                "full_slot": order.normal.get(name),
                "light_slot": order.light.get(name),
            })
        })
        .collect();
    let mut warning_counts = BTreeMap::<&str, u64>::new();
    for warning in &report.warnings {
        *warning_counts
            .entry(reader_summary_kind(warning))
            .or_default() += 1;
    }
    let mut notice_counts = BTreeMap::<&str, u64>::new();
    for notice in &report.notices {
        *notice_counts
            .entry(reader_summary_kind(notice))
            .or_default() += 1;
    }
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    fs::write(
        output.join("load-order.json"),
        serde_json::to_vec_pretty(&load_order).unwrap(),
    )
    .unwrap();
    fs::write(
        output.join("reader-report-control.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "scope": "legacy-reader records/DB export and shared report completeness only",
            "data": data,
            "strings": strings,
            "plugins_file": list,
            "selected_plugins": plugins.len(),
            "database_plugins": database_plugins,
            "winning_records": winning_records,
            "database_records": database_records,
            "warning_count": report.warnings.len(),
            "warning_kind_counts": warning_counts,
            "notice_count": report.notices.len(),
            "notice_kind_counts": notice_counts,
            "complete": report.complete,
            "warnings": report.warnings,
            "notices": report.notices,
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(database_plugins as usize, plugins.len());
    assert_eq!(database_records, winning_records as u64);
    assert!(database_records > 0);
    assert!(
        report.warnings.is_empty(),
        "official control warnings: {:?}",
        report.warnings
    );
    assert_eq!(
        report.notices,
        [
            "notice: dawnguard.esm: 1 FormID occurrences name a master index past the plugin's master list and were treated as the plugin's own (first: 0307B5B9 in ACTI)",
            "notice: skyrim.esm: 1 FormID occurrences name a master index past the plugin's master list and were treated as the plugin's own (first: 0123C00E in GMST)",
            "notice: skyrim.esm: 5 localized IDs missing from readable string tables (first: 0003C7E3 FNAM missing localized ID 17)",
        ],
        "official control must retain compatibility and readable-bank missing-key notices"
    );
    assert!(
        report.complete,
        "official zero-warning completeness control failed"
    );
}
