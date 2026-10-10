//! Keep empty streamed hierarchy nodes out of Bevy's per-camera culling loops.
//!
//! A converted glTF instance contains transform/visibility nodes as well as mesh primitives.
//! Bevy's CPU visibility query visits both, although a node without a visibility class or bounds
//! cannot contribute any camera or shadow draw. These structural nodes keep their visibility
//! inheritance, but use Bevy's change-driven `NoCpuCulling` path for their own `ViewVisibility`.
//! Actual renderables, bounds used by gizmos, lights, probes, cameras and picking targets retain
//! the standard CPU visibility path. If one is added later, this plugin restores that path before
//! Bevy resets and checks visibility in the same frame.

use crate::{streaming::lod::LodChunkRoot, world::components::StreamedCellRoot};
use bevy::{
    camera::{
        primitives::{Aabb, Frustum, Sphere},
        visibility::{
            NoCpuCulling, NoFrustumCulling, VisibilityClass, VisibilityRange, VisibilitySystems,
        },
    },
    gizmos::aabb::ShowAabbGizmo,
    light::{LightProbe, RectLight},
    picking::Pickable,
    prelude::*,
};
use bevy_rapier3d::prelude::Collider;

pub struct VisibilityOptimizationPlugin;

impl Plugin for VisibilityOptimizationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            (restore_cpu_visibility, classify_new_hierarchy_nodes)
                .chain()
                .after(VisibilitySystems::CalculateBounds)
                .before(VisibilitySystems::VisibilityPropagate),
        );
    }
}

/// Records ownership of `NoCpuCulling`; explicitly configured opt-outs are never removed.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub(crate) struct StructuralHierarchyNode;

// Keep the eligibility and restoration filters symmetric. Presence of even an empty visibility
// class is significant: another rendering plugin may populate it later in the frame.
type EmptyHierarchyNode = (
    With<Visibility>,
    With<GlobalTransform>,
    Without<NoCpuCulling>,
    (
        Without<VisibilityClass>,
        Without<Aabb>,
        Without<Sphere>,
        Without<Frustum>,
        Without<VisibilityRange>,
        Without<NoFrustumCulling>,
    ),
    (
        Without<Mesh3d>,
        Without<Mesh2d>,
        Without<Camera>,
        Without<DirectionalLight>,
        Without<PointLight>,
        Without<SpotLight>,
        Without<RectLight>,
        Without<LightProbe>,
    ),
    (Without<Collider>, Without<Pickable>, Without<ShowAabbGizmo>),
);

type CpuVisibilityParticipant = Or<(
    Or<(
        With<VisibilityClass>,
        With<Aabb>,
        With<Sphere>,
        With<Frustum>,
        With<VisibilityRange>,
        With<NoFrustumCulling>,
    )>,
    Or<(
        With<Mesh3d>,
        With<Mesh2d>,
        With<Camera>,
        With<DirectionalLight>,
        With<PointLight>,
        With<SpotLight>,
        With<RectLight>,
        With<LightProbe>,
    )>,
    Or<(With<Collider>, With<Pickable>, With<ShowAabbGizmo>)>,
)>;

type StreamingHierarchyRoots = Or<(With<StreamedCellRoot>, With<LodChunkRoot>)>;

type RestoredCpuVisibilityNodes = (
    With<StructuralHierarchyNode>,
    Or<(
        CpuVisibilityParticipant,
        Without<Visibility>,
        Without<GlobalTransform>,
        Without<NoCpuCulling>,
    )>,
);

fn restore_cpu_visibility(
    mut commands: Commands,
    changed: Query<Entity, RestoredCpuVisibilityNodes>,
) {
    for entity in &changed {
        commands
            .entity(entity)
            .remove::<(StructuralHierarchyNode, NoCpuCulling)>();
    }
}

