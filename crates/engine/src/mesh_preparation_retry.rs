//! Retry phase specialization when a visible mesh's GPU descriptor arrives late.
//!
//! Bevy 0.19 retries missing materials and mesh instances, but its main, prepass and shadow
//! specialization paths do not retain the missing-RenderMesh case. Track only changed, newly
//! visible or already-pending entities that encounter that case. Once their exact mesh ID is
//! prepared, request Bevy's standard specialization and extraction paths once.

use bevy::{
    asset::AssetId,
    ecs::system::SystemParam,
    pbr::{
        MeshesToReextractNextFrame, PendingMeshMaterialQueues, PendingPrepassMeshMaterialQueues,
        PendingShadowQueues, RenderMeshInstances, collect_meshes_for_gpu_building,
        set_mesh_motion_vector_flags,
    },
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        camera::{DirtySpecializations, PendingQueues},
        mesh::{RenderMesh, RenderMeshBufferInfo, allocator::MeshAllocator},
        render_asset::RenderAssets,
        sync_world::MainEntity,
        view::{
            ExtractedView, RenderShadowMapVisibleEntities, RenderVisibleEntities,
            RetainedViewEntity,
        },
    },
};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

const MAX_RESUME_SAMPLES: usize = 16;

pub struct MeshPreparationRetryPlugin;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
enum MeshPreparationRetrySystems {
    Resume,
    Observe,
    ObserveLateShadows,
}

impl Plugin for MeshPreparationRetryPlugin {
    fn build(&self, app: &mut App) {
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .init_resource::<PendingPreparations>()
                .configure_sets(
                    Render,
                    (
                        // Bevy's prepass specializes inside PrepareMeshes, before CreateViews.
                        // Resume retained records before its motion-vector ordering anchor.
                        MeshPreparationRetrySystems::Resume
                            .in_set(RenderSystems::PrepareMeshes)
                            .after(collect_meshes_for_gpu_building)
                            .before(set_mesh_motion_vector_flags),
                        MeshPreparationRetrySystems::Observe
                            .after(RenderSystems::CreateViews)
                            .before(RenderSystems::Specialize),
                        MeshPreparationRetrySystems::ObserveLateShadows
                            .after(RenderSystems::Specialize)
                            .before(RenderSystems::Queue),
                    ),
                )
                .add_systems(
                    Render,
                    (
                        resume_deferred_mesh_preparation
                            .in_set(MeshPreparationRetrySystems::Resume),
                        observe_deferred_mesh_preparation
                            .in_set(MeshPreparationRetrySystems::Observe),
                        observe_late_shadow_preparation
                            .in_set(MeshPreparationRetrySystems::ObserveLateShadows),
                    ),
                );
        }
    }
}

#[derive(Default, Resource)]
struct PendingPreparations {
    meshes: HashMap<MainEntity, AssetId<Mesh>>,
    observed_dirty_views: HashSet<RetainedViewEntity>,
    frame: u64,
    peak: usize,
    tracked_total: u64,
    resumed_total: u64,
    canceled_total: u64,
    logged_samples: usize,
}

#[derive(Default)]
struct Drain {
    resumed: Vec<(MainEntity, AssetId<Mesh>)>,
    canceled: usize,
}

impl PendingPreparations {
    // Do not retain handles: an unloaded entity/asset generation must remain free to disappear.
    fn observe_candidate(
        &mut self,
        entity: MainEntity,
        asset_id: Option<AssetId<Mesh>>,
        descriptor_exists: impl FnOnce(AssetId<Mesh>) -> bool,
    ) -> bool {
        let Some(asset_id) = asset_id else {
            return false;
        };
        if descriptor_exists(asset_id) || self.meshes.get(&entity) == Some(&asset_id) {
            return false;
        }
        if self.meshes.insert(entity, asset_id).is_some() {
            self.canceled_total += 1;
        }
        self.tracked_total += 1;
        self.peak = self.peak.max(self.meshes.len());
        true
    }

