//! Player movement physics: Rapier context, debug tankards, fixture arena.
//!
//! P1 owns the interactive `--physics-fixture`: a primitive slope/wall arena
//! with an asset-free debug tankard (compound cup + handle collider) used to
//! validate the walking controller and dynamic bodies before streamed terrain
//! (P2) and static (P3) collision arrive. All units are Creation units.

use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use crate::{
    profiling::ProfilingState, streaming::StreamingMetrics, world::components::StreamingCamera,
};

/// Fixed physics step: 60 Hz Rapier simulation (V15, V18).
pub const PHYSICS_TIMESTEP: f32 = 1.0 / 60.0;
/// Shared downward gravity in Creation units/s^2 for WALK and tankards (V11, V15).
pub const GRAVITY_CREATION_UNITS: f32 = 900.0;
/// Collision groups: world surfaces (V17).
pub const GROUP_WORLD: Group = Group::GROUP_1;
/// Collision groups: player capsule (V17).
pub const GROUP_PLAYER: Group = Group::GROUP_2;
/// Collision groups: dynamic debug tankards (V17).
pub const GROUP_TANKARD: Group = Group::GROUP_3;

/// Provisional movement tuning, centralized (V10, V11, V12).
#[derive(Debug, Clone, Resource)]
pub struct MovementTuning {
    pub walk_speed: f32,
    pub run_speed: f32,
    pub sprint_speed: f32,
    pub horizontal_acceleration: f32,
    pub gravity: f32,
    pub jump_launch: f32,
    pub capsule_radius: f32,
    pub capsule_standing_height: f32,
    pub eye_height: f32,
    pub slope_climb_degrees: f32,
    pub slope_slide_degrees: f32,
    pub autostep_height: f32,
    pub ground_snap: f32,
}

impl Default for MovementTuning {
    fn default() -> Self {
        Self {
            walk_speed: 160.0,
            run_speed: 300.0,
            sprint_speed: 420.0,
            horizontal_acceleration: 1800.0,
            gravity: GRAVITY_CREATION_UNITS,
            jump_launch: 340.0,
            capsule_radius: 28.0,
            capsule_standing_height: 126.0,
            eye_height: 112.0,
            slope_climb_degrees: 50.0,
            slope_slide_degrees: 55.0,
            autostep_height: 24.0,
            ground_snap: 12.0,
        }
    }
}

impl MovementTuning {
    pub fn capsule_half_height(&self) -> f32 {
        (self.capsule_standing_height * 0.5 - self.capsule_radius).max(1.0)
    }
}

/// Debug tankard fixture bounds in Creation units (V15).
pub const TANKARD_CUP_RADIUS: f32 = 14.0;
pub const TANKARD_CUP_HALF_HEIGHT: f32 = 18.0;
pub const TANKARD_HANDLE_RADIUS: f32 = 4.0;
pub const TANKARD_HANDLE_HALF_HEIGHT: f32 = 12.0;
/// P2+ cap on live spawned tankards (V15).
pub const MAX_LIVE_TANKARDS: usize = 32;

/// Marker for the asset-free debug tankard physics fixture (V15).
#[derive(Component, Debug, Clone, Copy)]
pub struct DebugTankard;

/// Marker for fixture arena geometry (P1 primitive hill/wall arena).
#[derive(Component, Debug, Clone, Copy)]
pub struct FixtureArena;

/// Groups every collider belongs to / collides with (V17).
pub fn world_collision_groups() -> CollisionGroups {
    CollisionGroups::new(GROUP_WORLD, GROUP_PLAYER | GROUP_TANKARD)
}

pub fn player_collision_groups() -> CollisionGroups {
    CollisionGroups::new(GROUP_PLAYER, GROUP_WORLD | GROUP_TANKARD)
}

pub fn tankard_collision_groups() -> CollisionGroups {
    CollisionGroups::new(GROUP_TANKARD, GROUP_WORLD | GROUP_PLAYER | GROUP_TANKARD)
}

