use crate::{
    config::EngineConfig,
    render::RendererMetrics,
    streaming::StreamingMetrics,
    streaming_gpu_metrics::{StreamingGpuMetricsBridge, StreamingGpuMetricsPlugin},
    streaming_trace::{SceneLoadStatus, StreamingTrace},
};
use bevy::{
    asset::{AssetId, DependencyLoadState, LoadState, RecursiveDependencyLoadState},
    diagnostic::DiagnosticsStore,
    platform::time::Instant as DiagnosticInstant,
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        render_resource::WgpuFeatures,
        renderer::{RenderAdapterInfo, RenderDevice},
        view::window::{ExtractedWindows, prepare_windows},
    },
    time::TimeSystems,
    window::PrimaryWindow,
    world_serialization::WorldAsset,
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const MAX_SAMPLES_PER_METRIC: usize = 200_000;
const MAX_TIMELINE_EVENTS: usize = 100_000;
const MAX_STREAMING_FRAMES: usize = 20_000;
const MAX_STREAMING_SCENES: usize = 20_000;

pub struct ProfilingPlugin;

impl Plugin for ProfilingPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(crate::scene_evidence::SceneEvidencePlugin);
        app.init_resource::<ProfilingState>()
            .add_systems(Update, (sample_profile_metadata, sample_scene_inventory));
        // Requested bundles also record whether each primary drawable was acquired. This keeps
        // occluded or interrupted captures from being accepted as ordinary renderer timings.
        let drawable_warmup = app
            .world()
            .get_resource::<EngineConfig>()
            .and_then(|config| {
                config
                    .profile_output_dir
                    .as_ref()
                    .map(|_| config.benchmark_warmup_frames)
            });
        if let Some(warmup_frames) = drawable_warmup
            && let Some(render_app) = app.get_sub_app_mut(RenderApp)
        {
            render_app
                .insert_resource(ProfilePrimaryDrawableProof::new(warmup_frames))
                .add_systems(
                    Render,
                    sample_primary_drawable
                        .after(prepare_windows)
                        .before(RenderSystems::Render),
                );
        }
        if std::env::var("MUDCRAB_PROFILE_DRAW_COUNTS").is_ok_and(|value| value == "1") {
            app.add_plugins(crate::indirect_metrics::IndirectDrawMetricsPlugin);
        }
        if app
            .world()
            .get_resource::<EngineConfig>()
            .is_some_and(|config| config.profile_output_dir.is_some())
        {
            app.world_mut().resource_mut::<ProfilingState>().trace = Some(StreamingTrace::new(
                MAX_STREAMING_FRAMES,
                MAX_STREAMING_SCENES,
            ));
            if app.world().resource::<EngineConfig>().profile_gpu_inventory {
                app.add_plugins(StreamingGpuMetricsPlugin);
                app.world_mut()
                    .resource_mut::<ProfilingState>()
                    .trace
                    .as_mut()
                    .unwrap()
                    .set_gpu_inventory_enabled(true);
            }
            app.add_message::<AssetEvent<WorldAsset>>()
                .add_systems(First, begin_trace_frame.after(TimeSystems));
        }
    }
}

#[derive(Resource)]
pub struct ProfilingState {
    started: Instant,
    frame: u64,
    cpu_spans_ms: BTreeMap<String, Vec<f64>>,
    render_metrics: BTreeMap<String, Vec<f64>>,
    last_render_measurement: BTreeMap<String, DiagnosticInstant>,
    rejected_render_measurements: BTreeMap<String, u64>,
    render_capabilities: Option<RenderCapabilities>,
    window: Option<WindowMetadata>,
    draw_submission: Option<crate::indirect_metrics::DrawSubmissionReport>,
    draw_submission_frames: Vec<crate::indirect_metrics::DrawSubmissionFrame>,
    dropped_draw_submission_frames: u64,
    counters: BTreeMap<String, u64>,
    gauges: BTreeMap<String, f64>,
    timeline: Vec<TimelineEvent>,
    memory: Vec<MemorySample>,
    pub(crate) trace: Option<StreamingTrace>,
    pub(crate) frame_cpu_spans_ms: BTreeMap<String, f64>,
    pub(crate) frame_completion_latencies_ms: BTreeMap<String, f64>,
    pub(crate) trace_delta_ms: f64,
    pub(crate) capture_window_resolution: Option<[u32; 2]>,
}

impl Default for ProfilingState {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            frame: 0,
            cpu_spans_ms: BTreeMap::new(),
            render_metrics: BTreeMap::new(),
            last_render_measurement: BTreeMap::new(),
            rejected_render_measurements: BTreeMap::new(),
            render_capabilities: None,
            window: None,
            draw_submission: None,
            draw_submission_frames: Vec::new(),
            dropped_draw_submission_frames: 0,
            counters: BTreeMap::new(),
            gauges: BTreeMap::new(),
            timeline: Vec::new(),
            memory: Vec::new(),
            trace: None,
            frame_cpu_spans_ms: BTreeMap::new(),
            frame_completion_latencies_ms: BTreeMap::new(),
            trace_delta_ms: 0.0,
            capture_window_resolution: None,
        }
    }
}

impl ProfilingState {
    pub(crate) fn set_draw_submission(
        &mut self,
        mut report: crate::indirect_metrics::DrawSubmissionReport,
        frames: Vec<crate::indirect_metrics::DrawSubmissionFrame>,
    ) {
        let remaining = crate::indirect_metrics::MAX_DRAW_FRAME_SAMPLES
            .saturating_sub(self.draw_submission_frames.len());
        self.dropped_draw_submission_frames = self
            .dropped_draw_submission_frames
            .saturating_add(frames.len().saturating_sub(remaining) as u64);
        report.dropped_frame_samples = report
            .dropped_frame_samples
            .saturating_add(self.dropped_draw_submission_frames);
        self.draw_submission = Some(report);
        self.draw_submission_frames
            .extend(frames.into_iter().take(remaining));
    }

    pub fn record_elapsed(&mut self, name: impl Into<String>, started: Instant) {
        self.record_ms(name, started.elapsed().as_secs_f64() * 1000.0);
    }