    fn drain_ready(
        &mut self,
        mut current_mesh: impl FnMut(MainEntity) -> Option<AssetId<Mesh>>,
        mut is_ready: impl FnMut(AssetId<Mesh>) -> bool,
    ) -> Drain {
        let mut drain = Drain::default();
        self.meshes.retain(|&entity, &mut asset_id| {
            if current_mesh(entity) != Some(asset_id) {
                drain.canceled += 1;
                return false;
            }
            if !is_ready(asset_id) {
                return true;
            }
            drain.resumed.push((entity, asset_id));
            false
        });
        self.resumed_total += drain.resumed.len() as u64;
        self.canceled_total += drain.canceled as u64;
        drain
    }

    fn observe_candidates(
        &mut self,
        candidates: impl IntoIterator<Item = MainEntity>,
        mut current_mesh: impl FnMut(MainEntity) -> Option<AssetId<Mesh>>,
        mut descriptor_exists: impl FnMut(AssetId<Mesh>) -> bool,
    ) -> usize {
        candidates
            .into_iter()
            .map(|entity| {
                usize::from(self.observe_candidate(
                    entity,
                    current_mesh(entity),
                    &mut descriptor_exists,
                ))
            })
            .sum()
    }
}

#[derive(SystemParam)]
struct PreparationResources<'w> {
    instances: Option<Res<'w, RenderMeshInstances>>,
    meshes: Option<Res<'w, RenderAssets<RenderMesh>>>,
    allocator: Option<Res<'w, MeshAllocator>>,
    material_pending: Option<Res<'w, PendingMeshMaterialQueues>>,
    prepass_pending: Option<Res<'w, PendingPrepassMeshMaterialQueues>>,
    shadow_pending: Option<Res<'w, PendingShadowQueues>>,
}

fn add_queue_candidates(candidates: &mut HashSet<MainEntity>, queues: &PendingQueues) {
    for view in queues.values() {
        candidates.extend(
            view.current_frame
                .iter()
                .chain(view.prev_frame.iter())
                .map(|&(_, entity)| entity),
        );
    }
}

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
        Some(0) | None => true,
        Some(logical_count) => index_range
            .and_then(|range| range.end.checked_sub(range.start))
            .is_some_and(|count| count >= logical_count),
    }
}

fn prepared_mesh_is_resident(
    asset_id: AssetId<Mesh>,
    meshes: &RenderAssets<RenderMesh>,
    allocator: &MeshAllocator,
) -> bool {
    let Some(mesh) = meshes.get(asset_id) else {
        return false;
    };
    let indexed_count = match mesh.buffer_info {
        RenderMeshBufferInfo::Indexed { count, .. } => Some(count),
        RenderMeshBufferInfo::NonIndexed => None,
    };
    resident_ranges_match(
        mesh.vertex_count,
        indexed_count,
        allocator
            .mesh_vertex_slice(&asset_id)
            .map(|slice| slice.range),
        allocator
            .mesh_index_slice(&asset_id)
            .map(|slice| slice.range),
    )
}

fn resume_ready_meshes(
    pending: &mut PendingPreparations,
    current_mesh: impl FnMut(MainEntity) -> Option<AssetId<Mesh>>,
    is_ready: impl FnMut(AssetId<Mesh>) -> bool,
    dirty: &mut DirtySpecializations,
    mut reextract: Option<&mut MeshesToReextractNextFrame>,
) -> Drain {
    let drain = pending.drain_ready(current_mesh, is_ready);
    for &(entity, _) in &drain.resumed {
        dirty.changed_renderables.insert(entity);
        if let Some(reextract) = reextract.as_mut() {
            reextract.insert(entity);
        }
    }
    drain
}