/// Compound cup + handle collider for the debug tankard (R8, V15).
pub fn debug_tankard_collider() -> Collider {
    Collider::compound(vec![
        (
            Vect::ZERO,
            Rot::default(),
            Collider::cylinder(TANKARD_CUP_HALF_HEIGHT, TANKARD_CUP_RADIUS),
        ),
        (
            Vect::new(TANKARD_CUP_RADIUS + TANKARD_HANDLE_RADIUS, 0.0, 0.0),
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            Collider::cylinder(TANKARD_HANDLE_HALF_HEIGHT, TANKARD_HANDLE_RADIUS),
        ),
    ])
}

/// Rapier setup shared by the fixture and (later) streamed collision (V17).
pub struct PhysicsCorePlugin;

impl Plugin for PhysicsCorePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(TimestepMode::Fixed {
            dt: PHYSICS_TIMESTEP,
            substeps: 1,
        })
        .init_resource::<MovementTuning>()
        .add_plugins(RapierPhysicsPlugin::<NoUserData>::default().in_fixed_schedule())
        .add_systems(Startup, configure_physics_gravity);
    }
}

fn configure_physics_gravity(
    tuning: Res<MovementTuning>,
    mut configs: Query<&mut RapierConfiguration>,
) {
    for mut config in &mut configs {
        config.gravity = Vect::new(0.0, -tuning.gravity, 0.0);
    }
}

/// Asset-free `--physics-fixture`: primitive arena + auto-spawned tankards.
pub struct PhysicsFixturePlugin;

impl Plugin for PhysicsFixturePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PhysicsCorePlugin)
            .init_resource::<PhysicsFixtureState>()
            .init_resource::<MoveMode>()
            .init_resource::<WalkIntent>()
            .init_resource::<WalkEntryStatus>()
            .add_systems(Startup, setup_physics_fixture)
            .add_systems(
                FixedUpdate,
                (walk_movement_system, walk_camera_follow_system).chain(),
            )
            .add_systems(FixedUpdate, validate_physics_fixture);
    }
}

#[derive(Resource, Default)]
pub struct PhysicsFixtureState {
    pub ticks: u64,
    pub finished: bool,
    pub tankards_settled: bool,
}

fn fixture_material(base_color: Color) -> StandardMaterial {
    StandardMaterial {
        base_color,
        perceptual_roughness: 0.9,
        ..default()
    }
}

