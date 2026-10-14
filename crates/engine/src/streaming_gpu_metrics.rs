//! Opt-in aggregate preparation observations. These are resource inventories, not
//! placement readiness, successful draws, collision coverage, or memory accounting.

use bevy::{
    pbr::{MaterialBindGroupAllocators, PreparedMaterial},
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        erased_render_asset::ErasedRenderAssets,
        mesh::{RenderMesh, RenderMeshBufferInfo, allocator::MeshAllocator},
        render_asset::RenderAssets,
        texture::GpuImage,
    },
};
use std::{
    ops::Range,
    sync::{Arc, Mutex},
    time::Instant,
};

pub(crate) const GPU_INVENTORY_INTERVAL_FRAMES: u64 = 8;

fn should_observe_inventory(source_main_frame: u64) -> bool {
    source_main_frame > 0
        && (source_main_frame == 1
            || source_main_frame.is_multiple_of(GPU_INVENTORY_INTERVAL_FRAMES))
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct GpuPreparationObservation {
    pub source_main_frame: u64,
    pub render_observation_seq: u64,
    pub render_available: bool,
    /// Collector CPU elapsed time, not GPU execution or a main-frame duration.
    pub collection_ms: Option<f64>,
    /// None means the resource needed to observe this count was unavailable.
    pub mesh_descriptors: Option<u64>,
    pub meshes_allocator_resident: Option<u64>,
    pub prepared_images: Option<u64>,
    pub prepared_material_records: Option<u64>,
    pub material_bind_groups_ready: Option<u64>,
}

#[derive(Default)]
struct BridgeState {
    requested_frame: Option<u64>,
    render_available: bool,
    latest: Option<GpuPreparationObservation>,
}

/// One outstanding request and one latest result; never retains asset handles.
#[derive(Resource, Clone, Default)]
pub struct StreamingGpuMetricsBridge(Arc<Mutex<BridgeState>>);

impl StreamingGpuMetricsBridge {
    pub fn request(&self, main_frame: u64) {
        let Ok(mut state) = self.0.lock() else { return };
        if state
            .requested_frame
            .is_some_and(|frame| main_frame < frame)
        {
            return;
        }
        state.requested_frame = Some(main_frame);
        if !state.render_available {
            state.latest = Some(GpuPreparationObservation {
                source_main_frame: main_frame,
                ..default()
            });
        }
    }

    pub fn latest(&self) -> Option<GpuPreparationObservation> {
        self.0.lock().ok()?.latest.clone()
    }

    fn requested_frame(&self) -> Option<u64> {
        self.0.lock().ok()?.requested_frame
    }

    fn publish(&self, observation: GpuPreparationObservation) {
        let Ok(mut state) = self.0.lock() else { return };
        // A pipelined render frame may legitimately lag the newest request. Reject
        // only observations older than the last published source frame/sequence.
        if state.latest.as_ref().is_some_and(|latest| {
            (
                observation.source_main_frame,
                observation.render_observation_seq,
            ) <= (latest.source_main_frame, latest.render_observation_seq)
        }) {
            return;
        }
        state.latest = Some(observation);
    }
}

pub struct StreamingGpuMetricsPlugin;

impl Plugin for StreamingGpuMetricsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StreamingGpuMetricsBridge>();
        let bridge = app.world().resource::<StreamingGpuMetricsBridge>().clone();
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            if let Ok(mut state) = bridge.0.lock() {
                state.render_available = true;
            }
            render_app
                .insert_resource(bridge)
                .init_resource::<ExtractedRequest>()
                .add_systems(ExtractSchedule, extract_request)
                .add_systems(
                    Render,
                    observe_preparation.after(RenderSystems::PrepareBindGroups),
                );
        }
    }
}

#[derive(Resource, Default)]
struct ExtractedRequest {
    source_main_frame: Option<u64>,
    sequence: u64,
}

fn extract_request(
    bridge: Extract<Res<StreamingGpuMetricsBridge>>,
    mut request: ResMut<ExtractedRequest>,
) {
    request.source_main_frame = bridge.requested_frame();
}

fn observe_preparation(
    bridge: Res<StreamingGpuMetricsBridge>,
    mut request: ResMut<ExtractedRequest>,
    meshes: Option<Res<RenderAssets<RenderMesh>>>,
    allocator: Option<Res<MeshAllocator>>,
    images: Option<Res<RenderAssets<GpuImage>>>,
    materials: Option<Res<ErasedRenderAssets<PreparedMaterial>>>,
    bind_groups: Option<Res<MaterialBindGroupAllocators>>,
) {
    let Some(source_main_frame) = request.source_main_frame else {
        return;
    };
    if !should_observe_inventory(source_main_frame) {
        return;
    }
    let collection_started = Instant::now();
    request.sequence = request.sequence.saturating_add(1);
    let mesh_descriptors = meshes.as_ref().map(|meshes| meshes.iter().count() as u64);
    let meshes_allocator_resident =
        meshes
            .as_ref()
            .zip(allocator.as_ref())
            .map(|(meshes, allocator)| {
                meshes
                    .iter()
                    .filter(|(id, mesh)| {
                        let indices = match mesh.buffer_info {
                            RenderMeshBufferInfo::Indexed { count, .. } => Some(count),
                            RenderMeshBufferInfo::NonIndexed => None,
                        };
                        resident_ranges_match(
                            mesh.vertex_count,
                            indices,
                            allocator.mesh_vertex_slice(id).map(|slice| slice.range),
                            allocator.mesh_index_slice(id).map(|slice| slice.range),
                        )
                    })
                    .count() as u64
            });
    let material_bind_groups_ready =
        materials
            .as_ref()
            .zip(bind_groups.as_ref())
            .map(|(materials, allocators)| {
                materials
                    .iter()
                    .filter(|(id, material)| {
                        allocators
                            .get(&id.type_id())
                            .and_then(|allocator| allocator.get(material.binding.group))
                            .is_some_and(|slab| slab.bind_group().is_some())
                    })
                    .count() as u64
            });
    let mut observation = GpuPreparationObservation {
        source_main_frame,
        render_observation_seq: request.sequence,
        render_available: true,
        collection_ms: None,
        mesh_descriptors,
        meshes_allocator_resident,
        prepared_images: images.as_ref().map(|images| images.iter().count() as u64),
        prepared_material_records: materials
            .as_ref()
            .map(|materials| materials.iter().count() as u64),
        material_bind_groups_ready,
    };
    if meshes.is_some() || images.is_some() || materials.is_some() {
        observation.collection_ms = Some(collection_started.elapsed().as_secs_f64() * 1000.0);
    }
    bridge.publish(observation);
}

