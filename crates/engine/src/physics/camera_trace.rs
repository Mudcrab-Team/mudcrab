//! Opt-in per-frame trace of the WALK camera (`--walk-camera-trace <file>`). Off by default.
//!
//! One CSV row per rendered frame: the frame time, how many fixed physics ticks ran in the frame,
//! the body position of the latest tick, the blended position that feeds the follow spring
//! ([`super::WalkViewPose`]), the camera position, and the spring's lag, weight and speed. It is a
//! diagnostic for a real stair climb: the headless stair-jitter test in `stair_tests.rs` measures
//! the same quantities on a fixture flight. Positions are in render space (game units, Y up), so
//! the floating origin is written too.
//!
//! `frame_dt_s` is the virtual (simulation) delta that the follow spring uses, which is 0 while the
//! game is paused. `real_dt_s`, the last column but one, is the wall-clock frame time, so a stall
//! or a paused frame still shows its real gap. `spring_sampled` is 1 when the camera follow system
//! ran this frame; on a frame where it returned early (a paused door crossing, no spring yet) the
//! `feet_*` and `lag_y` columns come from cached state and are not a spring sample.

use super::{
    LookIntent, MoveMode, MovementTuning, PlayerBody, WalkCameraFollowState, WalkState,
    WalkViewPose,
};
use crate::{config::EngineConfig, world::components::StreamingCamera};
use bevy::prelude::*;
use std::{
    fs::File,
    io::{BufWriter, Write},
};

const HEADER: &str = "frame,elapsed_s,frame_dt_s,ticks,overstep,mode,origin_x,origin_y,\
body_x,body_y,body_z,feet_x,feet_y,feet_z,camera_x,camera_y,camera_z,lag_y,weight,spring_speed,pitch,grounded,vertical_velocity,real_dt_s,spring_sampled";

/// Flush the file after this many rows, so a run that is killed keeps most of its rows.
const FLUSH_EVERY_FRAMES: u64 = 60;

/// The open trace file and the per-frame tick count.
#[derive(Resource)]
pub(super) struct WalkCameraTrace {
    out: BufWriter<File>,
    frame: u64,
    ticks: u32,
    /// A write already failed and was reported; the trace has stopped.
    failed: bool,
}

impl Drop for WalkCameraTrace {
    /// `BufWriter` ignores a failed flush on drop. Flush on a normal exit and report the failure
    /// through the same warning path as a failed write.
    fn drop(&mut self) {
        if self.failed {
            return;
        }
        if let Err(error) = self.out.flush() {
            warn!(
                "walk camera trace: the final flush failed after {} frames: {error}; the last rows may be missing",
                self.frame
            );
        }
    }
}

/// Opens the trace file when `--walk-camera-trace` names one.
pub(super) fn start_walk_camera_trace(config: Option<Res<EngineConfig>>, mut commands: Commands) {
    let Some(path) = config.and_then(|config| config.walk_camera_trace.clone()) else {
        return;
    };
    let opened = File::create(&path).and_then(|file| {
        let mut out = BufWriter::new(file);
        writeln!(out, "{HEADER}")?;
        Ok(out)
    });
    match opened {
        Ok(out) => {
            info!("walk camera trace: writing {}", path.display());
            commands.insert_resource(WalkCameraTrace {
                out,
                frame: 0,
                ticks: 0,
                failed: false,
            });
        }
        Err(error) => warn!(
            "walk camera trace: cannot write {}: {error}",
            path.display()
        ),
    }
}

/// Counts the fixed ticks that ran since the last rendered frame.
pub(super) fn count_walk_camera_trace_tick(mut trace: ResMut<WalkCameraTrace>) {
    trace.ticks += 1;
}

