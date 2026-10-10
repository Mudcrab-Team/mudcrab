use crate::{
    config::EngineConfig,
    pacing::{PacingReport, PacingTracker},
    profiling::{MetricSummary, ProfilingState, SystemMetadata, reconcile_scene, summarize},
    render::RendererMetrics,
    render_timing::{PipelineActivity, RenderTimingPlugin, RenderTimings},
    streaming::{RenderOrigin, StreamingMetrics},
    streaming_gpu_metrics::StreamingGpuMetricsBridge,
    streaming_trace::{BenchmarkWindow, CameraMotion, CameraObservation, StreamingFrameSample},
    world::components::{CELL_SIZE, StreamingCamera},
};
use bevy::{
    diagnostic::{
        DiagnosticsStore, EntityCountDiagnosticsPlugin, SystemInfo,
        SystemInformationDiagnosticsPlugin,
    },
    prelude::*,
    world_serialization::WorldAsset,
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct AcceptanceMetricsPlugin;

impl Plugin for AcceptanceMetricsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BenchmarkSamples>()
            .add_plugins((
                EntityCountDiagnosticsPlugin::default(),
                SystemInformationDiagnosticsPlugin,
                RenderTimingPlugin,
            ))
            .add_systems(
                Last,
                collect_and_finish.after(crate::render_timing::end_main_world),
            );
        if app
            .world()
            .get_resource::<EngineConfig>()
            .is_some_and(|config| config.profile_output_dir.is_some())
        {
            app.add_systems(Last, sample_streaming_trace.before(collect_and_finish));
        }
        let measures_pacing = app
            .world()
            .get_resource::<EngineConfig>()
            .map(EngineConfig::measures_pacing);
        install_schedule_timing(app, measures_pacing);
    }
}

/// Adds the schedule-timing plugin (which also counts asset arrivals) only when the run measures
/// pacing. An ordinary play session installs neither the eight marker schedules per frame nor the
/// four asset-arrival counts.
fn install_schedule_timing(app: &mut App, measures_pacing: Option<bool>) {
    if measures_pacing == Some(true) {
        app.add_plugins(crate::schedule_timing::ScheduleTimingPlugin);
    }
}

#[derive(Resource, Default)]
struct BenchmarkSamples {
    frames_seen: u32,
    frame_ms: Vec<f64>,
    measured_seconds: f64,
    peak_process_memory_gib: f64,
    first_process_memory_gib: Option<f64>,
    last_process_memory_gib: Option<f64>,
    measurement_complete: bool,
    screenshot_wait_started: Option<std::time::Instant>,
    finished: bool,
}

fn benchmark_window(config: &EngineConfig, samples: &BenchmarkSamples) -> BenchmarkWindow {
    if config.benchmark_frames.is_none() && config.benchmark_duration_secs.is_none() {
        BenchmarkWindow::OutsideBenchmark
    } else if samples.measurement_complete {
        BenchmarkWindow::Settlement
    } else if samples.frames_seen < config.benchmark_warmup_frames {
        BenchmarkWindow::Warmup
    } else {
        BenchmarkWindow::Measured
    }
}

#[derive(Clone)]
struct PreviousCamera {
    entity: Entity,
    worldspace_id: u32,
    position: [f64; 3],
    rotation: Quat,
}

fn observe_camera(
    entity: Entity,
    transform: &GlobalTransform,
    worldspace_id: u32,
    origin: IVec2,
    previous: &mut Option<PreviousCamera>,
) -> CameraObservation {
    let local = transform.translation();
    let position = [
        f64::from(local.x) + f64::from(origin.x) * f64::from(CELL_SIZE),
        f64::from(local.y),
        f64::from(local.z) - f64::from(origin.y) * f64::from(CELL_SIZE),
    ];
    let rotation = transform.rotation();
    let motion = match previous {
        Some(last) if last.entity == entity && last.worldspace_id == worldspace_id => {
            let translated = position
                .iter()
                .zip(last.position)
                .any(|(current, prior)| (current - prior).abs() > 0.001);
            let rotated = rotation.dot(last.rotation).abs() < 1.0 - 1.0e-6;
            match (translated, rotated) {
                (false, false) => CameraMotion::Stationary,
                (true, false) => CameraMotion::Translated,
                (false, true) => CameraMotion::Rotated,
                (true, true) => CameraMotion::TranslatedAndRotated,
            }
        }
        _ => CameraMotion::Unavailable,
    };
    *previous = Some(PreviousCamera {
        entity,
        worldspace_id,
        position,
        rotation,
    });
    CameraObservation {
        worldspace_id,
        render_origin_grid: [origin.x, origin.y],
        world_position: position,
        motion,
    }
}