    /// The samples recorded for a span so far, in milliseconds.
    #[cfg(test)]
    pub fn span_samples(&self, name: &str) -> &[f64] {
        self.cpu_spans_ms.get(name).map_or(&[], Vec::as_slice)
    }

    /// Whether a timeline event with exactly this stage was recorded.
    #[cfg(test)]
    pub fn has_event_stage(&self, stage: &str) -> bool {
        self.timeline.iter().any(|event| event.stage == stage)
    }

    pub fn record_micros(&mut self, name: impl Into<String>, micros: u64) {
        self.record_ms(name, micros as f64 / 1000.0);
    }

    /// Attribute a reported asynchronous latency to its arrival frame, not as CPU work.
    pub(crate) fn record_completed_latency_micros(&mut self, name: impl Into<String>, micros: u64) {
        self.record_completed_latency_ms(name.into(), micros as f64 / 1000.0);
    }

    pub(crate) fn record_completed_latency_elapsed(
        &mut self,
        name: impl Into<String>,
        started: Instant,
    ) {
        self.record_completed_latency_ms(name.into(), started.elapsed().as_secs_f64() * 1000.0);
    }

    fn record_completed_latency_ms(&mut self, name: String, value: f64) {
        if self.trace.is_some() {
            *self
                .frame_completion_latencies_ms
                .entry(name.clone())
                .or_default() += value;
        }
        // Preserve the existing aggregate profiling contract.
        push_bounded(self.cpu_spans_ms.entry(name).or_default(), value);
    }

    pub fn record_ms(&mut self, name: impl Into<String>, value: f64) {
        let name = name.into();
        if self.trace.is_some() {
            *self.frame_cpu_spans_ms.entry(name.clone()).or_default() += value;
        }
        push_bounded(self.cpu_spans_ms.entry(name).or_default(), value);
    }

    /// Observe an existing request without retaining its strong handle.
    pub(crate) fn observe_scene(&mut self, id: AssetId<WorldAsset>, server: &AssetServer) {
        if let Some(trace) = self.trace.as_mut()
            && trace.observe_scene(id)
        {
            reconcile_scene(trace, server, id);
        }
    }

    pub(crate) fn main_frame(&self) -> u64 {
        self.frame
    }

    pub(crate) fn elapsed_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    pub fn increment(&mut self, name: impl Into<String>, amount: u64) {
        let counter = self.counters.entry(name.into()).or_default();
        *counter = counter.saturating_add(amount);
    }

    pub(crate) fn terrain_gauges(&self) -> BTreeMap<String, f64> {
        self.gauges
            .iter()
            .filter(|(name, _)| name.starts_with("lod/"))
            .map(|(name, value)| (name.clone(), *value))
            .collect()
    }

    pub(crate) fn gauge(&self, name: &str) -> Option<f64> {
        self.gauges.get(name).copied()
    }

    pub fn set_gauge(&mut self, name: impl Into<String>, value: f64) {
        self.gauges.insert(name.into(), value);
    }

    pub fn event(
        &mut self,
        subject: impl Into<String>,
        stage: impl Into<String>,
        value_ms: Option<f64>,
    ) {
        if self.timeline.len() >= MAX_TIMELINE_EVENTS {
            return;
        }
        self.timeline.push(TimelineEvent {
            elapsed_ms: self.started.elapsed().as_secs_f64() * 1000.0,
            frame: self.frame,
            subject: subject.into(),
            stage: stage.into(),
            value_ms,
        });
    }

    pub fn sample_frame(
        &mut self,
        diagnostics: &DiagnosticsStore,
        process_memory_gib: Option<f64>,
    ) {
        if self.trace.is_none() {
            self.frame = self.frame.saturating_add(1);
        }
        for diagnostic in diagnostics.iter() {
            let path = diagnostic.path().as_str();
            if !diagnostic.is_enabled || !path.starts_with("render/") {
                continue;
            }
            let last_sample = self.last_render_measurement.get(path).copied();
            let Some(latest) = diagnostic.measurement() else {
                continue;
            };
            if last_sample.is_some_and(|time| latest.time <= time) {
                continue;
            }
            // Render diagnostics arrive asynchronously. A value can remain unchanged for
            // several main frames, or several new measurements can arrive between samples.
            for measurement in diagnostic
                .measurements()
                .filter(|measurement| last_sample.is_none_or(|time| measurement.time > time))
            {
                if measurement.value.is_finite() {
                    push_bounded(
                        self.render_metrics.entry(path.to_owned()).or_default(),
                        measurement.value,
                    );
                } else {
                    let rejected = self
                        .rejected_render_measurements
                        .entry(path.to_owned())
                        .or_default();
                    *rejected = rejected.saturating_add(1);
                }
            }
            self.last_render_measurement
                .insert(path.to_owned(), latest.time);
        }
        if self.frame.is_multiple_of(60)
            && let Some(process_memory_gib) = process_memory_gib
        {
            self.memory.push(MemorySample {
                elapsed_seconds: self.started.elapsed().as_secs_f64(),
                process_gib: process_memory_gib,
            });
        }
    }

