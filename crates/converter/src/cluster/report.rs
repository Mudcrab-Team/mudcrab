//! Functional results of the cluster stage, carried in the pipeline report.
//! The stage does not write a separate report file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageCounts {
    pub references_examined: u64,
    pub references_eligible: u64,
    pub models: u64,
    pub models_loaded: u64,
    pub models_known_from_previous: u64,
    pub models_missing_from_manifest: u64,
    pub models_failed_to_load: u64,
    pub primitives: u64,
    pub primitives_clustered: u64,
    pub meshes_built: u64,
    pub meshes_reused: u64,
    pub clusters: u64,
    pub groups: u64,
    pub source_triangles_clustered: u64,
    pub monotonic_fixups: u64,
    pub instances: u64,
    pub blocks: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageBytes {
    pub mesh_payloads: u64,
    pub instance_payloads: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageReport {
    pub requested_mode: String,
    /// The mode the stage ran in after the probe: `cluster` or `baked`.
    pub mode: String,
    pub probe: Option<serde_json::Value>,
    pub builder_identity: String,
    pub triangle_threshold: u32,
    pub jobs: usize,
    pub counts: StageCounts,
    pub bytes: StageBytes,
    /// First failing reference rule, counted.
    pub reference_rejects: BTreeMap<String, u64>,
    /// Primitive reject reasons, counted (what `cluster_rejects` holds).
    pub primitive_rejects: BTreeMap<String, u64>,
    pub notices: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_stage_report_contains_functional_counts_without_benchmark_estimates() {
        let mut report = StageReport::default();
        report.counts.meshes_built = 3;
        report.bytes.mesh_payloads = 17;
        report.notices.push("fixture notice".into());
        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value["counts"]["meshes_built"], 3);
        assert_eq!(value["bytes"]["mesh_payloads"], 17);
        assert_eq!(value["notices"][0], "fixture notice");
        for key in ["timings", "bench_baked", "peak_working_set_bytes"] {
            assert!(value.get(key).is_none(), "unexpected benchmark field {key}");
        }
        for key in value["counts"].as_object().unwrap().keys() {
            assert!(!key.starts_with("baked_"), "unexpected baked count {key}");
        }
        assert!(value["bytes"].get("baked_chunks").is_none());
    }
}
