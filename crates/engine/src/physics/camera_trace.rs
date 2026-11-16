//! Opt-in per-frame trace of the WALK camera (`--walk-camera-trace <file>`). Off by default.
//!
//! One CSV row per rendered frame: the frame time, how many fixed physics ticks ran in the frame,
//! the body position of the latest tick, the blended position that feeds the follow spring
//! ([`super::WalkViewPose`]), the camera position, and the spring's lag, weight and speed. It is a
//! diagnostic for a real stair climb: the headless stair-jitter test in `stair_tests.rs` measures
//! the same quantities on a fixture flight. Positions are in render space (game units, Y up), so
//! the floating origin is written too.

use super::{
    LookIntent, MoveMode, MovementTuning, PlayerBody, WalkCameraFollowState, WalkViewPose,
};
use crate::{config::EngineConfig, world::components::StreamingCamera};
use bevy::prelude::*;
use std::{
    fs::File,
    io::{BufWriter, Write},
};

const HEADER: &str = "frame,elapsed_s,frame_dt_s,ticks,overstep,mode,origin_x,origin_y,\
body_x,body_y,body_z,feet_x,feet_y,feet_z,camera_x,camera_y,camera_z,lag_y,weight,spring_speed,pitch";

/// Flush the file after this many rows, so a run that is killed keeps most of its rows.
const FLUSH_EVERY_FRAMES: u64 = 60;

/// The open trace file and the per-frame tick count.
#[derive(Resource)]
pub(super) struct WalkCameraTrace {
    out: BufWriter<File>,
    frame: u64,
    ticks: u32,
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
#[allow(clippy::too_many_arguments)]
pub(super) fn write_walk_camera_trace_row(
    mut trace: ResMut<WalkCameraTrace>,
    time: Res<Time>,
    fixed: Res<Time<Fixed>>,
    mode: Res<MoveMode>,
    tuning: Res<MovementTuning>,
    look: Res<LookIntent>,
    pose: Res<WalkViewPose>,
    follow: Res<WalkCameraFollowState>,
    render_origin: Option<Res<crate::streaming::RenderOrigin>>,
    player: Query<&Transform, (With<PlayerBody>, Without<StreamingCamera>)>,
    camera: Query<&Transform, With<StreamingCamera>>,
) {
    let trace = &mut *trace;
    let ticks = std::mem::take(&mut trace.ticks);
    let (Ok(body), Ok(view)) = (player.single(), camera.single()) else {
        return;
    };
    let overstep = fixed.overstep_fraction();
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
    let mode = match *mode {
        MoveMode::Walk => "walk",
        MoveMode::Noclip => "noclip",
    };
    let (b, f, c) = (body.translation, feet, view.translation);
    let written = writeln!(
        trace.out,
        "{},{:.6},{:.6},{ticks},{overstep:.4},{mode},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},\
         {:.3},{:.3},{:.3},{lag_y:.4},{weight:.4},{speed:.2},{:.5}",
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
    );
    trace.frame += 1;
    if written.is_err() || trace.frame.is_multiple_of(FLUSH_EVERY_FRAMES) {
        let _ = trace.out.flush();
    }
}