    pub fn write_bundle(
        &self,
        config: &EngineConfig,
        frame_metrics: &serde_json::Value,
        streaming: Option<&StreamingMetrics>,
        renderer: &RendererMetrics,
        system: Option<SystemMetadata>,
    ) -> std::io::Result<()> {
        let Some(root) = &config.profile_output_dir else {
            return Ok(());
        };
        fs::create_dir_all(root)?;
        let cpu: BTreeMap<_, _> = self
            .cpu_spans_ms
            .iter()
            .map(|(name, samples)| (name.clone(), summarize(samples)))
            .collect();
        let render: BTreeMap<_, _> = self
            .render_metrics
            .iter()
            .map(|(name, samples)| (name.clone(), summarize(samples)))
            .collect();
        let mut gpu = gpu_profile(render.clone(), self.render_capabilities.clone());
        gpu.rejected_nonfinite_measurements = self.rejected_render_measurements.clone();
        write_json(
            &root.join("metadata.json"),
            &Metadata {
                format_version: 2,
                engine_version: env!("CARGO_PKG_VERSION"),
                bevy_version: "0.19",
                generated_unix_ms: unix_ms(),
                scenario: config.profile_scenario.clone(),
                run_id: config.profile_run_id.clone(),
                commit: config.profile_commit.clone(),
                dirty_worktree: config.profile_dirty_worktree,
                hardware_label: config.profile_hardware.clone(),
                build_profile: if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                },
                resolution: self.capture_window_resolution.or_else(|| {
                    self.window
                        .as_ref()
                        .map(|window| window.physical_resolution)
                }),
                window: self.window.clone(),
                worldspace_id: config.worldspace_id,
                start_grid: config.start_grid,
                stream_radius: config.stream_radius,
                synthetic_instances: config.synthetic_instances,
                system,
            },
        )?;
        write_json(&root.join("frame-metrics.json"), frame_metrics)?;
        write_json(
            &root.join("cpu-spans.json"),
            &CpuProfile {
                spans: cpu.clone(),
                counters: self.counters.clone(),
                gauges: self.gauges.clone(),
            },
        )?;
        write_json(&root.join("gpu-passes.json"), &gpu)?;
        write_json(
            &root.join("streaming.json"),
            &StreamingProfile {
                aggregate: streaming.cloned(),
                timeline: self.timeline.clone(),
            },
        )?;
        if let Some(trace) = &self.trace {
            write_json(&root.join("streaming-frames.json"), &trace.report())?;
        }
        write_json(&root.join("renderer.json"), renderer)?;
        if let Some(draw_submission) = &self.draw_submission {
            write_json(
                &root.join("draw-submission.json"),
                &DrawSubmissionProfile {
                    report: draw_submission,
                    frames: &self.draw_submission_frames,
                },
            )?;
        }
        write_json(
            &root.join("memory.json"),
            &MemoryProfile {
                samples: self.memory.clone(),
                slope_gib_per_minute: memory_slope(&self.memory),
            },
        )?;
        fs::write(
            root.join("summary.md"),
            summary_markdown(config, frame_metrics, &cpu, &render, streaming, renderer),
        )?;
        Ok(())
    }
}

fn sample_profile_metadata(
    mut profiler: ResMut<ProfilingState>,
    windows: Query<&Window, With<PrimaryWindow>>,
    device: Option<Res<RenderDevice>>,
    adapter: Option<Res<RenderAdapterInfo>>,
) {
    if let Ok(window) = windows.single() {
        profiler.window = Some(WindowMetadata {
            physical_resolution: [window.physical_width(), window.physical_height()],
            logical_resolution: [window.width(), window.height()],
            scale_factor: window.scale_factor(),
        });
    }
    if let Some(device) = device
        && (device.is_changed() || profiler.render_capabilities.is_none())
    {
        let features = device.features();
        profiler.render_capabilities = Some(RenderCapabilities {
            backend: adapter
                .as_ref()
                .map(|adapter| format!("{:?}", adapter.backend)),
            timestamp_queries_supported: features.contains(WgpuFeatures::TIMESTAMP_QUERY),
            timestamp_queries_inside_passes_supported: features
                .contains(WgpuFeatures::TIMESTAMP_QUERY_INSIDE_PASSES),
            timestamp_queries_inside_encoders_supported: features
                .contains(WgpuFeatures::TIMESTAMP_QUERY_INSIDE_ENCODERS),
            pipeline_statistics_supported: features
                .contains(WgpuFeatures::PIPELINE_STATISTICS_QUERY),
            multi_draw_indirect_count_supported: features
                .contains(WgpuFeatures::MULTI_DRAW_INDIRECT_COUNT),
        });
    }
}

fn begin_trace_frame(
    time: Res<Time<Real>>,
    bridge: Option<Res<StreamingGpuMetricsBridge>>,
    mut profiler: ResMut<ProfilingState>,
) {
    profiler.frame = profiler.frame.saturating_add(1);
    profiler.trace_delta_ms = time.delta_secs_f64() * 1000.0;
    profiler.frame_cpu_spans_ms.clear();
    profiler.frame_completion_latencies_ms.clear();
    if let Some(bridge) = bridge {
        bridge.request(profiler.frame);
    }
}

pub(crate) fn reconcile_scene(
    trace: &mut StreamingTrace,
    server: &AssetServer,
    id: AssetId<WorldAsset>,
) {
    if let Some(status) = scene_status(server.get_load_states(id)) {
        trace.update_scene(id, status);
    } else {
        trace.remove_scene(id);
    }
}

