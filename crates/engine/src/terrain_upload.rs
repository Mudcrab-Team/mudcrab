//! Acknowledgments for newly created, immutable terrain mesh assets.
//!
//! The render world checks requested IDs after mesh preparation. An acknowledgment means Bevy
//! has prepared that mesh asset and allocated its required buffer ranges for submission. It does
//! not mean the GPU finished drawing it or that a later mutation of the same asset ID has been
//! uploaded. Terrain replacements must use a
//! new mesh ID for each generation and retain their previous geometry until that ID is ready.

use bevy::{
    asset::AssetId,
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        mesh::{RenderMesh, RenderMeshBufferInfo, allocator::MeshAllocator},
        render_asset::RenderAssets,
    },
};
use std::{
    collections::HashSet,
    ops::Range,
    sync::{Arc, Mutex, MutexGuard},
};

pub struct TerrainUploadPlugin;

impl Plugin for TerrainUploadPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerrainMeshUploadReadiness>();
        let bridge = app.world().resource::<TerrainMeshUploadReadiness>().clone();
        bridge.state().render_backend = false;
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            bridge.state().render_backend = true;
            render_app.insert_resource(bridge).add_systems(
                Render,
                acknowledge_prepared_meshes.after(RenderSystems::PrepareMeshes),
            );
        }
    }
}

#[derive(Default)]
struct UploadState {
    render_backend: bool,
    pending: HashSet<AssetId<Mesh>>,
    ready: HashSet<AssetId<Mesh>>,
}

/// Shared by the main and render worlds. This resource does not retain asset handles.
///
/// With no render backend, requests remain pending. Callers keep the original rendering path;
/// tests can explicitly acknowledge their requested IDs without constructing a GPU device.
#[derive(Clone, Default, Resource)]
pub(crate) struct TerrainMeshUploadReadiness(Arc<Mutex<UploadState>>);

impl TerrainMeshUploadReadiness {
    /// Request initial preparation of a new immutable asset ID. Duplicate requests are harmless.
    pub(crate) fn request(&self, id: AssetId<Mesh>) {
        let mut state = self.state();
        if !state.ready.contains(&id) {
            state.pending.insert(id);
        }
    }

    pub(crate) fn outstanding(&self) -> usize {
        let state = self.state();
        state.pending.len() + state.ready.len()
    }

    pub(crate) fn is_ready(&self, id: AssetId<Mesh>) -> bool {
        self.state().ready.contains(&id)
    }

    pub(crate) fn has_render_backend(&self) -> bool {
        self.state().render_backend
    }

    /// Whether an acknowledgment is waiting for its caller to activate or retire that generation.
    pub(crate) fn has_ready_uploads(&self) -> bool {
        !self.state().ready.is_empty()
    }

    /// Retire both a pending request and its acknowledgment when its mesh generation is discarded.
    pub(crate) fn forget(&self, id: AssetId<Mesh>) {
        self.cancel(id);
    }

    pub(crate) fn cancel(&self, id: AssetId<Mesh>) {
        let mut state = self.state();
        state.pending.remove(&id);
        state.ready.remove(&id);
    }

    fn state(&self) -> MutexGuard<'_, UploadState> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // Hold the lock through checking and acknowledgment so cancellation cannot race with a
    // snapshot of the request list and publish readiness for a retired generation afterward.
    fn acknowledge_requested_with(&self, mut is_prepared: impl FnMut(AssetId<Mesh>) -> bool) {
        let mut state = self.state();
        if state.pending.is_empty() {
            return;
        }
        let UploadState { pending, ready, .. } = &mut *state;
        pending.retain(|&id| {
            if is_prepared(id) {
                ready.insert(id);
                false
            } else {
                true
            }
        });
    }

    /// Simulate one requested mesh's initial preparation in a headless test harness.
    #[cfg(test)]
    pub(crate) fn acknowledge(&self, id: AssetId<Mesh>) -> bool {
        let mut state = self.state();
        if !state.pending.remove(&id) {
            return false;
        }
        state.ready.insert(id);
        true
    }

