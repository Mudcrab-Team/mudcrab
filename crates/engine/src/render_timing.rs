//! CPU time of the render side of a frame, for benchmark reports.
//!
//! A benchmark's frame time is far larger than the main world's CPU time plus the GPU's pass
//! time: with pipelined rendering most of a frame goes to the render thread (extract commands,
//! asset preparation, specialization, queueing, bind groups, render graph encoding, submit and
//! present) and to the main thread waiting for it. This measures those parts:
//!
//! * `main_world`: the main world's schedule, from `First` to `Last`.
//! * `wait_for_render_thread`: the main thread waiting for the render thread to hand the render
//!   world back before it can extract (pipelined rendering only).
//! * `extract`: the extract step itself, on the main thread.
//! * `render_thread`: the render schedule, and its phases between Bevy's `RenderSystems` sets:
//!   `render/extract_commands_and_assets`, `render/specialize_and_views`, `render/queue`,
//!   `render/prepare`, `render/graph_and_present`, `render/cleanup`.
//! * `render/swapchain_acquire`: from just before `prepare_windows` to just after it, an upper
//!   bound on the wait for a swapchain image (where a GPU-bound frame shows up on the CPU).
//!
//! * Pipeline activity: each render frame also records how many pipelines were newly queued
//!   (`PipelineCache` in the render world), how many finished creating, and how many still wait.
//!   The cache holds render and compute pipelines alike, so the counts are not render pipelines
//!   alone. [`PipelineActivity`] sets the frames with activity against the rest and lists the worst
//!   frames with their counts, which tests "the worst frames are pipelines built on first use".
//!
//! Samples are kept only while the benchmark records (after warmup). Phases are distributions,
//! not a per-frame series: a render frame runs one frame behind the main frame that fed it.

use bevy::{
    app::{First, Last},
    platform::time::Instant,
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems, pipelined_rendering::RenderExtractApp,
        render_resource::PipelineCache, view::prepare_windows,
    },
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub const MAIN_WORLD: &str = "main_world";
pub const WAIT_FOR_RENDER_THREAD: &str = "wait_for_render_thread";
pub const EXTRACT: &str = "extract";
pub const RENDER_THREAD: &str = "render_thread";
pub const SWAPCHAIN_ACQUIRE: &str = "render/swapchain_acquire";

/// Measures the render side of each frame; shared by the main world, the render world and the
/// extract wrappers.
pub struct RenderTimingPlugin;

impl Plugin for RenderTimingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RenderTimings>()
            .add_systems(First, start_main_world)
            .add_systems(Last, end_main_world);
    }

    // The extract functions are set while the render plugins build, and pipelined rendering moves
    // the render app to its thread in `cleanup`, so they are wrapped here, in between.
    fn finish(&self, app: &mut App) {
        let timings = app.world().resource::<RenderTimings>().clone();
        // Counting the pipeline cache only feeds the pacing report, so an ordinary play session
        // does not run it every render frame.
        let samples_pipelines = app
            .world()
            .get_resource::<crate::config::EngineConfig>()
            .is_some_and(crate::config::EngineConfig::measures_pacing);
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.insert_resource(timings.clone());
            if let Some(mut extract) = render_app.take_extract() {
                let timings = timings.clone();
                render_app.set_extract(move |main_world, render_world| {
                    let started = Instant::now();
                    extract(main_world, render_world);
                    timings.extracted(started.elapsed());
                });
            }
            add_render_marks(render_app, samples_pipelines);
        }
        if let Some(extract_app) = app.get_sub_app_mut(RenderExtractApp)
            && let Some(mut hand_over) = extract_app.take_extract()
        {
            let timings = timings.clone();
            extract_app.set_extract(move |main_world, world| {
                let started = Instant::now();
                hand_over(main_world, world);
                timings.handed_over(started.elapsed());
            });
        }
    }
}

fn add_render_marks(render_app: &mut SubApp, samples_pipelines: bool) {
    // `RenderSystems::Render` shares its name with the `Render` schedule, so the sets are spelled
    // out.
    type Set = RenderSystems;
    render_app.add_systems(
        Render,
        (
            start_render_frame.before(Set::ExtractCommands),
            lap("render/extract_commands_and_assets")
                .after(Set::PrepareMeshes)
                .before(Set::CreateViews),
            lap("render/specialize_and_views")
                .after(Set::PrepareViews)
                .before(Set::Queue),
            lap("render/queue")
                .after(Set::PhaseSort)
                .before(Set::Prepare),
            lap("render/prepare")
                .after(Set::Prepare)
                .before(Set::Render),
            lap("render/graph_and_present")
                .after(Set::Render)
                .before(Set::Cleanup),
            (
                lap("render/cleanup"),
                sample_pipelines.run_if(move || samples_pipelines),
                end_render_frame,
            )
                .chain()
                .after(Set::PostCleanup),
            start_swapchain_acquire
                .in_set(Set::PrepareViews)
                .before(prepare_windows),
            end_swapchain_acquire
                .in_set(Set::PrepareViews)
                .after(prepare_windows),
        ),
    );
}