#[allow(clippy::too_many_arguments)]
fn sample_streaming_trace(
    config: Res<EngineConfig>,
    samples: Res<BenchmarkSamples>,
    diagnostics: Res<DiagnosticsStore>,
    streaming: Option<Res<StreamingMetrics>>,
    admission: Option<Res<crate::streaming::admission::SceneAdmission>>,
    bridge: Option<Res<StreamingGpuMetricsBridge>>,
    server: Option<Res<AssetServer>>,
    mut events: MessageReader<AssetEvent<WorldAsset>>,
    cameras: Query<(Entity, &GlobalTransform), With<StreamingCamera>>,
    origin: Option<Res<RenderOrigin>>,
    mut previous_camera: Local<Option<PreviousCamera>>,
    mut pending_ids: Local<Vec<bevy::asset::AssetId<WorldAsset>>>,
    mut profiler: ResMut<ProfilingState>,
    runtime: Option<Res<crate::streaming::runtime::StreamingRuntime>>,
) {
    let camera = match cameras.single() {
        Ok((entity, transform)) => Some(observe_camera(
            entity,
            transform,
            config.worldspace_id,
            origin.as_ref().map_or(IVec2::ZERO, |origin| origin.0),
            &mut previous_camera,
        )),
        Err(_) => {
            *previous_camera = None;
            None
        }
    };
    let sample = StreamingFrameSample {
        main_frame: profiler.main_frame(),
        elapsed_ms: profiler.elapsed_ms(),
        delta_ms: profiler.trace_delta_ms,
        benchmark_window: benchmark_window(&config, &samples),
        camera,
        streaming: streaming.as_deref().map(Into::into),
        scene_admission: (config.prioritize_streaming || config.max_scene_loads != 0)
            .then(|| {
                admission
                    .as_ref()
                    .map(|admission| crate::streaming_trace::SceneAdmissionSnapshot {
                        configured_job_limit: config.max_scene_loads,
                        prioritization_enabled: config.prioritize_streaming,
                        jobs: admission.stats(),
                    })
            })
            .flatten(),
        scenes: Default::default(),
        cpu_spans_ms: profiler.frame_cpu_spans_ms.clone(),
        completion_latencies_ms: profiler.frame_completion_latencies_ms.clone(),
        process_memory_gib: diagnostic_value(
            &diagnostics,
            &SystemInformationDiagnosticsPlugin::PROCESS_MEM_USAGE,
        ),
        gpu: bridge.as_ref().and_then(|bridge| bridge.latest()),
        adaptive: runtime
            .as_ref()
            .and_then(|runtime| runtime.snapshot.clone()),
    };
    if let Some(trace) = profiler.trace.as_mut() {
        if let Some(server) = server {
            for event in events.read() {
                match event {
                    AssetEvent::Unused { id }
                    | AssetEvent::Added { id }
                    | AssetEvent::Modified { id }
                    | AssetEvent::LoadedWithDependencies { id } => {
                        reconcile_scene(trace, &server, *id);
                    }
                    AssetEvent::Removed { .. } => {}
                }
            }
            pending_ids.clear();
            pending_ids.extend(trace.pending_scene_ids());
            for id in pending_ids.iter().copied() {
                reconcile_scene(trace, &server, id);
            }
        }
        trace.push_frame(sample);
    }
}