    /// Enable the render path in a test harness; requests still require explicit acknowledgments.
    #[cfg(test)]
    pub(crate) fn enable_test_render_backend(&self) {
        self.state().render_backend = true;
    }
}

fn acknowledge_prepared_meshes(
    bridge: Res<TerrainMeshUploadReadiness>,
    meshes: Option<Res<RenderAssets<RenderMesh>>>,
    allocator: Option<Res<MeshAllocator>>,
) {
    let (Some(meshes), Some(allocator)) = (meshes, allocator) else {
        return;
    };
    bridge.acknowledge_requested_with(|id| {
        let Some(mesh) = meshes.get(id) else {
            return false;
        };
        prepared_mesh_is_allocated(
            mesh,
            allocator.mesh_vertex_slice(&id).map(|slice| slice.range),
            allocator.mesh_index_slice(&id).map(|slice| slice.range),
        )
    });
}

/// Descriptor preparation alone does not prove that DrawMesh can find its buffers. These ranges
/// come from MeshAllocator's resident allocations after PrepareAssets and PrepareMeshes. Queue
/// submission still orders their data uploads; this check does not fence GPU execution.
fn prepared_mesh_is_allocated(
    mesh: &RenderMesh,
    vertex_range: Option<Range<u32>>,
    index_range: Option<Range<u32>>,
) -> bool {
    let vertex_count = vertex_range.and_then(|range| range.end.checked_sub(range.start));
    if vertex_count.is_none_or(|count| count < mesh.vertex_count) {
        return false;
    }
    match mesh.buffer_info {
        // An empty selected mask needs no index elements. Its retained vertex streams still need
        // a resident allocation, and the caller keeps its empty batch hidden.
        RenderMeshBufferInfo::Indexed { count: 0, .. } => true,
        RenderMeshBufferInfo::Indexed { count, .. } => index_range
            .and_then(|range| range.end.checked_sub(range.start))
            .is_some_and(|allocated| allocated >= count),
        RenderMeshBufferInfo::NonIndexed => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        app::SubApp,
        asset::AssetPlugin,
        ecs::system::RunSystemOnce,
        mesh::{BaseMeshPipelineKey, MeshVertexBufferLayouts, PrimitiveTopology},
        render::render_resource::IndexFormat,
    };

    fn ids<const N: usize>() -> [AssetId<Mesh>; N] {
        let mut meshes = Assets::<Mesh>::default();
        std::array::from_fn(|_| meshes.add(Cuboid::default()).id())
    }

    fn render_mesh(vertex_count: u32, index_count: Option<u32>) -> RenderMesh {
        let mesh = Mesh::from(Cuboid::default());
        RenderMesh {
            vertex_count,
            aabb_center: Vec3::ZERO,
            buffer_info: match index_count {
                Some(count) => RenderMeshBufferInfo::Indexed {
                    count,
                    index_format: IndexFormat::Uint32,
                },
                None => RenderMeshBufferInfo::NonIndexed,
            },
            key_bits: BaseMeshPipelineKey::from_primitive_topology_and_strip_index(
                PrimitiveTopology::TriangleList,
                None,
            ),
            layout: mesh.get_mesh_vertex_buffer_layout(&mut MeshVertexBufferLayouts::default()),
        }
    }

    #[test]
    fn indexed_readiness_requires_sufficient_resident_vertex_and_index_ranges() {
        let mesh = render_mesh(4, Some(6));
        for (vertices, indices) in [
            (None, Some(40..46)),
            (Some(20..23), Some(40..46)),
            (Some(Range { start: 24, end: 20 }), Some(40..46)),
            (Some(20..24), None),
            (Some(20..24), Some(40..45)),
            (Some(20..24), Some(Range { start: 46, end: 40 })),
        ] {
            assert!(!prepared_mesh_is_allocated(&mesh, vertices, indices));
        }
        assert!(prepared_mesh_is_allocated(
            &mesh,
            Some(20..24),
            Some(40..46)
        ));
        assert!(prepared_mesh_is_allocated(
            &mesh,
            Some(20..25),
            Some(40..47)
        ));
    }

    #[test]
    fn empty_indices_and_nonindexed_meshes_still_require_vertex_residency() {
        for mesh in [render_mesh(4, Some(0)), render_mesh(4, None)] {
            assert!(!prepared_mesh_is_allocated(&mesh, None, None));
            assert!(!prepared_mesh_is_allocated(&mesh, Some(20..23), None));
            assert!(prepared_mesh_is_allocated(&mesh, Some(20..24), None));
        }
        assert!(!prepared_mesh_is_allocated(
            &render_mesh(0, Some(0)),
            None,
            None
        ));
    }

    #[test]
    fn requested_mesh_stays_pending_until_its_required_ranges_arrive() {
        let bridge = TerrainMeshUploadReadiness::default();
        let [id, unrelated] = ids();
        let mesh = render_mesh(4, Some(6));
        bridge.request(id);
        for (vertices, indices) in [
            (None, None),
            (Some(20..24), None),
            (Some(20..24), Some(40..45)),
        ] {
            bridge.acknowledge_requested_with(|requested| {
                assert_eq!(requested, id);
                prepared_mesh_is_allocated(&mesh, vertices.clone(), indices.clone())
            });
            assert!(!bridge.is_ready(id));
            assert!(!bridge.has_ready_uploads());
        }
        bridge.acknowledge_requested_with(|requested| {
            assert_eq!(requested, id);
            prepared_mesh_is_allocated(&mesh, Some(20..24), Some(40..46))
        });
        assert!(bridge.is_ready(id));
        assert!(!bridge.is_ready(unrelated));
        bridge.forget(id);
        assert!(!bridge.is_ready(id));
        assert!(!bridge.acknowledge(id));
    }

    #[test]
    fn requests_acknowledge_once_and_keep_readiness_until_retired() {
        let bridge = TerrainMeshUploadReadiness::default();
        let [id] = ids();
        bridge.request(id);
        bridge.request(id);
        assert!(!bridge.is_ready(id));
        assert!(!bridge.has_ready_uploads());
        assert!(bridge.acknowledge(id));
        assert!(!bridge.acknowledge(id));
        assert!(bridge.is_ready(id));
        assert!(bridge.has_ready_uploads());
        bridge.request(id);
        let mut checked = 0;
        bridge.acknowledge_requested_with(|_| {
            checked += 1;
            true
        });
        assert_eq!(checked, 0, "ready IDs are removed from render-world checks");
        assert!(bridge.is_ready(id));
        bridge.forget(id);
        assert!(!bridge.is_ready(id));
        assert!(!bridge.has_ready_uploads());
        assert!(!bridge.acknowledge(id));
    }

    #[test]
    fn canceled_or_unrequested_ids_cannot_publish_stale_readiness() {
        let bridge = TerrainMeshUploadReadiness::default();
        let [old, replacement, unrelated] = ids();
        bridge.request(old);
        bridge.cancel(old);
        bridge.request(replacement);
        bridge.acknowledge_requested_with(|id| id == old || id == unrelated);
        assert!(!bridge.is_ready(old));
        assert!(!bridge.is_ready(replacement));
        assert!(!bridge.is_ready(unrelated));
        assert!(!bridge.acknowledge(old));
        assert!(!bridge.acknowledge(unrelated));
        assert!(bridge.acknowledge(replacement));
        assert!(!bridge.is_ready(old));
        assert!(bridge.is_ready(replacement));
    }

    #[test]
    fn recycling_an_asset_slot_does_not_reuse_the_previous_generation_acknowledgment() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>();
        let handle = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::default());
        let old = handle.id();
        drop(handle);
        // Bevy's asset tracking recycles the dropped asset slot with an incremented generation.
        app.update();
        let replacement = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::default())
            .id();
        assert_ne!(old, replacement);
        let bridge = TerrainMeshUploadReadiness::default();
        bridge.request(old);
        assert!(bridge.acknowledge(old));
        bridge.request(replacement);
        bridge.acknowledge_requested_with(|id| id == old);
        assert!(bridge.is_ready(old));
        assert!(!bridge.is_ready(replacement));
        bridge.forget(old);
        assert!(bridge.acknowledge(replacement));
        assert!(!bridge.is_ready(old));
        assert!(bridge.is_ready(replacement));
    }

    #[test]
    fn render_checks_visit_only_pending_requested_ids() {
        let bridge = TerrainMeshUploadReadiness::default();
        let [prepared, delayed, unrelated] = ids();
        let available = HashSet::from([prepared, unrelated]);
        bridge.request(prepared);
        bridge.request(delayed);
        let mut checked = HashSet::new();
        bridge.acknowledge_requested_with(|id| {
            assert!(
                checked.insert(id),
                "a pending ID was checked more than once"
            );
            available.contains(&id)
        });
        assert_eq!(checked, HashSet::from([prepared, delayed]));
        assert!(bridge.is_ready(prepared));
        assert!(!bridge.is_ready(delayed));
        assert!(!bridge.is_ready(unrelated));
        checked.clear();
        bridge.acknowledge_requested_with(|id| {
            checked.insert(id);
            false
        });
        assert_eq!(checked, HashSet::from([delayed]));
    }

    #[test]
    fn headless_backend_preserves_requests_until_an_explicit_test_acknowledgment() {
        let mut app = App::new();
        app.add_plugins(TerrainUploadPlugin);
        let bridge = app.world().resource::<TerrainMeshUploadReadiness>();
        let [id] = ids();
        bridge.request(id);
        assert!(!bridge.has_render_backend());
        assert!(!bridge.is_ready(id));
        assert!(bridge.acknowledge(id));
        assert!(bridge.is_ready(id));
    }

    #[test]
    fn plugin_shares_acknowledgments_and_cancellation_between_worlds() {
        let mut app = App::new();
        app.insert_sub_app(RenderApp, SubApp::new());
        app.add_plugins(TerrainUploadPlugin);
        let main = app.world().resource::<TerrainMeshUploadReadiness>().clone();
        let render = app
            .sub_app(RenderApp)
            .world()
            .resource::<TerrainMeshUploadReadiness>()
            .clone();
        let [id] = ids();
        assert!(main.has_render_backend());
        main.request(id);
        assert!(render.acknowledge(id));
        assert!(main.is_ready(id));
        main.forget(id);
        assert!(!render.is_ready(id));
    }

    #[test]
    fn render_system_requires_the_requested_prepared_asset_and_allocator() {
        let mut world = World::new();
        let bridge = TerrainMeshUploadReadiness::default();
        let [id, unrelated] = ids();
        bridge.request(id);
        world.insert_resource(bridge.clone());
        world.run_system_once(acknowledge_prepared_meshes).unwrap();
        assert!(
            !bridge.is_ready(id),
            "missing render asset resources cannot acknowledge"
        );

        let render_mesh = render_mesh(4, None);
        let mut assets = RenderAssets::<RenderMesh>::default();
        assets.insert(unrelated, render_mesh.clone());
        world.insert_resource(assets);
        world.run_system_once(acknowledge_prepared_meshes).unwrap();
        assert!(!bridge.is_ready(id));
        assert!(!bridge.is_ready(unrelated));
        world
            .resource_mut::<RenderAssets<RenderMesh>>()
            .insert(id, render_mesh);
        world.run_system_once(acknowledge_prepared_meshes).unwrap();
        assert!(
            !bridge.is_ready(id),
            "a prepared descriptor without the allocator cannot acknowledge"
        );
        assert!(!bridge.is_ready(unrelated));
    }
}
