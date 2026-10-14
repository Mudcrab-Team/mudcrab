//! Optional observations of fixed streaming behavior, without retaining asset handles.
//!
//! The caller owns frame ordering and enables collection explicitly. Scene IDs identify
//! observed scene assets, not files, decode jobs, dependencies, or drawable placements.

use crate::streaming::StreamingMetrics;
use bevy::{asset::AssetId, prelude::Resource, world_serialization::WorldAsset};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BenchmarkWindow {
    Warmup,
    Measured,
    Settlement,
    OutsideBenchmark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CameraMotion {
    Stationary,
    Translated,
    Rotated,
    TranslatedAndRotated,
    /// No previous comparable camera observation exists.
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CameraObservation {
    pub worldspace_id: u32,
    pub render_origin_grid: [i32; 2],
    /// Stable runtime coordinates (Y-up) supplied by the caller, not rebased local coordinates.
    pub world_position: [f64; 3],
    pub motion: CameraMotion,
}

/// Snapshot fields retain the meanings of the corresponding fixed-mode metrics.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct StreamingSnapshot {
    pub loading_cells: usize,
    pub resident_cells: usize,
    pub retiring_cells: usize,
    pub active_cell_requests: usize,
    pub pending_lod_queries: usize,
    pub pending_lod_chunks: usize,
    pub resident_lod_chunks: usize,
    pub arming_queue_depth: usize,
    pub pending_model_placements: usize,
    pub pending_surface_instances: usize,
    pub instances_armed_this_frame: u64,
    pub instances_spawned_this_frame: u64,
    pub instances_completed_this_scan: u64,
    pub cells_despawned_this_frame: u64,
    pub lifetime_cpu_validated_model_placements: u64,
    pub lifetime_asset_load_failures: u64,
    pub lifetime_terrain_patches_validated: u64,
    pub lifetime_water_surfaces_validated: u64,
    pub resident_static_colliders: usize,
}

impl From<&StreamingMetrics> for StreamingSnapshot {
    fn from(value: &StreamingMetrics) -> Self {
        Self {
            loading_cells: value.loading_cells,
            resident_cells: value.resident_cells,
            retiring_cells: value.retiring_cells,
            active_cell_requests: value.active_requests,
            pending_lod_queries: value.pending_lod_queries,
            pending_lod_chunks: value.pending_lod_chunks,
            resident_lod_chunks: value.resident_lod_chunks,
            arming_queue_depth: value.arming_queue_depth,
            pending_model_placements: value.pending_asset_instances,
            pending_surface_instances: value.pending_surface_instances,
            instances_armed_this_frame: value.instances_armed_this_frame,
            instances_spawned_this_frame: value.instances_spawned_this_frame,
            instances_completed_this_scan: value.instances_completed_this_scan,
            cells_despawned_this_frame: value.despawns_this_frame,
            lifetime_cpu_validated_model_placements: value.assets_ready,
            lifetime_asset_load_failures: value.asset_load_failures,
            lifetime_terrain_patches_validated: value.terrain_patches_validated,
            lifetime_water_surfaces_validated: value.water_surfaces_validated,
            resident_static_colliders: value.resident_static_colliders,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SceneLoadStatus {
    Loading,
    CpuLoaded,
    Failed,
}

#[derive(Debug, Clone, Copy)]
struct SceneRecord {
    active: bool,
    status: SceneLoadStatus,
    ever_cpu_loaded: bool,
    ever_failed: bool,
}

/// Active counts cover only retained IDs whose current load state was observed.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct SceneCounts {
    pub lifetime_unique_observed: usize,
    pub lifetime_unique_cpu_loaded: usize,
    pub lifetime_unique_failed: usize,
    pub active_loading: usize,
    pub active_cpu_loaded: usize,
    pub active_failed: usize,
    pub inactive_observed: usize,
    pub identity_tracking_complete: bool,
    /// Calls rejected by the full ledger; these are not necessarily distinct IDs.
    pub untracked_observation_calls: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StreamingFrameSample {
    pub main_frame: u64,
    pub elapsed_ms: f64,
    /// Actual frame delta, including warmup; not an average or a synthesized interval.
    pub delta_ms: f64,
    pub benchmark_window: BenchmarkWindow,
    pub camera: Option<CameraObservation>,
    pub streaming: Option<StreamingSnapshot>,
    /// Populated by `push_frame` from the bounded scene ledger.
    pub scenes: SceneCounts,
    pub process_memory_gib: Option<f64>,
    /// Actual durations recorded within this main frame; repeated names are summed.
    pub cpu_spans_ms: BTreeMap<String, f64>,
    /// Sums reported completed latencies by name, not CPU cost in the enclosing frame.
    pub completion_latencies_ms: BTreeMap<String, f64>,
    /// Sampled render-world aggregates retain their own source-frame identifiers.
    pub gpu: Option<crate::streaming_gpu_metrics::GpuPreparationObservation>,
}

#[derive(Resource)]
pub(crate) struct StreamingTrace {
    sample_capacity: usize,
    scene_capacity: usize,
    samples: Vec<StreamingFrameSample>,
    dropped_samples: u64,
    scenes: HashMap<AssetId<WorldAsset>, SceneRecord>,
    untracked_observation_calls: u64,
    gpu_inventory_enabled: bool,
}

#[derive(Serialize)]
pub(crate) struct StreamingTraceReport<'a> {
    pub format_version: u32,
    pub gpu_inventory_enabled: bool,
    pub gpu_inventory_interval_frames: u64,
    pub sample_capacity: usize,
    pub scene_capacity: usize,
    pub dropped_samples: u64,
    pub scenes: SceneCounts,
    pub samples: &'a [StreamingFrameSample],
}

impl StreamingTrace {
    /// Construct only when the caller explicitly enables the trace.
    pub(crate) fn new(sample_capacity: usize, scene_capacity: usize) -> Self {
        Self {
            sample_capacity,
            scene_capacity,
            samples: Vec::new(),
            dropped_samples: 0,
            scenes: HashMap::new(),
            untracked_observation_calls: 0,
            gpu_inventory_enabled: false,
        }
    }

    pub(crate) fn set_gpu_inventory_enabled(&mut self, enabled: bool) {
        self.gpu_inventory_enabled = enabled;
    }

    /// Observe an existing request, without requesting or retaining the asset.
    /// Returns false if this ID could not enter the bounded lifetime ledger.
    pub(crate) fn observe_scene(&mut self, id: AssetId<WorldAsset>) -> bool {
        if let Some(record) = self.scenes.get_mut(&id) {
            if !record.active {
                record.active = true;
                record.status = SceneLoadStatus::Loading;
            }
            return true;
        }
        if self.scenes.len() >= self.scene_capacity {
            self.untracked_observation_calls = self.untracked_observation_calls.saturating_add(1);
            return false;
        }
        self.scenes.insert(
            id,
            SceneRecord {
                active: true,
                status: SceneLoadStatus::Loading,
                ever_cpu_loaded: false,
                ever_failed: false,
            },
        );
        true
    }

    /// Update only an already-observed ID. CPU-loaded means recursive dependencies loaded.
    pub(crate) fn update_scene(&mut self, id: AssetId<WorldAsset>, status: SceneLoadStatus) {
        if let Some(record) = self.scenes.get_mut(&id) {
            record.active = true;
            record.status = status;
            record.ever_cpu_loaded |= status == SceneLoadStatus::CpuLoaded;
            record.ever_failed |= status == SceneLoadStatus::Failed;
        }
    }

    /// Mark an absent load state inactive; this does not assert that memory was freed.
    pub(crate) fn remove_scene(&mut self, id: AssetId<WorldAsset>) {
        if let Some(record) = self.scenes.get_mut(&id) {
            record.active = false;
        }
    }

    /// Inspect bounded lifetime identity retention in tests.
    #[cfg(test)]
    pub(crate) fn scene_ids(&self) -> impl Iterator<Item = AssetId<WorldAsset>> + '_ {
        self.scenes.keys().copied()
    }

    /// Poll only active unfinished scenes to catch recursive dependency failures.
    pub(crate) fn pending_scene_ids(&self) -> impl Iterator<Item = AssetId<WorldAsset>> + '_ {
        self.scenes.iter().filter_map(|(&id, record)| {
            (record.active && record.status == SceneLoadStatus::Loading).then_some(id)
        })
    }

    pub(crate) fn scene_counts(&self) -> SceneCounts {
        let mut counts = SceneCounts {
            lifetime_unique_observed: self.scenes.len(),
            identity_tracking_complete: self.untracked_observation_calls == 0,
            untracked_observation_calls: self.untracked_observation_calls,
            ..Default::default()
        };
        for record in self.scenes.values() {
            counts.lifetime_unique_cpu_loaded += usize::from(record.ever_cpu_loaded);
            counts.lifetime_unique_failed += usize::from(record.ever_failed);
            if !record.active {
                counts.inactive_observed += 1;
            } else {
                match record.status {
                    SceneLoadStatus::Loading => counts.active_loading += 1,
                    SceneLoadStatus::CpuLoaded => counts.active_cpu_loaded += 1,
                    SceneLoadStatus::Failed => counts.active_failed += 1,
                }
            }
        }
        counts
    }

    pub(crate) fn push_frame(&mut self, mut sample: StreamingFrameSample) {
        if self.samples.len() >= self.sample_capacity {
            self.dropped_samples = self.dropped_samples.saturating_add(1);
            return;
        }
        sample.scenes = self.scene_counts();
        self.samples.push(sample);
    }

    pub(crate) fn report(&self) -> StreamingTraceReport<'_> {
        StreamingTraceReport {
            format_version: 1,
            gpu_inventory_enabled: self.gpu_inventory_enabled,
            gpu_inventory_interval_frames:
                crate::streaming_gpu_metrics::GPU_INVENTORY_INTERVAL_FRAMES,
            sample_capacity: self.sample_capacity,
            scene_capacity: self.scene_capacity,
            dropped_samples: self.dropped_samples,
            scenes: self.scene_counts(),
            samples: &self.samples,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::prelude::Assets;

    fn frame(main_frame: u64, benchmark_window: BenchmarkWindow) -> StreamingFrameSample {
        StreamingFrameSample {
            main_frame,
            elapsed_ms: main_frame as f64 * 20.0,
            delta_ms: 20.0,
            benchmark_window,
            camera: None,
            streaming: None,
            scenes: SceneCounts::default(),
            process_memory_gib: None,
            cpu_spans_ms: BTreeMap::new(),
            completion_latencies_ms: BTreeMap::new(),
            gpu: None,
        }
    }

    #[test]
    #[ignore = "manual CPU-only ledger diagnostic; timings are not an acceptance gate"]
    fn measures_bounded_ledger_collection_cost() {
        use std::{hint::black_box, time::Instant};

        const FRAMES: usize = 2_000;
        const REPEATS: usize = 7;
        for scene_count in [0, 442, 20_000] {
            let assets = Assets::<WorldAsset>::default();
            let handles: Vec<_> = (0..scene_count).map(|_| assets.reserve_handle()).collect();
            let mut baseline_ns = Vec::new();
            let mut collection_ns = Vec::new();
            for repeat in 0..REPEATS {
                let mut trace = StreamingTrace::new(FRAMES, 20_000);
                for handle in &handles {
                    trace.observe_scene(handle.id());
                    trace.update_scene(handle.id(), SceneLoadStatus::CpuLoaded);
                }
                let baseline = || {
                    let started = Instant::now();
                    for index in 0..FRAMES {
                        black_box(index);
                    }
                    started.elapsed().as_nanos() as f64 / FRAMES as f64
                };
                // Alternate order to reduce consistent warm-cache or scheduling bias.
                if repeat % 2 == 0 {
                    baseline_ns.push(baseline());
                }
                let started = Instant::now();
                for index in 0..FRAMES {
                    black_box(trace.pending_scene_ids().count());
                    trace.push_frame(frame(index as u64, BenchmarkWindow::Measured));
                }
                collection_ns.push(started.elapsed().as_nanos() as f64 / FRAMES as f64);
                if repeat % 2 != 0 {
                    baseline_ns.push(baseline());
                }
                assert_eq!(trace.report().samples.len(), FRAMES);
                assert_eq!(trace.report().scenes.lifetime_unique_observed, scene_count);
                assert_eq!(trace.report().dropped_samples, 0);
                black_box(trace);
            }
            println!(
                "{}",
                serde_json::json!({
                    "scope": "CPU-only loaded-scene ledger scan and frame append; excludes asset-server queries, ECS, GPU, serialization and retail rendering",
                    "scene_count": scene_count, "frames_per_repeat": FRAMES,
                    "baseline_ns_per_frame": baseline_ns,
                    "collection_ns_per_frame": collection_ns,
                    "performance_acceptance": false,
                })
            );
        }
    }

    #[test]
    fn keeps_startup_samples_and_reports_truncation() {
        let mut trace = StreamingTrace::new(2, 1);
        trace.push_frame(frame(1, BenchmarkWindow::Warmup));
        trace.push_frame(frame(2, BenchmarkWindow::Measured));
        trace.push_frame(frame(3, BenchmarkWindow::Settlement));
        let report = trace.report();
        assert_eq!(report.samples.len(), 2);
        assert_eq!(report.samples[0].main_frame, 1);
        assert_eq!(
            report.samples[1].benchmark_window,
            BenchmarkWindow::Measured
        );
        assert_eq!(report.dropped_samples, 1);
        let mut zero = StreamingTrace::new(0, 0);
        zero.push_frame(frame(1, BenchmarkWindow::OutsideBenchmark));
        assert!(zero.report().samples.is_empty());
        assert_eq!(zero.report().dropped_samples, 1);
    }

    #[test]
    fn shared_placements_and_reload_do_not_duplicate_lifetime_scene_counts() {
        let id = AssetId::<WorldAsset>::default();
        let mut trace = StreamingTrace::new(4, 1);
        assert!(trace.observe_scene(id));
        assert!(trace.observe_scene(id)); // A second placement shares this scene.
        assert_eq!(trace.pending_scene_ids().collect::<Vec<_>>(), vec![id]);
        trace.update_scene(id, SceneLoadStatus::Failed);
        assert_eq!(trace.scene_counts().active_failed, 1);
        assert_eq!(trace.pending_scene_ids().count(), 0);
        trace.update_scene(id, SceneLoadStatus::Loading);
        assert_eq!(trace.pending_scene_ids().count(), 1);
        trace.update_scene(id, SceneLoadStatus::CpuLoaded);
        assert_eq!(trace.pending_scene_ids().count(), 0);
        trace.remove_scene(id);
        assert_eq!(trace.pending_scene_ids().count(), 0);
        assert_eq!(trace.scene_counts().inactive_observed, 1);
        assert_eq!(trace.scene_counts().active_cpu_loaded, 0);
        assert!(trace.observe_scene(id));
        assert_eq!(trace.scene_counts().active_loading, 1);
        assert_eq!(trace.pending_scene_ids().count(), 1);
        trace.update_scene(id, SceneLoadStatus::CpuLoaded);
        let counts = trace.scene_counts();
        assert_eq!(counts.lifetime_unique_observed, 1);
        assert_eq!(counts.lifetime_unique_cpu_loaded, 1);
        assert_eq!(counts.lifetime_unique_failed, 1);
        assert_eq!(counts.active_cpu_loaded, 1);
        assert_eq!(counts.active_failed, 0);
    }

    #[test]
    fn full_identity_ledger_remains_bounded_and_marks_counts_incomplete() {
        let assets = Assets::<WorldAsset>::default();
        let first = assets.reserve_handle();
        let second = assets.reserve_handle();
        let mut trace = StreamingTrace::new(2, 1);
        assert!(trace.observe_scene(first.id()));
        assert!(!trace.observe_scene(second.id()));
        assert!(!trace.observe_scene(second.id()));
        trace.update_scene(second.id(), SceneLoadStatus::CpuLoaded);
        trace.remove_scene(first.id());
        // Inactive lifetime records are not evicted: that would corrupt unique counts.
        assert!(!trace.observe_scene(second.id()));
        assert_eq!(trace.scene_ids().count(), 1);
        trace.push_frame(frame(1, BenchmarkWindow::Warmup));
        let counts = &trace.report().samples[0].scenes;
        assert!(!counts.identity_tracking_complete);
        assert_eq!(counts.untracked_observation_calls, 3);
        assert_eq!(counts.lifetime_unique_observed, 1);
        assert_eq!(counts.lifetime_unique_cpu_loaded, 0);
    }

    #[test]
    fn sidecar_schema_preserves_unavailable_data_and_window_labels() {
        let mut trace = StreamingTrace::new(4, 0);
        for (index, window) in [
            BenchmarkWindow::Warmup,
            BenchmarkWindow::Measured,
            BenchmarkWindow::Settlement,
            BenchmarkWindow::OutsideBenchmark,
        ]
        .into_iter()
        .enumerate()
        {
            trace.push_frame(frame(index as u64 + 1, window));
        }
        let value = serde_json::to_value(trace.report()).unwrap();
        assert_eq!(value["format_version"], 1);
        assert_eq!(value["samples"][0]["benchmark_window"], "warmup");
        assert_eq!(value["samples"][1]["benchmark_window"], "measured");
        assert_eq!(value["samples"][2]["benchmark_window"], "settlement");
        assert_eq!(value["samples"][3]["benchmark_window"], "outside_benchmark");
        for field in ["camera", "streaming", "process_memory_gib", "gpu"] {
            assert!(value["samples"][0][field].is_null());
        }
        assert_eq!(value["samples"][0]["delta_ms"], 20.0);
        assert_eq!(value["samples"][0]["cpu_spans_ms"], serde_json::json!({}));
        assert_eq!(
            value["samples"][0]["completion_latencies_ms"],
            serde_json::json!({})
        );
        assert_eq!(
            value["samples"][0]["scenes"]["identity_tracking_complete"],
            true
        );
    }

    #[test]
    fn placement_snapshot_keeps_lifetime_and_current_work_separate() {
        let metrics = StreamingMetrics {
            pending_asset_instances: 12,
            instances_armed_this_frame: 3,
            instances_spawned_this_frame: 2,
            instances_completed_this_scan: 1,
            assets_ready: 50,
            asset_load_failures: 4,
            terrain_patches_validated: 20,
            water_surfaces_validated: 5,
            resident_static_colliders: 8,
            ..Default::default()
        };
        let snapshot = StreamingSnapshot::from(&metrics);
        assert_eq!(snapshot.pending_model_placements, 12);
        assert_eq!(snapshot.instances_armed_this_frame, 3);
        assert_eq!(snapshot.instances_spawned_this_frame, 2);
        assert_eq!(snapshot.instances_completed_this_scan, 1);
        assert_eq!(snapshot.lifetime_cpu_validated_model_placements, 50);
        assert_eq!(snapshot.lifetime_asset_load_failures, 4);
        assert_eq!(snapshot.lifetime_terrain_patches_validated, 20);
        assert_eq!(snapshot.lifetime_water_surfaces_validated, 5);
        assert_eq!(snapshot.resident_static_colliders, 8);
    }
}