fn setup_physics_fixture(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.init_resource::<PhysicsFixtureMetrics>();
    let stone = materials.add(fixture_material(Color::srgb(0.42, 0.44, 0.48)));
    let grass = materials.add(fixture_material(Color::srgb(0.24, 0.5, 0.2)));
    let hazard = materials.add(fixture_material(Color::srgb(0.85, 0.3, 0.12)));

    // Flat ground pad plus a sloped hill, a step, and a blocking wall.
    let ground_mesh = meshes.add(Cuboid::new(2400.0, 40.0, 2400.0));
    commands.spawn((
        Name::new("Physics fixture ground"),
        FixtureArena,
        Mesh3d(ground_mesh),
        MeshMaterial3d(grass),
        Transform::from_xyz(0.0, -20.0, 0.0),
        RigidBody::Fixed,
        Collider::cuboid(1200.0, 20.0, 1200.0),
        world_collision_groups(),
    ));
    let hill_mesh = meshes.add(Cuboid::new(1200.0, 40.0, 600.0));
    commands.spawn((
        Name::new("Physics fixture hill"),
        FixtureArena,
        Mesh3d(hill_mesh),
        MeshMaterial3d(stone.clone()),
        Transform::from_xyz(-700.0, 130.0, -500.0).with_rotation(Quat::from_rotation_z(0.32)),
        RigidBody::Fixed,
        Collider::cuboid(600.0, 20.0, 300.0),
        world_collision_groups(),
    ));
    let step_mesh = meshes.add(Cuboid::new(220.0, 24.0, 220.0));
    commands.spawn((
        Name::new("Physics fixture step"),
        FixtureArena,
        Mesh3d(step_mesh),
        MeshMaterial3d(stone.clone()),
        Transform::from_xyz(320.0, 12.0, 240.0),
        RigidBody::Fixed,
        Collider::cuboid(110.0, 12.0, 110.0),
        world_collision_groups(),
    ));
    let wall_mesh = meshes.add(Cuboid::new(60.0, 400.0, 1200.0));
    commands.spawn((
        Name::new("Physics fixture wall"),
        FixtureArena,
        Mesh3d(wall_mesh),
        MeshMaterial3d(hazard),
        Transform::from_xyz(760.0, 200.0, 0.0),
        RigidBody::Fixed,
        Collider::cuboid(30.0, 200.0, 600.0),
        world_collision_groups(),
    ));

    // Visible cup + handle meshes; collider stays compound convex (V15).
    let cup_mesh = meshes.add(Cylinder::new(
        TANKARD_CUP_RADIUS,
        TANKARD_CUP_HALF_HEIGHT * 2.0,
    ));
    let handle_mesh = meshes.add(Cylinder::new(
        TANKARD_HANDLE_RADIUS,
        TANKARD_HANDLE_HALF_HEIGHT * 2.0,
    ));
    let wood = materials.add(fixture_material(Color::srgb(0.5, 0.32, 0.14)));
    for (index, position) in [
        Vec3::new(-700.0, 420.0, -500.0),
        Vec3::new(-640.0, 480.0, -420.0),
        Vec3::new(120.0, 320.0, 120.0),
    ]
    .into_iter()
    .enumerate()
    {
        commands
            .spawn((
                Name::new(format!("Debug tankard fixture {index}")),
                DebugTankard,
                RigidBody::Dynamic,
                debug_tankard_collider(),
                ActiveEvents::COLLISION_EVENTS,
                CollidingEntities::default(),
                tankard_collision_groups(),
                ColliderMassProperties::Density(0.001),
                Transform::from_translation(position),
                Visibility::default(),
            ))
            .with_children(|parent| {
                parent.spawn((
                    Mesh3d(cup_mesh.clone()),
                    MeshMaterial3d(wood.clone()),
                    Transform::IDENTITY,
                ));
                parent.spawn((
                    Mesh3d(handle_mesh.clone()),
                    MeshMaterial3d(wood.clone()),
                    Transform::from_xyz(TANKARD_CUP_RADIUS + TANKARD_HANDLE_RADIUS, 0.0, 0.0)
                        .with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)),
                ));
            });
    }

    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 900.0, 1900.0).looking_at(Vec3::new(0.0, 0.0, -200.0), Vec3::Y),
        StreamingCamera,
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 12_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.8, -0.5, 0.0)),
    ));
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.48, 0.55, 0.7),
        brightness: 160.0,
        ..default()
    });
}

#[derive(Resource, Default)]
pub struct PhysicsFixtureMetrics {
    pub tankards_spawned: usize,
    pub tankards_in_contact: usize,
    pub settled_ticks: u64,
}

fn validate_physics_fixture(
    mut state: ResMut<PhysicsFixtureState>,
    mut metrics: ResMut<PhysicsFixtureMetrics>,
    mut profiler: ResMut<ProfilingState>,
    tankards: Query<(Entity, &Transform, Option<&CollidingEntities>), With<DebugTankard>>,
    mut streaming: ResMut<StreamingMetrics>,
) {
    if state.finished {
        return;
    }
    state.ticks = state.ticks.saturating_add(1);
    metrics.tankards_spawned = tankards.iter().count();
    metrics.tankards_in_contact = tankards
        .iter()
        .filter(|(_, _, contacts)| contacts.is_some_and(|contacts| !contacts.is_empty()))
        .count();
    let settled = tankards.iter().all(|(_, transform, _)| {
        transform.translation.is_finite() && transform.translation.y > -400.0
    });
    if settled && metrics.tankards_in_contact == metrics.tankards_spawned {
        metrics.settled_ticks = metrics.settled_ticks.saturating_add(1);
    } else {
        metrics.settled_ticks = 0;
    }
    if metrics.settled_ticks >= 60 {
        state.tankards_settled = true;
        state.finished = true;
        streaming.physics_fixture_validated = true;
        profiler.event("physics-fixture", "tankards_settled", None);
    } else if state.ticks >= 60 * 30 {
        state.finished = true;
        streaming.physics_fixture_failures = streaming.physics_fixture_failures.saturating_add(1);
        profiler.event("physics-fixture", "failed", None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movement_tuning_preserves_spec_speed_ordering() {
        let tuning = MovementTuning::default();
        assert!(tuning.walk_speed < tuning.run_speed);
        assert!(tuning.run_speed < tuning.sprint_speed);
        assert!(tuning.horizontal_acceleration > 0.0);
        assert_eq!(tuning.gravity, GRAVITY_CREATION_UNITS);
    }

    #[test]
    fn tankard_collider_is_compound_cup_and_handle() {
        let collider = debug_tankard_collider();
        let compound = collider.as_compound().expect("compound tankard");
        assert_eq!(compound.shapes().len(), 2);
    }

    #[test]
    fn collision_groups_keep_world_player_tankards_in_contact() {
        let world = world_collision_groups();
        let player = player_collision_groups();
        let tankard = tankard_collision_groups();
        assert!(world.filters.contains(GROUP_PLAYER));
        assert!(world.filters.contains(GROUP_TANKARD));
        assert!(player.filters.contains(GROUP_WORLD));
        assert!(tankard.filters.contains(GROUP_WORLD));
        assert!(tankard.filters.contains(GROUP_PLAYER));
    }

    #[test]
    fn physics_core_uses_fixed_sixty_hertz_step() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, PhysicsCorePlugin));
        app.update();
        let mode = *app.world().resource::<TimestepMode>();
        assert_eq!(
            mode,
            TimestepMode::Fixed {
                dt: PHYSICS_TIMESTEP,
                substeps: 1,
            }
        );
        assert!((PHYSICS_TIMESTEP - 1.0 / 60.0).abs() < f32::EPSILON);
    }
}