/// The shared clock: a handle cloned into both worlds and the extract wrappers.
#[derive(Resource, Clone, Default)]
pub struct RenderTimings(Arc<Shared>);

#[derive(Default)]
struct Shared {
    recording: AtomicBool,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    samples: BTreeMap<&'static str, Vec<f64>>,
    main_started: Option<Instant>,
    render_started: Option<Instant>,
    render_mark: Option<Instant>,
    acquire_started: Option<Instant>,
    last_extract: Option<Duration>,
    /// Pipeline counts for the render frame in progress, set just before it ends.
    pipelines: Option<PipelineFrame>,
    frames: Vec<RenderFrame>,
}

/// Pipelines (render and compute) in one render frame. Bevy's `PipelineCache` holds both, so these
/// counts are not render pipelines alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PipelineFrame {
    /// Pipelines newly added to the cache (queued for creation) this frame.
    pub created: usize,
    /// Pipelines that stopped waiting (finished creating, or failed) this frame.
    pub became_ready: usize,
    /// Pipelines still queued or being created at the end of the frame.
    pub waiting: usize,
}

#[derive(Debug, Clone, Copy)]
struct RenderFrame {
    render_thread_ms: f64,
    pipelines: PipelineFrame,
}

/// A render frame with its pipeline counts, for matching a spike with pipeline creation.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorstRenderFrame {
    pub render_thread_ms: f64,
    #[serde(flatten)]
    pub pipelines: PipelineFrame,
}

/// Whether the worst render frames are pipelines (render and compute) being built: render-thread
/// time on frames with pipeline activity against the others, and the slowest frames with their
/// counts.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PipelineActivity {
    pub frames: usize,
    pub frames_with_new_pipelines: usize,
    pub frames_with_pipelines_ready: usize,
    pub pipelines_created: u64,
    pub pipelines_became_ready: u64,
    /// Mean `render_thread` ms over frames that queued or finished a pipeline, and over the rest.
    pub render_thread_ms_mean_with_activity: Option<f64>,
    pub render_thread_ms_mean_without_activity: Option<f64>,
    /// The slowest render frames, worst first, with their pipeline counts.
    pub worst_frames: Vec<WorstRenderFrame>,
}

/// How many of the slowest render frames [`PipelineActivity`] lists.
const WORST_RENDER_FRAMES: usize = 10;

