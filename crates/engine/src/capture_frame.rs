//! Receipts tied to screenshot entities, rather than callback arrival frames.
//!
//! Bevy 0.19 extracts each new Screenshot once and marks it Capturing in that same
//! extraction. A ticket is sampled in that extraction and finalized after rendering;
//! ScreenshotCaptured for the identical entity certifies completion of its readback.
//! Automated runners serialize primary-window requests and keep the pose frozen.
use crate::{
    shots::shot_camera_translation, streaming::RenderOrigin, world::components::StreamingCamera,
};
use bevy::{
    diagnostic::FrameCount,
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        mesh::RenderMesh,
        render_asset::{ExtractedAssets, RenderAssets, prepare_assets},
        texture::GpuImage,
        view::screenshot::Screenshot,
    },
};
use serde::Serialize;
use std::sync::{Arc, Mutex};

#[derive(Clone, Component, Default)]
pub(crate) struct CaptureTicket(pub Arc<Mutex<Option<CaptureFrame>>>);

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CaptureFrame {
    pub main_frame: u32,
    pub render_frame: u64,
    pub camera_creation: [f32; 3],
    pub rotation_runtime_xyzw: [f32; 4],
    pub vertical_fov_radians: Option<f32>,
    pub render_origin: [i32; 2],
    pub pending_specializations: Option<usize>,
    pub work: crate::shots::SettleCounts,
    pub terrain_coverage: Vec<serde_json::Value>,
    pub terrain_gauges: std::collections::BTreeMap<String, f64>,
}

impl CaptureTicket {
    pub(crate) fn receipt(&self) -> Option<CaptureFrame> {
        self.0.lock().unwrap().clone()
    }
}

pub(crate) struct CaptureFramePlugin;
impl Plugin for CaptureFramePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RetryReadiness>()
            .init_resource::<TransferReadiness>();
        let bridge = app.world().resource::<RetryReadiness>().clone();
        let transfers = app.world().resource::<TransferReadiness>().clone();
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render
                .insert_resource(bridge)
                .insert_resource(transfers)
                .init_resource::<PendingRenderTransfers>()
                .init_resource::<ExtractedCaptures>()
                .add_systems(ExtractSchedule, extract_capture_frames)
                .add_systems(
                    Render,
                    (
                        observe_render_transfers
                            .before(prepare_assets::<RenderMesh>)
                            .before(prepare_assets::<GpuImage>),
                        publish_render_transfers
                            .after(RenderSystems::Queue)
                            .before(RenderSystems::Cleanup),
                        finalize_capture_frames.in_set(RenderSystems::Cleanup),
                    ),
                );
        }
    }
}

/// Updated after specialization/queue observation, and explicitly unavailable until observed.
#[derive(Resource, Clone, Default)]
pub(crate) struct RetryReadiness(pub Arc<Mutex<Option<usize>>>);

#[derive(Resource, Default)]
struct ExtractedCaptures {
    render_frame: u64,
    captures: Vec<(CaptureTicket, CaptureFrame)>,
}

#[allow(clippy::too_many_arguments)]
fn extract_capture_frames(
    mut extracted: ResMut<ExtractedCaptures>,
    frame: Extract<Res<FrameCount>>,
    origin: Extract<Res<RenderOrigin>>,
    camera: Extract<Query<(&Transform, &Projection), With<StreamingCamera>>>,
    tickets: Extract<Query<&CaptureTicket, With<Screenshot>>>,
    streaming: Extract<Res<crate::streaming::StreamingMetrics>>,
    renderer: Extract<Res<crate::render::RendererMetrics>>,
    profiler: Extract<Res<crate::profiling::ProfilingState>>,
    uploads: Extract<Res<crate::terrain_upload::TerrainMeshUploadReadiness>>,
    windows: Extract<Query<Entity, With<bevy::window::PrimaryWindow>>>,
    surfaces: Extract<Query<(&crate::streaming::TerrainCoverage, &InheritedVisibility)>>,
    batches: Extract<Query<&crate::streaming::lod::terrain_batching::LodTerrainBatches>>,
) {
    extracted.render_frame += 1;
    extracted.captures.clear();
    if windows.single().is_err() || tickets.iter().all(|ticket| ticket.receipt().is_some()) {
        return;
    }
    let Ok((transform, projection)) = camera.single() else {
        return;
    };
    // Reuse the same origin conversion as the pose driver; origin is an absolute cell offset.
    let offset = shot_camera_translation([0.0; 3], origin.0);
    let absolute = transform.translation - offset;
    let pose = shared::coordinates::runtime_to_creation_vector(absolute.to_array());
    let mut work = crate::shots::SettleCounts::read(
        &streaming,
        &renderer,
        default(),
        renderer.final_path_active(),
        0,
    );
    work.outstanding_terrain_uploads = uploads.outstanding();
    work.pending_batch_cpu = profiler
        .gauge("lod/pending_terrain_batch_chunks")
        .unwrap_or(0.0) as usize;
    work.pending_batch_initial = profiler
        .gauge("lod/pending_initial_terrain_upload_chunks")
        .unwrap_or(0.0) as usize;
    work.pending_batch_selection = profiler
        .gauge("lod/pending_terrain_selection_uploads")
        .unwrap_or(0.0) as usize;
    let record = CaptureFrame {
        main_frame: frame.0,
        render_frame: extracted.render_frame,
        camera_creation: pose,
        rotation_runtime_xyzw: transform.rotation.to_array(),
        vertical_fov_radians: match projection {
            Projection::Perspective(p) => Some(p.fov),
            _ => None,
        },
        render_origin: origin.0.to_array(),
        pending_specializations: None,
        work,
        terrain_gauges: profiler.terrain_gauges(),
        terrain_coverage: surfaces
            .iter()
            .filter(|(_, visible)| visible.get())
            .map(|(coverage, _)| serde_json::json!({"source": coverage.receipt()}))
            .chain(batches.iter().flat_map(|batch| batch.coverage_receipt()))
            .collect(),
    };
    for ticket in &tickets {
        if ticket.receipt().is_none() {
            extracted.captures.push((ticket.clone(), record.clone()));
        }
    }
}