// Matches #201's residency predicate without changing the retry module's API.
fn resident_ranges_match(
    vertex_count: u32,
    indexed_count: Option<u32>,
    vertex_range: Option<Range<u32>>,
    index_range: Option<Range<u32>>,
) -> bool {
    if !vertex_range
        .and_then(|range| range.end.checked_sub(range.start))
        .is_some_and(|count| count >= vertex_count)
    {
        return false;
    }
    match indexed_count {
        None | Some(0) => true,
        Some(count) => index_range
            .and_then(|range| range.end.checked_sub(range.start))
            .is_some_and(|available| available >= count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_requests_report_unavailable_not_zero_ready() {
        let mut app = App::new();
        app.add_plugins(StreamingGpuMetricsPlugin);
        let bridge = app.world().resource::<StreamingGpuMetricsBridge>();
        bridge.request(7);
        let result = bridge.latest().unwrap();
        assert_eq!(result.source_main_frame, 7);
        assert!(!result.render_available);
        assert_eq!(result.collection_ms, None);
        assert_eq!(result.meshes_allocator_resident, None);
        assert_eq!(result.material_bind_groups_ready, None);
    }

    #[test]
    fn bridge_accepts_lagging_pipeline_but_rejects_stale_results() {
        let bridge = StreamingGpuMetricsBridge::default();
        bridge.0.lock().unwrap().render_available = true;
        bridge.request(10);
        bridge.publish(GpuPreparationObservation {
            source_main_frame: 9,
            render_observation_seq: 2,
            render_available: true,
            ..default()
        });
        bridge.publish(GpuPreparationObservation {
            source_main_frame: 8,
            render_observation_seq: 3,
            ..default()
        });
        bridge.publish(GpuPreparationObservation {
            source_main_frame: 9,
            render_observation_seq: 1,
            ..default()
        });
        assert_eq!(bridge.latest().unwrap().source_main_frame, 9);
        assert_eq!(bridge.latest().unwrap().render_observation_seq, 2);
        bridge.request(6);
        assert_eq!(bridge.requested_frame(), Some(10));
    }

    #[test]
    fn descriptors_require_complete_allocator_ranges() {
        assert!(!resident_ranges_match(4, Some(6), None, Some(0..6)));
        assert!(!resident_ranges_match(4, Some(6), Some(0..4), None));
        assert!(!resident_ranges_match(4, Some(6), Some(0..4), Some(0..5)));
        let invalid_start = 4;
        let invalid_end = 2;
        assert!(!resident_ranges_match(
            4,
            None,
            Some(invalid_start..invalid_end),
            None
        ));
        assert!(resident_ranges_match(
            4,
            Some(6),
            Some(10..14),
            Some(20..26)
        ));
        assert!(resident_ranges_match(4, None, Some(0..4), None));
    }

    #[test]
    fn absent_inventory_differs_from_observed_empty_inventory() {
        let mut app = App::new();
        app.init_resource::<StreamingGpuMetricsBridge>()
            .insert_resource(ExtractedRequest {
                source_main_frame: Some(8),
                sequence: 0,
            })
            .add_systems(Update, observe_preparation);
        app.update();
        let absent = app
            .world()
            .resource::<StreamingGpuMetricsBridge>()
            .latest()
            .unwrap();
        assert!(absent.render_available);
        assert_eq!(absent.mesh_descriptors, None);
        assert_eq!(absent.prepared_images, None);
        assert_eq!(absent.collection_ms, None);
        app.init_resource::<RenderAssets<RenderMesh>>()
            .init_resource::<RenderAssets<GpuImage>>();
        app.update();
        let empty = app
            .world()
            .resource::<StreamingGpuMetricsBridge>()
            .latest()
            .unwrap();
        assert_eq!(empty.mesh_descriptors, Some(0));
        assert_eq!(empty.prepared_images, Some(0));
        assert_eq!(empty.meshes_allocator_resident, None);
        assert_eq!(empty.material_bind_groups_ready, None);
        assert_eq!(empty.render_observation_seq, 2);
    }

    #[test]
    fn inventory_cadence_samples_first_frame_and_eight_frame_multiples() {
        assert!(!should_observe_inventory(0));
        assert!(should_observe_inventory(1));
        assert!(!should_observe_inventory(2));
        assert!(!should_observe_inventory(7));
        assert!(should_observe_inventory(8));
        assert!(!should_observe_inventory(9));
        assert!(should_observe_inventory(16));
    }
}
