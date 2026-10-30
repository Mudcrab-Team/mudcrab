use super::*;
use crate::world::components::ExteriorCellGrid;
use bevy::{
    camera::visibility::VisibilityPlugin,
    pbr::{
        PreviousGlobalTransform, update_mesh_previous_global_transforms, update_previous_view_data,
    },
    transform::TransformPlugin,
};

#[derive(Resource, Default)]
pub(super) struct RebaseSteps(Vec<IVec2>);

#[derive(Component)]
pub(super) struct ShiftWithOrigin;

type ShiftedTransforms<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Transform,
        Has<crate::physics::PlayerBody>,
        Has<crate::physics::DebugTankard>,
    ),
    With<ShiftWithOrigin>,
>;

// A second origin writer deliberately has no call into the history repair. The
// central PreUpdate/PostUpdate boundary must cover its net displacement too.
fn rebase_in_update(
    mut origin: ResMut<RenderOrigin>,
    mut steps: ResMut<RebaseSteps>,
    mut transforms: ShiftedTransforms,
    mut participants: ResMut<ParticipantRebase>,
) {
    for shift in steps.0.drain(..) {
        origin.0 += shift;
        let displacement = grid_translation(shift, IVec2::ZERO);
        let mut shifted_participant = false;
        for (mut transform, body, tankard) in &mut transforms {
            transform.translation -= displacement;
            shifted_participant |= body || tankard;
        }
        if shifted_participant {
            participants.0 += displacement;
        }
    }
}

pub(super) struct Fixture {
    pub(super) app: App,
    pub(super) camera: Entity,
    pub(super) root: Entity,
    pub(super) mesh: Entity,
    pub(super) interior: Entity,
}

impl Fixture {
    pub(super) fn new() -> Self {
        Self::build(true)
    }

    pub(super) fn build(with_history: bool) -> Self {
        Self::build_with_setup(with_history, |_| {})
    }

    pub(super) fn build_with_setup(with_history: bool, setup: fn(&mut App)) -> Self {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            TransformPlugin,
            VisibilityPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<bevy::mesh::skinning::SkinnedMeshInverseBindposes>()
        .insert_resource(RenderOrigin(IVec2::new(3, -2)))
        .init_resource::<RebaseSteps>()
        .add_systems(
            PreUpdate,
            (
                update_previous_view_data,
                update_mesh_previous_global_transforms,
            ),
        )
        .add_systems(Update, rebase_in_update);
        if with_history {
            install(&mut app);
        } else {
            app.init_resource::<ParticipantRebase>();
        }
        setup(&mut app);
        let pose =
            Transform::from_xyz(100.0, 30.0, 50.0).with_rotation(Quat::from_rotation_y(0.37));
        let mut camera_data = Camera::default();
        camera_data.computed.clip_from_view = Mat4::perspective_infinite_reverse_rh(1.0, 1.5, 0.1);
        let camera = app
            .world_mut()
            .spawn((
                StreamingCamera,
                bevy::core_pipeline::prepass::MotionVectorPrepass,
                Camera3d::default(),
                camera_data,
                pose,
                ShiftWithOrigin,
            ))
            .id();
        let root = app
            .world_mut()
            .spawn((
                ExteriorCellGrid(IVec2::new(3, -2)),
                Transform::IDENTITY,
                Visibility::Inherited,
                ShiftWithOrigin,
            ))
            .id();
        let mesh = app
            .world_mut()
            .spawn((
                Mesh3d::default(),
                Transform::from_translation(pose.translation + pose.forward() * 1000.0),
                ChildOf(root),
                Visibility::Inherited,
            ))
            .id();
        let interior = app
            .world_mut()
            .spawn((Mesh3d::default(), Transform::from_xyz(12.0, 3.0, 9.0)))
            .id();
        app.update();
        app.update();
        Self {
            app,
            camera,
            root,
            mesh,
            interior,
        }
    }

    pub(super) fn rebase(&mut self, steps: &[IVec2]) {
        self.app.world_mut().resource_mut::<RebaseSteps>().0 = steps.to_vec();
        self.app.update();
    }

    pub(super) fn previous_model(&self, entity: Entity) -> Mat4 {
        Mat4::from(
            self.app
                .world()
                .get::<PreviousGlobalTransform>(entity)
                .unwrap()
                .0,
        )
    }

    pub(super) fn model(&self, entity: Entity) -> Mat4 {
        self.app
            .world()
            .get::<GlobalTransform>(entity)
            .unwrap()
            .to_matrix()
    }

    pub(super) fn previous_view(&self) -> &PreviousViewData {
        self.app
            .world()
            .get::<PreviousViewData>(self.camera)
            .unwrap()
    }

    pub(super) fn current_clip(&self, world_point: Vec3) -> Vec4 {
        let camera = self.app.world().get::<Camera>(self.camera).unwrap();
        camera.clip_from_view() * self.model(self.camera).inverse() * world_point.extend(1.0)
    }
}

pub(super) fn assert_matrix_close(actual: Mat4, expected: Mat4) {
    assert!(
        actual.abs_diff_eq(expected, 0.002),
        "actual={actual:?} expected={expected:?}"
    );
}

pub(super) fn uv(clip: Vec4) -> Vec2 {
    clip.truncate().truncate() / clip.w * Vec2::new(0.5, -0.5)
}

#[derive(Resource, Default)]
struct CurrentGlobalTicks(Vec<(Entity, bevy::ecs::change_detection::Tick)>);

pub(super) fn assert_current_global_ticks(mut f: Fixture, flat: bool) {
    f.app.init_resource::<CurrentGlobalTicks>().add_systems(
        PostUpdate,
        (|globals: Query<(Entity, Ref<GlobalTransform>)>,
          mut ticks: ResMut<CurrentGlobalTicks>| {
            ticks.0 = globals
                .iter()
                .map(|(entity, global)| (entity, global.last_changed()))
                .collect();
        })
        .after(VisibilitySystems::CheckVisibility)
        .before(rebase_previous_frame_data),
    );
    f.rebase(&[IVec2::X]);
    for &(entity, expected) in &f.app.world().resource::<CurrentGlobalTicks>().0 {
        let actual = f
            .app
            .world()
            .entity(entity)
            .get_ref::<GlobalTransform>()
            .unwrap()
            .last_changed();
        assert_eq!(
            actual, expected,
            "history repair changed {entity:?}, flat={flat}"
        );
    }
}
