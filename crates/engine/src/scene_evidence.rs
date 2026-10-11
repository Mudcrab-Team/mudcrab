//! Opt-in camera and queue records for matched profiling, with optional handoff images.
//! Image requests are asynchronous; their request pose is not an exact captured-frame pose.
use crate::{
    config::EngineConfig,
    streaming::{RenderOrigin, StreamingMetrics},
    world::components::{CELL_SIZE, StreamingCamera},
};
use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
    transform::TransformSystems,
};
use serde::Serialize;
use std::{
    fs,
    io::{BufWriter, Write},
};

const MAX_RECORDS: u64 = 20_000;

pub(crate) struct SceneEvidencePlugin;

impl Plugin for SceneEvidencePlugin {
    fn build(&self, app: &mut App) {
        if std::env::var("MUDCRAB_PROFILE_SCENE_EVIDENCE").as_deref() != Ok("1") {
            return;
        }
        let Some(root) = app
            .world()
            .resource::<EngineConfig>()
            .profile_output_dir
            .clone()
        else {
            warn!("scene evidence requires --profile-output");
            return;
        };
        let opened = fs::create_dir_all(&root).and_then(|()| {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join("scene-observations.ndjson"))
        });
        match opened {
            Ok(file) => {
                app.insert_resource(SceneEvidence {
                    writer: BufWriter::new(file),
                    frame: 0,
                    records: 0,
                    previous_position: None,
                    previous_direction: None,
                    previous_cell: None,
                    failed: false,
                    images: (std::env::var("MUDCRAB_PROFILE_HANDOFF_IMAGES").as_deref() == Ok("1"))
                        .then_some(root),
                })
                .add_systems(PostUpdate, record_scene.after(TransformSystems::Propagate));
            }
            Err(error) => error!(%error, "cannot open scene evidence; capture is incomplete"),
        }
    }
}

#[derive(Resource)]
struct SceneEvidence {
    writer: BufWriter<fs::File>,
    frame: u64,
    records: u64,
    previous_position: Option<Vec3>,
    previous_direction: Option<Vec3>,
    previous_cell: Option<IVec2>,
    failed: bool,
    images: Option<std::path::PathBuf>,
}

impl Drop for SceneEvidence {
    fn drop(&mut self) {
        if let Err(error) = self.writer.flush() {
            error!(%error, "scene evidence flush failed");
        }
        info!(
            records = self.records,
            failed = self.failed,
            capped = self.records >= MAX_RECORDS,
            "scene evidence complete; image poses are request poses"
        );
    }
}

#[derive(Serialize)]
struct SceneRecord {
    request_frame: u64,
    elapsed_seconds: f64,
    camera_creation: [f32; 3],
    rotation_runtime_xyzw: [f32; 4],
    vertical_fov_radians: Option<f32>,
    render_origin: [i32; 2],
    camera_cell: [i32; 2],
    crossed_cell: bool,
    reversed: bool,
    resident_cells: usize,
    pending_lod_queries: usize,
    pending_lod_chunks: usize,
    failed_lod_queries: u64,
    failed_lod_chunks: u64,
    loading_cells: usize,
    active_requests: usize,
    pending_asset_instances: usize,
    pending_surface_instances: usize,
    arming_queue_depth: usize,
    retiring_cells: usize,
    origin_rebases: u64,
    commit_budget_violations: u64,
    streaming_invariant_failures: u64,
    image_request: Option<String>,
}

fn absolute_creation_position(transform: &Transform, origin: IVec2) -> Vec3 {
    let position = transform.translation
        + Vec3::new(
            origin.x as f32 * CELL_SIZE,
            0.0,
            -(origin.y as f32) * CELL_SIZE,
        );
    Vec3::from_array(shared::coordinates::runtime_to_creation_vector(
        position.to_array(),
    ))
}

fn record_scene(
    mut commands: Commands,
    mut evidence: ResMut<SceneEvidence>,
    time: Res<Time>,
    origin: Res<RenderOrigin>,
    streaming: Res<StreamingMetrics>,
    camera: Query<(&Transform, &Projection), With<StreamingCamera>>,
) {
    evidence.frame += 1;
    if evidence.failed || evidence.records >= MAX_RECORDS {
        return;
    }
    let Ok((transform, projection)) = camera.single() else {
        return;
    };
    let position = absolute_creation_position(transform, origin.0);
    let cell = IVec2::new(
        (position.x / CELL_SIZE).floor() as i32,
        (position.y / CELL_SIZE).floor() as i32,
    );
    let displacement = evidence
        .previous_position
        .map(|previous| position - previous);
    let direction = displacement.and_then(Vec3::try_normalize);
    let reversed = direction
        .zip(evidence.previous_direction)
        .is_some_and(|(current, previous)| current.dot(previous) < -0.5);
    let crossed_cell = evidence.previous_cell != Some(cell);
    evidence.previous_position = Some(position);
    evidence.previous_cell = Some(cell);
    if direction.is_some() {
        evidence.previous_direction = direction;
    }
    if !crossed_cell && !reversed && !evidence.frame.is_multiple_of(15) {
        return;
    }
    let image = evidence
        .images
        .as_ref()
        .filter(|_| crossed_cell || reversed)
        .map(|root| root.join(format!("handoff-{:06}.png", evidence.frame)));
    let record = SceneRecord {
        request_frame: evidence.frame,
        elapsed_seconds: time.elapsed_secs_f64(),
        camera_creation: position.to_array(),
        rotation_runtime_xyzw: transform.rotation.to_array(),
        vertical_fov_radians: match projection {
            Projection::Perspective(p) => Some(p.fov),
            _ => None,
        },
        render_origin: origin.0.to_array(),
        camera_cell: cell.to_array(),
        crossed_cell,
        reversed,
        resident_cells: streaming.resident_cells,
        pending_lod_queries: streaming.pending_lod_queries,
        pending_lod_chunks: streaming.pending_lod_chunks,
        failed_lod_queries: streaming.failed_lod_queries,
        failed_lod_chunks: streaming.failed_lod_chunks,
        loading_cells: streaming.loading_cells,
        active_requests: streaming.active_requests,
        pending_asset_instances: streaming.pending_asset_instances,
        pending_surface_instances: streaming.pending_surface_instances,
        arming_queue_depth: streaming.arming_queue_depth,
        retiring_cells: streaming.retiring_cells,
        origin_rebases: streaming.origin_rebases,
        commit_budget_violations: streaming.commit_budget_violations,
        streaming_invariant_failures: streaming.streaming_invariant_failures,
        image_request: image
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
    };
    let written = serde_json::to_writer(&mut evidence.writer, &record)
        .map_err(std::io::Error::other)
        .and_then(|()| evidence.writer.write_all(b"\n"))
        .and_then(|()| evidence.writer.flush());
    if let Err(error) = written {
        evidence.failed = true;
        error!(%error, "scene evidence write failed; capture is incomplete");
        return;
    }
    evidence.records += 1;
    if let Some(path) = image {
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn camera_evidence_survives_rebase_and_negative_cells() {
        let before =
            absolute_creation_position(&Transform::from_xyz(22528.0, 6000.0, 47104.0), IVec2::ZERO);
        let after = absolute_creation_position(
            &Transform::from_xyz(2048.0, 6000.0, 2048.0),
            IVec2::new(5, -11),
        );
        assert_eq!(before, after);
        assert_eq!(before.to_array(), [22528.0, -47104.0, 6000.0]);
    }
}