fn resume_deferred_mesh_preparation(
    mut pending: ResMut<PendingPreparations>,
    mut dirty: Option<ResMut<DirtySpecializations>>,
    mut reextract: Option<ResMut<MeshesToReextractNextFrame>>,
    resources: PreparationResources,
) {
    let (Some(instances), Some(meshes), Some(allocator), Some(dirty)) = (
        resources.instances.as_ref(),
        resources.meshes.as_ref(),
        resources.allocator.as_ref(),
        dirty.as_mut(),
    ) else {
        return;
    };
    pending.frame += 1;
    let drain = resume_ready_meshes(
        &mut pending,
        |entity| instances.mesh_asset_id(entity),
        |id| prepared_mesh_is_resident(id, meshes, allocator),
        dirty,
        reextract.as_deref_mut(),
    );
    for &(entity, asset_id) in &drain.resumed {
        if pending.logged_samples < MAX_RESUME_SAMPLES {
            pending.logged_samples += 1;
            info!(
                render_frame = pending.frame,
                ?entity,
                ?asset_id,
                "late mesh preparation resumed"
            );
        }
    }
    if !drain.resumed.is_empty() || drain.canceled > 0 {
        info!(
            render_frame = pending.frame,
            pending = pending.meshes.len(),
            pending_peak = pending.peak,
            resumed = drain.resumed.len(),
            canceled = drain.canceled,
            tracked_total = pending.tracked_total,
            resumed_total = pending.resumed_total,
            canceled_total = pending.canceled_total,
            extraction_retry_available = reextract.is_some(),
            "late mesh preparation retry drain"
        );
    }
}

fn observe_deferred_mesh_preparation(
    mut pending: ResMut<PendingPreparations>,
    dirty: Option<Res<DirtySpecializations>>,
    resources: PreparationResources,
    views: Query<(&ExtractedView, &RenderVisibleEntities)>,
    shadow_views: Query<&RenderShadowMapVisibleEntities>,
) {
    let (Some(instances), Some(meshes), Some(dirty)) = (
        resources.instances.as_ref(),
        resources.meshes.as_ref(),
        dirty.as_ref(),
    ) else {
        return;
    };
    pending.observed_dirty_views.clear();
    let mut candidates: HashSet<_> = dirty.changed_renderables.iter().copied().collect();
    for (extracted_view, view) in &views {
        if let Some(class) = view.get::<Mesh3d>() {
            candidates.extend(class.added_entities().iter().map(|&(_, entity)| entity));
            if dirty.views.contains(&extracted_view.retained_view_entity) {
                pending
                    .observed_dirty_views
                    .insert(extracted_view.retained_view_entity);
                candidates.extend(class.iter_visible().map(|(_, &entity)| entity));
            }
        }
    }
    for light in &shadow_views {
        for (retained_view, view) in &light.subviews {
            if let Some(class) = view.get::<Mesh3d>() {
                candidates.extend(class.added_entities().iter().map(|&(_, entity)| entity));
                if dirty.views.contains(retained_view) {
                    pending.observed_dirty_views.insert(*retained_view);
                    candidates.extend(class.iter_visible().map(|(_, &entity)| entity));
                }
            }
        }
    }
    // A material may become ready after the entity's visibility/change events have expired.
    // Its existing pending queue supplies the candidate when the mesh instance first appears.
    for queues in [
        resources.material_pending.as_ref().map(|queues| &queues.0),
        resources.prepass_pending.as_ref().map(|queues| &queues.0),
        resources.shadow_pending.as_ref().map(|queues| &queues.0),
    ]
    .into_iter()
    .flatten()
    {
        add_queue_candidates(&mut candidates, queues);
    }
    let candidate_count = candidates.len();
    let canceled_before = pending.canceled_total;
    let newly_tracked = pending.observe_candidates(
        candidates,
        |entity| instances.mesh_asset_id(entity),
        |id| meshes.get(id).is_some(),
    );
    let canceled = pending.canceled_total - canceled_before;
    if newly_tracked > 0 || canceled > 0 {
        info!(
            render_frame = pending.frame,
            candidates = candidate_count,
            newly_tracked,
            pending = pending.meshes.len(),
            pending_peak = pending.peak,
            canceled,
            tracked_total = pending.tracked_total,
            resumed_total = pending.resumed_total,
            canceled_total = pending.canceled_total,
            "late mesh preparation observed for next frame"
        );
    }
}