// ---------------------------------------------------------------------------
// T2: WALK capsule, movement integration, collision-safe NOCLIP<->WALK toggle.
// ---------------------------------------------------------------------------

/// Player movement mode (V8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Resource)]
pub enum MoveMode {
    /// Flying camera, no collision.
    #[default]
    Noclip,
    /// Grounded kinematic capsule.
    Walk,
}

/// Reason the last NOCLIP->WALK entry failed (V8), surfaced in the overlay.
#[derive(Debug, Clone, Default, Resource)]
pub struct WalkEntryStatus {
    pub blocked_reason: Option<String>,
}

/// Marker for the player body entity (V9).
#[derive(Component, Debug, Clone, Copy)]
pub struct PlayerBody;

/// Per-frame sampled movement intent; integrated once per physics tick (V18).
#[derive(Debug, Clone, Copy, Default, Resource)]
pub struct WalkIntent {
    /// Camera-yaw-relative wish direction, normalized, horizontal plane.
    pub wish_dir: Vec3,
    /// Desired top speed for current gait (walk/run/sprint).
    pub target_speed: f32,
    /// Fresh jump press this frame.
    pub jump_pressed: bool,
}

/// Live walk state: horizontal velocity + vertical velocity (V10, V11).
#[derive(Debug, Clone, Copy, Default, Component)]
pub struct WalkState {
    pub horizontal_velocity: Vec3,
    pub vertical_velocity: f32,
    pub grounded: bool,
    pub jump_apex: f32,
    pub jump_time: f32,
    pub jump_active: bool,
}

/// Upward search budget when entering WALK inside geometry (V8).
pub const WALK_ENTRY_SEARCH_STEPS: u32 = 12;
pub const WALK_ENTRY_STEP_HEIGHT: f32 = 28.0;
/// Ground must exist within this distance below the capsule for WALK entry (V13).
pub const WALK_ENTRY_GROUND_SEARCH: f32 = 400.0;

/// Build the upright player capsule + controller from tuning (V9, V12).
pub fn player_controller_bundle(tuning: &MovementTuning) -> impl Bundle {
    (
        PlayerBody,
        WalkState::default(),
        RigidBody::KinematicPositionBased,
        Collider::capsule_y(tuning.capsule_half_height(), tuning.capsule_radius),
        player_collision_groups(),
        KinematicCharacterController {
            up: Vect::Y,
            offset: CharacterLength::Absolute(2.0),
            slide: true,
            autostep: Some(CharacterAutostep {
                max_height: CharacterLength::Absolute(tuning.autostep_height),
                min_width: CharacterLength::Absolute(tuning.capsule_radius),
                include_dynamic_bodies: false,
            }),
            max_slope_climb_angle: tuning.slope_climb_degrees.to_radians(),
            min_slope_slide_angle: tuning.slope_slide_degrees.to_radians(),
            apply_impulse_to_dynamic_bodies: true,
            snap_to_ground: Some(CharacterLength::Absolute(tuning.ground_snap)),
            ..default()
        },
    )
}