#[derive(Debug, Serialize)]
struct BenchmarkReport {
    format_version: u32,
    generated_unix_ms: u128,
    scenario: String,
    frames: usize,
    warmup_frames: u32,
    synthetic_instances: usize,
    elapsed_seconds: f64,
    average_fps: f64,
    frame_ms_mean: f64,
    frame_ms_p50: f64,
    frame_ms_p95: f64,
    frame_ms_p99: f64,
    frame_ms_worst: f64,
    peak_process_memory_gib: Option<f64>,
    process_memory_growth_gib: Option<f64>,
    entity_count: Option<u64>,
    system: Option<SystemSnapshot>,
    streaming: Option<StreamingMetrics>,
    renderer: RendererMetrics,
    /// CPU time of the parts of a frame the frame-time and GPU numbers do not split out, in
    /// milliseconds: the main world, the wait for the render thread, extract, and the render
    /// thread with its phases (see `render_timing`). Empty when the renderer did not run.
    render_world: BTreeMap<String, MetricSummary>,
    /// Render pipelines queued and finished per render frame, set against render-thread time (see
    /// `render_timing::PipelineActivity`). Serialised as `null` when the renderer did not run.
    render_pipelines: Option<PipelineActivity>,
    /// Time to a fully loaded world, the jump, and loading lag at speed (see `pacing`). The fields
    /// sit at the top level of the report and are absent for a run with no streaming.
    #[serde(flatten)]
    pacing: Option<PacingReport>,
    thresholds: Thresholds,
    passed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SystemSnapshot {
    os: String,
    kernel: String,
    cpu: String,
    core_count: String,
    memory: String,
}

#[derive(Debug, Serialize)]
struct Thresholds {
    minimum_average_fps: f64,
    maximum_p95_frame_ms: f64,
    maximum_memory_growth_gib: f64,
    no_streaming_failures: bool,
    commit_budget_respected: bool,
    streaming_lifecycle_validated: bool,
    renderer_path_active: bool,
    screenshot_captured: bool,
}

#[allow(clippy::too_many_arguments)]
fn collect_and_finish(
    time: Res<Time>,
    config: Res<EngineConfig>,
    diagnostics: Res<DiagnosticsStore>,
    system: Option<Res<SystemInfo>>,
    streaming: Option<Res<StreamingMetrics>>,
    renderer: Res<RendererMetrics>,
    render_timings: Res<RenderTimings>,
    pacing: Option<Res<PacingTracker>>,
    mut samples: ResMut<BenchmarkSamples>,
    mut profiler: ResMut<ProfilingState>,
    mut exit: MessageWriter<AppExit>,
) {
    if samples.finished || !config.is_benchmark_run() {
        return;
    }
    if !samples.measurement_complete {
        samples.frames_seen = samples.frames_seen.saturating_add(1);
        let process_memory = diagnostic_value(
            &diagnostics,
            &SystemInformationDiagnosticsPlugin::PROCESS_MEM_USAGE,
        );
        profiler.sample_frame(&diagnostics, process_memory);
        if samples.frames_seen > config.benchmark_warmup_frames {
            render_timings.set_recording(true);
            let milliseconds = time.delta_secs_f64() * 1000.0;
            if milliseconds.is_finite() && milliseconds > 0.0 {
                samples.frame_ms.push(milliseconds);
                samples.measured_seconds += milliseconds / 1000.0;
            }
        }
        if let Some(memory) = process_memory {
            samples.peak_process_memory_gib = samples.peak_process_memory_gib.max(memory);
            if samples.frames_seen > config.benchmark_warmup_frames
                && samples.measured_seconds >= memory_settle_seconds(&config)
            {
                samples.first_process_memory_gib.get_or_insert(memory);
                samples.last_process_memory_gib = Some(memory);
            }
        }
        let frame_limit_reached = config.benchmark_frames.is_some_and(|limit| {
            samples.frames_seen >= limit.saturating_add(config.benchmark_warmup_frames)
        });
        let duration_reached = config
            .benchmark_duration_secs
            .is_some_and(|limit| samples.frame_ms.iter().sum::<f64>() / 1000.0 >= limit);
        if !frame_limit_reached && !duration_reached {
            return;
        }
        samples.measurement_complete = true;
        render_timings.set_recording(false);
    }
    let screenshot_captured = config
        .acceptance_screenshot
        .as_ref()
        .is_none_or(|path| path.is_file());
    if !screenshot_captured {
        let waiting_since = samples
            .screenshot_wait_started
            .get_or_insert_with(std::time::Instant::now);
        if waiting_since.elapsed() < std::time::Duration::from_secs(10) {
            return;
        }
    }
    let render_world: BTreeMap<String, MetricSummary> = render_timings
        .take_samples()
        .into_iter()
        .map(|(name, values)| {
            for &value in &values {
                profiler.record_ms(format!("render_world/{name}"), value);
            }
            (name.to_owned(), summarize(&values))
        })
        .collect();
    let render_pipelines = render_timings.take_pipeline_activity();
    let pacing_report = pacing.as_deref().map(|tracker| {
        tracker.report(
            &config,
            streaming
                .as_deref()
                .map_or(0, |value| value.peak_arming_queue_depth),
        )
    });
    let mut ordered = samples.frame_ms.clone();
    ordered.sort_by(f64::total_cmp);
    let total_ms = ordered.iter().sum::<f64>();
    let mean = total_ms / ordered.len().max(1) as f64;
    let p50 = percentile(&ordered, 0.50);
    let p95 = percentile(&ordered, 0.95);
    let p99 = percentile(&ordered, 0.99);
    let worst = ordered.last().copied().unwrap_or_default();
    let average_fps = if mean > 0.0 { 1000.0 / mean } else { 0.0 };
    let no_streaming_failures = no_runtime_failures(
        streaming.as_deref(),
        config.material_fixture,
        config.terrain_water_fixture,
        config.transform_bounds_fixture,
    ) && (!config.streaming_fixture
        || streaming
            .as_deref()
            .is_some_and(|value| value.streaming_fixture_validated))
        && physics_fixture_ready(config.physics_fixture, streaming.as_deref());
    let commit_budget_respected = streaming
        .as_deref()
        .is_none_or(|value| value.commit_budget_violations == 0);
    let streaming_lifecycle_validated = streaming.as_deref().is_none_or(|value| {
        value.streaming_invariant_failures == 0
            && value.duplicate_cell_roots == 0
            && value.orphaned_cell_roots == 0
            && value.missing_cell_roots == 0
            && value.out_of_range_cell_roots == 0
            && value.streaming_fixture_failures == 0
    });
    let renderer_path_active = renderer.final_path_active()
        && (!config.renderer_fixture || renderer.renderer_fixture_validated);
    let memory_growth_gib = samples
        .first_process_memory_gib
        .zip(samples.last_process_memory_gib)
        .map(|(first, last)| (last - first).max(0.0));
    let passed = average_fps >= config.accept_min_fps
        && p95 <= config.accept_p95_ms
        && memory_growth_gib.is_none_or(|growth| growth <= config.accept_max_memory_growth_gib)
        && no_streaming_failures
        && commit_budget_respected
        && streaming_lifecycle_validated
        && renderer_path_active
        && screenshot_captured;
    let system_snapshot = system.map(|value| SystemSnapshot {
        os: value.os.clone(),
        kernel: value.kernel.clone(),
        cpu: value.cpu.clone(),
        core_count: value.core_count.clone(),
        memory: value.memory.clone(),
    });
    let report = BenchmarkReport {
        format_version: 8,
        generated_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis()),
        scenario: if config.profile_scenario.is_empty() {
            if config.streaming_fixture {
                "streaming".to_owned()
            } else if config.terrain_water_fixture {
                "terrain-water".to_owned()
            } else if config.renderer_fixture {
                "renderer".to_owned()
            } else if config.transform_bounds_fixture {
                "transform-bounds".to_owned()
            } else if config.material_fixture {
                "materials".to_owned()
            } else if config.benchmark_only {
                "synthetic".to_owned()
            } else {
                "world".to_owned()
            }
        } else {
            config.profile_scenario.clone()
        },
        frames: ordered.len(),
        warmup_frames: config.benchmark_warmup_frames,
        synthetic_instances: if config.benchmark_only {
            config.synthetic_instances
        } else {
            0
        },
        elapsed_seconds: total_ms / 1000.0,
        average_fps,
        frame_ms_mean: mean,
        frame_ms_p50: p50,
        frame_ms_p95: p95,
        frame_ms_p99: p99,
        frame_ms_worst: worst,
        peak_process_memory_gib: (samples.peak_process_memory_gib > 0.0)
            .then_some(samples.peak_process_memory_gib),
        process_memory_growth_gib: memory_growth_gib,
        entity_count: diagnostic_value(&diagnostics, &EntityCountDiagnosticsPlugin::ENTITY_COUNT)
            .map(|value| value as u64),
        system: system_snapshot.clone(),
        streaming: streaming.as_ref().map(|value| (*value).clone()),
        renderer: renderer.clone(),
        render_world,
        render_pipelines,
        pacing: pacing_report,
        thresholds: Thresholds {
            minimum_average_fps: config.accept_min_fps,
            maximum_p95_frame_ms: config.accept_p95_ms,
            maximum_memory_growth_gib: config.accept_max_memory_growth_gib,
            no_streaming_failures,
            commit_budget_respected,
            streaming_lifecycle_validated,
            renderer_path_active,
            screenshot_captured,
        },
        passed,
    };
    if let Some(parent) = config
        .benchmark_output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && let Err(error) = fs::create_dir_all(parent)
    {
        error!(%error, "failed to create benchmark report directory");
        exit.write(AppExit::error());
        samples.finished = true;
        return;
    }
    let frame_metrics = match serde_json::to_value(&report) {
        Ok(value) => value,
        Err(error) => {
            error!(%error, "failed to serialize benchmark report");
            exit.write(AppExit::error());
            samples.finished = true;
            return;
        }
    };
    let bundle_system = system_snapshot.map(|value| SystemMetadata {
        os: value.os,
        kernel: value.kernel,
        cpu: value.cpu,
        core_count: value.core_count,
        memory: value.memory,
    });
    if let Err(error) = profiler.write_bundle(
        &config,
        &frame_metrics,
        streaming.as_deref(),
        &renderer,
        bundle_system,
    ) {
        error!(%error, "failed to write profiling bundle");
        exit.write(AppExit::error());
        samples.finished = true;
        return;
    }
    if let Some(path) = &config.benchmark_frame_times
        && let Err(error) = write_frame_times(path, &samples.frame_ms)
    {
        error!(%error, path = %path.display(), "failed to write benchmark frame times");
        exit.write(AppExit::error());
        samples.finished = true;
        return;
    }
    match serde_json::to_vec_pretty(&report)
        .map_err(std::io::Error::other)
        .and_then(|json| fs::write(&config.benchmark_output, json))
    {
        Ok(()) => info!(
            path = %config.benchmark_output.display(),
            average_fps,
            p95_ms = p95,
            passed,
            "acceptance benchmark complete"
        ),
        Err(error) => error!(%error, "failed to write benchmark report"),
    }
    samples.finished = true;
    exit.write(if passed {
        AppExit::Success
    } else {
        AppExit::error()
    });
}

