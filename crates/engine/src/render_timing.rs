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
//! Samples are kept only while the benchmark records (after warmup). Phases are distributions,
//! not a per-frame series: a render frame runs one frame behind the main frame that fed it.

use bevy::{
    app::{First, Last},
    platform::time::Instant,
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems, pipelined_rendering::RenderExtractApp,
        view::prepare_windows,
    },
};
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
            add_render_marks(render_app);
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

fn add_render_marks(render_app: &mut SubApp) {
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
            (lap("render/cleanup"), end_render_frame)
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
        if let Some(started) = state.render_started.take() {
            self.record(&mut state, RENDER_THREAD, started.elapsed());
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

fn end_main_world(timings: Res<RenderTimings>) {
    timings.end_main();
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