/// Writes one row after the camera follow system placed the view.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn write_walk_camera_trace_row(
    mut commands: Commands,
    mut trace: ResMut<WalkCameraTrace>,
    time: Res<Time>,
    (real, virtual_time, crossing): (
        Res<Time<Real>>,
        Res<Time<Virtual>>,
        Option<Res<crate::door_crossing::DoorCrossing>>,
    ),
    fixed: Res<Time<Fixed>>,
    mode: Res<MoveMode>,
    tuning: Res<MovementTuning>,
    look: Res<LookIntent>,
    pose: Res<WalkViewPose>,
    follow: Res<WalkCameraFollowState>,
    render_origin: Option<Res<crate::streaming::RenderOrigin>>,
    player: Query<&Transform, (With<PlayerBody>, Without<StreamingCamera>)>,
    walk: Query<&WalkState, With<PlayerBody>>,
    camera: Query<&Transform, With<StreamingCamera>>,
) {
    let trace = &mut *trace;
    let ticks = std::mem::take(&mut trace.ticks);
    let (Ok(body), Ok(view)) = (player.single(), camera.single()) else {
        return;
    };
    let overstep = fixed.overstep_fraction();
    // The streamer can rebase the floating origin after the camera update and before this row. The
    // body and the camera are shifted at once, the view pose on its next sample, so move a copy of
    // the pose into the recorded origin to keep the row in one space.
    let mut pose = *pose;
    pose.rebase(render_origin.as_ref().map(|origin| origin.0));
    let feet = if pose.valid {
        pose.shown(overstep)
    } else {
        body.translation
    };
    let origin = render_origin.map(|origin| origin.0).unwrap_or_default();
    let (lag_y, weight, speed) = follow.spring.map_or((0.0, 0.0, 0.0), |spring| {
        (
            view.translation.y - (feet.y + tuning.eye_height),
            spring.weight,
            spring.velocity.length(),
        )
    });
    // The follow system returns early while a crossing holds the paused game, outside WALK, and
    // on its first frame (no spring yet); those rows carry cached values, not a spring sample.
    let sampled = *mode == MoveMode::Walk
        && follow.spring.is_some()
        && !(virtual_time.is_paused()
            && crossing
                .as_ref()
                .is_some_and(|crossing| crossing.is_active()));
    let mode = match *mode {
        MoveMode::Walk => "walk",
        MoveMode::Noclip => "noclip",
    };
    // `grounded` and the vertical velocity are the controller's state after the latest tick. A
    // body that keeps `grounded = 0` with a growing downward velocity is falling, not stepping.
    let (grounded, vertical_velocity) = walk.single().map_or((0, 0.0), |state| {
        (u8::from(state.grounded()), state.vertical_velocity())
    });
    let (b, f, c) = (body.translation, feet, view.translation);
    let written = writeln!(
        trace.out,
        "{},{:.6},{:.6},{ticks},{overstep:.4},{mode},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},\
         {:.3},{:.3},{:.3},{lag_y:.4},{weight:.4},{speed:.2},{:.5},{grounded},{vertical_velocity:.2},\
         {:.6},{}",
        trace.frame,
        time.elapsed_secs_f64(),
        time.delta_secs(),
        origin.x,
        origin.y,
        b.x,
        b.y,
        b.z,
        f.x,
        f.y,
        f.z,
        c.x,
        c.y,
        c.z,
        look.pitch,
        real.delta_secs(),
        u8::from(sampled),
    );
    trace.frame += 1;
    let flushed = if written.is_err() || trace.frame.is_multiple_of(FLUSH_EVERY_FRAMES) {
        trace.out.flush()
    } else {
        Ok(())
    };
    if let Err(error) = written.and(flushed) {
        // Report the first failure and stop: a CSV with missing rows would mislead the analysis.
        warn!(
            "walk camera trace: cannot write the trace after {} frames: {error}; the trace stops here",
            trace.frame
        );
        trace.failed = true;
        commands.remove_resource::<WalkCameraTrace>();
    }
}

#[cfg(test)]
mod tests {
    use super::super::{WalkIntent, headless};
    use super::*;

    /// `--walk-camera-trace <file>` makes the real systems write one row per rendered frame with the
    /// named columns, and a WALK row carries the frame time, the ticks run, the grounded flag and
    /// the vertical velocity. Without the option no file is written.
    #[test]
    fn the_trace_option_writes_one_row_per_frame_with_the_documented_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.csv");
        let mut app = headless::fixture_app_with(|app| {
            app.insert_resource(EngineConfig {
                walk_camera_trace: Some(path.clone()),
                ..EngineConfig::default()
            });
        });
        headless::place_player(&mut app, Vec3::new(0.0, 60.0, 850.0));
        for _ in 0..90 {
            app.update();
        }
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: 200.0,
            jump_pressed: false,
        });
        for _ in 0..60 {
            app.update();
        }
        drop(app);
        let text = std::fs::read_to_string(&path).expect("the trace file was written");
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().unwrap().split(',').collect();
        for column in [
            "frame_dt_s",
            "ticks",
            "grounded",
            "vertical_velocity",
            "feet_y",
            "camera_y",
            "lag_y",
            "weight",
            "real_dt_s",
            "spring_sampled",
        ] {
            assert!(
                header.contains(&column),
                "missing column {column}: {header:?}"
            );
        }
        let rows: Vec<Vec<&str>> = lines.map(|line| line.split(',').collect()).collect();
        assert!(
            rows.iter().all(|row| row.len() == header.len()),
            "ragged rows"
        );
        let column = |name: &str| header.iter().position(|c| *c == name).unwrap();
        let walking: Vec<_> = rows
            .iter()
            .filter(|row| row[column("mode")] == "walk")
            .collect();
        // The fixture's own startup frame is a NOCLIP row; every update after `place_player` is
        // one WALK row, none dropped.
        assert_eq!(
            rows.len() - walking.len(),
            1,
            "one startup row before WALK, got {} other rows",
            rows.len() - walking.len()
        );
        assert_eq!(
            walking.len(),
            150,
            "one WALK row for each of the 90 + 60 updates after place_player"
        );
        // The rows follow the body the player moves: it walked about 200 units forward in the
        // test, and the camera moves with it. A trace of an idle body would show one position.
        let range = |name: &str| {
            walking
                .iter()
                .map(|row| row[column(name)].parse::<f32>().unwrap())
                .fold((f32::MAX, f32::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)))
        };
        for name in ["body_x", "feet_x", "camera_x"] {
            let (low, high) = range(name);
            assert!(
                high - low > 100.0,
                "{name} did not follow the moving body: from {low} to {high}"
            );
        }
        let last = walking[walking.len() - 1];
        assert!(last[column("frame_dt_s")].parse::<f32>().unwrap() > 0.0);
        assert!(last[column("ticks")].parse::<u32>().is_ok());
        assert!(last[column("real_dt_s")].parse::<f32>().is_ok());
        assert_eq!(last[column("spring_sampled")], "1", "the follow system ran");
        assert_eq!(
            last[column("grounded")],
            "1",
            "standing on the fixture ground"
        );
        assert!(last[column("vertical_velocity")].parse::<f32>().is_ok());

        // Off by default: no option, no file.
        let other = dir.path().join("none.csv");
        let mut app = headless::fixture_app();
        for _ in 0..10 {
            app.update();
        }
        drop(app);
        assert!(!other.exists());
    }
}