fn memory_settle_seconds(config: &EngineConfig) -> f64 {
    config
        .benchmark_duration_secs
        .map_or(0.0, |duration| (duration * 0.1).min(60.0))
}

fn diagnostic_value(
    store: &DiagnosticsStore,
    path: &bevy::diagnostic::DiagnosticPath,
) -> Option<f64> {
    store.get(path).and_then(|diagnostic| diagnostic.value())
}

fn physics_fixture_ready(selected: bool, metrics: Option<&StreamingMetrics>) -> bool {
    !selected
        || metrics.is_some_and(|value| {
            value.physics_fixture_validated && value.physics_fixture_failures == 0
        })
}

fn no_runtime_failures(
    streaming: Option<&StreamingMetrics>,
    require_material_fixture: bool,
    require_terrain_fixture: bool,
    require_transform_fixture: bool,
) -> bool {
    streaming.map_or(
        !require_material_fixture && !require_terrain_fixture && !require_transform_fixture,
        |value| {
            value.failed_cells == 0
                && value.asset_load_failures == 0
                && value.material_validation_failures == 0
                && value.diagnostic_fallbacks == 0
                && value.terrain_validation_failures == 0
                && value.water_validation_failures == 0
                && value.transform_bounds_validation_failures == 0
                && value.streaming_invariant_failures == 0
                && value.duplicate_cell_roots == 0
                && value.orphaned_cell_roots == 0
                && value.missing_cell_roots == 0
                && value.out_of_range_cell_roots == 0
                && value.streaming_fixture_failures == 0
                && (!require_material_fixture || value.canonical_fixture_validated)
                && (!require_terrain_fixture || value.terrain_water_fixture_validated)
                && (!require_transform_fixture || value.transform_bounds_fixture_validated)
        },
    )
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// Writes the measured frame times as CSV (`frame,ms`), one row per frame after the warm-up, in
/// the order they were measured: the series behind the report's mean and percentiles.
fn write_frame_times(path: &std::path::Path, frame_ms: &[f64]) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, frame_times_csv(frame_ms))
}

