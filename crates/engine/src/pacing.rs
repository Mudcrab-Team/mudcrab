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

use crate::{
    app::benchmark_jump_position,
    config::EngineConfig,
    profiling::ProfilingState,
    streaming::{ActiveSpace, RenderOrigin, StreamingMetrics, StreamingWorld, window_center},
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
    /// Cells in the stream window, `(2r + 1)^2`.
    pub window_cells: usize,
    /// Cells of that window resident at full detail.
    pub resident_window_cells: usize,
    /// Cells of that window that failed to load (map edge, missing data). A failed cell never
    /// becomes resident and is never retried, so it counts as settled.
    pub failed_window_cells: usize,
    pub loading_cells: usize,
    pub active_requests: usize,
    /// Models whose scene is loaded but not yet handed to the spawner.
    pub arming_queue_depth: usize,
    pub pending_asset_instances: usize,
    pub pending_surface_instances: usize,
}

impl WorldReadyInputs {
    /// Whether the world around the camera is fully loaded. Each condition alone holds it back.
    /// A failed window cell would otherwise hold it back forever: it is never retried and never
    /// becomes resident, so resident plus failed cells must cover the window.
    pub fn is_ready(&self) -> bool {
        self.window_cells > 0
            && self.resident_window_cells + self.failed_window_cells >= self.window_cells
            && self.loading_cells == 0
            && self.active_requests == 0
            && self.arming_queue_depth == 0
            && self.pending_asset_instances == 0
            && self.pending_surface_instances == 0
    }

    fn from_streaming(
        metrics: &StreamingMetrics,
        window_cells: usize,
        resident_window_cells: usize,
        failed_window_cells: usize,
    ) -> Self {
        Self {
            window_cells,
            resident_window_cells,
            failed_window_cells,
            loading_cells: metrics.loading_cells,
            active_requests: metrics.active_requests,
            arming_queue_depth: metrics.arming_queue_depth,
            pending_asset_instances: metrics.pending_asset_instances,
            pending_surface_instances: metrics.pending_surface_instances,
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
            fly_lag: (config.auto_fly_speed > 0.0).then(|| {
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

/// Evaluates the world-ready predicate each frame, runs the benchmark jump, and keeps the tracker.
/// Runs at the end of the streaming chain, after the planner and the readiness scans.
#[allow(clippy::too_many_arguments)]
pub(crate) fn track_world_ready(
    config: Res<EngineConfig>,
    origin: Res<RenderOrigin>,
    space: Option<Res<ActiveSpace>>,
    streaming: Res<StreamingWorld>,
    metrics: Res<StreamingMetrics>,
    cache: Res<CellCache>,
    mut camera: Query<&mut Transform, With<StreamingCamera>>,
    mut tracker: ResMut<PacingTracker>,
    mut profiler: ResMut<ProfilingState>,
    // The window frame times are real wall-clock deltas. The virtual `Time` Bevy's schedules see is
    // clamped (250 ms by default), so a one-second hitch would read as 250 ms. The report's own
    // `frame_ms_*` still use that clamped clock; the windows deliberately do not.
    real_time: Res<Time<Real>>,
) {
    let Ok(mut camera) = camera.single_mut() else {
        return;
    };
    let started = *tracker.started.get_or_insert_with(Instant::now);
    let center = window_center(&config, camera.translation, origin.0);
    let space = space.as_deref().copied().unwrap_or_default();
    let (window_cells, resident, failed) = streaming.active_space_residency(
        &space,
        config.worldspace_id,
        center,
        config.stream_radius,
    );
    let inputs = WorldReadyInputs::from_streaming(&metrics, window_cells, resident, failed);
    let elapsed_millis = started.elapsed().as_secs_f64() * 1000.0;
    let ready = inputs.is_ready();
    // Frame-time windows are for benchmark runs; an ordinary session runs unbounded.
    tracker.record_frame_windows = config.is_benchmark_run();
    tracker.window_failed_cells = failed;
    let was_latched = tracker.first_ready.latched.is_some();
    let jump_now = tracker.observe(
        elapsed_millis,
        ready,
        config.benchmark_jump.is_some() && space.interior.is_none(),
        real_time.delta_secs_f64() * 1000.0,
    );
    if let Some(grid) = config.benchmark_jump.filter(|_| jump_now) {
        let mut active_config = config.clone();
        active_config.worldspace_id = space.exterior_worldspace(config.worldspace_id);
        let position = benchmark_jump_position(&active_config, &cache, origin.0, grid);
        camera.translation = position;
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
        let holds_back: [fn(&mut WorldReadyInputs); 7] = [
            |i| i.resident_window_cells = 24,
            |i| i.loading_cells = 1,
            |i| i.active_requests = 1,
            |i| i.arming_queue_depth = 1,
            |i| i.pending_asset_instances = 1,
            |i| i.pending_surface_instances = 1,
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
}
