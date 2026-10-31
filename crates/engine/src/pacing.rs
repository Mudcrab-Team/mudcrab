//! Measurements for the streaming pacing rework: how long the world takes to load around the
//! camera, how far behind loading falls while flying fast, and (with `render_timing`) what the
//! worst frames spend on. Measurement only: nothing here changes what the engine loads, in what
//! order, or how fast.
//!
//! * **World ready** ([`WorldReadyInputs::is_ready`]): every cell of the stream window resident, no
//!   cell loading, no database request in flight, the model arming queue empty, and no model or
//!   terrain/water surface still pending. [`PacingTracker`] requires that predicate to hold on two
//!   consecutive frames (a cell that just committed is resident while its references are still
//!   being created, so the first of the two can be early) and records the time and frame count
//!   from the first frame to the first of them.
//! * **Jump** (`--benchmark-jump`): once the world is first ready the camera moves to the centre of
//!   another cell, and the time to be ready again is recorded the same way.
//! * **Lag at speed** (`--auto-fly-speed`): the horizontal distance from the camera at which each
//!   model finished loading ([`FlyLag`]). A model that completes close to the camera arrived late.

mod loading_cover;
#[cfg(test)]
pub(crate) use loading_cover::teleport_cover_count;
pub(crate) use loading_cover::{LoadingCover, loading_controls_allowed};
pub(crate) mod startup;

use crate::{
    app::benchmark_jump_position,
    config::EngineConfig,
    profiling::ProfilingState,
    streaming::{ActiveSpace, RenderOrigin, StreamingMetrics, StreamingWorld, window_center},
    swap_bridge::{WaitingVisibility, pending_drawn},
    world::{cache::CellCache, components::CELL_SIZE, components::StreamingCamera},
};
use bevy::prelude::*;
use serde::Serialize;
use std::time::Instant;

/// The most model-ready distances kept; a run that completes more models keeps the first ones.
const MAX_READY_DISTANCES: usize = 200_000;

/// What the world-ready predicate reads, taken from one frame's streaming state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorldReadyInputs {
    /// Cells wanted by the planner: one interior, or an exterior window `(2r + 1)^2`.
    pub window_cells: usize,
    /// Cells of that window resident at full detail.
    pub resident_window_cells: usize,
    /// Wanted exterior squares successfully proven to have no cell record.
    pub absent_window_cells: usize,
    /// Cells whose full-cell loads failed permanently or exhausted their bounded retries.
    /// Retryable failures remain pending; terminal failures count as settled and are reported.
    pub failed_window_cells: usize,
    pub loading_cells: usize,
    pub active_requests: usize,
    /// Models whose scene is loaded but not yet handed to the spawner.
    pub arming_queue_depth: usize,
    pub pending_asset_instances: usize,
    pub pending_surface_instances: usize,
    /// Surfaces a view can see that the render world has not confirmed drawn: the
    /// world is not ready while the ground it shows may be a stand-in or a hole.
    pub pending_drawn: usize,
    /// LOD queries submitted but not answered, and LOD chunks requested but not resident: work that
    /// will still change the frame, so the world is not ready while either is nonzero.
    pub pending_lod_queries: usize,
    pub pending_lod_chunks: usize,
    /// Cluster batches a view wants whose material has not loaded.
    pub pending_cluster_batches: usize,
    pub failed_cluster_batches: usize,
    /// Required owned batches/views without a current drawable render snapshot.
    pub pending_cluster_drawability: usize,
    /// Distant stock work and failures from the active providers.
    pub queued_far_stock_queries: usize,
    pub pending_far_stock_queries: usize,
    pub pending_far_stock_qualification: usize,
    pub failed_far_stock_sources: usize,
}

impl WorldReadyInputs {
    /// Whether the world around the camera is fully loaded. Each condition alone holds it back.
    /// A failed window cell would otherwise hold it back forever: it is never retried and never
    /// becomes resident, so resident plus failed cells must cover the window.
    pub fn is_ready(&self) -> bool {
        self.window_cells > 0
            && self.resident_window_cells + self.absent_window_cells + self.failed_window_cells
                >= self.window_cells
            && self.loading_cells == 0
            && self.active_requests == 0
            && self.arming_queue_depth == 0
            && self.pending_asset_instances == 0
            && self.pending_surface_instances == 0
            && self.pending_drawn == 0
            && self.pending_lod_queries == 0
            && self.pending_lod_chunks == 0
            && self.pending_cluster_batches == 0
            && self.failed_cluster_batches == 0
            && self.pending_cluster_drawability == 0
            && self.queued_far_stock_queries == 0
            && self.pending_far_stock_queries == 0
            && self.pending_far_stock_qualification == 0
            && self.failed_far_stock_sources == 0
    }

    fn from_streaming(
        metrics: &StreamingMetrics,
        pending_drawn: usize,
        window_cells: usize,
        resident_window_cells: usize,
        failed_window_cells: usize,
        absent_window_cells: usize,
        pending_cluster_drawability: usize,
    ) -> Self {
        Self {
            window_cells,
            resident_window_cells,
            failed_window_cells,
            absent_window_cells,
            loading_cells: metrics.loading_cells,
            active_requests: metrics.active_requests,
            arming_queue_depth: metrics.arming_queue_depth,
            pending_asset_instances: metrics.pending_asset_instances,
            pending_surface_instances: metrics.pending_surface_instances,
            pending_drawn,
            pending_lod_queries: metrics.pending_lod_queries,
            pending_lod_chunks: metrics.pending_lod_chunks,
            pending_cluster_batches: metrics.pending_cluster_batches,
            failed_cluster_batches: metrics.failed_cluster_batches,
            pending_cluster_drawability,
            queued_far_stock_queries: metrics.queued_far_stock_queries,
            pending_far_stock_queries: metrics.pending_far_stock_queries,
            pending_far_stock_qualification: metrics.pending_far_stock_qualification,
            failed_far_stock_sources: metrics.failed_far_stock_sources,
        }
    }
}

/// When a state was first reached: milliseconds and frames since the start (or the jump).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mark {
    millis: f64,
    frames: u64,
}

/// How many consecutive frames the predicate must hold before it counts. A cell that just
/// committed is resident while its references are still in the command buffer, so the first frame
/// it holds can be one frame early; the second cannot.
const READY_FRAMES_REQUIRED: u32 = 2;

/// Latches "ready" once the predicate has held on [`READY_FRAMES_REQUIRED`] consecutive frames,
/// remembering the first frame of that run.
#[derive(Debug, Clone, Copy, Default)]
struct ReadyLatch {
    run_start: Option<Mark>,
    run_length: u32,
    latched: Option<Mark>,
}