fn frame_times_csv(frame_ms: &[f64]) -> String {
    let mut csv = String::with_capacity(16 + frame_ms.len() * 12);
    csv.push_str(
        "frame,ms
",
    );
    for (frame, milliseconds) in frame_ms.iter().enumerate() {
        csv.push_str(&format!(
            "{frame},{milliseconds:.4}
"
        ));
    }
    csv
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signal-timing app used by the gate test: minimal plugins, an asset store to add to,
    /// and a `ProfilingState` for the plugin (or the test) to record into.
    fn timing_app(measures_pacing: bool) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<ProfilingState>();
        install_schedule_timing(&mut app, Some(measures_pacing));
        app.finish();
        app.cleanup();
        app.update();
        app.world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Mesh::from(Cuboid::default()));
        app.update();
        app.update();
        app
    }

    /// A normal play session is not a benchmark, so it installs neither the marker schedules nor
    /// the asset-arrival counting; a benchmark run installs both.
    #[test]
    fn schedule_and_asset_instrumentation_is_benchmark_only() {
        let profiler = timing_app(false);
        let profiler = profiler.world().resource::<ProfilingState>();
        assert!(
            profiler.span_samples("main_schedule/Update").is_empty(),
            "a play session must not add the schedule markers"
        );
        assert!(
            profiler.span_samples("assets_added/mesh").is_empty(),
            "a play session must not count asset arrivals"
        );

        let profiler = timing_app(true);
        let profiler = profiler.world().resource::<ProfilingState>();
        assert!(
            !profiler.span_samples("main_schedule/Update").is_empty(),
            "a benchmark run times the schedules"
        );
        assert!(
            profiler
                .span_samples("assets_added/mesh")
                .iter()
                .sum::<f64>()
                >= 1.0,
            "a benchmark run counts asset arrivals"
        );
    }

    /// The plugin reads the run's `EngineConfig` while it builds, so the config must already be
    /// in the world; this drives that real path rather than the helper.
    #[test]
    fn the_plugin_installs_schedule_timing_from_the_inserted_config() {
        let installed = |config: Option<EngineConfig>| {
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, AssetPlugin::default()));
            if let Some(config) = config {
                app.insert_resource(config);
            }
            app.add_plugins(AcceptanceMetricsPlugin);
            app.is_plugin_added::<crate::schedule_timing::ScheduleTimingPlugin>()
        };
        assert!(installed(Some(EngineConfig {
            benchmark_frames: Some(60),
            ..EngineConfig::default()
        })));
        assert!(!installed(Some(EngineConfig::default())));
        assert!(!installed(None), "no config means no measured run");
    }

    #[test]
    fn trace_keeps_warmup_and_capture_settlement_on_one_frame_clock() {
        for gpu_inventory_enabled in [false, true] {
            check_trace_windows(gpu_inventory_enabled);
        }
    }

    fn check_trace_windows(gpu_inventory_enabled: bool) {
        use crate::profiling::ProfilingPlugin;
        use bevy::time::TimeUpdateStrategy;

        let directory = tempfile::tempdir().unwrap();
        let config = EngineConfig {
            profile_output_dir: Some(directory.path().join("profile")),
            profile_gpu_inventory: gpu_inventory_enabled,
            benchmark_output: directory.path().join("report.json"),
            benchmark_frames: Some(1),
            benchmark_warmup_frames: 2,
            acceptance_screenshot: Some(directory.path().join("pending.png")),
            ..default()
        };
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(config)
            .insert_resource(TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_millis(300),
            ))
            .init_resource::<DiagnosticsStore>()
            .init_resource::<RendererMetrics>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<Image>>()
            .add_plugins((AcceptanceMetricsPlugin, ProfilingPlugin))
            .add_systems(Update, |mut profiler: ResMut<ProfilingState>| {
                profiler.record_ms("test/work", 1.0);
                profiler.record_ms("test/work", 2.0);
                profiler.record_completed_latency_micros("test/ready", 10_000);
                profiler.event("test", "work", None);
            });
        for _ in 0..5 {
            app.update();
        }
        let profiler = app.world().resource::<ProfilingState>();
        let report = profiler.trace.as_ref().unwrap().report();
        assert_eq!(report.gpu_inventory_enabled, gpu_inventory_enabled);
        assert_eq!(report.samples.len(), 5);
        let windows: Vec<_> = report
            .samples
            .iter()
            .map(|sample| sample.benchmark_window)
            .collect();
        assert_eq!(
            windows,
            vec![
                BenchmarkWindow::Warmup,
                BenchmarkWindow::Warmup,
                BenchmarkWindow::Measured,
                BenchmarkWindow::Settlement,
                BenchmarkWindow::Settlement
            ]
        );
        for (index, sample) in report.samples.iter().enumerate() {
            assert_eq!(sample.main_frame, index as u64 + 1);
            assert_eq!(sample.cpu_spans_ms["test/work"], 3.0);
            assert!(!sample.cpu_spans_ms.contains_key("test/ready"));
            assert_eq!(sample.completion_latencies_ms["test/ready"], 10.0);
            if gpu_inventory_enabled {
                assert!(!sample.gpu.as_ref().unwrap().render_available);
            } else {
                assert!(sample.gpu.is_none());
            }
        }
        // The real interval is preserved even when virtual time caps a long frame.
        assert!((report.samples[4].delta_ms - 300.0).abs() < 0.001);
        let benchmark = app.world().resource::<BenchmarkSamples>();
        assert_eq!(benchmark.frames_seen, 3);
        assert_eq!(benchmark.frame_ms.len(), 1);
        assert!(benchmark.measurement_complete);
        assert!(!benchmark.finished);

        std::fs::write(directory.path().join("pending.png"), []).unwrap();
        app.update();
        let saved: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.path().join("profile/streaming-frames.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["samples"].as_array().unwrap().len(), 6);
        let timeline: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.path().join("profile/streaming.json")).unwrap(),
        )
        .unwrap();
        for (index, event) in timeline["timeline"].as_array().unwrap().iter().enumerate() {
            assert_eq!(event["frame"], index as u64 + 1);
        }
    }

    #[test]
    fn camera_motion_ignores_rebases_and_quaternion_sign() {
        let entity = Entity::from_raw_u32(1).unwrap();
        let mut previous = None;
        let first = observe_camera(
            entity,
            &GlobalTransform::from_xyz(10.0, 20.0, 30.0),
            1,
            IVec2::ZERO,
            &mut previous,
        );
        assert_eq!(first.motion, CameraMotion::Unavailable);
        let rebased = GlobalTransform::from(
            Transform::from_xyz(10.0 - CELL_SIZE, 20.0, 30.0 + CELL_SIZE)
                .with_rotation(-Quat::IDENTITY),
        );
        let next = observe_camera(entity, &rebased, 1, IVec2::ONE, &mut previous);
        assert_eq!(next.world_position, first.world_position);
        assert_eq!(next.motion, CameraMotion::Stationary);
        let moved = observe_camera(
            entity,
            &GlobalTransform::from_xyz(11.0, 20.0, 30.0),
            1,
            IVec2::ZERO,
            &mut previous,
        );
        assert_eq!(moved.motion, CameraMotion::Translated);
        let changed_space = observe_camera(
            entity,
            &GlobalTransform::IDENTITY,
            2,
            IVec2::ZERO,
            &mut previous,
        );
        assert_eq!(changed_space.motion, CameraMotion::Unavailable);
    }

    #[test]
    fn frame_times_are_written_in_measured_order() {
        assert_eq!(
            frame_times_csv(&[4.0, 3.25, 16.6667]),
            "frame,ms
0,4.0000
1,3.2500
2,16.6667
"
        );
        assert_eq!(
            frame_times_csv(&[]),
            "frame,ms
"
        );
    }

    #[test]
    fn parses_the_frame_times_path() {
        let config = EngineConfig::run_from_args(
            ["--benchmark-frame-times", "out/frames.csv"]
                .into_iter()
                .map(str::to_owned),
        );
        assert_eq!(
            config.benchmark_frame_times.as_deref(),
            Some(std::path::Path::new("out/frames.csv"))
        );
    }

    #[test]
    fn physics_fixture_benchmark_requires_validation_without_failures() {
        let mut metrics = StreamingMetrics::default();
        assert!(physics_fixture_ready(false, None));
        assert!(!physics_fixture_ready(true, None));
        assert!(!physics_fixture_ready(true, Some(&metrics)));
        metrics.physics_fixture_validated = true;
        assert!(physics_fixture_ready(true, Some(&metrics)));
        metrics.physics_fixture_failures = 1;
        assert!(!physics_fixture_ready(true, Some(&metrics)));
    }

    #[test]
    fn calculates_percentiles_at_the_shared_rank() {
        let samples: Vec<_> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&samples, 0.50), 51.0);
        assert_eq!(percentile(&samples, 0.95), 96.0);
        assert_eq!(percentile(&samples, 0.99), 100.0);
    }

    /// A report without streaming carries no pacing fields at all: `#[serde(flatten)]` on the
    /// `None` must not fail or leave a `null` behind.
    #[test]
    fn a_report_without_pacing_serialises_no_pacing_fields() {
        #[derive(Serialize)]
        struct Flattened {
            frames: usize,
            #[serde(flatten)]
            pacing: Option<PacingReport>,
        }
        let value = serde_json::to_value(Flattened {
            frames: 3,
            pacing: None,
        })
        .expect("a report with no pacing block must serialise");
        assert_eq!(value, serde_json::json!({ "frames": 3 }));
        let value = serde_json::to_value(Flattened {
            frames: 3,
            pacing: Some(PacingReport {
                world_ready_reached: true,
                ..Default::default()
            }),
        })
        .unwrap();
        assert_eq!(value["world_ready_reached"], serde_json::json!(true));
        assert_eq!(value["frames"], serde_json::json!(3));
    }

    #[test]
    fn settles_memory_after_ten_percent_capped_at_one_minute() {
        let mut config = EngineConfig {
            benchmark_duration_secs: Some(120.0),
            ..default()
        };
        assert_eq!(memory_settle_seconds(&config), 12.0);
        config.benchmark_duration_secs = Some(600.0);
        assert_eq!(memory_settle_seconds(&config), 60.0);
        config.benchmark_duration_secs = Some(1800.0);
        assert_eq!(memory_settle_seconds(&config), 60.0);
        config.benchmark_duration_secs = None;
        assert_eq!(memory_settle_seconds(&config), 0.0);
    }

    #[test]
    fn rejects_streaming_or_asset_load_failures() {
        assert!(no_runtime_failures(None, false, false, false));
        assert!(no_runtime_failures(
            Some(&StreamingMetrics::default()),
            false,
            false,
            false
        ));

        let streaming_failure = StreamingMetrics {
            failed_cells: 1,
            ..default()
        };
        assert!(!no_runtime_failures(
            Some(&streaming_failure),
            false,
            false,
            false
        ));

        let asset_failure = StreamingMetrics {
            asset_load_failures: 1,
            ..default()
        };
        assert!(!no_runtime_failures(
            Some(&asset_failure),
            false,
            false,
            false
        ));

        let validation_failure = StreamingMetrics {
            material_validation_failures: 1,
            ..default()
        };
        assert!(!no_runtime_failures(
            Some(&validation_failure),
            false,
            false,
            false
        ));

        let fallback = StreamingMetrics {
            diagnostic_fallbacks: 1,
            ..default()
        };
        assert!(!no_runtime_failures(Some(&fallback), false, false, false));
        assert!(!no_runtime_failures(
            Some(&StreamingMetrics {
                terrain_validation_failures: 1,
                ..default()
            }),
            false,
            false,
            false
        ));
        assert!(!no_runtime_failures(
            Some(&StreamingMetrics {
                water_validation_failures: 1,
                ..default()
            }),
            false,
            false,
            false
        ));
        assert!(!no_runtime_failures(
            Some(&StreamingMetrics {
                transform_bounds_validation_failures: 1,
                ..default()
            }),
            false,
            false,
            false
        ));
        assert!(!no_runtime_failures(
            Some(&StreamingMetrics::default()),
            true,
            false,
            false
        ));
        assert!(no_runtime_failures(
            Some(&StreamingMetrics {
                canonical_fixture_validated: true,
                ..default()
            }),
            true,
            false,
            false
        ));
        assert!(!no_runtime_failures(
            Some(&StreamingMetrics::default()),
            false,
            true,
            false
        ));
        assert!(no_runtime_failures(
            Some(&StreamingMetrics {
                terrain_water_fixture_validated: true,
                ..default()
            }),
            false,
            true,
            false
        ));
        assert!(!no_runtime_failures(
            Some(&StreamingMetrics::default()),
            false,
            false,
            true
        ));
        assert!(no_runtime_failures(
            Some(&StreamingMetrics {
                transform_bounds_fixture_validated: true,
                ..default()
            }),
            false,
            false,
            true
        ));
        assert!(!no_runtime_failures(
            Some(&StreamingMetrics {
                orphaned_cell_roots: 1,
                ..default()
            }),
            false,
            false,
            false
        ));
        assert!(!no_runtime_failures(
            Some(&StreamingMetrics {
                out_of_range_cell_roots: 1,
                ..default()
            }),
            false,
            false,
            false
        ));
    }
}