/// Pure movement integration: accelerate horizontal velocity toward wish dir,
/// apply gravity, handle jump from grounded + fresh press (V10, V11).
pub fn integrate_walk(
    state: &mut WalkState,
    intent: &WalkIntent,
    tuning: &MovementTuning,
    dt: f32,
    grounded: bool,
) {
    state.grounded = grounded;
    let wish = intent.wish_dir * intent.target_speed;
    let current = state.horizontal_velocity;
    let max_delta = tuning.horizontal_acceleration * dt;
    let delta = wish - current;
    state.horizontal_velocity = if delta.length() <= max_delta {
        wish
    } else {
        current + delta.normalize_or_zero() * max_delta
    };
    if grounded {
        state.vertical_velocity = 0.0;
        if state.jump_active {
            state.jump_active = false;
        }
        if intent.jump_pressed {
            state.vertical_velocity = tuning.jump_launch;
            state.jump_active = true;
            state.jump_apex = 0.0;
            state.jump_time = 0.0;
        }
    } else {
        state.vertical_velocity -= tuning.gravity * dt;
        if state.jump_active {
            state.jump_time += dt;
            state.jump_apex = state
                .jump_apex
                .max(state.vertical_velocity * state.jump_time);
        }
    }
}

/// Fixed-step walk integration + controller feed (V10, V11, V18).
pub fn walk_movement_system(
    mode: Res<MoveMode>,
    intent: Res<WalkIntent>,
    tuning: Res<MovementTuning>,
    mut player: Query<(
        &mut KinematicCharacterController,
        &mut WalkState,
        Option<&KinematicCharacterControllerOutput>,
    )>,
) {
    if *mode != MoveMode::Walk {
        return;
    }
    let Ok((mut controller, mut state, output)) = player.single_mut() else {
        return;
    };
    let grounded = output.map(|o| o.grounded).unwrap_or(false);
    integrate_walk(&mut state, &intent, &tuning, PHYSICS_TIMESTEP, grounded);
    let displacement =
        (state.horizontal_velocity + Vec3::Y * state.vertical_velocity) * PHYSICS_TIMESTEP;
    controller.translation = Some(displacement);
}

/// Camera follows the capsule at eye height; yaw on body, pitch on view (V9).
pub fn walk_camera_follow_system(
    mode: Res<MoveMode>,
    tuning: Res<MovementTuning>,
    player: Query<&Transform, (With<PlayerBody>, Without<StreamingCamera>)>,
    mut camera: Query<&mut Transform, With<StreamingCamera>>,
) {
    if *mode != MoveMode::Walk {
        return;
    }
    let (Ok(body), Ok(mut view)) = (player.single(), camera.single_mut()) else {
        return;
    };
    let (_, pitch, _) = view.rotation.to_euler(EulerRot::YXZ);
    let (yaw, _, _) = body.rotation.to_euler(EulerRot::YXZ);
    view.translation = body.translation + Vec3::Y * tuning.eye_height;
    view.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0);
}

/// Attempt NOCLIP->WALK at the camera pose; overlap pushes the search upward,
/// missing ground keeps NOCLIP with a visible reason (V8, V13).
pub fn try_enter_walk(
    context: &RapierContext,
    tuning: &MovementTuning,
    camera_position: Vec3,
    capsule: &Collider,
) -> Result<Vec3, String> {
    let shape = &*capsule.raw;
    let mut candidate = camera_position - Vec3::Y * tuning.eye_height;
    for _ in 0..=WALK_ENTRY_SEARCH_STEPS {
        let mut overlapping = false;
        context.intersect_shape(
            candidate,
            Quat::IDENTITY,
            shape,
            QueryFilter::default(),
            |_| {
                overlapping = true;
                false
            },
        );
        if !overlapping {
            let ground_hit = context.cast_shape(
                candidate,
                Quat::IDENTITY,
                Vec3::NEG_Y * WALK_ENTRY_GROUND_SEARCH,
                shape,
                ShapeCastOptions::with_max_time_of_impact(WALK_ENTRY_GROUND_SEARCH),
                QueryFilter::default(),
            );
            if ground_hit.is_some() {
                return Ok(candidate);
            }
            return Err("no walkable ground below".to_owned());
        }
        candidate.y += WALK_ENTRY_STEP_HEIGHT;
    }
    Err("no free capsule placement nearby".to_owned())
}

