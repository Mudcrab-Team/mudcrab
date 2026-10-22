use crate::{
    config::EngineConfig,
    fingerprint::{RendererFingerprint, RunFingerprint},
    render::RendererMetrics,
    streaming::StreamingMetrics,
    streaming_gpu_metrics::{StreamingGpuMetricsBridge, StreamingGpuMetricsPlugin},
    streaming_trace::{SceneLoadStatus, StreamingTrace},
};
use bevy::{
    asset::{AssetId, DependencyLoadState, LoadState, RecursiveDependencyLoadState},
    diagnostic::{Diagnostic, DiagnosticPath, DiagnosticsStore},
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        diagnostic::{
            ErasedRenderAssetDiagnosticPlugin, MeshAllocatorDiagnosticPlugin,
            RenderAssetDiagnosticPlugin,
        },
        mesh::RenderMesh,
        render_resource::WgpuFeatures,
        renderer::{RenderAdapterInfo, RenderDevice},
        texture::GpuImage,
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
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const MAX_SAMPLES_PER_METRIC: usize = 200_000;
const MAX_TIMELINE_EVENTS: usize = 100_000;
const MAX_STREAMING_FRAMES: usize = 20_000;
const MAX_STREAMING_SCENES: usize = 20_000;

/// Why the report has no per-pass GPU times or counters: the run had no renderer, so no device
/// measured anything. This is not the same as a device that lacks the feature, and a report must
/// not blame the backend for work that never happened.
const NO_RENDERER: &str = "no renderer in this run";

/// Why the report has no per-pass GPU times: Bevy's render diagnostics can time GPU work only on a
/// backend with timestamp queries (Vulkan and DX12), and record nothing on the others.
const TIMESTAMP_QUERIES_UNAVAILABLE: &str =
    "the graphics backend does not support timestamp queries, so Bevy records no per-pass GPU time";

/// Why the report has no per-pass shader-invocation counters: DX12 does not offer pipeline
/// statistics.
const PIPELINE_STATISTICS_UNAVAILABLE: &str =
    "the graphics backend does not support pipeline statistics queries";

/// Why a mesh-allocator counter is `null`: the render world has not measured the allocator in this
/// run.
///
/// Bevy's own plugin publishes these counters in the main world's `PreUpdate` every frame and they
/// start at zero, before any extraction has run; `Some(0.0)` from the store therefore proves
/// nothing, and the report waits for [`RenderMeasurementProbe`] instead.
const MESH_ALLOCATOR_UNAVAILABLE: &str =
    "the render world has not measured Bevy's mesh allocator in this run";

/// Why a render-asset count is missing: the render world has not measured that asset type in this
/// run. The same zero-before-extraction caveat as [`MESH_ALLOCATOR_UNAVAILABLE`] applies.
const RENDER_ASSET_UNAVAILABLE: &str =
    "the render world has not measured the live GPU-side count for this asset type in this run";

/// Why a scene count is `null`: the inventory is sampled every 60th frame of the run, so a measured
/// window that holds none of those frames has no count of its own — and the warm-up's count is not
/// reported in its place.
const SCENE_INVENTORY_UNAVAILABLE: &str =
    "the scene inventory was not sampled inside the measured window";

/// Why a pipeline-statistics counter is `null` on a device that supports them: none was recorded,
/// which is what a run with no rendered frame looks like.
const NO_PIPELINE_STATISTICS: &str =
    "no pipeline-statistics counter was recorded for this run, which renders nothing";

/// Why the entity count is `null`: Bevy's entity-count diagnostic was not sampled.
const ENTITY_COUNT_UNAVAILABLE: &str = "Bevy's entity-count diagnostic was not sampled in this run";

/// Why `scene.draw_calls` is always `null`: Bevy has no counter to read.
const DRAW_CALLS_UNAVAILABLE: &str = "Bevy 0.19 exposes no draw-call count";

/// Why `cpu.schedules` is empty: the schedule markers are installed only for a run that measures
/// pacing (see `schedule_timing`).
const SCHEDULE_TIMING_UNAVAILABLE: &str =
    "the run did not measure pacing, so Bevy's main schedules were not timed";

/// Why `cpu.render_world_prep` is always `null`: Bevy 0.19 times no render-app schedule.
///
/// What the engine does record of render-world work is in this report: the `render_world/*` spans
/// in `cpu.spans` (the main world, extract, the wait for the render thread, and the render thread's
/// own phases, from `render_timing`), and per pass `render/<pass>/elapsed_cpu` in the profile
/// bundle's `gpu-passes.json`. Neither is a Bevy schedule timer, so none is reported as one.
const RENDER_WORLD_PREP_UNAVAILABLE: &str = "Bevy 0.19 records no per-schedule time for the render app; the engine's render-world phases are the render_world/* spans here, and per-pass CPU time is render/<pass>/elapsed_cpu in the profile bundle";

/// Why `cpu.spans` is empty: nothing was recorded, which only an aborted run does.
const NO_CPU_SPANS: &str = "the run recorded no CPU spans";

/// Whether the render world has run the extraction systems that fill Bevy's GPU-memory counters.
///
/// Bevy's mesh-allocator and render-asset diagnostics publish atomics in the main world's
/// `PreUpdate` from the first frame — `bevy_render/src/diagnostic/mesh_allocator_diagnostic_plugin.rs`
/// and `render_asset_diagnostic_plugin.rs` — and those atomics start at zero. What puts a real
/// value in them is each plugin's system in the render world's `ExtractSchedule`. A diagnostic
/// value of zero therefore cannot be told from a value the render world never produced, so the
/// report waits for this flag, which only that extraction sets.
///
/// One `Arc<AtomicBool>` is shared between the worlds the way the engine's other cross-world
/// resources are (`render::RendererProofBridge`): the main world holds it as this resource and the
/// render app is given a clone plus the system that sets it.
#[derive(Resource, Clone, Default)]
pub struct RenderMeasurementProbe(Arc<AtomicBool>);

impl RenderMeasurementProbe {
    /// Whether the render world's extraction systems have run at least once, which is the first
    /// moment its counters describe anything.
    pub fn measured(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Records that the render world has measured, called from the render app's `ExtractSchedule`.
    pub fn mark_measured(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Where the sampling of one render-diagnostic path got to: the time of the last measurement taken
/// from it, and how many measurements at exactly that time were taken.
///
/// Bevy's `sync_diagnostics` adds a whole readback batch with one `Instant::now()`, so measurements
/// that share a time belong to one batch, and one batch can hold several observations of the same
/// path — one per camera that ran the pass. The count is what makes taking each of them exactly
/// once possible without taking a batch twice next frame; two batches cannot share a time unless
/// the clock steps backwards, and `Instant` is monotonic.
#[derive(Clone, Copy)]
struct RenderCursor {
    time: Instant,
    taken_at_time: usize,
}

pub struct ProfilingPlugin;

impl Plugin for ProfilingPlugin {
    /// Install aggregate profiling and enable bounded frame tracing only for profile output.
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
    /// How far the sampling of each render-diagnostic path has got, so every measurement is taken
    /// exactly once however many cameras wrote it and however the GPU readback lags.
    render_cursors: BTreeMap<String, RenderCursor>,
    /// The last value sampled for each GPU-memory diagnostic (the mesh allocator's counters and the
    /// live render-asset counts). These are gauges, not distributions: only the last value in the
    /// measured window is reported.
    memory_metrics: BTreeMap<String, f64>,
    /// The frames of the measured window this state sampled: the frames every GPU and
    /// pipeline-statistics number in the report rests on.
    measured_frames: u64,
    /// The measured frames in which the render world's GPU-memory counters were recorded. Zero when
    /// the render world never measured them, whatever zeros the diagnostics store published.
    gpu_memory_frames: u64,
    /// Non-finite render measurements, counted instead of sampled, per diagnostic path.
    rejected_render_measurements: BTreeMap<String, u64>,
    render_capabilities: Option<RenderCapabilities>,
    window: Option<WindowMetadata>,
    draw_submission: Option<crate::indirect_metrics::DrawSubmissionReport>,
    draw_submission_frames: Vec<crate::indirect_metrics::DrawSubmissionFrame>,
    dropped_draw_submission_frames: u64,
    counters: BTreeMap<String, u64>,
    gauges: BTreeMap<String, f64>,
    /// The counts [`ProfilingState::stage_scene_inventory`] took this frame, waiting for
    /// `sample_frame` to say whether the frame is inside the measured window. They are kept apart
    /// from `gauges` so a count taken during the warm-up cannot be reported as a measured one, and
    /// they hold at most the frame's own inventory: staging replaces them.
    staged_inventory: Option<BTreeMap<String, f64>>,
    timeline: Vec<TimelineEvent>,
    memory: Vec<MemorySample>,
    pub(crate) trace: Option<StreamingTrace>,
    pub(crate) frame_cpu_spans_ms: BTreeMap<String, f64>,
    pub(crate) frame_completion_latencies_ms: BTreeMap<String, f64>,
    pub(crate) trace_delta_ms: f64,
}

impl Default for ProfilingState {
    /// Start an empty aggregate profile with frame tracing disabled.
    fn default() -> Self {
        Self {
            started: Instant::now(),
            frame: 0,
            cpu_spans_ms: BTreeMap::new(),
            render_metrics: BTreeMap::new(),
            render_cursors: BTreeMap::new(),
            memory_metrics: BTreeMap::new(),
            measured_frames: 0,
            gpu_memory_frames: 0,
            rejected_render_measurements: BTreeMap::new(),
            render_capabilities: None,
            window: None,
            draw_submission: None,
            draw_submission_frames: Vec::new(),
            dropped_draw_submission_frames: 0,
            counters: BTreeMap::new(),
            gauges: BTreeMap::new(),
            staged_inventory: None,
            timeline: Vec::new(),
            memory: Vec::new(),
            trace: None,
            frame_cpu_spans_ms: BTreeMap::new(),
            frame_completion_latencies_ms: BTreeMap::new(),
            trace_delta_ms: 0.0,
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

    /// Record elapsed asynchronous latency in the arrival frame and the aggregate profile.
    pub(crate) fn record_completed_latency_elapsed(
        &mut self,
        name: impl Into<String>,
        started: Instant,
    ) {
        self.record_completed_latency_ms(name.into(), started.elapsed().as_secs_f64() * 1000.0);
    }

    /// Accumulate completion latency separately from frame CPU spans while preserving aggregate samples.
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

    /// Record a CPU span in the bounded aggregate and, when enabled, sum it into this frame.
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

    /// Return the shared main-frame index used by trace samples and timeline events.
    pub(crate) fn main_frame(&self) -> u64 {
        self.frame
    }

    /// Return milliseconds since this profiling session began.
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

    /// Records a gauge outright: the last value written is what the bundle carries, whatever frame
    /// it was written in.
    ///
    /// The scene inventory is the one gauge set the report reads a measured number out of, so it
    /// does not come through here — it is staged with [`ProfilingState::stage_scene_inventory`] and
    /// kept only if the frame is inside the measured window.
    pub fn set_gauge(&mut self, name: impl Into<String>, value: f64) {
        self.gauges.insert(name.into(), value);
    }

    /// Stages the scene inventory taken this frame, to be kept only if the frame turns out to be
    /// inside the measured window.
    ///
    /// The inventory runs in `Update` and the benchmark samples in `Last`, so the frame's
    /// measured-ness is not known when the counts are taken; `sample_frame` — later in the same
    /// frame — commits them to `gauges` or drops them. A staged inventory is never carried into a
    /// later frame, so a warm-up count cannot be reported as a measured one.
    fn stage_scene_inventory(&mut self, counts: BTreeMap<String, f64>) {
        self.staged_inventory = Some(counts);
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

    /// Samples a frame: the process memory every frame, and the GPU diagnostics when `measured`.
    ///
    /// `measured` is whether this frame is inside the measured window — after the warm-up frames a
    /// benchmark leaves out — and `gpu_memory_measured` is whether the render world has produced
    /// its counters at all ([`RenderMeasurementProbe`]). Both are the caller's answer; this state
    /// cannot see the warm-up gate or the render world itself.
    ///
    /// Every render-diagnostic measurement is taken exactly once, however many cameras wrote it and
    /// however long the GPU readback takes: [`take_render_measurements`] advances a cursor per path
    /// rather than resampling Bevy's latest value. Only measurements arriving inside the measured
    /// window are kept — the ones that arrive during the warm-up are consumed and dropped, so they
    /// cannot leak into a later frame's samples. Because the readback lags, a measurement taken in
    /// the first measured frames can describe a pass recorded a frame or two earlier, in the
    /// warm-up; there is no per-camera record to split that, and dropping a fixed number of frames
    /// instead would be a guess about the latency. The scene inventory taken earlier in the frame
    /// is kept under the same rule. The process-memory timeline stays whole-run: it is a growth
    /// curve, not one of the frame statistics.
    pub fn sample_frame(
        &mut self,
        diagnostics: &DiagnosticsStore,
        process_memory_gib: Option<f64>,
        measured: bool,
        gpu_memory_measured: bool,
    ) {
        if self.trace.is_none() {
            self.frame = self.frame.saturating_add(1);
        }
        // The scene inventory is staged earlier in the same frame (it runs in `Update`, this sample
        // in `Last`), so this is where its frame's measured-ness is known. The staged counts are
        // taken either way — a warm-up inventory is dropped, never left to be committed by a later
        // measured frame.
        let staged = self.staged_inventory.take();
        if measured && let Some(counts) = staged {
            self.gauges.extend(counts);
        }
        if measured {
            self.measured_frames = self.measured_frames.saturating_add(1);
            if gpu_memory_measured {
                self.gpu_memory_frames = self.gpu_memory_frames.saturating_add(1);
            }
        }
        for diagnostic in diagnostics.iter() {
            let path = diagnostic.path().as_str();
            if path.starts_with("render/") {
                // A disabled diagnostic keeps its history but is not a measurement of this run.
                if diagnostic.is_enabled {
                    take_render_measurements(
                        diagnostic,
                        &mut self.render_metrics,
                        &mut self.render_cursors,
                        &mut self.rejected_render_measurements,
                        measured,
                    );
                }
            }
            // The GPU-memory diagnostics do not sit under `render/`, so they are matched by their
            // own names, deliberately: the mesh allocator's counters and the live render-asset
            // counts, the only paths the report's memory section reads. They are gauges, so the
            // last value of the measured window is what is kept — and only once the render world
            // has measured them, never the zero Bevy publishes before its first extraction.
            else if measured
                && gpu_memory_measured
                && is_gpu_memory_path(path)
                && let Some(value) = diagnostic.value()
            {
                self.memory_metrics.insert(path.to_owned(), value);
            }
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

    /// The per-pass GPU times, summarised over the measured window (see [`GpuPassReport`]).
    ///
    /// `renderer` is the run's renderer fingerprint, or `None` for a run that had no renderer; it
    /// is where the device's capabilities come from, so a renderer-less run says so rather than
    /// blaming a backend that never ran.
    pub fn gpu_passes(&self, renderer: Option<&RendererFingerprint>) -> GpuPassReport {
        let timestamp_queries_supported =
            renderer.is_some_and(|value| value.timestamp_queries_supported);
        let pipeline_statistics_supported =
            renderer.is_some_and(|value| value.pipeline_statistics_supported);
        // Without timestamp queries no such sample can be real, so a stray one (a diagnostic left
        // over from another device, say) is dropped rather than reported as a pass time.
        let passes: BTreeMap<String, MetricSummary> = if timestamp_queries_supported {
            self.render_metrics
                .iter()
                .filter(|(path, _)| path.ends_with("/elapsed_gpu"))
                .map(|(path, samples)| (path.clone(), summarize(samples)))
                .collect()
        } else {
            BTreeMap::new()
        };
        let mut unavailable = BTreeMap::new();
        if renderer.is_none() {
            unavailable.insert("elapsed_gpu".to_owned(), NO_RENDERER.to_owned());
            unavailable.insert("pipeline_statistics".to_owned(), NO_RENDERER.to_owned());
        } else {
            if !timestamp_queries_supported {
                unavailable.insert(
                    "elapsed_gpu".to_owned(),
                    TIMESTAMP_QUERIES_UNAVAILABLE.to_owned(),
                );
            } else if passes.is_empty() {
                unavailable.insert(
                    "elapsed_gpu".to_owned(),
                    "no per-pass GPU time was recorded for this run".to_owned(),
                );
            }
            if !pipeline_statistics_supported {
                unavailable.insert(
                    "pipeline_statistics".to_owned(),
                    PIPELINE_STATISTICS_UNAVAILABLE.to_owned(),
                );
            }
        }
        GpuPassReport {
            timestamp_queries_supported,
            pipeline_statistics_supported,
            measured_frames: self.measured_frames,
            passes,
            unavailable,
        }
    }

    /// What the rendered scene contained: the entity counts and the per-pass counters the graphics
    /// backend can report (see [`SceneReport`]).
    ///
    /// The counts come from the inventory `sample_scene_inventory` takes every 60th frame (the mesh
    /// entities, terrain patches and water surfaces of the world) and `sample_frame` keeps only when
    /// that frame is inside the measured window — the same window as the counters below and the
    /// entity count, never the warm-up's numbers. The entity count comes from Bevy's own diagnostic,
    /// which `entity_count` carries. A count that was never sampled inside the window is `null` with
    /// the reason in `unavailable`, never a zero: zero mesh entities in a synthetic scene is a
    /// count, a missing sample is not.
    pub fn scene_report(
        &self,
        entity_count: Option<u64>,
        renderer: Option<&RendererFingerprint>,
    ) -> SceneReport {
        let pipeline_statistics_supported =
            renderer.is_some_and(|value| value.pipeline_statistics_supported);
        let mut unavailable = BTreeMap::new();
        if entity_count.is_none() {
            unavailable.insert(
                "entity_count".to_owned(),
                ENTITY_COUNT_UNAVAILABLE.to_owned(),
            );
        }
        let count = |name: &str| self.gauges.get(name).map(|value| value.max(0.0) as u64);
        let mesh_entities = count("scene/mesh_entities");
        let terrain_patches = count("scene/terrain_patches");
        let water_surfaces = count("scene/water_surfaces");
        for (field, sampled) in [
            ("mesh_entities", mesh_entities),
            ("terrain_patches", terrain_patches),
            ("water_surfaces", water_surfaces),
        ] {
            if sampled.is_none() {
                unavailable.insert(field.to_owned(), SCENE_INVENTORY_UNAVAILABLE.to_owned());
            }
        }
        // Without pipeline statistics no such counter can be real, so a stray sample (a diagnostic
        // left over from another device, say) is dropped rather than reported as a count.
        let counters = |suffix: &str| -> BTreeMap<String, MetricSummary> {
            if !pipeline_statistics_supported {
                return BTreeMap::new();
            }
            self.render_metrics
                .iter()
                .filter(|(path, _)| path.ends_with(suffix))
                .map(|(path, samples)| (path.clone(), summarize(samples)))
                .collect()
        };
        let vertex_shader_invocations = counters("/vertex_shader_invocations");
        let clipper_primitives_out = counters("/clipper_primitives_out");
        if renderer.is_none() {
            unavailable.insert("pipeline_statistics".to_owned(), NO_RENDERER.to_owned());
        } else if !pipeline_statistics_supported {
            unavailable.insert(
                "pipeline_statistics".to_owned(),
                PIPELINE_STATISTICS_UNAVAILABLE.to_owned(),
            );
        } else if vertex_shader_invocations.is_empty() && clipper_primitives_out.is_empty() {
            unavailable.insert(
                "pipeline_statistics".to_owned(),
                NO_PIPELINE_STATISTICS.to_owned(),
            );
        }
        unavailable.insert("draw_calls".to_owned(), DRAW_CALLS_UNAVAILABLE.to_owned());
        SceneReport {
            entity_count,
            mesh_entities,
            terrain_patches,
            water_surfaces,
            pipeline_statistics_supported,
            measured_frames: self.measured_frames,
            vertex_shader_invocations,
            clipper_primitives_out,
            draw_calls: None,
            unavailable,
        }
    }

    /// Where the run's CPU time went: the main schedules Bevy ran and the engine's own spans (see
    /// [`CpuReport`]).
    pub fn cpu_report(&self) -> CpuReport {
        let mut schedules = BTreeMap::new();
        let mut spans = BTreeMap::new();
        for (name, samples) in &self.cpu_spans_ms {
            // `assets_added/*` hold counts, not milliseconds (see `schedule_timing`), so they are
            // left out of a table of CPU time; they stay in the profile bundle's `cpu-spans.json`.
            if name.starts_with("assets_added/") {
                continue;
            }
            let summary = summarize(samples);
            if name.starts_with("main_schedule/") {
                schedules.insert(name.clone(), summary);
            } else {
                spans.insert(name.clone(), summary);
            }
        }
        let mut unavailable = BTreeMap::new();
        if schedules.is_empty() {
            unavailable.insert(
                "schedules".to_owned(),
                SCHEDULE_TIMING_UNAVAILABLE.to_owned(),
            );
        }
        if spans.is_empty() {
            unavailable.insert("spans".to_owned(), NO_CPU_SPANS.to_owned());
        }
        unavailable.insert(
            "render_world_prep".to_owned(),
            RENDER_WORLD_PREP_UNAVAILABLE.to_owned(),
        );
        CpuReport {
            schedules,
            spans,
            render_world_prep: None,
            unavailable,
        }
    }

    /// The last value of the GPU-memory diagnostics in the measured window (see
    /// [`GpuMemoryReport`]).
    ///
    /// `renderer` is the run's renderer fingerprint, or `None` for a run with no renderer; a
    /// counter that was not measured is `null` and the reason says which of the two it was.
    pub fn gpu_memory(&self, renderer: Option<&RendererFingerprint>) -> GpuMemoryReport {
        let mut unavailable = BTreeMap::new();
        let reason = |specific: &str| {
            if renderer.is_none() {
                NO_RENDERER.to_owned()
            } else {
                specific.to_owned()
            }
        };
        let mut mesh_allocator = |path: &DiagnosticPath, field: &str| match self.memory_sample(path)
        {
            Some(value) => Some(value.max(0.0) as u64),
            None => {
                unavailable.insert(field.to_owned(), reason(MESH_ALLOCATOR_UNAVAILABLE));
                None
            }
        };
        let mesh_allocator_slab_bytes = mesh_allocator(
            MeshAllocatorDiagnosticPlugin::slabs_size_diagnostic_path(),
            "mesh_allocator_slab_bytes",
        );
        let mesh_allocator_slabs = mesh_allocator(
            MeshAllocatorDiagnosticPlugin::slabs_diagnostic_path(),
            "mesh_allocator_slabs",
        );
        let mesh_allocator_allocations = mesh_allocator(
            MeshAllocatorDiagnosticPlugin::allocations_diagnostic_path(),
            "mesh_allocator_allocations",
        );
        let mut render_asset_counts = BTreeMap::new();
        for (label, path) in gpu_asset_diagnostics() {
            match self.memory_sample(&path) {
                Some(value) => {
                    render_asset_counts.insert(label.to_owned(), value.max(0.0) as u64);
                }
                None => {
                    unavailable.insert(
                        format!("gpu_asset_counts/{label}"),
                        reason(RENDER_ASSET_UNAVAILABLE),
                    );
                }
            }
        }
        GpuMemoryReport {
            mesh_allocator_slab_bytes,
            mesh_allocator_slabs,
            mesh_allocator_allocations,
            render_asset_counts,
            measured_frames: self.gpu_memory_frames,
            unavailable,
        }
    }

    /// The last value sampled for a diagnostic path, if it was sampled at all.
    fn memory_sample(&self, path: &DiagnosticPath) -> Option<f64> {
        self.memory_metrics.get(path.as_str()).copied()
    }

    /// Write aggregate profiling files and the optional frame trace to the configured output directory.
    /// Return without writing when no directory is configured; propagate filesystem and JSON errors.
    pub fn write_bundle(
        &self,
        config: &EngineConfig,
        frame_metrics: &serde_json::Value,
        streaming: Option<&StreamingMetrics>,
        renderer: &RendererMetrics,
        system: Option<SystemMetadata>,
        fingerprint: Option<&RunFingerprint>,
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
        // The device's answer, read from the run fingerprint rather than inferred from which pass
        // paths were sampled: warm-up samples are dropped, so a device that supports timestamp
        // queries but sampled no pass inside the measured window would otherwise say `false` here
        // while the report says `true`. The report takes these two flags from the same fingerprint,
        // so the bundle and the report cannot disagree; a run with no renderer says `false` in both,
        // which is what the report's own section states for such a run.
        let device = fingerprint.and_then(|value| value.renderer.as_ref());
        let timestamp_queries_supported =
            device.is_some_and(|value| value.timestamp_queries_supported);
        let pipeline_statistics_supported =
            device.is_some_and(|value| value.pipeline_statistics_supported);
        let mut gpu = gpu_profile(render.clone(), self.render_capabilities.clone());
        gpu.rejected_nonfinite_measurements = self.rejected_render_measurements.clone();
        gpu.timestamp_queries_supported = timestamp_queries_supported;
        gpu.pipeline_statistics_supported = pipeline_statistics_supported;
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
                build_profile: option_env!("MUDCRAB_BUILD_PROFILE").unwrap_or(
                    if cfg!(debug_assertions) {
                        "debug"
                    } else {
                        "release"
                    },
                ),
                resolution: self
                    .window
                    .as_ref()
                    .map(|window| window.physical_resolution),
                window: self.window.clone(),
                worldspace_id: config.worldspace_id,
                start_grid: config.start_grid,
                stream_radius: config.stream_radius,
                synthetic_instances: config.synthetic_instances,
                system,
                fingerprint: fingerprint.cloned(),
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

/// Advance the trace clock, retain the real frame delta and clear frame-local span totals.
/// Request a matching render observation when GPU inventory is enabled.
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

/// Refresh an observed scene from asset-server state, marking it inactive if no state remains.
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

/// Classify scene loading, requiring recursive dependencies for CPU readiness.
/// An absent load state remains unknown; scene or recursive dependency failures count as failed.
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

/// Takes the measurements of one render diagnostic that no earlier sample saw, advancing its cursor.
///
/// Bevy's [`Diagnostic::value`] is only the latest measurement, and its history is a bounded deque
/// (`DEFAULT_MAX_HISTORY_LENGTH`, 120), so this walks the history and takes everything newer than
/// the cursor: several cameras' observations of one pass in one readback batch are all kept, and a
/// frame whose readback has not arrived yet adds nothing. More than 120 observations of one path
/// between two samples would lose the oldest — that takes more than 120 cameras running one pass in
/// a single batch, and Bevy itself would have dropped them.
///
/// Bevy records no camera with a measurement (`RenderDiagnostic` is a path, a suffix and a value),
/// so a path's samples mix every camera that ran that pass; there is no way to tell them apart.
fn take_render_measurements(
    diagnostic: &Diagnostic,
    metrics: &mut BTreeMap<String, Vec<f64>>,
    cursors: &mut BTreeMap<String, RenderCursor>,
    rejected: &mut BTreeMap<String, u64>,
    keep: bool,
) {
    let path = diagnostic.path().as_str();
    let cursor = cursors.get(path).copied();
    let mut seen_at_cursor_time = 0;
    let mut next = cursor;
    for measurement in diagnostic.measurements() {
        if let Some(cursor) = cursor {
            if measurement.time < cursor.time {
                continue;
            }
            if measurement.time == cursor.time {
                seen_at_cursor_time += 1;
                if seen_at_cursor_time <= cursor.taken_at_time {
                    continue;
                }
            }
        }
        if keep {
            if measurement.value.is_finite() {
                push_bounded(
                    metrics.entry(path.to_owned()).or_default(),
                    measurement.value,
                );
            } else {
                let count = rejected.entry(path.to_owned()).or_default();
                *count = count.saturating_add(1);
            }
        }
        next = Some(match next {
            Some(next) if next.time == measurement.time => RenderCursor {
                taken_at_time: next.taken_at_time.saturating_add(1),
                ..next
            },
            _ => RenderCursor {
                time: measurement.time,
                taken_at_time: 1,
            },
        });
    }
    if let Some(next) = next {
        cursors.insert(path.to_owned(), next);
    }
}

/// Takes the scene inventory every 60th frame — the mesh entities, terrain patches and water
/// surfaces of the world — and stages it for the frame's `sample_frame` to keep or drop.
///
/// The system runs in `Update`, before the benchmark's own sampling in `Last`, so it cannot know
/// whether the frame is inside the measured window. The counts it stages are kept only if it is
/// (see [`ProfilingState::stage_scene_inventory`]): a run whose measured window holds none of these
/// frames reports the counts as `null` with a reason rather than the warm-up's numbers.
#[allow(clippy::too_many_arguments)]
fn sample_scene_inventory(
    mut profiler: ResMut<ProfilingState>,
    mut frames: Local<u64>,
    meshes: Query<(), With<Mesh3d>>,
    terrain: Query<&ViewVisibility, With<crate::world::components::TerrainPatch>>,
    merged_terrain: Query<&ViewVisibility, With<crate::streaming::MergedTerrainCell>>,
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
    let mut counts = BTreeMap::new();
    counts.insert(
        "scene/mesh_entities".to_owned(),
        meshes.iter().count() as f64,
    );
    counts.insert(
        "scene/terrain_patches".to_owned(),
        terrain.iter().count() as f64,
    );
    counts.insert(
        "scene/lod_terrain_batches".to_owned(),
        lod_terrain_batches.iter().count() as f64,
    );
    counts.insert(
        "scene/structural_nodes_no_cpu_culling".to_owned(),
        structural_nodes.iter().count() as f64,
    );
    counts.insert(
        "scene/visible_terrain_patches".to_owned(),
        terrain.iter().filter(|visible| visible.get()).count() as f64,
    );
    // A terrain patch is one drawn terrain mesh: one per merged cell, four per quadrant-path cell.
    let merged_cells = merged_terrain.iter().count();
    counts.insert("scene/terrain_merged_cells".to_owned(), merged_cells as f64);
    counts.insert(
        "scene/terrain_quadrant_patches".to_owned(),
        (terrain.iter().count() - merged_cells) as f64,
    );
    counts.insert(
        "scene/water_surfaces".to_owned(),
        water.iter().count() as f64,
    );
    counts.insert(
        "scene/bounded_instances".to_owned(),
        bounds.iter().count() as f64,
    );
    counts.insert("assets/meshes".to_owned(), mesh_assets.len() as f64);
    counts.insert("assets/images".to_owned(), images.len() as f64);
    profiler.stage_scene_inventory(counts);
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

/// The profile bundle's metadata: what this run was, and where the bundle came from.
///
/// `fingerprint` is the authoritative record of the machine, build and settings. The fields that
/// also appear in it (commit, dirty state, build profile, resolution, versions, stream radius) are
/// still written because existing readers of `metadata.json` depend on them, and they come from
/// the same config and build values the fingerprint is collected from.
#[derive(Debug, Clone, Serialize)]
struct Metadata {
    format_version: u32,
    engine_version: &'static str,
    bevy_version: &'static str,
    generated_unix_ms: u128,
    scenario: String,
    run_id: String,
    hardware_label: String,
    commit: String,
    dirty_worktree: bool,
    build_profile: &'static str,
    resolution: Option<[u32; 2]>,
    window: Option<WindowMetadata>,
    worldspace_id: u32,
    start_grid: (i32, i32),
    stream_radius: i32,
    synthetic_instances: usize,
    system: Option<SystemMetadata>,
    fingerprint: Option<RunFingerprint>,
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

/// The report's per-pass GPU times: one summary per Bevy render-diagnostic path, plus what the
/// device could not measure.
///
/// The paths are Bevy's own: `render/<pass>/elapsed_gpu`, where `<pass>` is the label the render
/// pass was created with (`main_opaque_pass_3d`, `tonemapping`, `shadow_directional_light_0_cascade_1`,
/// …). Several cameras that run the same pass write the same path and Bevy records no camera with a
/// measurement, so a summary covers every camera that ran it and its samples mix them; compare
/// cameras by running them separately. Every sample is taken once from Bevy's own history, not
/// resampled from its latest value, so `count` is the number of pass observations kept — never more
/// than the frames they were read in. When the device cannot time GPU work the passes are left
/// empty and `unavailable` says why, rather than reporting zeros.
#[derive(Debug, Clone, Serialize)]
pub struct GpuPassReport {
    pub timestamp_queries_supported: bool,
    pub pipeline_statistics_supported: bool,
    /// The frames of the measured window the passes were sampled over: the frame count behind these
    /// numbers. A pass's own `count` can be smaller — its GPU readback lags — and larger — several
    /// cameras ran it.
    pub measured_frames: u64,
    pub passes: BTreeMap<String, MetricSummary>,
    pub unavailable: BTreeMap<String, String>,
}

/// The run's memory, split by where it is: Bevy's CPU-side asset data, and the GPU-side counters
/// the render diagnostics expose.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryReport {
    pub cpu: CpuAssetBytes,
    pub gpu: GpuMemoryReport,
}

/// The CPU-side bytes of the mesh and image assets, read once at the end of the run.
///
/// A mesh Bevy has extracted to the render world without keeping a main-world copy (a
/// `RenderAssetUsages::RENDER_WORLD` mesh, whose GPU data was taken) has no attribute or index data
/// left to measure; those meshes are counted in `meshes_without_cpu_data` and contribute no bytes
/// to `mesh_bytes`. Bevy 0.19's glTF loader keeps both worlds by default
/// (`GltfLoaderSettings::load_meshes` is `RenderAssetUsages::default()`) and the engine does not
/// override it, so streamed models keep their main-world mesh data and are measured here.
#[derive(Debug, Clone, Serialize)]
pub struct CpuAssetBytes {
    pub meshes: usize,
    pub mesh_bytes: u64,
    pub meshes_without_cpu_data: usize,
    pub images: usize,
    pub image_bytes: u64,
}

/// The GPU-side memory counters, from Bevy's mesh-allocator and render-asset diagnostics.
///
/// Every field is a counter, not a VRAM total: wgpu reports how many resources are live, and the
/// engine can read no vendor memory counter. `mesh_allocator_slab_bytes` is the byte size of the
/// allocator's own slabs, the one real byte figure available. A counter is `null` with the reason
/// in `unavailable` until the render world has measured it, never the zero Bevy publishes before
/// its first extraction.
#[derive(Debug, Clone, Serialize)]
pub struct GpuMemoryReport {
    pub mesh_allocator_slab_bytes: Option<u64>,
    pub mesh_allocator_slabs: Option<u64>,
    pub mesh_allocator_allocations: Option<u64>,
    /// Live render-asset counts, by asset type: a count of assets, never a byte size.
    pub render_asset_counts: BTreeMap<String, u64>,
    /// The measured frames in which the render world's counters were sampled: the number of frames
    /// these values rest on, `0` when the render world never measured them.
    pub measured_frames: u64,
    pub unavailable: BTreeMap<String, String>,
}

/// What the run's scene contained: the entity counts, and the per-pass pipeline-statistics
/// counters the graphics backend can report.
///
/// The counters are Bevy's own render diagnostics, `render/<pass>/vertex_shader_invocations` and
/// `render/<pass>/clipper_primitives_out`. Only Vulkan offers pipeline statistics, so on DX12 (and
/// on every backend without them) both maps are empty and `unavailable` says why; the counts are
/// per pass, and several cameras that run the same pass write the same path.
///
/// `draw_calls` is always `null`: Bevy 0.19 exposes no draw-call count. It is spelled out rather
/// than left off so a reader of the report does not have to conclude it is missing.
#[derive(Debug, Clone, Serialize)]
pub struct SceneReport {
    pub entity_count: Option<u64>,
    pub mesh_entities: Option<u64>,
    pub terrain_patches: Option<u64>,
    pub water_surfaces: Option<u64>,
    pub pipeline_statistics_supported: bool,
    /// The frames of the measured window the counters were sampled over: the frame count behind
    /// their per-pass `count`s.
    pub measured_frames: u64,
    /// Per pass: shader invocations, over the measured frames, as `mean`/`p95`/`worst` and friends.
    pub vertex_shader_invocations: BTreeMap<String, MetricSummary>,
    /// Per pass: primitives that survived clipping, over the measured frames.
    pub clipper_primitives_out: BTreeMap<String, MetricSummary>,
    /// Always `null`; see `unavailable["draw_calls"]` for why.
    pub draw_calls: Option<u64>,
    pub unavailable: BTreeMap<String, String>,
}

/// Where the run's CPU time went, in milliseconds.
///
/// `schedules` are the eight Bevy main schedules `schedule_timing` times (`main_schedule/<name>`);
/// `spans` is every other span the engine recorded by hand, the `streaming/*` spans among them.
/// The two are kept apart so a reader can see Bevy's own frame structure without sorting it out of
/// the engine's spans. Asset-arrival counts (`assets_added/*`) are counts, not milliseconds, so they
/// appear in neither map; the profile bundle's `cpu-spans.json` keeps them.
///
/// `render_world_prep` is always `null` on Bevy 0.19 and says so; see
/// `unavailable["render_world_prep"]`.
#[derive(Debug, Clone, Serialize)]
pub struct CpuReport {
    pub schedules: BTreeMap<String, MetricSummary>,
    pub spans: BTreeMap<String, MetricSummary>,
    /// Always `null`; see `unavailable["render_world_prep"]` for why and for what does exist.
    pub render_world_prep: Option<BTreeMap<String, MetricSummary>>,
    pub unavailable: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct CpuProfile {
    spans: BTreeMap<String, MetricSummary>,
    counters: BTreeMap<String, u64>,
    gauges: BTreeMap<String, f64>,
}

/// The profile bundle's per-pass GPU metrics.
///
/// The two capability flags are the device's own answer, taken from the run fingerprint in
/// `metadata.json` (see [`write_bundle`](ProfilingState::write_bundle)): the same flags the report's
/// `gpu_passes` section carries, so the bundle and the report cannot disagree about the device.
/// Which paths were *sampled* is not an answer to that question — a warm-up-only run samples none.
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

/// Whether a diagnostic path is one of the GPU-memory counters.
///
/// These sit outside the `render/` tree — the mesh allocator's counters are top-level paths and the
/// render-asset counts are under `render_asset/` — so [`ProfilingState::sample_frame`] matches them
/// by name instead of by prefix, keeping only the paths the report's memory section reads.
fn is_gpu_memory_path(path: &str) -> bool {
    path.starts_with("mesh_allocator_")
        || path.starts_with("render_asset/")
        || path.starts_with("erased_render_asset/")
}

/// The live GPU-asset counts the report records, each with the label it is reported under.
///
/// Bevy's plugins build each path out of the Rust type name of the *prepared* asset (Bevy 0.19
/// prepares a source asset into a GPU-side one), so each path is taken from the plugin itself
/// rather than spelled out here; the labels are the report's own, stable across versions.
///
/// Materials are one shared count, not one per material type: every `MeshMaterial3d<M>` prepares
/// into the same erased `PreparedMaterial` map, so Bevy reports the same number for each material
/// type it is asked about (see `ErasedRenderAssetDiagnosticPlugin`).
fn gpu_asset_diagnostics() -> [(&'static str, DiagnosticPath); 3] {
    [
        (
            "mesh",
            RenderAssetDiagnosticPlugin::<RenderMesh>::render_asset_diagnostic_path(),
        ),
        (
            "image",
            RenderAssetDiagnosticPlugin::<GpuImage>::render_asset_diagnostic_path(),
        ),
        (
            "material",
            ErasedRenderAssetDiagnosticPlugin::<MeshMaterial3d<StandardMaterial>>::
                render_asset_diagnostic_path(),
        ),
    ]
}

/// The CPU-side bytes of the run's mesh and image assets (see [`CpuAssetBytes`]).
///
/// A mesh's bytes are its vertex attributes (each attribute's element size times its vertex count)
/// plus its indices; an image's are the bytes of its pixel data. Both are read once, at the end of
/// the run, rather than per frame: the figures move only when assets load or unload.
pub fn cpu_asset_bytes(meshes: &Assets<Mesh>, images: &Assets<Image>) -> CpuAssetBytes {
    let mut mesh_bytes = 0;
    let mut meshes_without_cpu_data = 0;
    for (_, mesh) in meshes.iter() {
        let Ok(attributes) = mesh.try_attributes() else {
            meshes_without_cpu_data += 1;
            continue;
        };
        mesh_bytes += attributes
            .map(|(attribute, values)| values.len() as u64 * attribute.format.size())
            .sum::<u64>();
        if let Ok(indices) = mesh.try_indices() {
            mesh_bytes += indices.len() as u64 * index_size(indices);
        }
    }
    let image_bytes = images
        .iter()
        .map(|(_, image)| image.data.as_ref().map_or(0, Vec::len) as u64)
        .sum();
    CpuAssetBytes {
        meshes: meshes.len(),
        mesh_bytes,
        meshes_without_cpu_data,
        images: images.len(),
        image_bytes,
    }
}

/// The size in bytes of one index of a mesh's index buffer.
fn index_size(indices: &bevy::mesh::Indices) -> u64 {
    match indices {
        bevy::mesh::Indices::U16(_) => 2,
        bevy::mesh::Indices::U32(_) => 4,
    }
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
    use bevy::platform::time::Instant as DiagnosticInstant;
    use std::time::Duration;

    /// Feeds a diagnostic store with one measurement of a path, the way a diagnostic plugin's own
    /// system does, so the tests drive the real sampling rather than a shortcut.
    fn feed(diagnostics: &mut DiagnosticsStore, path: &str, value: f64) {
        feed_at(diagnostics, path, value, Instant::now());
    }

    /// Feeds a measurement with an explicit time. Bevy's `sync_diagnostics` stamps a whole readback
    /// batch with one `Instant::now()`, so a test that wants several observations in one batch —
    /// several cameras running one pass — feeds them with the same time.
    fn feed_at(diagnostics: &mut DiagnosticsStore, path: &str, value: f64, time: Instant) {
        let path = DiagnosticPath::new(path.to_owned());
        if diagnostics.get(&path).is_none() {
            diagnostics.add(Diagnostic::new(path.clone()));
        }
        diagnostics
            .get_mut(&path)
            .expect("the diagnostic was just added")
            .add_measurement(DiagnosticMeasurement { time, value });
    }

    /// A renderer fingerprint carrying the two GPU-diagnostic features, for the report sections
    /// that read the device's capabilities rather than a bare pair of flags.
    fn renderer(
        timestamp_queries_supported: bool,
        pipeline_statistics_supported: bool,
    ) -> RendererFingerprint {
        RendererFingerprint {
            backend: "Vulkan".into(),
            adapter: "Test Adapter".into(),
            driver: "test-driver".into(),
            driver_info: "1.2.3".into(),
            device_type: "DiscreteGpu".into(),
            timestamp_queries_supported,
            pipeline_statistics_supported,
        }
    }

    /// Per-pass GPU time is summarised over every measured frame, and the pipeline-statistics
    /// counters that share the `render/` tree stay out of the per-pass table.
    #[test]
    fn summarises_per_pass_gpu_time_from_sampled_diagnostics() {
        let mut diagnostics = DiagnosticsStore::default();
        let mut profiler = ProfilingState::default();
        for value in [1.0, 2.0, 3.0, 4.0] {
            feed(
                &mut diagnostics,
                "render/main_opaque_pass_3d/elapsed_gpu",
                value,
            );
            feed(
                &mut diagnostics,
                "render/tonemapping/elapsed_gpu",
                value * 2.0,
            );
            feed(
                &mut diagnostics,
                "render/main_opaque_pass_3d/vertex_shader_invocations",
                100.0,
            );
            profiler.sample_frame(&diagnostics, None, true, true);
        }
        let report = profiler.gpu_passes(Some(&renderer(true, true)));
        assert!(report.timestamp_queries_supported);
        assert!(report.pipeline_statistics_supported);
        assert!(report.unavailable.is_empty());
        assert_eq!(report.measured_frames, 4);
        let pass = &report.passes["render/main_opaque_pass_3d/elapsed_gpu"];
        assert_eq!(pass.count, 4);
        assert_eq!(pass.mean, 2.5);
        assert_eq!(pass.total, 10.0);
        assert_eq!(pass.p95, 4.0);
        assert_eq!(pass.worst, 4.0);
        assert_eq!(report.passes["render/tonemapping/elapsed_gpu"].mean, 5.0);
        assert!(
            !report
                .passes
                .contains_key("render/main_opaque_pass_3d/vertex_shader_invocations"),
            "a pipeline-statistics counter is not a pass time"
        );
    }

    /// Every observation Bevy recorded is taken exactly once: two cameras running one pass in one
    /// readback batch both count, a frame whose readback has not changed adds nothing, and a pass
    /// that stops running adds nothing further.
    #[test]
    fn takes_every_new_render_measurement_once() {
        let mut diagnostics = DiagnosticsStore::default();
        let mut profiler = ProfilingState::default();
        let path = "render/main_opaque_pass_3d/elapsed_gpu";
        let batch = Instant::now();
        feed_at(&mut diagnostics, path, 4.0, batch);
        feed_at(&mut diagnostics, path, 6.0, batch);
        profiler.sample_frame(&diagnostics, None, true, true);
        let report = profiler.gpu_passes(Some(&renderer(true, true)));
        assert_eq!(
            report.passes[path].count, 2,
            "both cameras' observations of the pass are kept"
        );
        assert_eq!(report.passes[path].total, 10.0);

        // The readback has not arrived: the store is unchanged and nothing may be resampled.
        profiler.sample_frame(&diagnostics, None, true, true);
        let report = profiler.gpu_passes(Some(&renderer(true, true)));
        assert_eq!(
            report.passes[path].count, 2,
            "an unchanged store adds no sample"
        );
        assert_eq!(
            report.measured_frames, 2,
            "the window counts frames, not samples"
        );

        // A later frame: another pass keeps producing, this one has stopped running.
        let later = batch + Duration::from_millis(16);
        feed_at(
            &mut diagnostics,
            "render/tonemapping/elapsed_gpu",
            3.0,
            later,
        );
        profiler.sample_frame(&diagnostics, None, true, true);
        let report = profiler.gpu_passes(Some(&renderer(true, true)));
        assert_eq!(
            report.passes[path].count, 2,
            "a pass that stopped running adds no further sample"
        );
        assert_eq!(
            report.passes["render/tonemapping/elapsed_gpu"].count, 1,
            "a pass that is still running does"
        );
    }

    /// The GPU sections cover the measured window alone: samples taken while a benchmark is warming
    /// up are consumed and dropped rather than dug up later, and the report says how many measured
    /// frames its numbers rest on.
    #[test]
    fn the_measured_window_excludes_the_warm_up() {
        let mut diagnostics = DiagnosticsStore::default();
        let mut profiler = ProfilingState::default();
        let path = "render/main_opaque_pass_3d/elapsed_gpu";
        for value in [10.0, 20.0] {
            feed(&mut diagnostics, path, value);
            feed(&mut diagnostics, "mesh_allocator_slabs", value);
            profiler.sample_frame(&diagnostics, None, false, true);
        }
        let warm = profiler.gpu_passes(Some(&renderer(true, true)));
        assert!(
            warm.passes.is_empty(),
            "a warm-up pass sample is not measured"
        );
        assert_eq!(warm.measured_frames, 0);
        assert_eq!(
            profiler
                .gpu_memory(Some(&renderer(true, true)))
                .mesh_allocator_slabs,
            None,
            "a warm-up counter is not a measured one"
        );

        // The first measured frame finds no new measurement and must not report the warm-up ones.
        profiler.sample_frame(&diagnostics, None, true, true);
        let measured = profiler.gpu_passes(Some(&renderer(true, true)));
        assert!(
            measured.passes.is_empty(),
            "the warm-up history is not a measured window"
        );
        assert_eq!(measured.measured_frames, 1);

        // A measurement that arrives inside the window is the only one reported.
        feed(&mut diagnostics, path, 5.0);
        feed(&mut diagnostics, "mesh_allocator_slabs", 3.0);
        profiler.sample_frame(&diagnostics, None, true, true);
        let measured = profiler.gpu_passes(Some(&renderer(true, true)));
        assert_eq!(measured.passes[path].count, 1);
        assert_eq!(measured.passes[path].mean, 5.0);
        assert_eq!(measured.measured_frames, 2);
        let memory = profiler.gpu_memory(Some(&renderer(true, true)));
        assert_eq!(memory.mesh_allocator_slabs, Some(3));
        assert_eq!(memory.measured_frames, 2);
    }

    /// A backend without timestamp queries reports the reason and no pass times — never zeros, and
    /// never a stray sample from somewhere else. A run with no renderer says that instead.
    #[test]
    fn gpu_passes_say_why_they_are_empty_instead_of_reporting_zeros() {
        let mut diagnostics = DiagnosticsStore::default();
        let mut profiler = ProfilingState::default();
        feed(
            &mut diagnostics,
            "render/main_opaque_pass_3d/elapsed_gpu",
            3.0,
        );
        profiler.sample_frame(&diagnostics, None, true, true);

        let unsupported = profiler.gpu_passes(Some(&renderer(false, false)));
        assert!(unsupported.passes.is_empty());
        assert!(!unsupported.timestamp_queries_supported);
        assert!(!unsupported.pipeline_statistics_supported);
        assert_eq!(
            unsupported.unavailable["elapsed_gpu"],
            TIMESTAMP_QUERIES_UNAVAILABLE
        );
        assert_eq!(
            unsupported.unavailable["pipeline_statistics"],
            PIPELINE_STATISTICS_UNAVAILABLE
        );

        // A device that can time GPU work, but a run in which no pass was recorded.
        let empty = ProfilingState::default().gpu_passes(Some(&renderer(true, true)));
        assert!(empty.passes.is_empty());
        assert!(
            empty.unavailable["elapsed_gpu"].contains("no per-pass GPU time"),
            "an empty run must say so: {:?}",
            empty.unavailable
        );
    }

    /// A run with no renderer says so, rather than blaming the backend for a device it never had.
    #[test]
    fn a_run_with_no_renderer_says_so_instead_of_blaming_the_backend() {
        let mut diagnostics = DiagnosticsStore::default();
        let mut profiler = ProfilingState::default();
        feed(
            &mut diagnostics,
            "render/main_opaque_pass_3d/elapsed_gpu",
            3.0,
        );
        profiler.sample_frame(&diagnostics, None, true, true);

        let passes = profiler.gpu_passes(None);
        assert!(passes.passes.is_empty());
        assert!(!passes.timestamp_queries_supported);
        assert!(!passes.pipeline_statistics_supported);
        assert_eq!(passes.unavailable["elapsed_gpu"], NO_RENDERER);
        assert_eq!(passes.unavailable["pipeline_statistics"], NO_RENDERER);

        let scene = profiler.scene_report(None, None);
        assert_eq!(scene.unavailable["pipeline_statistics"], NO_RENDERER);

        let memory = profiler.gpu_memory(None);
        assert_eq!(memory.mesh_allocator_slabs, None);
        assert_eq!(memory.unavailable["mesh_allocator_slabs"], NO_RENDERER);
        assert_eq!(memory.unavailable["gpu_asset_counts/mesh"], NO_RENDERER);
    }

    /// The probe reports measured only once the render world's extraction has marked it.
    #[test]
    fn the_render_measurement_probe_reports_only_what_extraction_marked() {
        let probe = RenderMeasurementProbe::default();
        assert!(!probe.measured());
        probe.mark_measured();
        assert!(probe.measured());
    }

    /// A zero Bevy published before its first extraction is not a measurement: until the render
    /// world has produced a value, the counters are `null` with a reason.
    #[test]
    fn gpu_memory_stays_null_until_the_render_world_measures_it() {
        let mut diagnostics = DiagnosticsStore::default();
        let mut profiler = ProfilingState::default();
        feed(&mut diagnostics, "mesh_allocator_slabs", 0.0);
        feed(
            &mut diagnostics,
            RenderAssetDiagnosticPlugin::<RenderMesh>::render_asset_diagnostic_path().as_str(),
            0.0,
        );
        // Bevy's plugins publish these atomics in the main world from the first frame, and they are
        // zero until the render world's extraction fills them: a `Some(0.0)` from the store proves
        // nothing, which is exactly what the flag says.
        profiler.sample_frame(&diagnostics, None, true, false);
        let gpu = profiler.gpu_memory(Some(&renderer(true, true)));
        assert_eq!(gpu.mesh_allocator_slabs, None);
        assert!(gpu.render_asset_counts.is_empty());
        assert_eq!(gpu.measured_frames, 0);
        assert_eq!(
            gpu.unavailable["mesh_allocator_slabs"],
            MESH_ALLOCATOR_UNAVAILABLE
        );

        // Once the render world has measured, its value is the one reported.
        feed(&mut diagnostics, "mesh_allocator_slabs", 2.0);
        profiler.sample_frame(&diagnostics, None, true, true);
        let gpu = profiler.gpu_memory(Some(&renderer(true, true)));
        assert_eq!(gpu.mesh_allocator_slabs, Some(2));
        assert_eq!(gpu.measured_frames, 1);
    }

    /// The memory section reports the last sample of each counter, counts assets as counts, and
    /// names a reason for every counter the run did not sample.
    #[test]
    fn reports_the_gpu_memory_counters_that_were_sampled() {
        let mut diagnostics = DiagnosticsStore::default();
        let mut profiler = ProfilingState::default();
        feed(&mut diagnostics, "mesh_allocator_slabs", 2.0);
        feed(&mut diagnostics, "mesh_allocator_slabs_size", 8_388_608.0);
        feed(&mut diagnostics, "mesh_allocator_allocations", 42.0);
        feed(
            &mut diagnostics,
            RenderAssetDiagnosticPlugin::<RenderMesh>::render_asset_diagnostic_path().as_str(),
            7.0,
        );
        profiler.sample_frame(&diagnostics, None, true, true);
        feed(&mut diagnostics, "mesh_allocator_allocations", 45.0);
        profiler.sample_frame(&diagnostics, None, true, true);

        let gpu = profiler.gpu_memory(Some(&renderer(true, true)));
        assert_eq!(gpu.mesh_allocator_slab_bytes, Some(8_388_608));
        assert_eq!(gpu.mesh_allocator_slabs, Some(2));
        assert_eq!(
            gpu.mesh_allocator_allocations,
            Some(45),
            "the reported value is the run's last sample"
        );
        assert_eq!(gpu.measured_frames, 2);
        assert_eq!(gpu.render_asset_counts["mesh"], 7);
        assert!(
            !gpu.unavailable.contains_key("gpu_asset_counts/mesh"),
            "a sampled counter is not unavailable"
        );
        assert!(gpu.unavailable.contains_key("gpu_asset_counts/image"));
        assert!(gpu.unavailable.contains_key("gpu_asset_counts/material"));

        let unsampled = ProfilingState::default().gpu_memory(Some(&renderer(true, true)));
        assert_eq!(unsampled.mesh_allocator_slab_bytes, None);
        assert!(unsampled.render_asset_counts.is_empty());
        assert_eq!(unsampled.measured_frames, 0);
        assert_eq!(
            unsampled.unavailable["mesh_allocator_slab_bytes"],
            MESH_ALLOCATOR_UNAVAILABLE
        );
        assert_eq!(
            unsampled.unavailable["gpu_asset_counts/mesh"],
            RENDER_ASSET_UNAVAILABLE
        );
    }

    /// The CPU-side bytes count each mesh's attributes and indices and each image's pixels; a mesh
    /// Bevy extracted without a main-world copy is counted but contributes no bytes.
    #[test]
    fn sums_the_cpu_bytes_of_meshes_and_images() {
        use bevy::{
            asset::RenderAssetUsages,
            render::render_resource::{
                Extent3d, PrimitiveTopology, TextureDimension, TextureFormat,
            },
        };

        let mut meshes = Assets::<Mesh>::default();
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        );
        mesh.insert_indices(bevy::mesh::Indices::U32(vec![0, 1, 2]));
        meshes.add(mesh);
        let mut extracted = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        );
        extracted.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0, 0.0, 0.0]]);
        extracted
            .take_gpu_data()
            .expect("the render-world-only mesh's data must be extractable");
        meshes.add(extracted);

        let mut images = Assets::<Image>::default();
        images.add(Image::new_fill(
            Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0, 0, 0, 0],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        ));

        let bytes = cpu_asset_bytes(&meshes, &images);
        assert_eq!(bytes.meshes, 2);
        assert_eq!(
            bytes.mesh_bytes,
            3 * 12 + 3 * 4,
            "three positions and three u32 indices, and no bytes from the extracted mesh"
        );
        assert_eq!(bytes.meshes_without_cpu_data, 1);
        assert_eq!(bytes.images, 1);
        assert_eq!(bytes.image_bytes, 16);
    }

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
        profiler.sample_frame(&diagnostics, None, true, true);
        profiler.sample_frame(&diagnostics, None, true, true);
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
        profiler.sample_frame(&diagnostics, None, true, true);
        profiler.sample_frame(&diagnostics, None, true, true);
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
        profiler.sample_frame(&diagnostics, None, true, true);
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
        profiler.sample_frame(&diagnostics, None, true, true);
        profiler.sample_frame(&diagnostics, None, true, true);
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

    /// The scene section reports the sampled counts, the per-pass counters on a backend that has
    /// them, and `null` for the draw-call count Bevy cannot give — with no zero standing in for a
    /// sample that never happened.
    #[test]
    fn reports_the_scene_counts_and_pipeline_statistics() {
        let mut diagnostics = DiagnosticsStore::default();
        let mut profiler = ProfilingState::default();
        profiler.set_gauge("scene/mesh_entities", 4.0);
        profiler.set_gauge("scene/terrain_patches", 9.0);
        profiler.set_gauge("scene/water_surfaces", 2.0);
        for value in [100.0, 200.0] {
            feed(
                &mut diagnostics,
                "render/main_opaque_pass_3d/vertex_shader_invocations",
                value,
            );
            feed(
                &mut diagnostics,
                "render/main_opaque_pass_3d/clipper_primitives_out",
                value / 2.0,
            );
            profiler.sample_frame(&diagnostics, None, true, true);
        }
        let scene = profiler.scene_report(Some(42), Some(&renderer(true, true)));
        assert_eq!(scene.entity_count, Some(42));
        assert_eq!(scene.mesh_entities, Some(4));
        assert_eq!(scene.terrain_patches, Some(9));
        assert_eq!(scene.water_surfaces, Some(2));
        assert!(scene.pipeline_statistics_supported);
        assert_eq!(scene.measured_frames, 2);
        assert_eq!(
            scene.vertex_shader_invocations["render/main_opaque_pass_3d/vertex_shader_invocations"]
                .mean,
            150.0
        );
        assert_eq!(
            scene.clipper_primitives_out["render/main_opaque_pass_3d/clipper_primitives_out"].mean,
            75.0
        );
        assert_eq!(scene.draw_calls, None, "Bevy exposes no draw-call count");
        assert_eq!(
            scene.unavailable["draw_calls"],
            "Bevy 0.19 exposes no draw-call count"
        );
        assert!(
            !scene.unavailable.contains_key("mesh_entities"),
            "a sampled count is not unavailable"
        );
    }

    /// A backend without pipeline statistics reports neither counter, and says why — even when a
    /// stray diagnostic holds samples. A scene the inventory never sampled is `null` with a reason,
    /// not a zero.
    #[test]
    fn the_scene_section_says_why_a_counter_is_missing() {
        let mut diagnostics = DiagnosticsStore::default();
        let mut profiler = ProfilingState::default();
        feed(
            &mut diagnostics,
            "render/main_opaque_pass_3d/vertex_shader_invocations",
            7.0,
        );
        profiler.sample_frame(&diagnostics, None, true, true);
        let scene = profiler.scene_report(None, Some(&renderer(false, false)));
        assert!(!scene.pipeline_statistics_supported);
        assert!(scene.vertex_shader_invocations.is_empty());
        assert!(scene.clipper_primitives_out.is_empty());
        assert_eq!(
            scene.unavailable["pipeline_statistics"],
            PIPELINE_STATISTICS_UNAVAILABLE
        );
        assert_eq!(scene.entity_count, None);
        assert!(scene.unavailable.contains_key("entity_count"));
        for field in ["mesh_entities", "terrain_patches", "water_surfaces"] {
            assert_eq!(scene.unavailable[field], SCENE_INVENTORY_UNAVAILABLE);
        }

        // A supported backend that recorded no counter says that instead of blaming the backend.
        let unsampled =
            ProfilingState::default().scene_report(Some(1), Some(&renderer(true, true)));
        assert!(unsampled.vertex_shader_invocations.is_empty());
        assert_eq!(
            unsampled.unavailable["pipeline_statistics"],
            NO_PIPELINE_STATISTICS
        );
    }

    /// The scene counts are the measured window's, like every other number in the section: an
    /// inventory taken while the benchmark is warming up is dropped, and a measured window that
    /// holds no inventory of its own reports `null` with a reason rather than the warm-up's count.
    #[test]
    fn the_scene_inventory_is_kept_only_from_a_measured_frame() {
        let diagnostics = DiagnosticsStore::default();
        let mut profiler = ProfilingState::default();

        // The inventory runs in `Update` and the benchmark samples in `Last`, so the counts are
        // staged first and the sample of the same frame is what decides them. A warm-up frame's
        // inventory is dropped.
        profiler.stage_scene_inventory(BTreeMap::from([("scene/mesh_entities".to_owned(), 3.0)]));
        profiler.sample_frame(&diagnostics, None, false, true);
        let scene = profiler.scene_report(Some(11), Some(&renderer(true, true)));
        assert_eq!(scene.mesh_entities, None, "a warm-up count is not measured");
        assert_eq!(
            scene.unavailable["mesh_entities"],
            SCENE_INVENTORY_UNAVAILABLE
        );
        assert_eq!(scene.measured_frames, 0);

        // It is not carried either: a measured frame that took no inventory of its own keeps none.
        profiler.sample_frame(&diagnostics, None, true, true);
        assert_eq!(
            profiler
                .scene_report(Some(11), Some(&renderer(true, true)))
                .mesh_entities,
            None,
            "the warm-up count must not be committed by a later measured frame"
        );

        // An inventory taken inside a measured frame is the count the section reports.
        profiler.stage_scene_inventory(BTreeMap::from([
            ("scene/mesh_entities".to_owned(), 7.0),
            ("scene/terrain_patches".to_owned(), 9.0),
            ("scene/water_surfaces".to_owned(), 2.0),
        ]));
        profiler.sample_frame(&diagnostics, None, true, true);
        let scene = profiler.scene_report(Some(11), Some(&renderer(true, true)));
        assert_eq!(scene.mesh_entities, Some(7));
        assert_eq!(scene.terrain_patches, Some(9));
        assert_eq!(scene.water_surfaces, Some(2));
        assert!(
            !scene.unavailable.contains_key("mesh_entities"),
            "a measured count is not unavailable"
        );
        assert_eq!(scene.measured_frames, 2);
    }

    /// The CPU section splits Bevy's main schedules from the engine's own spans, keeps the
    /// asset-arrival counts out of both (they are counts, not milliseconds), and says that Bevy
    /// offers no render-world schedule timing.
    #[test]
    fn the_cpu_section_splits_schedules_from_spans() {
        let mut profiler = ProfilingState::default();
        profiler.record_ms("main_schedule/Update", 4.0);
        profiler.record_ms("main_schedule/Update", 6.0);
        profiler.record_ms("streaming/plan_cells", 1.0);
        profiler.record_ms("streaming/plan_cells", 3.0);
        profiler.record_ms("render_world/render_thread", 2.0);
        profiler.record_ms("assets_added/mesh", 12.0);
        let cpu = profiler.cpu_report();
        assert_eq!(cpu.schedules.len(), 1);
        assert_eq!(cpu.schedules["main_schedule/Update"].mean, 5.0);
        assert_eq!(cpu.schedules["main_schedule/Update"].worst, 6.0);
        assert!(
            !cpu.spans.contains_key("main_schedule/Update"),
            "a schedule is not one of the engine's own spans"
        );
        assert_eq!(cpu.spans["streaming/plan_cells"].total, 4.0);
        assert_eq!(cpu.spans["streaming/plan_cells"].p95, 3.0);
        assert!(
            cpu.spans.contains_key("render_world/render_thread"),
            "the render-world phases the engine records are spans"
        );
        assert!(
            !cpu.spans.contains_key("assets_added/mesh"),
            "an asset-arrival count is not CPU time"
        );
        assert!(
            cpu.render_world_prep.is_none(),
            "Bevy 0.19 has no render-app schedule timing to report"
        );
        assert!(cpu.unavailable.contains_key("render_world_prep"));
        assert!(!cpu.unavailable.contains_key("schedules"));
        assert!(!cpu.unavailable.contains_key("spans"));
    }

    /// A run that recorded nothing CPU-side says so rather than serialising two empty tables as if
    /// the frame had cost nothing.
    #[test]
    fn the_cpu_section_says_why_it_is_empty() {
        let cpu = ProfilingState::default().cpu_report();
        assert!(cpu.schedules.is_empty());
        assert!(cpu.spans.is_empty());
        assert!(cpu.unavailable.contains_key("schedules"));
        assert!(cpu.unavailable.contains_key("spans"));
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

    /// Verify aggregate bundle files are written without a frame sidecar when tracing is disabled.
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
        let fingerprint = RunFingerprint::collect(&config, None, None, None, false);
        profiler
            .write_bundle(
                &config,
                &frame,
                Some(&StreamingMetrics::default()),
                &RendererMetrics::default(),
                None,
                Some(&fingerprint),
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
        assert!(
            metadata["fingerprint"]["commit"].is_string(),
            "the bundle metadata must carry the run fingerprint"
        );
        assert!(
            metadata["commit"].is_string(),
            "the bundle metadata keeps the commit main writes, beside the fingerprint"
        );
        assert!(
            metadata["build_profile"].is_string(),
            "the bundle metadata keeps the build profile main writes, beside the fingerprint"
        );
        assert!(!directory.path().join("streaming-frames.json").exists());
    }

    /// The bundle's device capabilities are the fingerprint's, not an inference from which pass
    /// paths happened to be sampled: a Vulkan-like device that sampled no pass still writes `true`,
    /// the same answer the report's `gpu_passes` section gives for that run.
    #[test]
    fn the_bundle_takes_the_device_capabilities_from_the_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let config = EngineConfig {
            profile_output_dir: Some(directory.path().to_owned()),
            profile_scenario: "vulkan".into(),
            ..Default::default()
        };
        let profiler = ProfilingState::default();
        let frame = serde_json::json!({
            "average_fps": 90.0,
            "frame_ms_p95": 14.0,
            "passed": true
        });
        let mut fingerprint = RunFingerprint::collect(&config, None, None, None, false);
        fingerprint.renderer = Some(renderer(true, true));
        assert!(
            profiler.render_metrics.is_empty(),
            "the run sampled no pass, which is what must not be read as a missing capability"
        );
        profiler
            .write_bundle(
                &config,
                &frame,
                None,
                &RendererMetrics::default(),
                None,
                Some(&fingerprint),
            )
            .unwrap();
        let gpu: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.path().join("gpu-passes.json")).unwrap())
                .unwrap();
        assert_eq!(
            gpu["timestamp_queries_supported"],
            serde_json::json!(true),
            "the device supports timestamp queries even with no sampled pass"
        );
        assert_eq!(
            gpu["pipeline_statistics_supported"],
            serde_json::json!(true),
            "the sampled paths do not answer what the device supports"
        );
    }

    /// Verify ordinary runs install neither frame tracing nor the GPU inventory bridge.
    #[test]
    fn normal_runs_do_not_install_trace_collection() {
        let mut app = App::new();
        app.insert_resource(EngineConfig::default())
            .add_plugins(ProfilingPlugin);
        assert!(app.world().resource::<ProfilingState>().trace.is_none());
        assert!(!app.world().contains_resource::<StreamingGpuMetricsBridge>());
    }

    /// Verify profile output enables frame tracing while leaving GPU inventory disabled.
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

    /// Verify recursive dependency loading and failure determine scene readiness.
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