fn finalize_capture_frames(
    mut extracted: ResMut<ExtractedCaptures>,
    retries: Res<RetryReadiness>,
    transfers: Res<TransferReadiness>,
) {
    let pending = *retries.0.lock().unwrap();
    for (ticket, mut record) in extracted.captures.drain(..) {
        record.pending_specializations = pending;
        record.work.pending_specializations = pending;
        record.work.pending_render_transfers = *transfers.0.lock().unwrap();
        let mut receipt = ticket.0.lock().unwrap();
        // Never overwrite the render frame when asynchronous readback takes more frames.
        if receipt.is_none() {
            *receipt = Some(record);
        }
    }
}

pub(crate) fn validate_receipt(
    ticket: &CaptureTicket,
    requested: [f32; 3],
) -> Result<CaptureFrame, String> {
    let record = ticket.receipt().ok_or("no captured render-frame receipt")?;
    if record.camera_creation != requested {
        return Err("captured pose differs from requested pose".into());
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_capture_has_no_receipt() {
        assert!(validate_receipt(&CaptureTicket::default(), [0.0; 3]).is_err());
    }
    #[test]
    fn render_receipt_is_immutable_across_callback_delay() {
        let ticket = CaptureTicket::default();
        let record = CaptureFrame {
            main_frame: 3,
            render_frame: 2,
            camera_creation: [1.0; 3],
            rotation_runtime_xyzw: [0.0; 4],
            vertical_fov_radians: None,
            render_origin: [0; 2],
            pending_specializations: Some(0),
            work: default(),
            terrain_gauges: default(),
            terrain_coverage: default(),
        };
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(ExtractedCaptures {
            render_frame: 2,
            captures: vec![(ticket.clone(), record.clone())],
        });
        world.insert_resource(RetryReadiness(Arc::new(Mutex::new(Some(0)))));
        world.insert_resource(TransferReadiness(Arc::new(Mutex::new(Some(0)))));
        world.run_system_once(finalize_capture_frames).unwrap();
        let mut later = record;
        later.main_frame = 99;
        later.render_frame = 98;
        world
            .resource_mut::<ExtractedCaptures>()
            .captures
            .push((ticket.clone(), later));
        world.run_system_once(finalize_capture_frames).unwrap();
        assert_eq!(validate_receipt(&ticket, [1.0; 3]).unwrap().render_frame, 2);
        assert!(validate_receipt(&ticket, [0.0; 3]).is_err());
    }
}

// Track asset generations before Bevy drains its extracted lists. Updated generations remove
// their old descriptor before preparation, so a retained descriptor cannot acknowledge old bytes.
#[derive(Resource, Clone, Default)]
pub(crate) struct TransferReadiness(pub Arc<Mutex<Option<usize>>>);

#[derive(Resource, Default)]
struct PendingRenderTransfers {
    meshes: std::collections::HashSet<bevy::asset::AssetId<Mesh>>,
    images: std::collections::HashSet<bevy::asset::AssetId<Image>>,
}

fn observe_render_transfers(
    mut pending: ResMut<PendingRenderTransfers>,
    meshes: Res<ExtractedAssets<RenderMesh>>,
    images: Res<ExtractedAssets<GpuImage>>,
) {
    for id in &meshes.removed {
        pending.meshes.remove(id);
    }
    for id in &images.removed {
        pending.images.remove(id);
    }
    pending
        .meshes
        .extend(meshes.extracted.iter().map(|(id, _)| *id));
    pending
        .images
        .extend(images.extracted.iter().map(|(id, _)| *id));
}

fn publish_render_transfers(
    mut pending: ResMut<PendingRenderTransfers>,
    meshes: Res<RenderAssets<RenderMesh>>,
    images: Res<RenderAssets<GpuImage>>,
    bridge: Res<TransferReadiness>,
) {
    pending.meshes.retain(|id| meshes.get(*id).is_none());
    pending.images.retain(|id| images.get(*id).is_none());
    *bridge.0.lock().unwrap() = Some(pending.meshes.len() + pending.images.len());
}