impl ReadyLatch {
    /// Feeds one frame (`mark` is that frame's time and count). True on the frame it latches.
    fn feed(&mut self, mark: Mark, ready: bool) -> bool {
        if self.latched.is_some() {
            return false;
        }
        if !ready {
            self.run_start = None;
            self.run_length = 0;
            return false;
        }
        self.run_start.get_or_insert(mark);
        self.run_length += 1;
        if self.run_length >= READY_FRAMES_REQUIRED {
            self.latched = self.run_start;
            return true;
        }
        false
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct JumpState {
    started_millis: f64,
    started_frame: u64,
    latch: ReadyLatch,
    /// Set on the frame the world is ready again; later frames are outside the loading window.
    window_closed: bool,
}

/// Frame-time statistics over one window of a run, from the same frame deltas as the report's
/// `frame_ms_*` fields — but, unlike those fields, warm-up frames are included, and in a jump run
/// [`PacingReport::frames_after_ready`] also covers the frames of the jump's loading window.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FrameWindowStats {
    /// Frames in the window.
    pub frames: u64,
    /// The 99th percentile frame time, in milliseconds: the entry at `ceil((n - 1) * 0.99)`, the
    /// higher of the two ranks around `(n - 1) * 0.99` (the same formula as `profiling::percentile`).
    pub p99_ms: f64,
    /// The longest frame, in milliseconds.
    pub worst_ms: f64,
    /// Frames longer than 33 ms (below 30 fps).
    pub over_33ms: u64,
    /// Frames longer than 50 ms.
    pub over_50ms: u64,
}

impl FrameWindowStats {
    /// The statistics of `frame_ms`, or `None` when the window has no frames.
    pub fn from_frames(frame_ms: &[f64]) -> Option<Self> {
        if frame_ms.is_empty() {
            return None;
        }
        let mut sorted = frame_ms.to_vec();
        sorted.sort_by(f64::total_cmp);
        let index = ((sorted.len() - 1) as f64 * 0.99).ceil() as usize;
        Some(Self {
            frames: sorted.len() as u64,
            p99_ms: sorted[index.min(sorted.len() - 1)],
            worst_ms: sorted[sorted.len() - 1],
            over_33ms: frame_ms.iter().filter(|&&ms| ms > 33.0).count() as u64,
            over_50ms: frame_ms.iter().filter(|&&ms| ms > 50.0).count() as u64,
        })
    }
}

/// Time-to-ready, jump and fly-lag measurements for one run.
#[derive(Resource, Default)]
pub struct PacingTracker {
    started: Option<Instant>,
    frames: u64,
    first_ready: ReadyLatch,
    /// Failed cells in the window on the frame the first ready latch fired, for
    /// [`PacingReport::failed_window_cells_at_ready`].
    failed_window_cells_at_ready: Option<usize>,
    /// The stream window's failed-cell count on the current frame; [`Self::observe`] must be fed
    /// with this set, as the caller ([`track_world_ready`]) does every frame.
    window_failed_cells: usize,
    jump: Option<JumpState>,
    /// Distances of the first [`MAX_READY_DISTANCES`] models, for the percentile.
    ready_distances: Vec<f32>,
    ready_models_total: u64,
    /// Counted as each distance arrives, so a capped list does not undercount them.
    ready_within_one_cell: u64,
    ready_distance_min: Option<f32>,
    /// Whether to keep the two frame-time windows. Only a benchmark run does: an ordinary play
    /// session runs for as long as the player wants, and both vectors grow with every frame
    /// ([`track_world_ready`] sets this from [`EngineConfig::is_benchmark_run`] each frame).
    record_frame_windows: bool,
    /// Frame times of every frame after the first world-ready latch.
    frames_after_ready: Vec<f64>,
    /// Frame times from the jump's own frame to the frame the world is ready again.
    jump_window: Vec<f64>,
}

impl PacingTracker {
    /// Feeds one frame. `elapsed_millis` is the time since the first frame and `frame_ms` the
    /// frame's delta (non-finite or non-positive deltas are left out of the windows, as the
    /// benchmark's own frame times do). The frame's `window_failed_cells` must be set to the
    /// stream window's failed-cell count first; it is what the report records next to the ready
    /// time. The world counts as ready once the predicate has held on two consecutive frames, timed
    /// at the first of them. Returns true on the frame that ready is latched when a jump is
    /// configured: the caller moves the camera now.
    ///
    /// Two frame-time windows are kept, in a benchmark run only (see
    /// [`Self::record_frame_windows`]): every frame after the first latch (the latch frame itself
    /// excluded), and the jump's loading window, from the frame the jump is issued up to and
    /// including the frame the world is ready again (the second frame of the ready run), or to the
    /// end of the run if it never is. A frame's delta is the interval that ended at that frame, so
    /// a window's first entry covers the work that led up to the frame that opened it.
    pub fn observe(
        &mut self,
        elapsed_millis: f64,
        ready: bool,
        jump_configured: bool,
        frame_ms: f64,
    ) -> bool {
        let record = self.record_frame_windows && frame_ms.is_finite() && frame_ms > 0.0;
        if record && self.first_ready.latched.is_some() {
            self.frames_after_ready.push(frame_ms);
        }
        self.frames += 1;
        let frames = self.frames;
        let now = Mark {
            millis: elapsed_millis,
            frames,
        };
        if self.first_ready.latched.is_none() {
            let latched = self.first_ready.feed(now, ready);
            if latched {
                self.failed_window_cells_at_ready = Some(self.window_failed_cells);
            }
            if latched && jump_configured {
                self.jump = Some(JumpState {
                    started_millis: elapsed_millis,
                    started_frame: frames,
                    latch: ReadyLatch::default(),
                    window_closed: false,
                });
                if record {
                    self.jump_window.push(frame_ms);
                }
                return true;
            }
            return false;
        }
        // The planner has not seen the new camera position on the jump's own frame.
        if let Some(jump) = &mut self.jump
            && frames > jump.started_frame
        {
            if !jump.window_closed && record {
                self.jump_window.push(frame_ms);
            }
            if jump.latch.feed(now, ready) {
                jump.window_closed = true;
            }
        }
        false
    }

    /// Whether the world has been ready (the predicate held on two consecutive frames) at least
    /// once. A route run starts its walk on the frame after this turns true.
    pub fn world_ready_reached(&self) -> bool {
        self.first_ready.latched.is_some()
    }

    /// Records a model that finished loading `distance` units (horizontally) from the camera.
    pub fn record_model_ready(&mut self, distance: f32) {
        self.ready_models_total = self.ready_models_total.saturating_add(1);
        if !distance.is_finite() {
            return;
        }
        if distance <= CELL_SIZE {
            self.ready_within_one_cell += 1;
        }
        self.ready_distance_min = Some(
            self.ready_distance_min
                .map_or(distance, |min| min.min(distance)),
        );
        if self.ready_distances.len() < MAX_READY_DISTANCES {
            self.ready_distances.push(distance);
        }
    }

