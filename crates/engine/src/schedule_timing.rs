//! Time of each Bevy main schedule per frame, and assets that arrived per frame.
//!
//! `render_timing` records the whole main-world update as one phase (`main_world`). When a frame
//! is long while every streaming span of ours is short, this says which Bevy schedule holds the
//! time: `First`, `PreUpdate`, `StateTransition`, `RunFixedMainLoop`, `Update`, `SpawnScene`,
//! `PostUpdate` or `Last`.
//!
//! How: [`MainScheduleOrder`] lists the schedules `Main` runs in order. After every listed
//! schedule (and once before the first) this plugin inserts a tiny marker schedule whose single
//! system reads the clock; the time since the previous marker belongs to the schedule just run.
//! This needs no ordering against the other systems (a "first/last system in the schedule" set
//! cannot be guaranteed against systems that order themselves `before`/`after` everything), and
//! it also covers the schedule's own overhead (executor, apply-deferred). The markers are added
//! in `finish`, once every plugin has had its say over the order. Cost: eight one-system
//! schedules per frame.
//!
//! Measurement only, and only when a run measures pacing (a benchmark run or a `--benchmark-jump`
//! run): `AcceptanceMetricsPlugin` installs this plugin
//! only when [`crate::config::EngineConfig::measures_pacing`] holds, so an ordinary play session
//! adds neither the marker schedules nor the asset-arrival counting.
//!
//! Spans (milliseconds, in `cpu-spans.json`): `main_schedule/<name>` with `<name>` the schedule
//! name above. Companions: `assets_added/{mesh,image,standard_material,world_asset}`, the count of
//! `AssetEvent::Added` seen that frame, recorded every frame. These four are counts, not
//! milliseconds, so they are left out of the profile summary's top-CPU-spans table (they stay in
//! `cpu-spans.json`). The bundle keeps distributions, not per-frame series — the samples are
//! summarised away when it is written — so a spike frame cannot be matched against a burst frame
//! by frame number; the largest burst and the worst frame can only be read side by side.
//!
//! The count is read in `Last`, which sees that frame's events: in bevy_asset 0.19.0,
//! `Assets::<A>::asset_events` runs in `PostUpdate` in the `AssetEventSystems` set (lib.rs:661-666),
//! which `Last` follows.

use crate::profiling::ProfilingState;
use bevy::{
    app::{
        First, Last, MainScheduleOrder, PostUpdate, PreUpdate, RunFixedMainLoop, SpawnScene, Update,
    },
    ecs::schedule::{InternedScheduleLabel, ScheduleLabel},
    platform::time::Instant,
    prelude::*,
    state::state::StateTransition,
};

/// The schedules timed, with the span name each gets.
fn timed_schedules() -> Vec<(InternedScheduleLabel, &'static str)> {
    vec![
        (First.intern(), "main_schedule/First"),
        (PreUpdate.intern(), "main_schedule/PreUpdate"),
        (StateTransition.intern(), "main_schedule/StateTransition"),
        (RunFixedMainLoop.intern(), "main_schedule/RunFixedMainLoop"),
        (Update.intern(), "main_schedule/Update"),
        (SpawnScene.intern(), "main_schedule/SpawnScene"),
        (PostUpdate.intern(), "main_schedule/PostUpdate"),
        (Last.intern(), "main_schedule/Last"),
    ]
}

/// A marker schedule that runs right after (or, for `Start`, before) a timed schedule.
#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
struct ScheduleMark(&'static str);

#[derive(Resource, Default)]
struct ScheduleClock {
    last: Option<Instant>,
}

pub struct ScheduleTimingPlugin;

impl Plugin for ScheduleTimingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProfilingState>()
            .init_resource::<ScheduleClock>()
            .add_message::<AssetEvent<Mesh>>()
            .add_message::<AssetEvent<Image>>()
            .add_message::<AssetEvent<StandardMaterial>>()
            .add_message::<AssetEvent<WorldAsset>>()
            .add_systems(Last, count_asset_arrivals);
    }

    fn finish(&self, app: &mut App) {
        let present: Vec<InternedScheduleLabel> =
            app.world().resource::<MainScheduleOrder>().labels.clone();
        let start = ScheduleMark("start");
        app.add_systems(start.clone(), start_clock);
        let mut marks = Vec::new();
        for (label, name) in timed_schedules() {
            if present.contains(&label) {
                let mark = ScheduleMark(name);
                app.add_systems(
                    mark.clone(),
                    move |mut clock: ResMut<ScheduleClock>,
                          mut profiler: ResMut<ProfilingState>| {
                        if let Some(last) = clock.last {
                            profiler.record_elapsed(name, last);
                        }
                        clock.last = Some(Instant::now());
                    },
                );
                marks.push((label, mark));
            }
        }
        // `insert_after` compares a label by value and does not accept the interned form, so the
        // list is edited directly.
        let mut order = app.world_mut().resource_mut::<MainScheduleOrder>();
        order.labels.insert(0, start.intern());
        for (label, mark) in marks {
            if let Some(index) = order.labels.iter().position(|current| *current == label) {
                order.labels.insert(index + 1, mark.intern());
            }
        }
    }
}

fn start_clock(mut clock: ResMut<ScheduleClock>) {
    clock.last = Some(Instant::now());
}

fn count_asset_arrivals(
    mut meshes: MessageReader<AssetEvent<Mesh>>,
    mut images: MessageReader<AssetEvent<Image>>,
    mut materials: MessageReader<AssetEvent<StandardMaterial>>,
    mut worlds: MessageReader<AssetEvent<WorldAsset>>,
    mut profiler: ResMut<ProfilingState>,
) {
    fn added<A: Asset>(reader: &mut MessageReader<AssetEvent<A>>) -> f64 {
        reader
            .read()
            .filter(|event| matches!(event, AssetEvent::Added { .. }))
            .count() as f64
    }
    profiler.record_ms("assets_added/mesh", added(&mut meshes));
    profiler.record_ms("assets_added/image", added(&mut images));
    profiler.record_ms("assets_added/standard_material", added(&mut materials));
    profiler.record_ms("assets_added/world_asset", added(&mut worlds));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_each_schedule_and_asset_arrivals() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), ScheduleTimingPlugin))
            .init_asset::<Mesh>();
        app.finish();
        app.cleanup();
        app.update();
        app.world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Mesh::from(Cuboid::default()));
        app.update();
        app.update();
        let profiler = app.world().resource::<ProfilingState>();
        for (_, name) in timed_schedules() {
            if name == "main_schedule/StateTransition" || name == "main_schedule/SpawnScene" {
                continue; // only present when their plugins are
            }
            assert!(
                !profiler.span_samples(name).is_empty(),
                "no samples for {name}"
            );
        }
        assert!(
            profiler
                .span_samples("assets_added/mesh")
                .iter()
                .sum::<f64>()
                >= 1.0,
            "mesh arrival not counted"
        );
    }
}