fn classify_new_hierarchy_nodes(
    mut commands: Commands,
    new_nodes: Query<Entity, (EmptyHierarchyNode, Added<ViewVisibility>)>,
    cell_roots: Query<(), StreamingHierarchyRoots>,
    parents: Query<&ChildOf>,
) {
    for entity in &new_nodes {
        if belongs_to_streamed_cell(entity, &cell_roots, &parents) {
            commands
                .entity(entity)
                .insert((StructuralHierarchyNode, NoCpuCulling));
        }
    }
}

fn belongs_to_streamed_cell(
    entity: Entity,
    cell_roots: &Query<(), StreamingHierarchyRoots>,
    parents: &Query<&ChildOf>,
) -> bool {
    if cell_roots.contains(entity) {
        return true;
    }
    parents
        .iter_ancestors::<ChildOf>(entity)
        .any(|ancestor| cell_roots.contains(ancestor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        asset::AssetPlugin,
        camera::{
            CameraProjection,
            visibility::{RenderLayers, VisibilityPlugin, VisibleEntities},
        },
        mesh::MeshPlugin,
    };
    use std::any::TypeId;

    fn app(optimize: bool) -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            MeshPlugin,
            TransformPlugin,
            VisibilityPlugin,
        ));
        if optimize {
            app.add_plugins(VisibilityOptimizationPlugin);
        }
        app
    }

    fn root(app: &mut App) -> Entity {
        app.world_mut()
            .spawn((
                StreamedCellRoot,
                Visibility::Inherited,
                Transform::default(),
            ))
            .id()
    }

    fn node(app: &mut App, parent: Entity) -> Entity {
        app.world_mut()
            .spawn((Visibility::Inherited, Transform::default(), ChildOf(parent)))
            .id()
    }

    fn camera(app: &mut App, layers: RenderLayers) -> Entity {
        let projection = PerspectiveProjection::default();
        let frustum = projection.compute_frustum(&GlobalTransform::IDENTITY);
        app.world_mut()
            .spawn((
                Camera::default(),
                VisibleEntities::default(),
                frustum,
                layers,
            ))
            .id()
    }

    fn visible_entities(app: &App, camera: Entity) -> Vec<Entity> {
        app.world()
            .get::<VisibleEntities>(camera)
            .unwrap()
            .get(TypeId::of::<Mesh3d>())
            .to_vec()
    }

    #[test]
    fn only_empty_streamed_nodes_receive_owned_opt_outs() {
        let mut app = app(true);
        let root = root(&mut app);
        let structural = node(&mut app, root);
        let unrelated = app
            .world_mut()
            .spawn((Visibility::Inherited, Transform::default()))
            .id();
        let primitive = app
            .world_mut()
            .spawn((Mesh3d::default(), ChildOf(structural)))
            .id();
        let mut class = VisibilityClass::default();
        class.push(TypeId::of::<Mesh3d>());
        let classified = app
            .world_mut()
            .spawn((
                class,
                Visibility::Inherited,
                Transform::default(),
                ChildOf(root),
            ))
            .id();
        let bounds = app
            .world_mut()
            .spawn((
                Aabb::default(),
                Visibility::Inherited,
                Transform::default(),
                ChildOf(root),
            ))
            .id();
        let explicit = app
            .world_mut()
            .spawn((
                NoCpuCulling,
                Visibility::Inherited,
                Transform::default(),
                ChildOf(root),
            ))
            .id();
        app.update();

        for entity in [root, structural] {
            assert!(app.world().get::<StructuralHierarchyNode>(entity).is_some());
            assert!(app.world().get::<NoCpuCulling>(entity).is_some());
            assert!(app.world().get::<Visibility>(entity).is_some());
            assert!(app.world().get::<InheritedVisibility>(entity).is_some());
        }
        for entity in [unrelated, primitive, classified, bounds, explicit] {
            assert!(app.world().get::<StructuralHierarchyNode>(entity).is_none());
        }
        assert!(app.world().get::<NoCpuCulling>(primitive).is_none());
        assert!(app.world().get::<NoCpuCulling>(explicit).is_some());
    }

    #[test]
    fn visibility_still_propagates_through_optimized_nodes() {
        let mut app = app(true);
        let camera = camera(&mut app, RenderLayers::default());
        let root = root(&mut app);
        let structural = node(&mut app, root);
        let primitive = app
            .world_mut()
            .spawn((
                Mesh3d::default(),
                Transform::from_xyz(0.0, 0.0, -10.0),
                Aabb::from_min_max(-Vec3::ONE, Vec3::ONE),
                ChildOf(structural),
            ))
            .id();
        app.update();
        assert_eq!(visible_entities(&app, camera), vec![primitive]);
        app.world_mut().entity_mut(root).insert(Visibility::Hidden);
        app.update();
        for entity in [root, structural, primitive] {
            assert!(
                !app.world()
                    .get::<InheritedVisibility>(entity)
                    .unwrap()
                    .get()
            );
            assert!(!app.world().get::<ViewVisibility>(entity).unwrap().get());
        }
        assert!(visible_entities(&app, camera).is_empty());
        app.world_mut()
            .entity_mut(root)
            .insert(Visibility::Inherited);
        app.update();
        assert_eq!(visible_entities(&app, camera), vec![primitive]);
    }

    #[test]
    fn adding_a_mesh_restores_same_frame_membership_for_each_view() {
        let mut app = app(true);
        let main = camera(&mut app, RenderLayers::from_layers(&[0, 1]));
        let reflection = camera(&mut app, RenderLayers::layer(0));
        let root = root(&mut app);
        let structural = node(&mut app, root);
        app.update();
        assert!(app.world().get::<NoCpuCulling>(structural).is_some());
        app.world_mut().entity_mut(structural).insert((
            Mesh3d::default(),
            Transform::from_xyz(0.0, 0.0, -10.0),
            Aabb::from_min_max(-Vec3::ONE, Vec3::ONE),
            RenderLayers::layer(1),
        ));
        app.update();
        assert!(
            app.world()
                .get::<StructuralHierarchyNode>(structural)
                .is_none()
        );
        assert!(app.world().get::<NoCpuCulling>(structural).is_none());
        assert_eq!(visible_entities(&app, main), vec![structural]);
        assert!(visible_entities(&app, reflection).is_empty());
        // The standard CPU path must repopulate membership even while every input is unchanged.
        app.update();
        assert_eq!(visible_entities(&app, main), vec![structural]);
        assert!(app.world().get::<ViewVisibility>(structural).unwrap().get());
        app.world_mut()
            .entity_mut(structural)
            .insert(RenderLayers::layer(0));
        app.update();
        assert_eq!(visible_entities(&app, reflection), vec![structural]);
    }

    #[test]
    fn deferred_mesh_additions_and_auto_bounds_restore_before_visibility_checks() {
        #[derive(Resource)]
        struct LateMesh(Entity, Handle<Mesh>);

        fn materialize(mut commands: Commands, mesh: Res<LateMesh>, mut finished: Local<bool>) {
            if !*finished {
                commands
                    .entity(mesh.0)
                    .insert((Mesh3d(mesh.1.clone()), Transform::from_xyz(0.0, 0.0, -10.0)));
                *finished = true;
            }
        }

        let mut app = app(true);
        let camera = camera(&mut app, RenderLayers::default());
        let root = root(&mut app);
        let structural = node(&mut app, root);
        app.update();
        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::default());
        app.insert_resource(LateMesh(structural, mesh)).add_systems(
            PostUpdate,
            materialize.before(VisibilitySystems::CalculateBounds),
        );
        app.update();
        assert!(app.world().get::<Aabb>(structural).is_some());
        assert!(app.world().get::<NoCpuCulling>(structural).is_none());
        assert_eq!(visible_entities(&app, camera), vec![structural]);
    }

    fn assert_restored<C: Component>(component: C) {
        let mut app = app(true);
        let root = root(&mut app);
        let entity = node(&mut app, root);
        app.update();
        assert!(app.world().get::<NoCpuCulling>(entity).is_some());
        app.world_mut().entity_mut(entity).insert(component);
        app.update();
        assert!(app.world().get::<StructuralHierarchyNode>(entity).is_none());
        assert!(app.world().get::<NoCpuCulling>(entity).is_none());
    }

    #[test]
    fn bounds_classes_lights_probes_and_picking_restore_cpu_culling() {
        assert_restored(Aabb::default());
        assert_restored(Sphere::default());
        assert_restored(VisibilityClass::default());
        assert_restored(Frustum::default());
        assert_restored(VisibilityRange::default());
        assert_restored(NoFrustumCulling);
        assert_restored(Mesh3d::default());
        assert_restored(Mesh2d::default());
        assert_restored(Camera::default());
        assert_restored(DirectionalLight::default());
        assert_restored(PointLight::default());
        assert_restored(SpotLight::default());
        assert_restored(RectLight::default());
        assert_restored(LightProbe::default());
        assert_restored(Collider::ball(1.0));
        assert_restored(Pickable::default());
        assert_restored(ShowAabbGizmo::default());
    }

    #[test]
    fn restoration_preserves_explicit_opt_outs() {
        let mut app = app(true);
        let root = root(&mut app);
        let explicit = app
            .world_mut()
            .spawn((
                NoCpuCulling,
                Visibility::Inherited,
                Transform::default(),
                ChildOf(root),
            ))
            .id();
        app.update();
        app.world_mut().entity_mut(explicit).insert(Aabb::default());
        app.update();
        assert!(app.world().get::<NoCpuCulling>(explicit).is_some());
        assert!(
            app.world()
                .get::<StructuralHierarchyNode>(explicit)
                .is_none()
        );
    }

    #[test]
    fn camera_memberships_match_standard_culling_after_origin_rebase_and_bounds_change() {
        let mut baseline = app(false);
        let mut optimized = app(true);
        fn scene(app: &mut App) -> (Entity, Entity, Entity) {
            let camera = camera(app, RenderLayers::default());
            let root = root(app);
            let structural = node(app, root);
            let primitive = app
                .world_mut()
                .spawn((
                    Mesh3d::default(),
                    Transform::from_xyz(0.0, 0.0, -10.0),
                    Aabb::from_min_max(-Vec3::ONE, Vec3::ONE),
                    ChildOf(structural),
                ))
                .id();
            (camera, root, primitive)
        }
        let (baseline_camera, baseline_root, baseline_primitive) = scene(&mut baseline);
        let (optimized_camera, optimized_root, optimized_primitive) = scene(&mut optimized);
        let expected_visible = [true, false, true, false];
        for (step, &expected_visible) in expected_visible.iter().enumerate() {
            for (app, root, primitive) in [
                (&mut baseline, baseline_root, baseline_primitive),
                (&mut optimized, optimized_root, optimized_primitive),
            ] {
                if step == 1 {
                    app.world_mut()
                        .entity_mut(root)
                        .insert(Transform::from_xyz(1_000.0, 0.0, 0.0));
                } else if step == 2 {
                    app.world_mut()
                        .entity_mut(primitive)
                        .insert(Aabb::from_min_max(
                            Vec3::new(-1_001.0, -1.0, -1.0),
                            Vec3::ONE,
                        ));
                } else if step == 3 {
                    app.world_mut().entity_mut(root).insert(Visibility::Hidden);
                }
                app.update();
            }
            assert_eq!(
                !visible_entities(&baseline, baseline_camera).is_empty(),
                expected_visible,
                "baseline scene did not exercise the intended culling boundary at step {step}",
            );
            assert_eq!(
                visible_entities(&baseline, baseline_camera).is_empty(),
                visible_entities(&optimized, optimized_camera).is_empty(),
                "camera membership diverged at step {step}",
            );
            assert_eq!(
                baseline
                    .world()
                    .get::<ViewVisibility>(baseline_primitive)
                    .unwrap()
                    .get(),
                optimized
                    .world()
                    .get::<ViewVisibility>(optimized_primitive)
                    .unwrap()
                    .get(),
            );
        }
    }
}