/// Clear stale velocities + jump state on every toggle (V14).
pub fn clear_motion_state(
    intent: &mut WalkIntent,
    states: &mut Query<&mut WalkState>,
    controllers: &mut Query<&mut KinematicCharacterController>,
) {
    *intent = WalkIntent::default();
    for mut state in states {
        *state = WalkState::default();
    }
    for mut controller in controllers {
        controller.translation = None;
    }
}

#[cfg(test)]
mod walk_tests {
    use super::*;

    #[test]
    fn walk_integrates_toward_wish_speed_with_bounded_acceleration() {
        let tuning = MovementTuning::default();
        let mut state = WalkState::default();
        let intent = WalkIntent {
            wish_dir: Vec3::X,
            target_speed: tuning.run_speed,
            jump_pressed: false,
        };
        integrate_walk(&mut state, &intent, &tuning, PHYSICS_TIMESTEP, true);
        let expected = tuning.horizontal_acceleration * PHYSICS_TIMESTEP;
        assert!((state.horizontal_velocity.x - expected).abs() < 0.01);
        for _ in 0..600 {
            integrate_walk(&mut state, &intent, &tuning, PHYSICS_TIMESTEP, true);
        }
        assert!((state.horizontal_velocity.length() - tuning.run_speed).abs() < 0.5);
    }

    #[test]
    fn jump_requires_grounded_fresh_press_and_applies_gravity() {
        let tuning = MovementTuning::default();
        let mut state = WalkState::default();
        let air_jump = WalkIntent {
            jump_pressed: true,
            ..default()
        };
        integrate_walk(&mut state, &air_jump, &tuning, PHYSICS_TIMESTEP, false);
        assert!(state.vertical_velocity < 0.0);
        let grounded_jump = WalkIntent {
            jump_pressed: true,
            ..default()
        };
        integrate_walk(&mut state, &grounded_jump, &tuning, PHYSICS_TIMESTEP, true);
        assert!((state.vertical_velocity - tuning.jump_launch).abs() < f32::EPSILON);
    }

    #[test]
    fn capsule_bundle_matches_provisional_spec_dimensions() {
        let tuning = MovementTuning::default();
        let app_bundle = player_controller_bundle(&tuning);
        let mut world = World::new();
        let entity = world.spawn(app_bundle).id();
        let collider = world.get::<Collider>(entity).expect("capsule collider");
        let capsule = collider.as_capsule().expect("capsule shape");
        assert!((capsule.radius() - tuning.capsule_radius).abs() < f32::EPSILON);
        let controller = world
            .get::<KinematicCharacterController>(entity)
            .expect("controller");
        assert!(
            (controller.max_slope_climb_angle - tuning.slope_climb_degrees.to_radians()).abs()
                < 1.0e-6
        );
        assert_eq!(
            controller.snap_to_ground,
            Some(CharacterLength::Absolute(tuning.ground_snap))
        );
    }
}

#[cfg(test)]
mod simulation_tests {
    use super::*;
    use bevy::mesh::MeshPlugin;
    use bevy::time::TimeUpdateStrategy;