// Shadow view keys can first become dirty inside Specialize. Capture only those view-change
// events here; changing DirtySpecializations now would be erased by the next ExtractSchedule.
// Keep these records for the next frame's pre-Specialize drain instead.
fn observe_late_shadow_preparation(
    mut pending: ResMut<PendingPreparations>,
    dirty: Option<Res<DirtySpecializations>>,
    resources: PreparationResources,
    shadow_views: Query<&RenderShadowMapVisibleEntities>,
) {
    let (Some(dirty), Some(instances), Some(meshes)) = (
        dirty.as_ref(),
        resources.instances.as_ref(),
        resources.meshes.as_ref(),
    ) else {
        return;
    };
    let mut candidates = HashSet::new();
    for light in &shadow_views {
        for (retained_view, view) in &light.subviews {
            if !dirty.views.contains(retained_view)
                || pending.observed_dirty_views.contains(retained_view)
            {
                continue;
            }
            if let Some(class) = view.get::<Mesh3d>() {
                pending.observed_dirty_views.insert(*retained_view);
                candidates.extend(class.iter_visible().map(|(_, &entity)| entity));
            }
        }
    }
    let candidate_count = candidates.len();
    let canceled_before = pending.canceled_total;
    let newly_tracked = pending.observe_candidates(
        candidates,
        |entity| instances.mesh_asset_id(entity),
        |id| meshes.get(id).is_some(),
    );
    let canceled = pending.canceled_total - canceled_before;
    if newly_tracked > 0 || canceled > 0 {
        info!(
            render_frame = pending.frame,
            candidates = candidate_count,
            newly_tracked,
            pending = pending.meshes.len(),
            pending_peak = pending.peak,
            canceled,
            tracked_total = pending.tracked_total,
            resumed_total = pending.resumed_total,
            canceled_total = pending.canceled_total,
            "late shadow preparation observed for next frame"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        app::SubApp, ecs::system::RunSystemOnce, pbr::RenderMeshInstanceGpu,
        render::view::RenderVisibleEntitiesClass,
    };
    use std::any::TypeId;

    fn observer_world() -> World {
        let mut world = World::new();
        world.init_resource::<PendingPreparations>();
        world.init_resource::<DirtySpecializations>();
        world.init_resource::<RenderAssets<RenderMesh>>();
        world.insert_resource(RenderMeshInstances::GpuBuilding(default()));
        world
    }

    fn insert_instance(world: &mut World, entity: MainEntity, id: AssetId<Mesh>) {
        let mut instances = world.resource_mut::<RenderMeshInstances>();
        let RenderMeshInstances::GpuBuilding(instances) = &mut *instances else {
            unreachable!();
        };
        let instance = RenderMeshInstanceGpu {
            shared: default(),
            gpu_specific: default(),
            render_layers: None,
        };
        instance.shared.set_mesh_asset_id(id);
        instances.insert(entity, instance);
    }

    fn visible_class(entity: MainEntity, newly_visible: bool) -> RenderVisibleEntitiesClass {
        let pair = (Entity::PLACEHOLDER, entity);
        RenderVisibleEntitiesClass {
            entities_cpu_culling: vec![pair],
            added_entities: if newly_visible { vec![pair] } else { vec![] },
            ..default()
        }
    }

    #[test]
    fn changed_mesh_is_observed_after_its_original_change_event_expires() {
        let [id] = mesh_ids();
        let mut world = observer_world();
        let entity = world.spawn_empty().id().into();
        insert_instance(&mut world, entity, id);
        world
            .resource_mut::<DirtySpecializations>()
            .changed_renderables
            .insert(entity);
        world
            .run_system_once(observe_deferred_mesh_preparation)
            .unwrap();
        world
            .resource_mut::<DirtySpecializations>()
            .changed_renderables
            .clear();
        world
            .run_system_once(observe_deferred_mesh_preparation)
            .unwrap();
        let pending = world.resource::<PendingPreparations>();
        assert_eq!(pending.meshes.get(&entity), Some(&id));
        assert_eq!(pending.tracked_total, 1);
    }

    #[test]
    fn every_phase_pending_queue_can_observe_an_instance_that_appears_late() {
        for phase in 0..3 {
            let [id] = mesh_ids();
            let mut world = observer_world();
            let entity = world.spawn_empty().id().into();
            let view = RetainedViewEntity::new(entity, None, 0);
            let pair = (Entity::PLACEHOLDER, entity);
            match phase {
                0 => {
                    world.init_resource::<PendingMeshMaterialQueues>();
                    world
                        .resource_mut::<PendingMeshMaterialQueues>()
                        .prepare_for_new_frame(view)
                        .current_frame
                        .insert(pair);
                }
                1 => {
                    world.init_resource::<PendingPrepassMeshMaterialQueues>();
                    world
                        .resource_mut::<PendingPrepassMeshMaterialQueues>()
                        .prepare_for_new_frame(view)
                        .current_frame
                        .insert(pair);
                }
                _ => {
                    world.init_resource::<PendingShadowQueues>();
                    world
                        .resource_mut::<PendingShadowQueues>()
                        .prepare_for_new_frame(view)
                        .current_frame
                        .insert(pair);
                }
            }
            world
                .run_system_once(observe_deferred_mesh_preparation)
                .unwrap();
            assert!(world.resource::<PendingPreparations>().meshes.is_empty());
            insert_instance(&mut world, entity, id);
            world
                .run_system_once(observe_deferred_mesh_preparation)
                .unwrap();
            assert_eq!(
                world.resource::<PendingPreparations>().meshes.get(&entity),
                Some(&id),
                "phase {phase} must preserve the late instance candidate"
            );
        }
    }

    #[test]
    fn new_visibility_and_late_shadow_view_changes_preserve_missing_meshes() {
        for newly_visible in [true, false] {
            let [id] = mesh_ids();
            let mut world = observer_world();
            let entity = world.spawn_empty().id().into();
            let view = RetainedViewEntity::new(entity, None, 0);
            insert_instance(&mut world, entity, id);
            let mut visible = RenderVisibleEntities::default();
            visible
                .classes
                .insert(TypeId::of::<Mesh3d>(), visible_class(entity, newly_visible));
            let mut shadows = RenderShadowMapVisibleEntities::default();
            shadows.subviews.insert(view, visible);
            world.spawn(shadows);
            world
                .run_system_once(observe_deferred_mesh_preparation)
                .unwrap();
            if !newly_visible {
                assert!(world.resource::<PendingPreparations>().meshes.is_empty());
                // Bevy creates this dirty shadow-view key inside Specialize, after observation.
                world
                    .resource_mut::<DirtySpecializations>()
                    .views
                    .insert(view);
                world
                    .run_system_once(observe_late_shadow_preparation)
                    .unwrap();
            }
            world.resource_mut::<DirtySpecializations>().views.clear();
            assert_eq!(
                world.resource::<PendingPreparations>().meshes.get(&entity),
                Some(&id)
            );
        }
    }

    #[derive(Resource)]
    struct ScheduleFixture {
        entity: MainEntity,
        mesh: AssetId<Mesh>,
        view: RetainedViewEntity,
        class: RenderVisibleEntitiesClass,
        frame: u64,
        prepared_at: Option<u64>,
        present: bool,
        deliveries: Vec<(u64, &'static str, usize)>,
    }

    fn resume_fixture(
        mut fixture: ResMut<ScheduleFixture>,
        mut pending: ResMut<PendingPreparations>,
        mut dirty: ResMut<DirtySpecializations>,
        mut reextract: ResMut<MeshesToReextractNextFrame>,
    ) {
        fixture.frame += 1;
        let entity = fixture.entity;
        let mesh = fixture.mesh;
        let present = fixture.present;
        let prepared = fixture.prepared_at.is_some_and(|at| fixture.frame >= at);
        resume_ready_meshes(
            &mut pending,
            |current| (present && current == entity).then_some(mesh),
            |current| prepared && current == mesh,
            &mut dirty,
            Some(&mut reextract),
        );
    }

    fn record_delivery(
        fixture: &mut ScheduleFixture,
        dirty: &DirtySpecializations,
        phase: &'static str,
    ) {
        let count = dirty
            .iter_to_specialize(fixture.view, &fixture.class, &default())
            .count();
        fixture.deliveries.push((fixture.frame, phase, count));
    }

    fn test_prepass(mut fixture: ResMut<ScheduleFixture>, dirty: Res<DirtySpecializations>) {
        record_delivery(&mut fixture, &dirty, "prepass");
    }

    fn test_main(mut fixture: ResMut<ScheduleFixture>, dirty: Res<DirtySpecializations>) {
        record_delivery(&mut fixture, &dirty, "main");
    }

    fn test_shadow(mut fixture: ResMut<ScheduleFixture>, dirty: Res<DirtySpecializations>) {
        record_delivery(&mut fixture, &dirty, "shadow");
    }

    fn schedule_fixture(prepared_at: Option<u64>) -> App {
        let mut app = App::new();
        let mut render = SubApp::new();
        render.add_schedule(Render::base_schedule());
        render.world_mut().init_resource::<DirtySpecializations>();
        render
            .world_mut()
            .init_resource::<MeshesToReextractNextFrame>();
        app.insert_sub_app(RenderApp, render);
        app.add_plugins(MeshPreparationRetryPlugin);
        let render = app.sub_app_mut(RenderApp);
        let [mesh] = mesh_ids();
        let entity = render.world_mut().spawn_empty().id().into();
        render
            .world_mut()
            .resource_mut::<PendingPreparations>()
            .observe_candidate(entity, Some(mesh), |_| false);
        render.insert_resource(ScheduleFixture {
            entity,
            mesh,
            view: RetainedViewEntity::new(entity, None, 0),
            class: visible_class(entity, false),
            frame: 0,
            prepared_at,
            present: true,
            deliveries: vec![],
        });
        render.add_systems(
            Render,
            (
                // These are Bevy's actual ordering anchors. GPU work is disabled in this CPU test.
                collect_meshes_for_gpu_building
                    .in_set(RenderSystems::PrepareMeshes)
                    .run_if(|| false),
                set_mesh_motion_vector_flags
                    .in_set(RenderSystems::PrepareMeshes)
                    .run_if(|| false),
                bevy::render::camera::clear_dirty_specializations
                    .in_set(RenderSystems::ExtractCommands),
                resume_fixture.in_set(MeshPreparationRetrySystems::Resume),
                test_prepass
                    .in_set(RenderSystems::PrepareMeshes)
                    .after(set_mesh_motion_vector_flags),
                test_main.in_set(RenderSystems::Specialize),
                test_shadow.in_set(RenderSystems::Specialize),
            ),
        );
        app
    }

    #[test]
    fn delayed_preparation_reaches_prepass_main_and_shadow_after_change_events_expire() {
        let mut app = schedule_fixture(Some(3));
        let render = app.sub_app_mut(RenderApp);
        for _ in 0..5 {
            render.world_mut().run_schedule(Render);
        }
        let fixture = render.world().resource::<ScheduleFixture>();
        assert_eq!(fixture.deliveries.len(), 15);
        for &(frame, phase, count) in &fixture.deliveries {
            assert_eq!(
                count,
                usize::from(frame == 3),
                "frame {frame}, phase {phase}"
            );
        }
        assert_eq!(
            render
                .world()
                .resource::<PendingPreparations>()
                .resumed_total,
            1
        );
        assert!(
            render
                .world()
                .resource::<MeshesToReextractNextFrame>()
                .contains(&fixture.entity)
        );
    }

    #[test]
    fn permanently_absent_mesh_never_resumes_and_unloading_cancels_its_record() {
        let mut app = schedule_fixture(None);
        let render = app.sub_app_mut(RenderApp);
        for _ in 0..20 {
            render.world_mut().run_schedule(Render);
        }
        assert!(
            render
                .world()
                .resource::<ScheduleFixture>()
                .deliveries
                .iter()
                .all(|&(_, _, count)| count == 0)
        );
        assert_eq!(
            render
                .world()
                .resource::<PendingPreparations>()
                .meshes
                .len(),
            1
        );
        assert!(
            render
                .world()
                .resource::<MeshesToReextractNextFrame>()
                .is_empty()
        );
        render.world_mut().resource_mut::<ScheduleFixture>().present = false;
        render.world_mut().run_schedule(Render);
        let pending = render.world().resource::<PendingPreparations>();
        assert!(pending.meshes.is_empty());
        assert_eq!(pending.resumed_total, 0);
        assert_eq!(pending.canceled_total, 1);
    }

    fn mesh_ids<const N: usize>() -> [AssetId<Mesh>; N] {
        let mut meshes = Assets::<Mesh>::default();
        std::array::from_fn(|_| meshes.add(Cuboid::default()).id())
    }

    fn entity() -> MainEntity {
        World::new().spawn_empty().id().into()
    }

    #[test]
    fn missing_descriptor_retries_once_after_full_readiness_without_stalling_other_meshes() {
        let [delayed, unrelated] = mesh_ids();
        let mut world = World::new();
        let delayed_entity = world.spawn_empty().id().into();
        let unrelated_entity = world.spawn_empty().id().into();
        let current = HashMap::from([(delayed_entity, delayed), (unrelated_entity, unrelated)]);
        let mut pending = PendingPreparations::default();
        assert!(pending.observe_candidate(delayed_entity, Some(delayed), |_| false));
        assert!(!pending.observe_candidate(unrelated_entity, Some(unrelated), |_| true));
        assert!(!pending.observe_candidate(delayed_entity, Some(delayed), |_| false));
        let drain = pending.drain_ready(|entity| current.get(&entity).copied(), |_| false);
        assert!(drain.resumed.is_empty());
        assert_eq!(pending.meshes.len(), 1);
        let drain = pending.drain_ready(|entity| current.get(&entity).copied(), |id| id == delayed);
        assert_eq!(drain.resumed, vec![(delayed_entity, delayed)]);
        assert_eq!(drain.canceled, 0);
        assert!(pending.meshes.is_empty());
        assert!(!pending.observe_candidate(delayed_entity, Some(delayed), |_| true));
        assert!(
            pending
                .drain_ready(|entity| current.get(&entity).copied(), |_| true)
                .resumed
                .is_empty()
        );
        assert_eq!(pending.tracked_total, 1);
        assert_eq!(pending.resumed_total, 1);
    }

    #[test]
    fn a_material_pending_candidate_can_start_tracking_after_its_instance_appears() {
        let [id] = mesh_ids();
        let entity = entity();
        let mut pending = PendingPreparations::default();
        assert!(!pending.observe_candidate(entity, None, |_| false));
        assert!(pending.observe_candidate(entity, Some(id), |_| false));
        assert_eq!(
            pending.drain_ready(|_| Some(id), |_| true).resumed,
            vec![(entity, id)]
        );
    }

    #[test]
    fn replacing_an_asset_cancels_the_old_pending_generation() {
        let [old, replacement] = mesh_ids();
        let entity = entity();
        let mut pending = PendingPreparations::default();
        pending.observe_candidate(entity, Some(old), |_| false);
        let drain = pending.drain_ready(|_| Some(replacement), |id| id == old);
        assert_eq!(drain.canceled, 1);
        assert!(
            drain.resumed.is_empty(),
            "a stale ready asset cannot resume its replacement"
        );
        assert!(pending.observe_candidate(entity, Some(replacement), |_| false));
        assert_eq!(
            pending.drain_ready(|_| Some(replacement), |_| true).resumed,
            vec![(entity, replacement)]
        );
        assert_eq!(pending.canceled_total, 1);
    }

    #[test]
    fn same_frame_missing_asset_replacement_supersedes_the_previous_request() {
        let [old, replacement] = mesh_ids();
        let entity = entity();
        let mut pending = PendingPreparations::default();
        pending.observe_candidate(entity, Some(old), |_| false);
        pending.observe_candidate(entity, Some(replacement), |_| false);
        assert_eq!(pending.meshes.len(), 1);
        assert_eq!(pending.meshes.get(&entity), Some(&replacement));
        assert_eq!(pending.canceled_total, 1);
        assert_eq!(
            pending
                .drain_ready(|_| Some(replacement), |id| id == old)
                .resumed
                .len(),
            0
        );
    }

    #[test]
    fn invisible_or_unloaded_entities_cancel_even_when_their_asset_is_ready() {
        let [id] = mesh_ids();
        let entity = entity();
        let mut pending = PendingPreparations::default();
        pending.observe_candidate(entity, Some(id), |_| false);
        let drain = pending.drain_ready(|_| None, |_| true);
        assert_eq!(drain.canceled, 1);
        assert!(drain.resumed.is_empty());
        assert!(pending.meshes.is_empty());
    }

    #[test]
    fn recycled_entity_generation_does_not_receive_the_old_retry() {
        let [id] = mesh_ids();
        let mut world = World::new();
        let old = world.spawn_empty().id();
        let mut pending = PendingPreparations::default();
        pending.observe_candidate(old.into(), Some(id), |_| false);
        world.despawn(old);
        let replacement = world.spawn_empty().id();
        assert_ne!(old, replacement);
        let current = HashMap::from([(MainEntity::from(replacement), id)]);
        let drain = pending.drain_ready(|entity| current.get(&entity).copied(), |_| true);
        assert_eq!(drain.canceled, 1);
        assert!(drain.resumed.is_empty());
        pending.observe_candidate(replacement.into(), Some(id), |_| false);
        assert_eq!(
            pending
                .drain_ready(|entity| current.get(&entity).copied(), |_| true)
                .resumed,
            vec![(replacement.into(), id)]
        );
    }

    #[test]
    fn late_observation_survives_frame_reset_and_only_the_next_drain_resumes() {
        let [id] = mesh_ids();
        let entity = entity();
        let mut pending = PendingPreparations::default();
        assert_eq!(
            pending.observe_candidates([entity, entity], |_| Some(id), |_| false),
            1
        );
        assert_eq!(
            pending.resumed_total, 0,
            "observation cannot resume a phase late"
        );
        // ExtractSchedule clears the dirty sets. The retry's independent pending records remain.
        pending.observed_dirty_views.clear();
        assert_eq!(pending.meshes.get(&entity), Some(&id));
        assert_eq!(
            pending.drain_ready(|_| Some(id), |_| true).resumed,
            vec![(entity, id)]
        );
        assert!(
            pending
                .drain_ready(|_| Some(id), |_| true)
                .resumed
                .is_empty()
        );
        assert_eq!(pending.resumed_total, 1);
    }

    #[test]
    fn indexed_nonindexed_and_empty_geometry_use_their_required_ranges() {
        assert!(!resident_ranges_match(4, Some(6), None, Some(20..26)));
        assert!(!resident_ranges_match(
            4,
            Some(6),
            Some(10..13),
            Some(20..26)
        ));
        assert!(!resident_ranges_match(4, Some(6), Some(10..14), None));
        assert!(!resident_ranges_match(
            4,
            Some(6),
            Some(10..14),
            Some(20..25)
        ));
        assert!(!resident_ranges_match(
            4,
            Some(6),
            Some(10..14),
            Some(Range { start: 26, end: 20 })
        ));
        assert!(resident_ranges_match(
            4,
            Some(6),
            Some(10..14),
            Some(20..26)
        ));
        assert!(resident_ranges_match(4, None, Some(10..14), None));
        assert!(resident_ranges_match(4, Some(0), Some(10..14), None));
        assert!(!resident_ranges_match(4, Some(0), None, None));
    }
}
