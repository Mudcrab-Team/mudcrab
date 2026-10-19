//! Assertions shared by synthetic publication fixtures with reader recovery warnings.
use converter::PipelineReport;
use std::{fs, path::Path};

/// Reader recovery publishes usable assets without certifying complete record data.
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