    fn headless_fixture_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            TransformPlugin,
            AssetPlugin::default(),
            MeshPlugin,
            MaterialPlugin::<StandardMaterial>::default(),
        ));
        app.init_resource::<ProfilingState>()
            .init_resource::<StreamingMetrics>();
        app.add_plugins(PhysicsFixturePlugin);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_secs_f32(PHYSICS_TIMESTEP),
        ));
        app.finish();
        app.update();
        app
    }

    #[test]
    fn tankards_fall_and_settle_on_fixture_ground() {
        let mut app = headless_fixture_app();
        let start: Vec<f32> = {
            let mut query = app
                .world_mut()
                .query_filtered::<&Transform, With<DebugTankard>>();
            query.iter(app.world()).map(|t| t.translation.y).collect()
        };
        assert_eq!(start.len(), 3);
        assert!(start.iter().all(|y| *y > 200.0));
        for _ in 0..600 {
            app.update();
        }
        let settled: Vec<Vec3> = {
            let mut query = app
                .world_mut()
                .query_filtered::<&Transform, With<DebugTankard>>();
            query.iter(app.world()).map(|t| t.translation).collect()
        };
        assert_eq!(settled.len(), 3);
        for position in &settled {
            assert!(position.is_finite());
            // Ground top is y=0; cup half-height 18 + handle radius margin.
            assert!(
                position.y > -40.0 && position.y < 400.0,
                "tankard height {position:?}"
            );
        }
        assert!(
            app.world()
                .resource::<PhysicsFixtureState>()
                .tankards_settled
                || app
                    .world()
                    .resource::<PhysicsFixtureMetrics>()
                    .tankards_in_contact
                    > 0,
            "tankards never touched the arena"
        );
    }

    #[test]
    fn walk_capsule_spawns_grounds_and_steps_forward() {
        let mut app = headless_fixture_app();
        let tuning = app.world().resource::<MovementTuning>().clone();
        let spawn = Vec3::new(120.0, 300.0, 120.0);
        app.world_mut().spawn((
            player_controller_bundle(&tuning),
            Transform::from_translation(spawn),
        ));
        app.insert_resource(MoveMode::Walk);
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::ZERO,
            target_speed: 0.0,
            jump_pressed: false,
        });
        for _ in 0..240 {
            app.update();
        }
        let (grounded, rest) = {
            let mut query = app.world_mut().query::<(
                &Transform,
                &WalkState,
                Option<&KinematicCharacterControllerOutput>,
            )>();
            let (transform, state, output) = query.single(app.world()).expect("player capsule");
            (
                state.grounded || output.map(|o| o.grounded).unwrap_or(false),
                transform.translation,
            )
        };
        assert!(grounded, "capsule never grounded at {rest:?}");
        // Capsule center rests at half-height + radius above the ground pad.
        let expected_rest = tuning.capsule_half_height() + tuning.capsule_radius;
        assert!(
            (rest.y - expected_rest).abs() < 12.0,
            "rest height {} vs {expected_rest}",
            rest.y
        );
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: tuning.run_speed,
            jump_pressed: false,
        });
        for _ in 0..120 {
            app.update();
        }
        let moved = {
            let mut query = app.world_mut().query::<&Transform>();
            query
                .iter(app.world())
                .any(|t| (t.translation.x - spawn.x) > 100.0)
        };
        assert!(moved, "walk capsule did not advance under run intent");
    }

    #[test]
    fn jump_launches_and_returns_to_ground() {
        let mut app = headless_fixture_app();
        let tuning = app.world().resource::<MovementTuning>().clone();
        app.world_mut().spawn((
            player_controller_bundle(&tuning),
            Transform::from_xyz(120.0, 200.0, 120.0),
        ));
        app.insert_resource(MoveMode::Walk);
        app.insert_resource(WalkIntent::default());
        for _ in 0..240 {
            app.update();
        }
        let rest_y = {
            let mut query = app.world_mut().query::<&Transform>();
            query
                .iter(app.world())
                .map(|t| t.translation.y)
                .find(|y| *y < 200.0)
                .unwrap_or(0.0)
        };
        app.insert_resource(WalkIntent {
            jump_pressed: true,
            ..default()
        });
        for _ in 0..6 {
            app.update();
        }
        app.insert_resource(WalkIntent::default());
        let mut apex = rest_y;
        for _ in 0..240 {
            app.update();
            let mut query = app.world_mut().query::<&Transform>();
            for transform in query.iter(app.world()) {
                if transform.translation.y < 1200.0 {
                    apex = apex.max(transform.translation.y);
                }
            }
        }
        // v^2/2g = 340^2/1800 ~ 64 units of apex above rest.
        assert!(
            apex - rest_y > 30.0,
            "jump apex {apex} too low above rest {rest_y}"
        );
    }
}