fn scene_status(
    states: Option<(LoadState, DependencyLoadState, RecursiveDependencyLoadState)>,
) -> Option<SceneLoadStatus> {
    match states {
        Some((LoadState::Failed(_), _, _))
        | Some((_, _, RecursiveDependencyLoadState::Failed(_))) => Some(SceneLoadStatus::Failed),
        Some((LoadState::Loaded, _, RecursiveDependencyLoadState::Loaded)) => {
            Some(SceneLoadStatus::CpuLoaded)
        }
        Some(_) => Some(SceneLoadStatus::Loading),
        None => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn sample_scene_inventory(
    mut profiler: ResMut<ProfilingState>,
    mut frames: Local<u64>,
    meshes: Query<(), With<Mesh3d>>,
    terrain: Query<&ViewVisibility, With<crate::world::components::TerrainPatch>>,
    lod_terrain_batches: Query<(), With<crate::streaming::lod::LodTerrainBatch>>,
    structural_nodes: Query<
        (),
        (
            With<crate::visibility_optimization::StructuralHierarchyNode>,
            With<bevy::camera::visibility::NoCpuCulling>,
        ),
    >,
    water: Query<(), With<crate::world::components::WaterSurface>>,
    bounds: Query<(), With<crate::world::components::InstanceBounds>>,
    mesh_assets: Res<Assets<Mesh>>,
    images: Res<Assets<Image>>,
) {
    *frames = (*frames).saturating_add(1);
    if !(*frames).is_multiple_of(60) {
        return;
    }
    let started = Instant::now();
    profiler.set_gauge("scene/mesh_entities", meshes.iter().len() as f64);
    profiler.set_gauge("scene/terrain_patches", terrain.iter().len() as f64);
    profiler.set_gauge(
        "scene/lod_terrain_batches",
        lod_terrain_batches.iter().len() as f64,
    );
    profiler.set_gauge(
        "scene/structural_nodes_no_cpu_culling",
        structural_nodes.iter().len() as f64,
    );
    profiler.set_gauge(
        "scene/visible_terrain_patches",
        terrain.iter().filter(|visible| visible.get()).count() as f64,
    );
    profiler.set_gauge("scene/water_surfaces", water.iter().len() as f64);
    profiler.set_gauge("scene/bounded_instances", bounds.iter().len() as f64);
    profiler.set_gauge("assets/meshes", mesh_assets.len() as f64);
    profiler.set_gauge("assets/images", images.len() as f64);
    profiler.record_elapsed("profiling/scene_inventory", started);
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemMetadata {
    pub os: String,
    pub kernel: String,
    pub cpu: String,
    pub core_count: String,
    pub memory: String,
}

#[derive(Debug, Clone, Serialize)]
struct Metadata<'a> {
    format_version: u32,
    engine_version: &'a str,
    bevy_version: &'a str,
    generated_unix_ms: u128,
    scenario: String,
    run_id: String,
    commit: String,
    dirty_worktree: bool,
    hardware_label: String,
    build_profile: &'a str,
    resolution: Option<[u32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window: Option<WindowMetadata>,
    worldspace_id: u32,
    start_grid: (i32, i32),
    stream_radius: i32,
    synthetic_instances: usize,
    system: Option<SystemMetadata>,
}

#[derive(Debug, Clone, Serialize)]
struct TimelineEvent {
    elapsed_ms: f64,
    frame: u64,
    subject: String,
    stage: String,
    value_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct MemorySample {
    elapsed_seconds: f64,
    process_gib: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricSummary {
    pub count: usize,
    pub total: f64,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub worst: f64,
    pub minimum: f64,
}

#[derive(Serialize)]
struct CpuProfile {
    spans: BTreeMap<String, MetricSummary>,
    counters: BTreeMap<String, u64>,
    gauges: BTreeMap<String, f64>,
}

#[derive(Serialize)]
struct DrawSubmissionProfile<'a> {
    #[serde(flatten)]
    report: &'a crate::indirect_metrics::DrawSubmissionReport,
    frames: &'a [crate::indirect_metrics::DrawSubmissionFrame],
}

#[derive(Serialize)]
struct GpuProfile {
    timestamp_queries_supported: bool,
    pipeline_statistics_supported: bool,
    query_support_observed: bool,
    timestamp_measurements_available: bool,
    timestamp_measurement_status: TimestampMeasurementStatus,
    timestamp_metrics: BTreeMap<String, TimestampMeasurementStatus>,
    rejected_nonfinite_measurements: BTreeMap<String, u64>,
    device: Option<RenderCapabilities>,
    metrics: BTreeMap<String, MetricSummary>,
    unavailable: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
struct WindowMetadata {
    physical_resolution: [u32; 2],
    logical_resolution: [f32; 2],
    scale_factor: f32,
}

/// Features enabled on the device used for the run, independent of diagnostic values.
#[derive(Debug, Clone, Default, Serialize)]
struct RenderCapabilities {
    backend: Option<String>,
    timestamp_queries_supported: bool,
    timestamp_queries_inside_passes_supported: bool,
    timestamp_queries_inside_encoders_supported: bool,
    pipeline_statistics_supported: bool,
    multi_draw_indirect_count_supported: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TimestampMeasurementStatus {
    NotObserved,
    AllZero,
    PositiveValuesObserved,
    MixedValuesObserved,
    InvalidNegativeValues,
}

fn timestamp_status(summary: &MetricSummary) -> TimestampMeasurementStatus {
    if summary.count == 0 {
        TimestampMeasurementStatus::NotObserved
    } else if summary.minimum < 0.0 {
        TimestampMeasurementStatus::InvalidNegativeValues
    } else if summary.worst > 0.0 {
        TimestampMeasurementStatus::PositiveValuesObserved
    } else {
        TimestampMeasurementStatus::AllZero
    }
}

fn gpu_profile(
    metrics: BTreeMap<String, MetricSummary>,
    capabilities: Option<RenderCapabilities>,
) -> GpuProfile {
    let timestamp_metrics: BTreeMap<_, _> = metrics
        .iter()
        .filter(|(path, _)| path.ends_with("/elapsed_gpu"))
        .map(|(path, summary)| (path.clone(), timestamp_status(summary)))
        .collect();
    let timestamp_measurements_available = timestamp_metrics
        .values()
        .any(|status| *status == TimestampMeasurementStatus::PositiveValuesObserved);
    let has_negative_values = timestamp_metrics
        .values()
        .any(|status| *status == TimestampMeasurementStatus::InvalidNegativeValues);
    let timestamp_measurement_status = if timestamp_metrics.is_empty()
        || timestamp_metrics
            .values()
            .all(|status| *status == TimestampMeasurementStatus::NotObserved)
    {
        TimestampMeasurementStatus::NotObserved
    } else if timestamp_measurements_available
        && timestamp_metrics
            .values()
            .any(|status| *status != TimestampMeasurementStatus::PositiveValuesObserved)
    {
        TimestampMeasurementStatus::MixedValuesObserved
    } else if has_negative_values {
        TimestampMeasurementStatus::InvalidNegativeValues
    } else if timestamp_measurements_available {
        TimestampMeasurementStatus::PositiveValuesObserved
    } else {
        TimestampMeasurementStatus::AllZero
    };
    let mut unavailable = unavailable_render_metrics(&metrics);
    if !timestamp_measurements_available {
        unavailable.insert(
            "elapsed_gpu".to_owned(),
            match timestamp_measurement_status {
                TimestampMeasurementStatus::AllZero => {
                    "all observed GPU timestamps are zero; pass timings are unavailable"
                }
                TimestampMeasurementStatus::InvalidNegativeValues => {
                    "observed GPU elapsed times contain negative values"
                }
                _ => "no GPU elapsed measurements were observed",
            }
            .to_owned(),
        );
    }
    GpuProfile {
        timestamp_queries_supported: capabilities
            .as_ref()
            .is_some_and(|device| device.timestamp_queries_supported),
        pipeline_statistics_supported: capabilities
            .as_ref()
            .is_some_and(|device| device.pipeline_statistics_supported),
        query_support_observed: capabilities.is_some(),
        timestamp_measurements_available,
        timestamp_measurement_status,
        timestamp_metrics,
        rejected_nonfinite_measurements: BTreeMap::new(),
        device: capabilities,
        metrics,
        unavailable,
    }
}

#[derive(Serialize)]
struct StreamingProfile {
    aggregate: Option<StreamingMetrics>,
    timeline: Vec<TimelineEvent>,
}

#[derive(Serialize)]
struct MemoryProfile {
    samples: Vec<MemorySample>,
    slope_gib_per_minute: Option<f64>,
}

pub fn summarize(samples: &[f64]) -> MetricSummary {
    let mut sorted: Vec<_> = samples
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect();
    sorted.sort_by(f64::total_cmp);
    let total = sorted.iter().sum::<f64>();
    MetricSummary {
        count: sorted.len(),
        total,
        mean: total / sorted.len().max(1) as f64,
        p50: percentile(&sorted, 0.50),
        p95: percentile(&sorted, 0.95),
        p99: percentile(&sorted, 0.99),
        worst: sorted.last().copied().unwrap_or_default(),
        minimum: sorted.first().copied().unwrap_or_default(),
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn memory_slope(samples: &[MemorySample]) -> Option<f64> {
    let first = samples.first()?;
    let last = samples.last()?;
    let minutes = (last.elapsed_seconds - first.elapsed_seconds) / 60.0;
    (minutes > 0.0).then_some((last.process_gib - first.process_gib) / minutes)
}

fn unavailable_render_metrics(
    render: &BTreeMap<String, MetricSummary>,
) -> BTreeMap<String, String> {
    [
        ("draw_calls", "Bevy's built-in render diagnostics do not expose draw-call count for every render phase"),
        ("indirect_draw_count", "the native GPU preprocessing path does not currently publish this counter"),
        ("occlusion_rejected_instances", "OcclusionCulling does not export the rejected instance count to the main world"),
    ]
    .into_iter()
    .filter(|(name, _)| !render.keys().any(|path| path.ends_with(name)))
    .map(|(name, reason)| (name.to_owned(), reason.to_owned()))
    .collect()
}

fn summary_markdown(
    config: &EngineConfig,
    frame: &serde_json::Value,
    cpu: &BTreeMap<String, MetricSummary>,
    render: &BTreeMap<String, MetricSummary>,
    streaming: Option<&StreamingMetrics>,
    renderer: &RendererMetrics,
) -> String {
    // `assets_added/*` spans hold counts, not milliseconds (see `schedule_timing`), so they must not
    // be listed among the slowest spans. They stay in `cpu-spans.json`.
    let mut top_cpu: Vec<_> = cpu
        .iter()
        .filter(|(name, _)| !name.starts_with("assets_added/"))
        .collect();
    top_cpu.sort_by(|left, right| right.1.total.total_cmp(&left.1.total));
    let mut top_gpu: Vec<_> = render
        .iter()
        .filter(|(name, summary)| {
            name.ends_with("/elapsed_gpu")
                && timestamp_status(summary) == TimestampMeasurementStatus::PositiveValuesObserved
        })
        .collect();
    top_gpu.sort_by(|left, right| right.1.mean.total_cmp(&left.1.mean));
    let mut output = format!(
        "# Profiling summary — {}\n\n- Average FPS: {:.2}\n- Frame P95: {:.2} ms\n- Passed: {}\n- GPU preprocessing: {}\n- GPU culling: {}\n- Indirect drawing: {}\n- HZB views: {}\n",
        config.profile_scenario,
        frame["average_fps"].as_f64().unwrap_or_default(),
        frame["frame_ms_p95"].as_f64().unwrap_or_default(),
        frame["passed"].as_bool().unwrap_or(false),
        renderer.gpu_preprocessing_active,
        renderer.gpu_culling_active,
        renderer.indirect_drawing_active,
        renderer.hzb_views,
    );
    for (key, title) in [
        ("frames_after_ready", "Frames after world ready"),
        ("jump_load_window", "Jump loading window"),
    ] {
        let window = &frame[key];
        if window.is_object() {
            output.push_str(&format!(
                "- {title}: {} frames, P99 {:.2} ms, worst {:.2} ms, over 33 ms: {}, over 50 ms: {}\n",
                window["frames"].as_u64().unwrap_or_default(),
                window["p99_ms"].as_f64().unwrap_or_default(),
                window["worst_ms"].as_f64().unwrap_or_default(),
                window["over_33ms"].as_u64().unwrap_or_default(),
                window["over_50ms"].as_u64().unwrap_or_default(),
            ));
        }
    }
    output.push_str(
        "\n## Top CPU spans\n\n| Span | Mean ms | P95 ms | Total ms |\n|---|---:|---:|---:|\n",
    );
    for (name, value) in top_cpu.into_iter().take(10) {
        output.push_str(&format!(
            "| {name} | {:.3} | {:.3} | {:.3} |\n",
            value.mean, value.p95, value.total
        ));
    }
    output.push_str("\n## Top GPU passes\n\n| Pass | Mean ms | P95 ms |\n|---|---:|---:|\n");
    if top_gpu.is_empty() {
        output.push_str("\nGPU pass timings are unavailable; see gpu-passes.json for device support and measurement status.\n");
    }
    for (name, value) in top_gpu.into_iter().take(10) {
        output.push_str(&format!(
            "| {name} | {:.3} | {:.3} |\n",
            value.mean, value.p95
        ));
    }
    if let Some(streaming) = streaming {
        output.push_str(&format!(
            "\n## Streaming and assets\n\n- Requests: {}\n- Active requests: {} (peak {})\n- Resident roots: {}\n- Failed cells: {}\n- Stale responses: {}\n- Unloaded cells: {}\n- Max despawns per frame: {}\n- Despawned entities: {}\n- Max instances spawned per frame: {}\n- Max instances armed per frame: {}\n- Arming queue depth: {} (peak {})\n- Max instances completed per scan: {}\n- Retiring cells: {} (peak {})\n- Revived cells: {}\n- Retire backlog overflows: {}\n- Origin rebases: {}\n- Lifecycle invariant failures: {}\n- Duplicate/orphaned/missing/out-of-range roots: {}/{}/{}/{}\n- Streaming fixture validated: {}\n- Assets ready: {}\n- Empty model references: {}\n- Model assets pending: {}\n- Surface assets pending: {}\n- Meshes validated: {}\n- Materials validated: {}\n- Images validated: {}\n- Asset failures: {}\n- Material validation failures: {}\n- Diagnostic fallbacks: {}\n- Canonical fixture validated: {}\n- Terrain patches validated: {}\n- Terrain seams validated: {}\n- Terrain seam points welded: {}\n- Terrain edges left as authored: {}\n- Terrain failures: {}\n- Water surfaces validated: {}\n- Water failures: {}\n- Terrain/water fixture validated: {}\n- Max query: {:.3} ms\n- Max cell commit: {:.3} ms\n- Max frame commit: {:.3} ms (budget {:.3} ms, violations {})\n",
            streaming.requests_submitted,
            streaming.active_requests,
            streaming.peak_active_requests,
            streaming.resident_roots,
            streaming.failed_cells,
            streaming.stale_responses,
            streaming.unloaded_cells,
            streaming.max_despawns_per_frame,
            streaming.despawned_entities,
            streaming.max_instances_spawned_per_frame,
            streaming.max_instances_armed_per_frame,
            streaming.arming_queue_depth,
            streaming.peak_arming_queue_depth,
            streaming.max_instances_completed_per_scan,
            streaming.retiring_cells,
            streaming.peak_retiring_cells,
            streaming.revived_cells,
            streaming.retire_backlog_overflows,
            streaming.origin_rebases,
            streaming.streaming_invariant_failures,
            streaming.duplicate_cell_roots,
            streaming.orphaned_cell_roots,
            streaming.missing_cell_roots,
            streaming.out_of_range_cell_roots,
            streaming.streaming_fixture_validated,
            streaming.assets_ready,
            streaming.empty_model_references,
            streaming.pending_asset_instances,
            streaming.pending_surface_instances,
            streaming.meshes_validated,
            streaming.materials_validated,
            streaming.images_validated,
            streaming.asset_load_failures,
            streaming.material_validation_failures,
            streaming.diagnostic_fallbacks,
            streaming.canonical_fixture_validated,
            streaming.terrain_patches_validated,
            streaming.terrain_seams_validated,
            streaming.terrain_seam_points_welded,
            streaming.terrain_edges_left_as_authored,
            streaming.terrain_validation_failures,
            streaming.water_surfaces_validated,
            streaming.water_validation_failures,
            streaming.terrain_water_fixture_validated,
            streaming.max_query_micros as f64 / 1000.0,
            streaming.max_commit_micros as f64 / 1000.0,
            streaming.max_frame_commit_micros as f64 / 1000.0,
            streaming.commit_budget_micros as f64 / 1000.0,
            streaming.commit_budget_violations,
        ));
    }
    output
}

fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?,
    )
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn push_bounded(values: &mut Vec<f64>, value: f64) {
    if values.len() < MAX_SAMPLES_PER_METRIC && value.is_finite() {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::diagnostic::{Diagnostic, DiagnosticMeasurement, DiagnosticPath};
    use std::time::Duration;

    #[test]
    fn render_measurements_are_sampled_once_per_timestamp() {
        let path = DiagnosticPath::new("render/test/elapsed_cpu");
        let mut diagnostics = DiagnosticsStore::default();
        diagnostics.add(Diagnostic::new(path.clone()));
        let started = DiagnosticInstant::now();
        diagnostics
            .get_mut(&path)
            .unwrap()
            .add_measurement(DiagnosticMeasurement {
                time: started,
                value: 2.0,
            });
        let mut profiler = ProfilingState::default();
        profiler.sample_frame(&diagnostics, None);
        profiler.sample_frame(&diagnostics, None);
        assert_eq!(profiler.render_metrics[path.as_str()], [2.0]);

        // Identical values are distinct observations when their timestamps differ.
        for offset in [1, 2] {
            diagnostics
                .get_mut(&path)
                .unwrap()
                .add_measurement(DiagnosticMeasurement {
                    time: started + Duration::from_millis(offset),
                    value: 2.0,
                });
        }
        profiler.sample_frame(&diagnostics, None);
        profiler.sample_frame(&diagnostics, None);
        assert_eq!(profiler.render_metrics[path.as_str()], [2.0, 2.0, 2.0]);
    }

    #[test]
    fn sampling_collects_all_unseen_history_and_skips_disabled_diagnostics() {
        let path = DiagnosticPath::new("render/test/elapsed_cpu");
        let disabled_path = DiagnosticPath::new("render/disabled/elapsed_cpu");
        let mut diagnostic = Diagnostic::new(path.clone());
        let started = DiagnosticInstant::now();
        for offset in [0, 1, 2] {
            diagnostic.add_measurement(DiagnosticMeasurement {
                time: started + Duration::from_millis(offset),
                value: offset as f64,
            });
        }
        let mut disabled = Diagnostic::new(disabled_path.clone());
        disabled.add_measurement(DiagnosticMeasurement {
            time: started,
            value: 100.0,
        });
        disabled.is_enabled = false;
        let mut diagnostics = DiagnosticsStore::default();
        diagnostics.add(diagnostic);
        diagnostics.add(disabled);
        let mut profiler = ProfilingState::default();
        profiler.sample_frame(&diagnostics, None);
        assert_eq!(profiler.render_metrics[path.as_str()], [0.0, 1.0, 2.0]);
        assert!(!profiler.render_metrics.contains_key(disabled_path.as_str()));
    }

    #[test]
    fn all_zero_gpu_timings_are_unavailable_even_when_queries_are_supported() {
        let gpu = gpu_profile(
            BTreeMap::from([("render/main/elapsed_gpu".into(), summarize(&[0.0; 3]))]),
            Some(RenderCapabilities {
                timestamp_queries_supported: true,
                ..Default::default()
            }),
        );
        assert!(gpu.timestamp_queries_supported);
        assert!(gpu.query_support_observed);
        assert!(!gpu.timestamp_measurements_available);
        assert_eq!(
            gpu.timestamp_measurement_status,
            TimestampMeasurementStatus::AllZero
        );
        assert_eq!(gpu.metrics["render/main/elapsed_gpu"].count, 3);
        assert!(gpu.unavailable["elapsed_gpu"].contains("zero"));
    }

    #[test]
    fn positive_gpu_diagnostics_do_not_prove_device_query_support() {
        let gpu = gpu_profile(
            BTreeMap::from([("render/main/elapsed_gpu".into(), summarize(&[1.0, 2.0]))]),
            None,
        );
        assert!(!gpu.timestamp_queries_supported);
        assert!(!gpu.query_support_observed);
        assert!(gpu.timestamp_measurements_available);
        assert_eq!(
            gpu.timestamp_measurement_status,
            TimestampMeasurementStatus::PositiveValuesObserved
        );
        assert!(!gpu.unavailable.contains_key("elapsed_gpu"));
    }

    #[test]
    fn supported_queries_without_measurements_remain_unavailable() {
        let gpu = gpu_profile(
            BTreeMap::new(),
            Some(RenderCapabilities {
                timestamp_queries_supported: true,
                ..Default::default()
            }),
        );
        assert!(gpu.timestamp_queries_supported);
        assert!(!gpu.timestamp_measurements_available);
        assert_eq!(
            gpu.timestamp_measurement_status,
            TimestampMeasurementStatus::NotObserved
        );
    }

    #[test]
    fn negative_elapsed_values_are_marked_invalid() {
        let summary = summarize(&[-1.0, 2.0, 3.0]);
        assert_eq!(
            timestamp_status(&summary),
            TimestampMeasurementStatus::InvalidNegativeValues
        );
    }

    #[test]
    fn empty_gpu_summaries_are_unobserved_and_mixed_results_keep_invalid_status() {
        let empty = gpu_profile(
            BTreeMap::from([("render/empty/elapsed_gpu".into(), summarize(&[]))]),
            None,
        );
        assert_eq!(
            empty.timestamp_measurement_status,
            TimestampMeasurementStatus::NotObserved
        );
        let mixed = gpu_profile(
            BTreeMap::from([
                ("render/valid/elapsed_gpu".into(), summarize(&[1.0])),
                (
                    "render/invalid/elapsed_gpu".into(),
                    summarize(&[-1.0, 2.0, 3.0]),
                ),
            ]),
            None,
        );
        assert!(mixed.timestamp_measurements_available);
        assert_eq!(
            mixed.timestamp_measurement_status,
            TimestampMeasurementStatus::MixedValuesObserved
        );
        assert_eq!(
            mixed.timestamp_metrics["render/invalid/elapsed_gpu"],
            TimestampMeasurementStatus::InvalidNegativeValues
        );
    }

    #[test]
    fn nonfinite_render_measurements_are_rejected_once() {
        let path = DiagnosticPath::new("render/test/elapsed_gpu");
        let mut diagnostic = Diagnostic::new(path.clone());
        diagnostic.add_measurement(DiagnosticMeasurement {
            time: DiagnosticInstant::now(),
            value: f64::NAN,
        });
        let mut diagnostics = DiagnosticsStore::default();
        diagnostics.add(diagnostic);
        let mut profiler = ProfilingState::default();
        profiler.sample_frame(&diagnostics, None);
        profiler.sample_frame(&diagnostics, None);
        assert!(!profiler.render_metrics.contains_key(path.as_str()));
        assert_eq!(profiler.rejected_render_measurements[path.as_str()], 1);
        assert_eq!(summarize(&[f64::NAN, f64::INFINITY, 2.0]).count, 1);
    }

    #[test]
    fn metadata_observes_physical_and_logical_window_dimensions() {
        let mut app = App::new();
        app.init_resource::<ProfilingState>()
            .add_systems(Update, sample_profile_metadata);
        app.world_mut().spawn((
            Window {
                resolution: bevy::window::WindowResolution::new(3200, 1800)
                    .with_scale_factor_override(2.0),
                ..Default::default()
            },
            PrimaryWindow,
        ));
        app.update();
        let profiler = app.world().resource::<ProfilingState>();
        let window = profiler.window.as_ref().unwrap();
        assert_eq!(window.physical_resolution, [3200, 1800]);
        assert_eq!(window.logical_resolution, [1600.0, 900.0]);
        assert_eq!(window.scale_factor, 2.0);
        assert!(profiler.render_capabilities.is_none());
    }

    #[test]
    fn summarizes_percentiles_and_worst_value() {
        let values: Vec<_> = (1..=100).map(f64::from).collect();
        let summary = summarize(&values);
        assert_eq!(summary.count, 100);
        assert_eq!(summary.p95, 96.0);
        assert_eq!(summary.worst, 100.0);
    }

    #[test]
    fn the_top_cpu_table_omits_asset_count_spans() {
        let mut cpu = BTreeMap::new();
        cpu.insert("assets_added/mesh".to_owned(), summarize(&[9.0, 9.0]));
        cpu.insert("streaming/plan".to_owned(), summarize(&[2.0, 3.0]));
        let render = BTreeMap::new();
        let summary = summary_markdown(
            &EngineConfig::default(),
            &serde_json::json!({}),
            &cpu,
            &render,
            None,
            &RendererMetrics::default(),
        );
        assert!(
            summary.contains("| streaming/plan |"),
            "a real span must stay in the table"
        );
        assert!(
            !summary.contains("assets_added/mesh"),
            "a count span must not be shown as milliseconds"
        );
    }

    #[test]
    fn computes_memory_growth_slope() {
        let samples = vec![
            MemorySample {
                elapsed_seconds: 0.0,
                process_gib: 1.0,
            },
            MemorySample {
                elapsed_seconds: 120.0,
                process_gib: 1.2,
            },
        ];
        assert!((memory_slope(&samples).unwrap() - 0.1).abs() < 0.0001);
    }

    #[test]
    fn writes_complete_profile_bundle() {
        let directory = tempfile::tempdir().unwrap();
        let config = EngineConfig {
            profile_output_dir: Some(directory.path().to_owned()),
            profile_scenario: "synthetic".into(),
            ..Default::default()
        };
        let mut profiler = ProfilingState::default();
        profiler.record_ms("streaming/test", 2.0);
        profiler.increment("test/count", 1);
        profiler.event("cell", "requested", None);
        let frame = serde_json::json!({
            "average_fps": 90.0,
            "frame_ms_p95": 14.0,
            "passed": true
        });
        profiler
            .write_bundle(
                &config,
                &frame,
                Some(&StreamingMetrics::default()),
                &RendererMetrics::default(),
                None,
            )
            .unwrap();
        for name in [
            "metadata.json",
            "frame-metrics.json",
            "cpu-spans.json",
            "gpu-passes.json",
            "streaming.json",
            "renderer.json",
            "memory.json",
            "summary.md",
        ] {
            assert!(directory.path().join(name).is_file(), "missing {name}");
        }
        let summary = fs::read_to_string(directory.path().join("summary.md")).unwrap();
        let metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.path().join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(metadata["format_version"], 2);
        assert!(metadata["resolution"].is_null());
        assert!(summary.contains("GPU pass timings are unavailable"));
        assert!(
            summary.contains("Terrain seam points welded: 0"),
            "the summary must report the welded terrain seam points"
        );
        assert!(!directory.path().join("streaming-frames.json").exists());
    }

    #[test]
    fn normal_runs_do_not_install_trace_collection() {
        let mut app = App::new();
        app.insert_resource(EngineConfig::default())
            .add_plugins(ProfilingPlugin);
        assert!(app.world().resource::<ProfilingState>().trace.is_none());
        assert!(!app.world().contains_resource::<StreamingGpuMetricsBridge>());
    }

    #[test]
    fn profile_trace_does_not_install_expensive_gpu_inventory_by_default() {
        let mut app = App::new();
        app.insert_resource(EngineConfig {
            profile_output_dir: Some("unused-test-output".into()),
            ..default()
        })
        .add_plugins(ProfilingPlugin);
        assert!(app.world().resource::<ProfilingState>().trace.is_some());
        assert!(!app.world().contains_resource::<StreamingGpuMetricsBridge>());
    }

    #[test]
    fn scene_status_requires_recursive_dependencies_and_catches_their_failures() {
        use bevy::asset::{AssetLoadError, io::AssetReaderError};
        use std::sync::Arc;
        let error = Arc::new(AssetLoadError::AssetReaderError(
            AssetReaderError::NotFound("missing-image.ktx2".into()),
        ));
        assert_eq!(scene_status(None), None);
        assert_eq!(
            scene_status(Some((
                LoadState::Loaded,
                DependencyLoadState::Loaded,
                RecursiveDependencyLoadState::Loading
            ))),
            Some(SceneLoadStatus::Loading)
        );
        assert_eq!(
            scene_status(Some((
                LoadState::Loaded,
                DependencyLoadState::Loaded,
                RecursiveDependencyLoadState::Failed(error.clone())
            ))),
            Some(SceneLoadStatus::Failed)
        );
        assert_eq!(
            scene_status(Some((
                LoadState::Failed(error),
                DependencyLoadState::Loaded,
                RecursiveDependencyLoadState::Loaded
            ))),
            Some(SceneLoadStatus::Failed)
        );
        assert_eq!(
            scene_status(Some((
                LoadState::Loaded,
                DependencyLoadState::Loaded,
                RecursiveDependencyLoadState::Loaded
            ))),
            Some(SceneLoadStatus::CpuLoaded)
        );
    }
}

// Samples acquisition before presentation consumes the swapchain texture.
#[derive(Resource)]
struct ProfilePrimaryDrawableProof {
    frames: u64,
    warmup_frames: u64,
    present_frames: u64,
    missing_frames: u64,
    measured_present_frames: u64,
    measured_missing_frames: u64,
}

impl ProfilePrimaryDrawableProof {
    fn new(warmup_frames: u32) -> Self {
        Self {
            frames: 0,
            warmup_frames: u64::from(warmup_frames).saturating_add(5),
            present_frames: 0,
            missing_frames: 0,
            measured_present_frames: 0,
            measured_missing_frames: 0,
        }
    }

    fn log(&self, final_summary: bool) {
        info!(
            final_summary,
            render_frames = self.frames,
            warmup_plus_five = self.warmup_frames,
            present_frames = self.present_frames,
            missing_frames = self.missing_frames,
            measured_present_frames = self.measured_present_frames,
            measured_missing_frames = self.measured_missing_frames,
            "native profiling primary drawable proof"
        );
    }
}

impl Drop for ProfilePrimaryDrawableProof {
    fn drop(&mut self) {
        self.log(true);
    }
}

fn sample_primary_drawable(
    windows: Res<ExtractedWindows>,
    mut proof: ResMut<ProfilePrimaryDrawableProof>,
) {
    let present = windows
        .primary
        .and_then(|primary| windows.get(&primary))
        .is_some_and(|window| window.swap_chain_texture.is_some());
    proof.frames = proof.frames.saturating_add(1);
    if present {
        proof.present_frames = proof.present_frames.saturating_add(1);
    } else {
        proof.missing_frames = proof.missing_frames.saturating_add(1);
    }
    if proof.frames > proof.warmup_frames {
        if present {
            proof.measured_present_frames = proof.measured_present_frames.saturating_add(1);
        } else {
            proof.measured_missing_frames = proof.measured_missing_frames.saturating_add(1);
        }
    }
    if proof.frames.is_multiple_of(300) {
        proof.log(false);
    }
}