    /// The report fields. The lag block is only reported for a run that flies
    /// (`--auto-fly-speed`).
    pub fn report(&self, config: &EngineConfig, peak_arming_queue_depth: usize) -> PacingReport {
        let first = self.first_ready.latched;
        let after_jump = self.jump.and_then(|jump| {
            jump.latch.latched.map(|mark| Mark {
                millis: mark.millis - jump.started_millis,
                frames: mark.frames - jump.started_frame,
            })
        });
        PacingReport {
            world_ready_reached: first.is_some(),
            time_to_world_ready_ms: first.map(|mark| mark.millis),
            frames_to_world_ready: first.map(|mark| mark.frames),
            failed_window_cells_at_ready: first.and(self.failed_window_cells_at_ready),
            jump_target: config.benchmark_jump.map(|(x, y)| [x, y]),
            jump_issued: self.jump.is_some(),
            time_to_world_ready_after_jump_ms: after_jump.map(|mark| mark.millis),
            frames_to_world_ready_after_jump: after_jump.map(|mark| mark.frames),
            fly_lag: config.flies().then(|| {
                FlyLag::new(
                    &self.ready_distances,
                    self.ready_models_total,
                    self.ready_within_one_cell,
                    self.ready_distance_min,
                )
            }),
            peak_arming_queue_depth,
            frames_after_ready: FrameWindowStats::from_frames(&self.frames_after_ready),
            jump_load_window: FrameWindowStats::from_frames(&self.jump_window),
        }
    }
}

/// Fields added to the benchmark report at its top level (flattened in, and absent altogether for a
/// run with no streaming, such as the synthetic benchmark). `null` means "never happened": a world
/// that never became ready has `world_ready_reached: false` and `null` times.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct PacingReport {
    /// Whether the world-ready predicate ever held on two consecutive frames. False means the
    /// other times are `null`.
    pub world_ready_reached: bool,
    /// Milliseconds from the first frame to the first of the two consecutive frames on which the
    /// world was fully loaded.
    pub time_to_world_ready_ms: Option<f64>,
    pub frames_to_world_ready: Option<u64>,
    /// How many cells of the stream window had failed (map edge, missing data) when the ready
    /// latch fired. Such cells never become resident and are never retried; they only count as
    /// settled for the ready predicate. `null` when the world never became ready.
    pub failed_window_cells_at_ready: Option<usize>,
    /// The `--benchmark-jump` target cell, when one was given.
    pub jump_target: Option<[i32; 2]>,
    /// Whether the jump happened (it waits for the world to be ready first).
    pub jump_issued: bool,
    /// Milliseconds from the jump to the start of the next two-frame ready run.
    pub time_to_world_ready_after_jump_ms: Option<f64>,
    pub frames_to_world_ready_after_jump: Option<u64>,
    /// `null` when the run did not fly (`--auto-fly-speed`), otherwise the lag block. (The fields
    /// of a non-null block are themselves `null` when nothing was measured.)
    pub fly_lag: Option<FlyLag>,
    /// The largest number of models waiting to be armed at once.
    pub peak_arming_queue_depth: usize,
    /// Frame times of every frame after the first world-ready latch, so the one-off startup frames
    /// are left out; in a jump run this covers the jump's loading window too, since one follows the
    /// latch. Covers fly runs too; only a benchmark run (`--benchmark-frames` or
    /// `--benchmark-duration`) keeps it. `null` when the run was not a benchmark, the world never
    /// became ready, or no frame followed.
    pub frames_after_ready: Option<FrameWindowStats>,
    /// Frame times of the jump's loading window: from the frame the jump is issued up to and
    /// including the frame the world is ready again (to the end of the run if it never is), and, as
    /// each frame's delta is the interval that ended at it, the first entry covers the work up to
    /// the frame that opened the window. Null when no jump was issued or the run was not a
    /// benchmark.
    pub jump_load_window: Option<FrameWindowStats>,
}

/// How close to the camera models finished loading while flying: each model's horizontal distance
/// from the camera on the frame it became ready.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FlyLag {
    /// Models that finished loading during the run.
    pub models_ready: u64,
    /// Of those, how many finished within one cell (4096 units) of the camera. Counted over every
    /// model, not only the sampled ones.
    pub ready_within_one_cell: u64,
    /// The 5th percentile of the sampled distances, in units; null with no models.
    pub ready_distance_p5: Option<f32>,
    /// How many distances the percentile was taken over. Below `models_ready` the sample was
    /// capped and the percentile covers only the first models of the run.
    pub p5_sample_size: usize,
    /// The smallest distance over every model; null with no models.
    pub ready_distance_min: Option<f32>,
}

impl FlyLag {
    pub fn new(
        sampled: &[f32],
        models_ready: u64,
        ready_within_one_cell: u64,
        ready_distance_min: Option<f32>,
    ) -> Self {
        let mut sorted: Vec<f32> = sampled.to_vec();
        sorted.sort_by(f32::total_cmp);
        Self {
            models_ready,
            ready_within_one_cell,
            ready_distance_p5: distance_percentile(&sorted, 0.05),
            p5_sample_size: sorted.len(),
            ready_distance_min,
        }
    }
}

/// Percentile of an ascending list: the entry at `ceil((n - 1) * percentile)`, the higher of the two
/// ranks around `(n - 1) * percentile` (the same formula as `profiling::percentile`), `None` when the
/// list is empty.
fn distance_percentile(sorted: &[f32], percentile: f64) -> Option<f32> {
    if sorted.is_empty() {
        return None;
    }
    let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    Some(sorted[index.min(sorted.len() - 1)])
}

/// Horizontal (render x/z plane) distance between two positions.
pub fn horizontal_distance(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

/// Evaluate readiness for the active space using this frame's real pending counters.
#[allow(clippy::too_many_arguments)]
pub(crate) fn world_ready_at_frame_end(
    config: &EngineConfig,
    origin: IVec2,
    streaming: &StreamingWorld,
    space: &ActiveSpace,
    metrics: &StreamingMetrics,
    camera: Vec3,
    pending_drawn: usize,
    gate: Option<&crate::cluster::loading::ClusterLoadingGate>,
) -> (bool, usize) {
    let inputs = world_ready_inputs_at_frame_end(
        config,
        origin,
        streaming,
        space,
        metrics,
        camera,
        pending_drawn,
        gate,
    );
    (inputs.is_ready(), inputs.failed_window_cells)
}

/// The facts [`world_ready_at_frame_end`] decides on, for a caller that also needs to tell
/// the exact pending inputs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn world_ready_inputs_at_frame_end(
    config: &EngineConfig,
    origin: IVec2,
    streaming: &StreamingWorld,
    space: &ActiveSpace,
    metrics: &StreamingMetrics,
    camera: Vec3,
    pending_drawn: usize,
    gate: Option<&crate::cluster::loading::ClusterLoadingGate>,
) -> WorldReadyInputs {
    let center = window_center(config, camera, origin);
    let (window_cells, resident, failed, absent) =
        streaming.active_residency(space, config.worldspace_id, center, config.stream_radius);
    let pending_cluster_drawability = usize::from(!crate::cluster::loading::ready_for_active(
        config, space, gate,
    ));
    WorldReadyInputs::from_streaming(
        metrics,
        pending_drawn,
        window_cells,
        resident,
        failed,
        absent,
        pending_cluster_drawability,
    )
}

/// The frame delta the loading frame-time windows are measured with: the real wall-clock delta,
/// never the virtual clock. Bevy clamps `Time<Virtual>` at 250 ms, so a one-second hitch would
/// otherwise be recorded as 250 ms, which is exactly the stall those windows exist to catch. The
/// report's own `frame_ms_*` fields still use the clamped clock; the windows deliberately do not.
/// This is the single place the clock for them is chosen.
fn window_delta_ms(real: &Time<Real>) -> f64 {
    real.delta_secs_f64() * 1000.0
}

