//! Assertions shared by synthetic publication fixtures with reader warnings or notices.
use converter::PipelineReport;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Keep deterministic pipeline fixtures independent of GPU availability.
// Each test crate uses only some of these helpers.
#[allow(dead_code)]
pub fn cpu_lod_config(
    data_dir: impl Into<PathBuf>,
    output_dir: impl Into<PathBuf>,
) -> converter::PipelineConfig {
    let mut config = converter::PipelineConfig::new(data_dir, output_dir);
    config.lod_texture_encoder = converter::TextureEncoder::Cpu;
    config
}

/// Reader recovery publishes usable assets without certifying complete record data.
#[allow(dead_code)]
pub fn assert_reader_warning_publication(report: &PipelineReport, output: &Path) {
    assert!(
        !report.complete,
        "reader loss must remain incomplete: {report:?}"
    );
    assert_eq!(
        report.skipped, 0,
        "reader loss is not an asset failure: {report:?}"
    );
    assert!(
        !report.warnings.is_empty(),
        "missing reader recovery warning"
    );
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| warning.contains("in-house reader")
                || warning.contains("inhouse adapter")),
        "unexpected non-reader failure: {report:?}"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["complete"], false);
    assert_eq!(manifest["record_data_complete"], false);
    assert!(manifest["failures"].as_object().unwrap().is_empty());
    for artifact in ["skyrim_world.db", "cell_cache.rkyv", "record-reader.json"] {
        assert!(output.join(artifact).is_file(), "unpublished {artifact}");
    }
}

/// Faithful reader outcomes publish complete assets and retain bounded diagnostics.
#[allow(
    dead_code,
    reason = "Each integration target selects its expected reader severity."
)]
pub fn assert_reader_notice_publication(report: &PipelineReport, output: &Path) {
    assert!(report.complete, "faithful reader outcome: {report:?}");
    assert_eq!(report.skipped, 0, "reader notice is not an asset failure");
    assert!(report.warnings.is_empty(), "{report:?}");
    assert!(
        report.notices.iter().any(|notice| {
            notice.contains("in-house reader")
                || notice.contains("localized IDs missing from readable string tables")
        }),
        "missing reader notice: {report:?}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["complete"], true);
    assert_eq!(manifest["record_data_complete"], true);
    assert!(manifest["failures"].as_object().unwrap().is_empty());

    for artifact in ["skyrim_world.db", "cell_cache.rkyv", "record-reader.json"] {
        assert!(output.join(artifact).is_file(), "unpublished {artifact}");
    }
    for artifact in ["record-reader.json", "inhouse-reader-diagnostics.json"] {
        assert!(
            report.artifacts.contains(&PathBuf::from(artifact)),
            "reader report omitted {artifact}: {report:?}"
        );
    }

    let reader: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("record-reader.json")).unwrap()).unwrap();
    assert_eq!(reader["mode"], "inhouse");
    assert!(reader["schema_sha256"].as_str().is_some());

    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    for section in ["decoder", "adapter", "adapter_notices"] {
        assert!(
            diagnostics[section].is_object(),
            "reader diagnostics omitted {section}: {diagnostics}"
        );
    }
}