fn summarize_pipeline_frames(frames: &[RenderFrame]) -> Option<PipelineActivity> {
    if frames.is_empty() {
        return None;
    }
    let active =
        |frame: &RenderFrame| frame.pipelines.created > 0 || frame.pipelines.became_ready > 0;
    let mean = |values: Vec<f64>| {
        (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
    };
    let mut worst: Vec<WorstRenderFrame> = frames
        .iter()
        .map(|frame| WorstRenderFrame {
            render_thread_ms: frame.render_thread_ms,
            pipelines: frame.pipelines,
        })
        .collect();
    worst.sort_by(|a, b| b.render_thread_ms.total_cmp(&a.render_thread_ms));
    worst.truncate(WORST_RENDER_FRAMES);
    Some(PipelineActivity {
        frames: frames.len(),
        frames_with_new_pipelines: frames.iter().filter(|f| f.pipelines.created > 0).count(),
        frames_with_pipelines_ready: frames
            .iter()
            .filter(|f| f.pipelines.became_ready > 0)
            .count(),
        pipelines_created: frames.iter().map(|f| f.pipelines.created as u64).sum(),
        pipelines_became_ready: frames.iter().map(|f| f.pipelines.became_ready as u64).sum(),
        render_thread_ms_mean_with_activity: mean(
            frames
                .iter()
                .filter(|f| active(f))
                .map(|f| f.render_thread_ms)
                .collect(),
        ),
        render_thread_ms_mean_without_activity: mean(
            frames
                .iter()
                .filter(|f| !active(f))
                .map(|f| f.render_thread_ms)
                .collect(),
        ),
        worst_frames: worst,
    })
}

impl RenderTimings {
    /// Keep samples from now on (true) or stop keeping them (false).
    pub fn set_recording(&self, recording: bool) {
        self.0.recording.store(recording, Ordering::Relaxed);
    }

    /// Every kept sample so far, in milliseconds, by name; the store is emptied.
    pub fn take_samples(&self) -> BTreeMap<&'static str, Vec<f64>> {
        std::mem::take(&mut self.state().samples)
    }

    /// The pipeline activity of every recorded render frame so far; the store is emptied. `None`
    /// when no frame was recorded.
    pub fn take_pipeline_activity(&self) -> Option<PipelineActivity> {
        summarize_pipeline_frames(&std::mem::take(&mut self.state().frames))
    }

    fn pipelines(&self, frame: PipelineFrame) {
        self.state().pipelines = Some(frame);
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn record(&self, state: &mut State, name: &'static str, elapsed: Duration) {
        if self.0.recording.load(Ordering::Relaxed) {
            state
                .samples
                .entry(name)
                .or_default()
                .push(elapsed.as_secs_f64() * 1000.0);
        }
    }

    fn extracted(&self, elapsed: Duration) {
        let mut state = self.state();
        state.last_extract = Some(elapsed);
        self.record(&mut state, EXTRACT, elapsed);
    }

    /// The pipelined hand-over: waiting for the render world, then extracting into it.
    fn handed_over(&self, elapsed: Duration) {
        let mut state = self.state();
        if let Some(extract) = state.last_extract.take() {
            self.record(
                &mut state,
                WAIT_FOR_RENDER_THREAD,
                elapsed.saturating_sub(extract),
            );
        }
    }

    fn start_main(&self) {
        self.state().main_started = Some(Instant::now());
    }

    fn end_main(&self) {
        let mut state = self.state();
        if let Some(started) = state.main_started.take() {
            self.record(&mut state, MAIN_WORLD, started.elapsed());
        }
    }

    fn start_render(&self) {
        let now = Instant::now();
        let mut state = self.state();
        state.render_started = Some(now);
        state.render_mark = Some(now);
    }

    fn lap(&self, name: &'static str) {
        let now = Instant::now();
        let mut state = self.state();
        if let Some(mark) = state.render_mark.replace(now) {
            self.record(&mut state, name, now - mark);
        }
    }

    fn end_render(&self) {
        let mut state = self.state();
        state.render_mark = None;
        let pipelines = state.pipelines.take();
        if let Some(started) = state.render_started.take() {
            self.record(&mut state, RENDER_THREAD, started.elapsed());
            if self.0.recording.load(Ordering::Relaxed)
                && let Some(pipelines) = pipelines
            {
                state.frames.push(RenderFrame {
                    render_thread_ms: started.elapsed().as_secs_f64() * 1000.0,
                    pipelines,
                });
            }
        }
    }

    fn start_acquire(&self) {
        self.state().acquire_started = Some(Instant::now());
    }

    fn end_acquire(&self) {
        let mut state = self.state();
        if let Some(started) = state.acquire_started.take() {
            self.record(&mut state, SWAPCHAIN_ACQUIRE, started.elapsed());
        }
    }
}

fn start_main_world(timings: Res<RenderTimings>) {
    timings.start_main();
}

/// Ends the main world's sample. A benchmark that stops recording in `Last` runs after this, so
/// the frame that completes the measurement keeps its `main_world` sample.
pub(crate) fn end_main_world(timings: Res<RenderTimings>) {
    timings.end_main();
}

/// Counts the pipelines (render and compute) this frame added to the cache and the ones that
/// stopped waiting.
/// `PipelineCache::pipelines` lists every pipeline ever queued (the cache never removes one), and
/// `waiting_pipelines` the ones still queued or being created, so the frame's new pipelines are the
/// growth of the first and the ones that finished are what the waiting set lost beyond the new.
/// Runs after the cache has processed its queue (`RenderSystems::Render`).
fn sample_pipelines(
    timings: Res<RenderTimings>,
    cache: Option<Res<PipelineCache>>,
    mut previous: Local<(usize, usize)>,
) {
    let Some(cache) = cache else {
        return;
    };
    let total = cache.pipelines().count();
    let waiting = cache.waiting_pipelines().count();
    let (previous_total, previous_waiting) = *previous;
    let frame = pipeline_frame(previous_total, previous_waiting, total, waiting);
    *previous = (total, waiting);
    timings.pipelines(frame);
}

fn pipeline_frame(
    previous_total: usize,
    previous_waiting: usize,
    total: usize,
    waiting: usize,
) -> PipelineFrame {
    let created = total.saturating_sub(previous_total);
    PipelineFrame {
        created,
        became_ready: (created + previous_waiting).saturating_sub(waiting),
        waiting,
    }
}

fn start_render_frame(timings: Res<RenderTimings>) {
    timings.start_render();
}

fn end_render_frame(timings: Res<RenderTimings>) {
    timings.end_render();
}

fn start_swapchain_acquire(timings: Res<RenderTimings>) {
    timings.start_acquire();
}

fn end_swapchain_acquire(timings: Res<RenderTimings>) {
    timings.end_acquire();
}

fn lap(name: &'static str) -> impl FnMut(Res<RenderTimings>) {
    move |timings: Res<RenderTimings>| timings.lap(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_samples_only_while_recording() {
        let timings = RenderTimings::default();
        timings.start_main();
        timings.end_main();
        assert!(timings.take_samples().is_empty());

        timings.set_recording(true);
        timings.start_main();
        timings.end_main();
        timings.set_recording(false);
        timings.start_main();
        timings.end_main();
        assert_eq!(timings.take_samples()[MAIN_WORLD].len(), 1);
        assert!(
            timings.take_samples().is_empty(),
            "taking empties the store"
        );
    }

    #[test]
    fn laps_split_the_render_frame_and_sum_to_it() {
        let timings = RenderTimings::default();
        timings.set_recording(true);
        timings.lap("render/queue");
        assert!(
            timings.take_samples().is_empty(),
            "a lap before the frame started has no mark"
        );

        timings.start_render();
        timings.lap("render/extract_commands_and_assets");
        std::thread::sleep(Duration::from_millis(2));
        timings.lap("render/queue");
        timings.end_render();
        timings.lap("render/cleanup");
        let samples = timings.take_samples();
        assert!(
            !samples.contains_key("render/cleanup"),
            "the frame had ended"
        );
        let queue = samples["render/queue"][0];
        let phases = samples["render/extract_commands_and_assets"][0] + queue;
        assert!(queue >= 2.0);
        assert!(samples[RENDER_THREAD][0] >= phases);
    }

    #[test]
    fn pipeline_counts_follow_the_cache() {
        // Nothing queued, nothing waiting.
        assert_eq!(pipeline_frame(10, 0, 10, 0), PipelineFrame::default());
        // Five queued this frame, none finished yet.
        assert_eq!(
            pipeline_frame(10, 0, 15, 5),
            PipelineFrame {
                created: 5,
                became_ready: 0,
                waiting: 5
            }
        );
        // Two new on top of five waiting; four are still waiting, so three finished.
        assert_eq!(
            pipeline_frame(15, 5, 17, 4),
            PipelineFrame {
                created: 2,
                became_ready: 3,
                waiting: 4
            }
        );
    }

    #[test]
    fn pipeline_activity_sets_active_frames_against_quiet_ones() {
        let frame = |ms, created, became_ready| RenderFrame {
            render_thread_ms: ms,
            pipelines: PipelineFrame {
                created,
                became_ready,
                waiting: 0,
            },
        };
        assert_eq!(summarize_pipeline_frames(&[]), None);
        let activity = summarize_pipeline_frames(&[
            frame(4.0, 0, 0),
            frame(6.0, 0, 0),
            frame(30.0, 3, 0),
            frame(10.0, 0, 2),
        ])
        .unwrap();
        assert_eq!(activity.frames, 4);
        assert_eq!(activity.frames_with_new_pipelines, 1);
        assert_eq!(activity.frames_with_pipelines_ready, 1);
        assert_eq!(activity.pipelines_created, 3);
        assert_eq!(activity.pipelines_became_ready, 2);
        assert_eq!(activity.render_thread_ms_mean_with_activity, Some(20.0));
        assert_eq!(activity.render_thread_ms_mean_without_activity, Some(5.0));
        assert_eq!(activity.worst_frames[0].render_thread_ms, 30.0);
        assert_eq!(activity.worst_frames[0].pipelines.created, 3);
    }

    #[test]
    fn render_frames_keep_their_pipeline_counts_only_while_recording() {
        let timings = RenderTimings::default();
        timings.start_render();
        timings.pipelines(PipelineFrame {
            created: 1,
            ..default()
        });
        timings.end_render();
        assert!(timings.take_pipeline_activity().is_none());

        timings.set_recording(true);
        timings.start_render();
        timings.pipelines(PipelineFrame {
            created: 2,
            became_ready: 1,
            waiting: 1,
        });
        timings.end_render();
        let activity = timings.take_pipeline_activity().unwrap();
        assert_eq!(activity.frames, 1);
        assert_eq!(activity.pipelines_created, 2);
    }

    #[test]
    fn the_wait_is_the_hand_over_less_its_extract() {
        let timings = RenderTimings::default();
        timings.set_recording(true);
        timings.extracted(Duration::from_millis(3));
        timings.handed_over(Duration::from_millis(10));
        timings.handed_over(Duration::from_millis(10));
        let samples = timings.take_samples();
        assert_eq!(samples[EXTRACT], vec![3.0]);
        assert_eq!(
            samples[WAIT_FOR_RENDER_THREAD],
            vec![7.0],
            "a hand-over without a new extract is not counted"
        );
    }
}