/// Evaluates the world-ready predicate each frame, runs the benchmark jump, and keeps the tracker.
/// Runs at the end of the streaming chain, after the planner and the readiness scans.
#[allow(clippy::too_many_arguments)]
pub(crate) fn track_world_ready(
    config: Res<EngineConfig>,
    space: Option<Res<ActiveSpace>>,
    origin: Res<RenderOrigin>,
    streaming: Res<StreamingWorld>,
    metrics: Res<StreamingMetrics>,
    cache: Res<CellCache>,
    mut camera: Query<&mut Transform, With<StreamingCamera>>,
    waiting: WaitingVisibility,
    mut tracker: ResMut<PacingTracker>,
    mut profiler: ResMut<ProfilingState>,
    // The window frame times come from the real clock; `window_delta_ms` is the one place that
    // choice is made, so it is also the one place a test has to pin.
    real_time: Res<Time<Real>>,
    mut gate: Option<ResMut<crate::cluster::loading::ClusterLoadingGate>>,
    startup: Option<Res<startup::StartupLoad>>,
    (mode, hold): (
        Option<Res<crate::physics::MoveMode>>,
        Option<Res<crate::physics::WalkSpawnHold>>,
    ),
) {
    let Ok(mut camera) = camera.single_mut() else {
        return;
    };
    let active_space = space.as_deref().copied().unwrap_or_default();
    if config.clusters.is_some()
        && (!tracker.world_ready_reached()
            || tracker
                .jump
                .as_ref()
                .is_some_and(|jump| jump.latch.latched.is_none()))
        && let Some(gate) = gate.as_deref_mut()
    {
        // Controls and collision placement run before this end-of-streaming
        // observation. Never latch first-ready using the previous camera pose.
        gate.observe_loading_pose(&camera, origin.0, active_space);
    }
    let started = *tracker.started.get_or_insert_with(Instant::now);
    let (mut ready, failed) = world_ready_at_frame_end(
        &config,
        origin.0,
        &streaming,
        &active_space,
        &metrics,
        camera.translation,
        pending_drawn(&waiting),
        gate.as_deref(),
    );
    ready &= config.clusters.is_none()
        || crate::cluster::loading::physics_settled(mode.as_deref(), hold.as_deref());
    // Admission begins the fade independently; measurement starts after the cover clears.
    ready &= startup.as_deref().is_none_or(|load| !load.cover_pending());
    let elapsed_millis = started.elapsed().as_secs_f64() * 1000.0;
    // Frame-time windows are for benchmark runs; an ordinary session runs unbounded.
    tracker.record_frame_windows = config.is_benchmark_run();
    tracker.window_failed_cells = failed;
    let was_latched = tracker.first_ready.latched.is_some();
    let jump_now = tracker.observe(
        elapsed_millis,
        ready,
        config.benchmark_jump.is_some(),
        window_delta_ms(&real_time),
    );
    if let Some(grid) = config.benchmark_jump.filter(|_| jump_now) {
        let position = benchmark_jump_position(&config, &cache, origin.0, grid);
        camera.translation = position;
        if config.clusters.is_some()
            && let Some(gate) = gate.as_deref_mut()
        {
            // The benchmark mutation occurs after first-ready's pose observation.
            // Withdraw that proof immediately, including jumps inside a resident cell.
            gate.observe_loading_pose(&camera, origin.0, active_space);
        }
        profiler.event(
            "pacing",
            format!("benchmark_jump_{}_{}", grid.0, grid.1),
            Some(elapsed_millis),
        );
        info!(?grid, ?position, elapsed_millis, "benchmark jump issued");
    }
    profiler.set_gauge("pacing/world_ready", f64::from(u8::from(ready)));
    if let Some(mark) = tracker.first_ready.latched
        && !was_latched
    {
        profiler.event("pacing", "world_ready", Some(mark.millis));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::{TimeUpdateStrategy, Virtual};
    use std::time::Duration;

    /// A tracker as a benchmark run leaves it: the frame-time windows are kept.
    fn benchmark_tracker() -> PacingTracker {
        PacingTracker {
            record_frame_windows: true,
            ..default()
        }
    }

    fn ready_inputs() -> WorldReadyInputs {
        WorldReadyInputs {
            window_cells: 25,
            resident_window_cells: 25,
            ..default()
        }
    }

    #[test]
    fn a_fully_loaded_window_is_ready() {
        assert!(ready_inputs().is_ready());
    }

    #[test]
    fn each_condition_alone_holds_the_world_back() {
        let holds_back: [fn(&mut WorldReadyInputs); 17] = [
            |i| i.resident_window_cells = 24,
            |i| i.loading_cells = 1,
            |i| i.active_requests = 1,
            |i| i.arming_queue_depth = 1,
            |i| i.pending_asset_instances = 1,
            |i| i.pending_surface_instances = 1,
            |i| i.pending_drawn = 1,
            |i| i.pending_lod_queries = 1,
            |i| i.pending_lod_chunks = 1,
            |i| i.pending_cluster_batches = 1,
            |i| i.failed_cluster_batches = 1,
            |i| i.pending_cluster_drawability = 1,
            |i| i.queued_far_stock_queries = 1,
            |i| i.pending_far_stock_queries = 1,
            |i| i.pending_far_stock_qualification = 7,
            |i| i.failed_far_stock_sources = 1,
            |i| i.window_cells = 0,
        ];
        for (index, hold) in holds_back.iter().enumerate() {
            let mut inputs = ready_inputs();
            hold(&mut inputs);
            assert!(!inputs.is_ready(), "condition {index} did not hold it back");
        }
    }

    /// A cell that failed (map edge, missing data) never becomes resident and is never retried, so
    /// waiting for the whole window to be resident would never latch; a failed cell counts as
    /// settled, and how many there were is reported.
    #[test]
    fn a_failed_window_cell_counts_as_settled_and_is_reported() {
        let mut inputs = WorldReadyInputs {
            window_cells: 25,
            resident_window_cells: 24,
            ..default()
        };
        assert!(!inputs.is_ready(), "one resident cell short is not ready");
        inputs.failed_window_cells = 1;
        assert!(inputs.is_ready(), "the failed cell settles the window");
        // One resident short of window minus failed is still not ready.
        inputs.failed_window_cells = 0;
        inputs.resident_window_cells = 23;
        inputs.failed_window_cells = 1;
        assert!(!inputs.is_ready());

        let mut tracker = PacingTracker {
            window_failed_cells: 2,
            ..default()
        };
        assert!(!tracker.observe(0.0, true, false, 16.0));
        // The return value only signals a jump to issue; with no jump configured it is false even
        // on the latch frame. The count is what the report carries.
        tracker.observe(16.0, true, false, 16.0);
        let report = tracker.report(&EngineConfig::default(), 0);
        assert!(report.world_ready_reached);
        assert_eq!(report.failed_window_cells_at_ready, Some(2));
        // A run whose world never became ready has no count.
        let never = PacingTracker::default().report(&EngineConfig::default(), 0);
        assert_eq!(never.failed_window_cells_at_ready, None);
    }

    #[test]
    fn ready_needs_two_consecutive_frames_and_is_timed_at_the_first() {
        let mut tracker = PacingTracker::default();
        assert!(!tracker.observe(0.0, false, false, 16.0));
        assert!(!tracker.observe(16.0, false, false, 16.0));
        // One frame of ready, then a dip: the early frame does not count.
        assert!(!tracker.observe(32.0, true, false, 16.0));
        assert!(!tracker.observe(48.0, false, false, 16.0));
        let early = tracker.report(&EngineConfig::default(), 0);
        assert!(
            !early.world_ready_reached,
            "a single ready frame is not enough"
        );
        assert!(!tracker.observe(64.0, true, false, 16.0));
        assert!(!tracker.observe(80.0, true, false, 16.0));
        // A later dip does not move the latched time.
        assert!(!tracker.observe(96.0, false, false, 16.0));
        let report = tracker.report(&EngineConfig::default(), 3);
        assert!(report.world_ready_reached);
        assert_eq!(report.time_to_world_ready_ms, Some(64.0));
        assert_eq!(report.frames_to_world_ready, Some(5));
        assert!(!report.jump_issued);
        assert_eq!(report.peak_arming_queue_depth, 3);
        assert!(report.fly_lag.is_none());
    }

    #[test]
    fn a_world_that_never_becomes_ready_says_so() {
        let mut tracker = PacingTracker::default();
        for frame in 0..10 {
            // Ready only every other frame: never two in a row.
            tracker.observe(f64::from(frame) * 16.0, frame % 2 == 0, true, 16.0);
        }
        let report = tracker.report(&EngineConfig::default(), 0);
        assert!(!report.world_ready_reached);
        assert_eq!(report.time_to_world_ready_ms, None);
        assert!(!report.jump_issued, "no jump before the first ready");
        assert_eq!(report.time_to_world_ready_after_jump_ms, None);
    }

    #[test]
    fn the_jump_fires_once_and_times_the_next_two_frame_ready_run() {
        let config = EngineConfig {
            benchmark_jump: Some((3, 4)),
            ..default()
        };
        let mut tracker = PacingTracker::default();
        assert!(!tracker.observe(0.0, false, true, 16.0));
        assert!(!tracker.observe(100.0, true, true, 16.0));
        assert!(
            tracker.observe(116.0, true, true, 16.0),
            "the jump fires on the second ready frame"
        );
        // The jump's own frame does not count; loading restarts.
        assert!(!tracker.observe(132.0, false, true, 16.0));
        assert!(!tracker.observe(148.0, true, true, 16.0));
        assert!(!tracker.observe(164.0, true, true, 16.0));
        // Once recorded the time stays, and no second jump fires.
        assert!(!tracker.observe(500.0, true, true, 16.0));
        let report = tracker.report(&config, 0);
        assert!(report.jump_issued);
        assert_eq!(report.time_to_world_ready_ms, Some(100.0));
        assert_eq!(report.time_to_world_ready_after_jump_ms, Some(32.0));
        assert_eq!(report.frames_to_world_ready_after_jump, Some(2));
        assert_eq!(report.jump_target, Some([3, 4]));
    }

    #[test]
    fn a_ready_frame_right_after_the_jump_needs_a_partner() {
        let config = EngineConfig {
            benchmark_jump: Some((3, 4)),
            ..default()
        };
        let mut tracker = PacingTracker::default();
        tracker.observe(10.0, true, true, 16.0);
        assert!(tracker.observe(26.0, true, true, 16.0));
        assert!(!tracker.observe(42.0, true, true, 16.0));
        assert_eq!(
            tracker.report(&config, 0).time_to_world_ready_after_jump_ms,
            None,
            "one frame after the jump is not a run of two"
        );
        tracker.observe(58.0, true, true, 16.0);
        assert_eq!(
            tracker.report(&config, 0).time_to_world_ready_after_jump_ms,
            Some(16.0)
        );
    }

    #[test]
    fn frame_window_stats_cover_p99_thresholds_and_empty() {
        assert_eq!(FrameWindowStats::from_frames(&[]), None);
        let mut frames: Vec<f64> = vec![10.0; 97];
        frames.extend([33.0, 34.0, 60.0]);
        let stats = FrameWindowStats::from_frames(&frames).unwrap();
        assert_eq!(stats.frames, 100);
        // Index ceil(99 * 0.99) = 99, the largest: the rank above the one nearest the index.
        assert_eq!(stats.p99_ms, 60.0);
        assert_eq!(stats.worst_ms, 60.0);
        assert_eq!(stats.over_33ms, 2, "33.0 is not over 33 ms");
        assert_eq!(stats.over_50ms, 1);
        let single = FrameWindowStats::from_frames(&[7.0]).unwrap();
        assert_eq!(
            (single.p99_ms, single.worst_ms, single.frames),
            (7.0, 7.0, 1)
        );
    }

    #[test]
    fn the_frame_windows_start_and_end_on_the_documented_frames() {
        let config = EngineConfig {
            benchmark_jump: Some((3, 4)),
            ..default()
        };
        let mut tracker = benchmark_tracker();
        // Startup: never counted.
        tracker.observe(0.0, false, true, 150.0);
        tracker.observe(150.0, false, true, 150.0);
        tracker.observe(166.0, true, true, 16.0);
        // Second ready frame: latches and issues the jump. Not in frames_after_ready, first of the
        // jump window.
        assert!(tracker.observe(182.0, true, true, 17.0));
        // Loading after the jump.
        tracker.observe(220.0, false, true, 38.0);
        tracker.observe(260.0, false, true, 40.0);
        tracker.observe(270.0, true, true, 10.0);
        // Ready again: the last frame of the window.
        tracker.observe(280.0, true, true, 11.0);
        // Afterwards: after-ready only.
        tracker.observe(290.0, true, true, 12.0);
        let report = tracker.report(&config, 0);
        let after = report.frames_after_ready.unwrap();
        assert_eq!(after.frames, 5);
        assert_eq!(after.worst_ms, 40.0);
        assert_eq!(after.over_33ms, 2);
        let window = report.jump_load_window.unwrap();
        assert_eq!(window.frames, 5, "17, 38, 40, 10, 11");
        assert_eq!(window.worst_ms, 40.0);
        assert_eq!(window.over_50ms, 0);
    }

    #[test]
    fn a_jump_window_that_never_closes_runs_to_the_end_and_no_jump_has_none() {
        let config = EngineConfig {
            benchmark_jump: Some((3, 4)),
            ..default()
        };
        let mut tracker = benchmark_tracker();
        tracker.observe(0.0, true, true, 100.0);
        tracker.observe(16.0, true, true, 16.0);
        tracker.observe(40.0, false, true, 24.0);
        tracker.observe(60.0, false, true, 20.0);
        let window = tracker.report(&config, 0).jump_load_window.unwrap();
        assert_eq!(window.frames, 3);
        let mut plain = benchmark_tracker();
        plain.observe(0.0, true, false, 100.0);
        plain.observe(16.0, true, false, 16.0);
        plain.observe(32.0, false, false, 16.0);
        let report = plain.report(&EngineConfig::default(), 0);
        assert!(report.jump_load_window.is_none());
        assert_eq!(report.frames_after_ready.unwrap().frames, 1);
    }

    /// A bare app whose time advances by exactly 1000 ms per update: the real clock's delta is
    /// 1000 ms and the virtual clock's is Bevy's 250 ms clamp of it. The caller adds its own
    /// resources, entities and systems before the first update.
    fn thousand_millisecond_frames() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            1000,
        )));
        app
    }

    /// The window delta comes from the real wall clock. A 1000 ms frame leaves the two clocks far
    /// apart — the virtual one is clamped to 250 ms — and `window_delta_ms` reads the real one. Had
    /// the helper taken `&Time<Virtual>`, this would read 250.
    #[test]
    fn the_window_delta_is_the_real_clock_not_the_clamped_virtual_one() {
        let mut app = thousand_millisecond_frames();
        // Two frames: the plugin's first is its own warm-up, and a full second is the delta from
        // there on.
        app.update();
        app.update();

        let virtual_clock = app.world().resource::<Time<Virtual>>();
        assert_eq!(
            virtual_clock.delta_secs_f64() * 1000.0,
            250.0,
            "the premise: Bevy clamps the virtual delta, so reading it would hide the hitch"
        );
        let real = app.world().resource::<Time<Real>>();
        assert_eq!(real.delta_secs_f64() * 1000.0, 1000.0);
        assert_eq!(window_delta_ms(real), 1000.0);
    }

    /// An empty `CellCache`: `track_world_ready` requires the resource, and a run with no jump
    /// configured never reads it. The directory is returned because the map must outlive the app.
    pub(super) fn empty_cell_cache() -> (tempfile::TempDir, CellCache) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cell_cache.rkyv");
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&shared::CellCache {
            version: shared::CELL_CACHE_VERSION,
            cells: vec![],
        })
        .unwrap();
        std::fs::write(&path, bytes).unwrap();
        (directory, CellCache::open(&path).unwrap())
    }

    /// The system itself, not just the helper: `track_world_ready` in a Bevy app, on frames whose
    /// real delta is 1000 ms and whose virtual delta is the 250 ms clamp of it, must record the
    /// 1000 ms. The tracker starts already latched ready — that predicate needs a populated
    /// streaming world, which is not what this test is about — so the clock is all the system can
    /// get the window frame times from. A system reading `Time<Virtual>`, the regression this pins,
    /// would record 250 ms.
    #[test]
    fn track_world_ready_records_the_window_frame_time_from_the_real_clock() {
        let (_cache_directory, cache) = empty_cell_cache();
        let mut app = thousand_millisecond_frames();
        app.insert_resource(EngineConfig {
            benchmark_duration_secs: Some(1.0),
            ..default()
        })
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(StreamingWorld::default())
        .insert_resource(StreamingMetrics::default())
        .insert_resource(cache)
        .insert_resource(PacingTracker {
            first_ready: ReadyLatch {
                latched: Some(Mark {
                    millis: 0.0,
                    frames: 1,
                }),
                ..default()
            },
            ..default()
        })
        .init_resource::<ProfilingState>()
        .add_systems(Update, track_world_ready);
        app.world_mut()
            .spawn((Transform::default(), StreamingCamera));

        // A benchmark run, so the system keeps the windows; every frame is a one-second hitch.
        for _ in 0..3 {
            app.update();
        }

        let tracker = app.world().resource::<PacingTracker>();
        assert!(
            !tracker.frames_after_ready.is_empty(),
            "the system must record the frames of the window"
        );
        for frame_ms in &tracker.frames_after_ready {
            assert_eq!(
                *frame_ms, 1000.0,
                "the window keeps the real clock's frame times, not the virtual clock's 250 ms"
            );
        }
    }

    #[test]
    fn world_ready_tracks_the_active_interior_or_destination_worldspace() {
        use crate::world::database::CellKey;
        for interior in [Some(77), None] {
            let (_cache_directory, cache) = empty_cell_cache();
            let mut app = thousand_millisecond_frames();
            let space = ActiveSpace {
                worldspace_id: Some(42),
                interior,
            };
            let mut streaming = StreamingWorld::default();
            let key = interior.map_or(
                CellKey::Exterior {
                    worldspace_id: 42,
                    grid_x: 0,
                    grid_y: 0,
                },
                CellKey::Interior,
            );
            if interior.is_some() {
                let root = app.world_mut().spawn_empty().id();
                streaming.set_resident_for_test(key, root);
            } else {
                streaming.set_failed_for_test(key);
            }
            app.insert_resource(EngineConfig {
                stream_radius: 0,
                ..default()
            })
            .insert_resource(space)
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(streaming)
            .insert_resource(StreamingMetrics::default())
            .insert_resource(cache)
            .init_resource::<PacingTracker>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, track_world_ready);
            app.world_mut()
                .spawn((Transform::default(), StreamingCamera));
            for _ in 0..3 {
                app.update();
            }
            let tracker = app.world().resource::<PacingTracker>();
            assert!(tracker.first_ready.latched.is_some(), "space={space:?}");
            assert_eq!(tracker.window_failed_cells, usize::from(interior.is_none()));
        }
    }

    /// A normal play session is not a benchmark: nothing bounds its length, so the tracker keeps no
    /// frame times at all. The world-ready time is a pair of counters, and is still measured.
    #[test]
    fn a_run_that_is_not_a_benchmark_keeps_no_frame_times() {
        let config = EngineConfig {
            benchmark_jump: Some((3, 4)),
            ..default()
        };
        assert!(!config.is_benchmark_run(), "no frame limit and no duration");
        let mut tracker = PacingTracker::default();
        tracker.observe(0.0, false, true, 150.0);
        tracker.observe(100.0, true, true, 16.0);
        assert!(tracker.observe(116.0, true, true, 17.0), "the jump fires");
        tracker.observe(200.0, false, true, 40.0);
        let report = tracker.report(&config, 0);
        assert!(report.world_ready_reached);
        assert_eq!(report.time_to_world_ready_ms, Some(100.0));
        assert!(report.jump_issued);
        assert!(report.frames_after_ready.is_none());
        assert!(report.jump_load_window.is_none());
    }

    #[test]
    fn distance_percentile_uses_the_shared_rank_formula() {
        assert_eq!(distance_percentile(&[], 0.05), None);
        assert_eq!(distance_percentile(&[7.0], 0.05), Some(7.0));
        let sorted: Vec<f32> = (1..=100).map(|n| n as f32).collect();
        assert_eq!(distance_percentile(&sorted, 0.05), Some(6.0));
        assert_eq!(distance_percentile(&sorted, 1.0), Some(100.0));
    }

    #[test]
    fn fly_lag_counts_models_inside_one_cell() {
        let mut tracker = PacingTracker::default();
        for distance in [9000.0, 100.0, 4096.0, 4097.0, 20_000.0, f32::NAN] {
            tracker.record_model_ready(distance);
        }
        let flying = EngineConfig {
            auto_fly_speed: 5000.0,
            ..default()
        };
        let lag = tracker.report(&flying, 0).fly_lag.expect("a flying run");
        assert_eq!(lag.models_ready, 6);
        assert_eq!(lag.ready_within_one_cell, 2);
        assert_eq!(lag.ready_distance_min, Some(100.0));
        // Five finite distances: index ceil(4 * 0.05) = 1, the second.
        assert_eq!(lag.ready_distance_p5, Some(4096.0));
        assert_eq!(lag.p5_sample_size, 5);
        // An empty run reports nulls rather than zeros.
        let empty = PacingTracker::default().report(&flying, 0).fly_lag.unwrap();
        assert_eq!(empty.ready_distance_min, None);
        assert_eq!(empty.ready_distance_p5, None);
        assert_eq!(empty.p5_sample_size, 0);
        assert_eq!(empty.ready_within_one_cell, 0);
    }

    #[test]
    fn a_capped_sample_keeps_the_count_and_minimum_exact() {
        let mut tracker = PacingTracker::default();
        for _ in 0..MAX_READY_DISTANCES {
            tracker.record_model_ready(10_000.0);
        }
        // Past the cap: the sample no longer grows, the count and minimum still do.
        tracker.record_model_ready(50.0);
        tracker.record_model_ready(60.0);
        let flying = EngineConfig {
            auto_fly_speed: 5000.0,
            ..default()
        };
        let lag = tracker.report(&flying, 0).fly_lag.unwrap();
        assert_eq!(lag.models_ready, MAX_READY_DISTANCES as u64 + 2);
        assert_eq!(lag.ready_within_one_cell, 2);
        assert_eq!(lag.ready_distance_min, Some(50.0));
        assert_eq!(lag.p5_sample_size, MAX_READY_DISTANCES);
    }

    #[test]
    fn horizontal_distance_ignores_height() {
        assert_eq!(
            horizontal_distance(Vec3::new(0.0, 0.0, 0.0), Vec3::new(3.0, 500.0, -4.0)),
            5.0
        );
    }
    /// "Ready" means drawn, not validated: a surface a view can see that the render
    /// world has not confirmed drawn holds the mark, so the benchmark does not time a stand-in or a
    /// hole - and, on its own, nothing else can be the reason.
    #[test]
    fn world_ready_waits_for_drawn() {
        let drawn = ready_inputs();
        assert!(drawn.is_ready());

        let waiting = WorldReadyInputs {
            pending_drawn: 1,
            ..ready_inputs()
        };
        assert!(
            !waiting.is_ready(),
            "a visible surface the render world has not confirmed drawn holds the world-ready mark"
        );
        assert!(
            WorldReadyInputs {
                pending_drawn: 0,
                ..waiting
            }
            .is_ready(),
            "and once it is drawn the mark latches: nothing else about the view changed"
        );
    }

    #[test]
    fn stock_ready_inputs_require_actual_batch_and_draw_completion() {
        let metrics = StreamingMetrics::default();
        let ready = WorldReadyInputs::from_streaming(&metrics, 0, 25, 25, 0, 0, 0);
        assert!(
            ready.is_ready(),
            "global legacy terrain readiness is not current-view demand"
        );
        let undrawn = WorldReadyInputs::from_streaming(&metrics, 1, 25, 25, 0, 0, 0);
        assert!(
            !undrawn.is_ready(),
            "current-view actual draw readiness remains required"
        );
    }

    #[test]
    fn existing_pending_metrics_block_the_world_ready_mark() {
        for metrics in [
            StreamingMetrics {
                pending_lod_queries: 1,
                ..default()
            },
            StreamingMetrics {
                pending_lod_chunks: 1,
                ..default()
            },
            StreamingMetrics {
                pending_cluster_batches: 1,
                ..default()
            },
            StreamingMetrics {
                failed_cluster_batches: 1,
                ..default()
            },
        ] {
            let (_cache_directory, cache) = empty_cell_cache();
            let mut app = thousand_millisecond_frames();
            let mut streaming = StreamingWorld::default();
            streaming.set_failed_for_test(crate::world::database::CellKey::Interior(77));
            app.insert_resource(EngineConfig {
                stream_radius: 0,
                ..default()
            })
            .insert_resource(ActiveSpace {
                interior: Some(77),
                worldspace_id: None,
            })
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(streaming)
            .insert_resource(metrics)
            .insert_resource(cache)
            .init_resource::<PacingTracker>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, track_world_ready);
            app.world_mut()
                .spawn((Transform::default(), StreamingCamera));
            for _ in 0..3 {
                app.update();
            }
            assert!(
                app.world()
                    .resource::<PacingTracker>()
                    .first_ready
                    .latched
                    .is_none()
            );
        }
    }

    #[test]
    fn ordinary_readiness_is_unchanged_by_an_unready_cluster_gate() {
        let config = EngineConfig {
            worldspace_id: 42,
            stream_radius: 0,
            ..default()
        };
        let space = ActiveSpace::default();
        let mut streaming = StreamingWorld::default();
        streaming.set_failed_for_test(crate::world::database::CellKey::Exterior {
            worldspace_id: 42,
            grid_x: 0,
            grid_y: 0,
        });
        let metrics = StreamingMetrics::default();
        let gate = crate::cluster::loading::ClusterLoadingGate::default();
        let inputs = |gate| {
            world_ready_inputs_at_frame_end(
                &config,
                IVec2::ZERO,
                &streaming,
                &space,
                &metrics,
                Vec3::ZERO,
                0,
                gate,
            )
        };
        let ordinary = inputs(None);
        assert!(ordinary.is_ready());
        assert_eq!(ordinary.failed_window_cells, 1);
        assert_eq!(inputs(Some(&gate)), ordinary);
    }

    #[test]
    fn absent_window_cells_and_cluster_drawability_are_independent() {
        let metrics = StreamingMetrics::default();
        let absent = WorldReadyInputs::from_streaming(&metrics, 0, 25, 24, 0, 1, 0);
        assert_eq!(absent.absent_window_cells, 1);
        assert_eq!(absent.failed_window_cells, 0);
        assert!(absent.is_ready());
        let waiting = WorldReadyInputs::from_streaming(&metrics, 0, 25, 24, 0, 1, 1);
        assert_eq!(waiting.absent_window_cells, 1);
        assert_eq!(waiting.pending_cluster_drawability, 1);
        assert!(!waiting.is_ready());
    }

    #[test]
    fn global_terrain_diagnostic_does_not_block_a_drawn_stock_view() {
        let mut metrics = StreamingMetrics {
            pending_cluster_terrain_blocks: 37,
            ..default()
        };
        let ready = WorldReadyInputs::from_streaming(&metrics, 0, 25, 25, 0, 0, 0);
        assert!(
            ready.is_ready(),
            "global legacy terrain readiness is not current-view demand"
        );
        metrics.pending_cluster_batches = 1;
        let waiting = WorldReadyInputs::from_streaming(&metrics, 0, 25, 25, 0, 0, 0);
        assert!(
            !waiting.is_ready(),
            "actual pending cluster batches still hold readiness"
        );
        metrics.pending_cluster_batches = 0;
        metrics.failed_cluster_batches = 1;
        assert!(!WorldReadyInputs::from_streaming(&metrics, 0, 25, 25, 0, 0, 0).is_ready());
        metrics.failed_cluster_batches = 0;
        let undrawn = WorldReadyInputs::from_streaming(&metrics, 1, 25, 25, 0, 0, 0);
        assert!(
            !undrawn.is_ready(),
            "current-view actual draw readiness remains required"
        );
    }

    #[test]
    fn world_ready_maps_current_distant_stock_work_and_failures() {
        let metrics = StreamingMetrics {
            queued_far_stock_queries: 2,
            pending_far_stock_queries: 3,
            pending_far_stock_qualification: 7,
            failed_far_stock_sources: 5,
            ..default()
        };
        let inputs = WorldReadyInputs::from_streaming(&metrics, 0, 25, 25, 0, 0, 0);
        assert_eq!(inputs.queued_far_stock_queries, 2);
        assert_eq!(inputs.pending_far_stock_queries, 3);
        assert_eq!(inputs.pending_far_stock_qualification, 7);
        assert_eq!(inputs.failed_far_stock_sources, 5);
        assert!(!inputs.is_ready());
        assert!(
            WorldReadyInputs::from_streaming(&StreamingMetrics::default(), 0, 25, 25, 0, 0, 0)
                .is_ready()
        );
    }

    #[test]
    fn first_ready_waits_for_the_current_owned_view_snapshot() {
        use crate::cluster::loading::{ClusterLoadingGate, ClusterWorldReadiness};
        use crate::world::database::CellKey;
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package = std::sync::Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let (_cache_directory, cache) = empty_cell_cache();
        let mut app = thousand_millisecond_frames();
        let mut streaming = StreamingWorld::default();
        streaming.set_failed_for_test(CellKey::Exterior {
            worldspace_id: 42,
            grid_x: 0,
            grid_y: 0,
        });
        app.insert_resource(EngineConfig {
            worldspace_id: 42,
            stream_radius: 0,
            clusters: Some("selected".into()),
            cluster_own_statics: true,
            ..default()
        })
        .insert_resource(ActiveSpace::default())
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(streaming)
        .init_resource::<StreamingMetrics>()
        .insert_resource(cache)
        .init_resource::<PacingTracker>()
        .init_resource::<ProfilingState>()
        .init_resource::<ClusterLoadingGate>()
        .add_systems(Update, track_world_ready);
        app.world_mut()
            .spawn((Transform::default(), StreamingCamera));
        for _ in 0..3 {
            app.update();
        }
        assert!(
            !app.world()
                .resource::<PacingTracker>()
                .world_ready_reached()
        );
        let generation = app.world().resource::<ClusterLoadingGate>().generation();
        app.world_mut()
            .resource_mut::<ClusterLoadingGate>()
            .publish(
                &package,
                7,
                generation,
                1,
                std::collections::HashMap::from([(
                    42,
                    ClusterWorldReadiness {
                        applicable_cameras: 1,
                        needed_batches: 2,
                        ..default()
                    },
                )]),
            );
        app.update();
        assert!(
            !app.world()
                .resource::<PacingTracker>()
                .world_ready_reached()
        );
        let mut camera = app
            .world_mut()
            .query_filtered::<&mut Transform, With<StreamingCamera>>();
        camera.single_mut(app.world_mut()).unwrap().translation.y += 10.0;
        app.update();
        assert!(
            !app.world()
                .resource::<PacingTracker>()
                .world_ready_reached(),
            "first-ready must invalidate this frame's new pose before reading an old snapshot"
        );
        let current = app.world().resource::<ClusterLoadingGate>().generation();
        assert_ne!(current, generation);
        app.world_mut()
            .resource_mut::<ClusterLoadingGate>()
            .publish(
                &package,
                7,
                current,
                2,
                std::collections::HashMap::from([(
                    42,
                    ClusterWorldReadiness {
                        applicable_cameras: 1,
                        needed_batches: 2,
                        ..default()
                    },
                )]),
            );
        app.update();
        app.update();
        assert!(
            app.world()
                .resource::<PacingTracker>()
                .world_ready_reached()
        );
    }

    fn benchmark_pose_app(clusters: bool) -> (tempfile::TempDir, App) {
        use crate::cluster::loading::ClusterLoadingGate;
        use crate::world::database::CellKey;
        let (directory, cache) = empty_cell_cache();
        let mut app = thousand_millisecond_frames();
        assert_eq!(
            crate::streaming::streaming_center(Vec3::new(100.0, 12_345.0, -200.0), IVec2::ZERO),
            IVec2::ZERO
        );
        let root = app.world_mut().spawn_empty().id();
        let mut streaming = StreamingWorld::default();
        streaming.set_resident_for_test(
            CellKey::Exterior {
                worldspace_id: 42,
                grid_x: 0,
                grid_y: 0,
            },
            root,
        );
        app.insert_resource(EngineConfig {
            worldspace_id: 42,
            stream_radius: 0,
            clusters: clusters.then(|| "selected".into()),
            cluster_own_statics: clusters,
            benchmark_jump: Some((0, 0)),
            ..default()
        })
        .insert_resource(ActiveSpace::default())
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(streaming)
        .init_resource::<StreamingMetrics>()
        .insert_resource(cache)
        .init_resource::<PacingTracker>()
        .init_resource::<ProfilingState>()
        .init_resource::<ClusterLoadingGate>()
        .add_systems(Update, track_world_ready);
        app.world_mut().spawn((
            Transform::from_xyz(100.0, 12_345.0, -200.0),
            StreamingCamera,
        ));
        (directory, app)
    }

    #[test]
    fn benchmark_jump_requires_fresh_render_proof_for_its_final_pose() {
        use crate::cluster::loading::{ClusterLoadingGate, ClusterWorldReadiness};
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package = std::sync::Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let (_cache_directory, mut app) = benchmark_pose_app(true);
        let publish = |app: &mut App, generation, frame| {
            assert!(
                app.world_mut()
                    .resource_mut::<ClusterLoadingGate>()
                    .publish(
                        &package,
                        7,
                        generation,
                        frame,
                        std::collections::HashMap::from([(
                            42,
                            ClusterWorldReadiness {
                                applicable_cameras: 1,
                                needed_batches: 2,
                                ..default()
                            }
                        )]),
                    )
            );
        };
        app.update();
        let before = app.world().resource::<ClusterLoadingGate>().generation();
        publish(&mut app, before, 1);
        app.update();
        app.update();
        assert!(app.world().resource::<PacingTracker>().jump.is_some());
        let jumped = app.world().resource::<ClusterLoadingGate>().generation();
        assert_ne!(
            jumped, before,
            "the benchmark camera mutation must withdraw pre-jump proof immediately"
        );
        for _ in 0..3 {
            app.update();
        }
        assert!(
            app.world()
                .resource::<PacingTracker>()
                .jump
                .unwrap()
                .latch
                .latched
                .is_none()
        );
        app.world_mut()
            .query_filtered::<&mut Transform, With<StreamingCamera>>()
            .single_mut(app.world_mut())
            .unwrap()
            .translation
            .y += 10.0;
        app.update();
        let final_pose = app.world().resource::<ClusterLoadingGate>().generation();
        assert_ne!(
            final_pose, jumped,
            "collision or camera placement while jump readiness is pending needs fresh proof"
        );
        assert!(
            !app.world_mut()
                .resource_mut::<ClusterLoadingGate>()
                .publish(&package, 7, jumped, 2, std::collections::HashMap::new(),)
        );
        for _ in 0..3 {
            app.update();
        }
        assert!(
            app.world()
                .resource::<PacingTracker>()
                .jump
                .unwrap()
                .latch
                .latched
                .is_none()
        );
        assert_eq!(
            app.world().resource::<ClusterLoadingGate>().generation(),
            final_pose
        );
        publish(&mut app, final_pose, 3);
        app.update();
        assert!(
            app.world()
                .resource::<PacingTracker>()
                .jump
                .unwrap()
                .latch
                .latched
                .is_none()
        );
        app.update();
        assert!(
            app.world()
                .resource::<PacingTracker>()
                .jump
                .unwrap()
                .latch
                .latched
                .is_some()
        );
        app.world_mut()
            .query_filtered::<&mut Transform, With<StreamingCamera>>()
            .single_mut(app.world_mut())
            .unwrap()
            .translation
            .y += 10.0;
        app.update();
        assert_eq!(
            app.world().resource::<ClusterLoadingGate>().generation(),
            final_pose,
            "ordinary movement after measurement must not globally invalidate rendering"
        );
        assert!(!app.world().contains_resource::<LoadingCover>());
    }

    #[test]
    fn ordinary_benchmark_jump_still_latches_without_cluster_publication_or_cover() {
        use crate::cluster::loading::ClusterLoadingGate;
        let (_directory, mut app) = benchmark_pose_app(false);
        let before = app.world().resource::<ClusterLoadingGate>().generation();
        for _ in 0..4 {
            app.update();
        }
        assert!(
            app.world()
                .resource::<PacingTracker>()
                .jump
                .unwrap()
                .latch
                .latched
                .is_some()
        );
        assert_eq!(
            app.world().resource::<ClusterLoadingGate>().generation(),
            before
        );
        assert!(!app.world().contains_resource::<LoadingCover>());
    }
}

#[cfg(test)]
mod stock_ready_input_supplement {
    use super::*;
    #[test]
    fn stock_ready_inputs_require_actual_batch_and_draw_completion() {
        let mut metrics = StreamingMetrics::default();
        let ready = WorldReadyInputs::from_streaming(&metrics, 0, 25, 25, 0, 0, 0);
        assert!(
            ready.is_ready(),
            "global legacy terrain readiness is not current-view demand"
        );
        metrics.pending_cluster_batches = 1;
        let waiting = WorldReadyInputs::from_streaming(&metrics, 0, 25, 25, 0, 0, 0);
        assert!(
            !waiting.is_ready(),
            "actual pending cluster batches still hold readiness"
        );
        metrics.pending_cluster_batches = 0;
        metrics.failed_cluster_batches = 1;
        assert!(!WorldReadyInputs::from_streaming(&metrics, 0, 25, 25, 0, 0, 0).is_ready());
        metrics.failed_cluster_batches = 0;
        let undrawn = WorldReadyInputs::from_streaming(&metrics, 1, 25, 25, 0, 0, 0);
        assert!(
            !undrawn.is_ready(),
            "current-view actual draw readiness remains required"
        );
    }
}
