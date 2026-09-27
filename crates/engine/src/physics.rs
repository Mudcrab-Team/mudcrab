//! Player movement physics: one controller for Riverwood and the regression fixture.
//! All units are Creation units.

use bevy::{
    input::mouse::MouseMotion,
    prelude::*,
    window::{CursorGrabMode, CursorOptions},
};
use bevy_rapier3d::prelude::*;

use crate::{
    profiling::ProfilingState,
    streaming::{StreamingMetrics, TerrainCollider},
    world::components::StreamingCamera,
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
            autostep_height: 36.0,
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
        app.add_plugins(PlayerControlsPlugin)
            .init_resource::<PhysicsFixtureState>()
            .add_systems(
                Startup,
                (setup_physics_fixture, setup_fixture_player).chain(),
            )
            .add_systems(FixedUpdate, validate_physics_fixture);
    }
}

/// Mouse and movement systems shared by Riverwood and the regression fixture.
pub struct PlayerControlsPlugin;

impl Plugin for PlayerControlsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PhysicsCorePlugin)
            .init_resource::<MoveMode>()
            .init_resource::<WalkIntent>()
            .init_resource::<LookIntent>()
            .init_resource::<WalkEntryStatus>()
            .init_resource::<CursorCapture>()
            .add_systems(
                Update,
                (
                    cursor_lifecycle_system,
                    look_input_system,
                    noclip_flight_system,
                    walk_intent_system,
                    toggle_mode_system,
                    walk_camera_follow_system,
                    overlay_system,
                )
                    .chain(),
            )
            .add_systems(FixedUpdate, walk_movement_system);
    }
}

pub struct WorldPlayerPlugin;

impl Plugin for WorldPlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PlayerControlsPlugin)
            .init_resource::<HeldTankard>()
            .add_systems(PostStartup, setup_world_player)
            .add_systems(Update, (world_tankard_input, move_held_tankard).chain());
    }
}

#[derive(Resource, Default)]
struct HeldTankard(Option<Entity>);

#[derive(Resource)]
struct TankardVisuals {
    cup: Handle<Mesh>,
    handle: Handle<Mesh>,
    wood: Handle<StandardMaterial>,
}

fn tankard_visuals(
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> TankardVisuals {
    TankardVisuals {
        cup: meshes.add(Cylinder::new(
            TANKARD_CUP_RADIUS,
            TANKARD_CUP_HALF_HEIGHT * 2.0,
        )),
        handle: meshes.add(Cylinder::new(
            TANKARD_HANDLE_RADIUS,
            TANKARD_HANDLE_HALF_HEIGHT * 2.0,
        )),
        wood: materials.add(fixture_material(Color::srgb(0.5, 0.32, 0.14))),
    }
}

fn spawn_debug_tankard(commands: &mut Commands, visuals: &TankardVisuals, position: Vec3) {
    commands
        .spawn((
            Name::new("Debug tankard"),
            DebugTankard,
            RigidBody::Dynamic,
            debug_tankard_collider(),
            ActiveEvents::COLLISION_EVENTS,
            CollidingEntities::default(),
            tankard_collision_groups(),
            ColliderMassProperties::Density(0.001),
            Velocity::zero(),
            Transform::from_translation(position),
            Visibility::default(),
        ))
        .with_children(|parent| {
            parent.spawn((
                Mesh3d(visuals.cup.clone()),
                MeshMaterial3d(visuals.wood.clone()),
                Transform::IDENTITY,
            ));
            parent.spawn((
                Mesh3d(visuals.handle.clone()),
                MeshMaterial3d(visuals.wood.clone()),
                Transform::from_xyz(TANKARD_CUP_RADIUS + TANKARD_HANDLE_RADIUS, 0.0, 0.0)
                    .with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)),
            ));
        });
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
        Transform::from_xyz(-700.0, 190.0, -500.0).with_rotation(Quat::from_rotation_z(-0.32)),
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
    let visuals = tankard_visuals(&mut meshes, &mut materials);
    for position in [
        Vec3::new(-700.0, 420.0, -500.0),
        Vec3::new(-640.0, 480.0, -420.0),
        Vec3::new(120.0, 320.0, 120.0),
    ] {
        spawn_debug_tankard(&mut commands, &visuals, position);
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
            // NOTE: upstream Rapier 0.35 manifold-transfer panic (see T4 gate
            // hill test); re-enable with push validation once upgraded.
            apply_impulse_to_dynamic_bodies: false,
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
    context: ReadRapierContext,
    mut status: ResMut<WalkEntryStatus>,
    mut player: Query<(
        &Transform,
        &mut KinematicCharacterController,
        &mut WalkState,
        Option<&KinematicCharacterControllerOutput>,
    )>,
) {
    if *mode != MoveMode::Walk {
        return;
    }
    let Ok((pose, mut controller, mut state, output)) = player.single_mut() else {
        return;
    };
    let ground_available = context.single().is_ok_and(|context| {
        context
            .cast_ray(
                pose.translation,
                Vec3::NEG_Y,
                WALK_ENTRY_GROUND_SEARCH,
                true,
                QueryFilter::default().groups(CollisionGroups::new(GROUP_PLAYER, GROUP_WORLD)),
            )
            .is_some()
    });
    if !ground_available {
        controller.translation = None;
        *state = WalkState::default();
        status.blocked_reason = Some("terrain loading below player".to_owned());
        return;
    }
    status.blocked_reason = None;
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
    look: Res<LookIntent>,
    player: Query<&Transform, (With<PlayerBody>, Without<StreamingCamera>)>,
    mut camera: Query<&mut Transform, With<StreamingCamera>>,
) {
    if *mode != MoveMode::Walk {
        return;
    }
    let (Ok(body), Ok(mut view)) = (player.single(), camera.single_mut()) else {
        return;
    };
    let (yaw, _, _) = body.rotation.to_euler(EulerRot::YXZ);
    view.translation = body.translation + Vec3::Y * tuning.eye_height;
    view.rotation = Quat::from_euler(EulerRot::YXZ, yaw, look.pitch, 0.0);
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
            QueryFilter::default().groups(CollisionGroups::new(GROUP_PLAYER, GROUP_WORLD)),
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
                QueryFilter::default().groups(CollisionGroups::new(GROUP_PLAYER, GROUP_WORLD)),
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
pub(crate) mod headless {
    use super::*;
    use bevy::mesh::MeshPlugin;
    use bevy::time::TimeUpdateStrategy;

    pub(crate) fn fixture_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            TransformPlugin,
            bevy::input::InputPlugin,
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

    /// Place the fixture player body, enable its rigid body, enter WALK.
    pub(crate) fn place_player(app: &mut App, position: Vec3) {
        let mut query = app.world_mut().query_filtered::<(
            Entity,
            &mut Transform,
            Option<&RigidBodyDisabled>,
        ), With<PlayerBody>>();
        let (entity, mut transform, disabled) = query
            .single_mut(app.world_mut())
            .expect("fixture player body");
        transform.translation = position;
        if disabled.is_some() {
            app.world_mut()
                .entity_mut(entity)
                .remove::<RigidBodyDisabled>();
        }
        app.insert_resource(MoveMode::Walk);
        app.insert_resource(WalkIntent::default());
    }

    pub(crate) fn player_pose(app: &mut App) -> (Vec3, bool) {
        let mut query = app.world_mut().query_filtered::<(
            &Transform,
            &WalkState,
            Option<&KinematicCharacterControllerOutput>,
        ), With<PlayerBody>>();
        let (transform, state, output) = query.single(app.world()).expect("player body");
        (
            transform.translation,
            state.grounded || output.map(|o| o.grounded).unwrap_or(false),
        )
    }
}

#[cfg(test)]
mod simulation_tests {
    use super::headless;
    use super::*;

    #[test]
    fn tankards_fall_and_settle_on_fixture_ground() {
        let mut app = headless::fixture_app();
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
        let mut app = headless::fixture_app();
        let tuning = app.world().resource::<MovementTuning>().clone();
        let spawn = Vec3::new(120.0, 300.0, 120.0);
        headless::place_player(&mut app, spawn);
        for _ in 0..240 {
            app.update();
        }
        let (rest, grounded) = headless::player_pose(&mut app);
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
        let mut app = headless::fixture_app();
        headless::place_player(&mut app, Vec3::new(120.0, 200.0, 120.0));
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

// ---------------------------------------------------------------------------
// T3: mouse-look NOCLIP, V toggle, overlay, cursor lifecycle.
// ---------------------------------------------------------------------------

/// Noclip flight tuning (V6).
pub const NOCLIP_SPEED: f32 = 900.0;
pub const NOCLIP_FAST_MULTIPLIER: f32 = 4.0;
pub const MOUSE_SENSITIVITY: f32 = 0.0025;
pub const MAX_PITCH_RADIANS: f32 = 1.5533;

/// Sampled look intent (V6, V18).
#[derive(Debug, Clone, Copy, Default, Resource)]
pub struct LookIntent {
    pub yaw: f32,
    pub pitch: f32,
}

/// Marker for the NOCLIP status overlay text (V7).
#[derive(Component, Debug, Clone, Copy)]
pub struct NoclipOverlay;

/// Marker for the camera owned by the NOCLIP/WALK controller.
#[derive(Component, Debug, Clone, Copy)]
pub struct ControlledCamera;

/// Cursor capture state machine (V6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Resource)]
pub enum CursorCapture {
    #[default]
    Released,
    Captured,
}

/// Clamp pitch into the bounded look range (V6).
pub fn clamp_pitch(pitch: f32) -> f32 {
    pitch.clamp(-MAX_PITCH_RADIANS, MAX_PITCH_RADIANS)
}

/// Sample mouse motion into look intent; uncaptured pointer yields nothing (V6).
pub fn sample_look_intent(capture: &CursorCapture, mouse_delta: Vec2, intent: &mut LookIntent) {
    if *capture != CursorCapture::Captured {
        return;
    }
    intent.yaw -= mouse_delta.x * MOUSE_SENSITIVITY;
    intent.pitch = clamp_pitch(intent.pitch - mouse_delta.y * MOUSE_SENSITIVITY);
}

/// Noclip displacement from held keys relative to the view (V6).
pub fn noclip_displacement(
    forward: Vec3,
    right: Vec3,
    fly_up: bool,
    fly_down: bool,
    strafe: Vec2,
    fast: bool,
    dt: f32,
) -> Vec3 {
    let mut direction = forward * -strafe.y + right * strafe.x;
    if fly_up {
        direction += Vec3::Y;
    }
    if fly_down {
        direction -= Vec3::Y;
    }
    let speed = if fast {
        NOCLIP_SPEED * NOCLIP_FAST_MULTIPLIER
    } else {
        NOCLIP_SPEED
    };
    direction.normalize_or_zero() * speed * dt
}

fn noclip_overlay_text(mode: MoveMode, status: &WalkEntryStatus) -> String {
    let base = match mode {
        MoveMode::Noclip => "NOCLIP: ON  [V]",
        MoveMode::Walk => "NOCLIP: OFF  [V]",
    };
    match &status.blocked_reason {
        Some(reason) => format!("{base}  [T] tankard  [E] grab/drop  WALK blocked: {reason}"),
        None => format!("{base}  [T] tankard  [E] grab/drop"),
    }
}

fn setup_fixture_player(
    mut commands: Commands,
    tuning: Res<MovementTuning>,
    mut look: ResMut<LookIntent>,
    camera: Query<(Entity, &Transform), With<StreamingCamera>>,
) {
    let Ok((camera, view)) = camera.single() else {
        return;
    };
    setup_controlled_player(&mut commands, &tuning, &mut look, camera, view);
}

fn setup_world_player(
    mut commands: Commands,
    tuning: Res<MovementTuning>,
    mut look: ResMut<LookIntent>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    camera: Query<(Entity, &Transform), With<StreamingCamera>>,
) {
    let Ok((camera, view)) = camera.single() else {
        return;
    };
    setup_controlled_player(&mut commands, &tuning, &mut look, camera, view);
    commands.insert_resource(tankard_visuals(&mut meshes, &mut materials));
}

fn setup_controlled_player(
    commands: &mut Commands,
    tuning: &MovementTuning,
    look: &mut LookIntent,
    camera: Entity,
    view: &Transform,
) {
    let (yaw, pitch, _) = view.rotation.to_euler(EulerRot::YXZ);
    *look = LookIntent { yaw, pitch };
    let body = commands
        .spawn((
            player_controller_bundle(tuning),
            Transform::from_translation(view.translation - Vec3::Y * tuning.eye_height),
        ))
        .id();
    // Noclip starts ON with the capsule disabled (V5, V8).
    commands.entity(body).insert(RigidBodyDisabled);
    commands.entity(camera).insert(ControlledCamera);
    commands.spawn((
        Name::new("Noclip overlay"),
        NoclipOverlay,
        Text::new("NOCLIP: ON  [V]    [T] tankard  [E] grab/drop"),
        TextFont::from_font_size(18.0),
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(12.0),
            left: Val::Px(12.0),
            ..default()
        },
    ));
}

/// Riverwood test objects stay in world coordinates and are never children of a streamed cell.
#[allow(clippy::too_many_arguments)]
fn world_tankard_input(
    keyboard: Res<ButtonInput<KeyCode>>,
    capture: Res<CursorCapture>,
    camera: Query<&Transform, (With<StreamingCamera>, With<ControlledCamera>)>,
    terrain: Query<(), With<TerrainCollider>>,
    tankards: Query<(), With<DebugTankard>>,
    player: Query<Entity, With<PlayerBody>>,
    visuals: Option<Res<TankardVisuals>>,
    mut held: ResMut<HeldTankard>,
    context: ReadRapierContext,
    mut commands: Commands,
) {
    if *capture != CursorCapture::Captured {
        return;
    }
    let (Ok(camera), Ok(context)) = (camera.single(), context.single()) else {
        return;
    };
    if keyboard.just_pressed(KeyCode::KeyT)
        && tankards.iter().count() < MAX_LIVE_TANKARDS
        && let Some(visuals) = visuals
    {
        let horizontal = camera.forward().as_vec3().with_y(0.0).normalize_or_zero();
        let origin = camera.translation + horizontal * 140.0 + Vec3::Y * 300.0;
        let is_terrain = |entity| terrain.get(entity).is_ok();
        if let Some((_, distance)) = context.cast_ray(
            origin,
            Vec3::NEG_Y,
            700.0,
            true,
            QueryFilter::default().predicate(&is_terrain),
        ) {
            spawn_debug_tankard(
                &mut commands,
                &visuals,
                origin - Vec3::Y * distance + Vec3::Y * 120.0,
            );
        }
    }
    if !keyboard.just_pressed(KeyCode::KeyE) {
        return;
    }
    if let Some(entity) = held.0.take() {
        if tankards.get(entity).is_ok() {
            commands
                .entity(entity)
                .remove::<(RigidBodyDisabled, ColliderDisabled)>();
            commands.entity(entity).insert(Velocity::zero());
        }
        return;
    }
    let filter = if let Ok(player) = player.single() {
        QueryFilter::default().exclude_rigid_body(player)
    } else {
        QueryFilter::default()
    };
    if let Some((entity, _)) = context.cast_ray(
        camera.translation,
        camera.forward().as_vec3(),
        240.0,
        true,
        filter,
    ) && tankards.get(entity).is_ok()
    {
        commands
            .entity(entity)
            .insert((RigidBodyDisabled, ColliderDisabled, Velocity::zero()));
        held.0 = Some(entity);
    }
}

#[allow(clippy::type_complexity)]
fn move_held_tankard(
    mut held: ResMut<HeldTankard>,
    camera: Query<
        &Transform,
        (
            With<StreamingCamera>,
            With<ControlledCamera>,
            Without<DebugTankard>,
        ),
    >,
    mut tankards: Query<&mut Transform, (With<DebugTankard>, Without<StreamingCamera>)>,
) {
    let Some(entity) = held.0 else { return };
    let (Ok(camera), Ok(mut tankard)) = (camera.single(), tankards.get_mut(entity)) else {
        held.0 = None;
        return;
    };
    tankard.translation = camera.translation + camera.forward().as_vec3() * 110.0;
}

#[allow(clippy::too_many_arguments)]
fn cursor_lifecycle_system(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    keyboard: Res<ButtonInput<KeyCode>>,
    windows: Query<(Entity, Option<&Window>)>,
    mut capture: ResMut<CursorCapture>,
    mut cursor_options: Query<&mut CursorOptions>,
    mut intent: ResMut<WalkIntent>,
    mut states: Query<&mut WalkState>,
    mut controllers: Query<&mut KinematicCharacterController>,
) {
    // Focus loss or Escape releases; click recaptures (V6).
    let focused = windows
        .iter()
        .all(|(_, window)| window.map(|window| window.focused).unwrap_or(true));
    if !focused || keyboard.just_pressed(KeyCode::Escape) {
        if *capture == CursorCapture::Captured {
            *capture = CursorCapture::Released;
            clear_motion_state(&mut intent, &mut states, &mut controllers);
        }
    } else if mouse_buttons.just_pressed(MouseButton::Left) && *capture == CursorCapture::Released {
        *capture = CursorCapture::Captured;
    }
    let (grab_mode, visible) = match *capture {
        CursorCapture::Captured => (CursorGrabMode::Locked, false),
        CursorCapture::Released => (CursorGrabMode::None, true),
    };
    for mut options in &mut cursor_options {
        options.grab_mode = grab_mode;
        options.visible = visible;
    }
}

fn look_input_system(
    capture: Res<CursorCapture>,
    mut mouse_motion: MessageReader<MouseMotion>,
    mut intent: ResMut<LookIntent>,
) {
    let mut delta = Vec2::ZERO;
    for motion in mouse_motion.read() {
        delta += motion.delta;
    }
    sample_look_intent(&capture, delta, &mut intent);
}

fn noclip_flight_system(
    mode: Res<MoveMode>,
    capture: Res<CursorCapture>,
    time: Res<Time>,
    keyboard: Res<ButtonInput<KeyCode>>,
    look: Res<LookIntent>,
    mut camera: Query<&mut Transform, (With<StreamingCamera>, With<ControlledCamera>)>,
) {
    if *mode != MoveMode::Noclip || *capture != CursorCapture::Captured {
        return;
    }
    let Ok(mut transform) = camera.single_mut() else {
        return;
    };
    transform.rotation = Quat::from_euler(EulerRot::YXZ, look.yaw, look.pitch, 0.0);
    let forward = transform.forward().as_vec3();
    let right = transform.right().as_vec3();
    let strafe = Vec2::new(
        (keyboard.pressed(KeyCode::KeyD) as i8 - keyboard.pressed(KeyCode::KeyA) as i8) as f32,
        (keyboard.pressed(KeyCode::KeyS) as i8 - keyboard.pressed(KeyCode::KeyW) as i8) as f32,
    );
    transform.translation += noclip_displacement(
        forward,
        right,
        keyboard.pressed(KeyCode::Space),
        keyboard.pressed(KeyCode::ControlLeft) || keyboard.pressed(KeyCode::ControlRight),
        strafe,
        keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight),
        time.delta_secs(),
    );
}

fn walk_intent_system(
    mode: Res<MoveMode>,
    capture: Res<CursorCapture>,
    keyboard: Res<ButtonInput<KeyCode>>,
    tuning: Res<MovementTuning>,
    look: Res<LookIntent>,
    mut intent: ResMut<WalkIntent>,
    mut player: Query<&mut Transform, (With<PlayerBody>, Without<StreamingCamera>)>,
) {
    if *mode != MoveMode::Walk || *capture != CursorCapture::Captured {
        return;
    }
    let Ok(mut body) = player.single_mut() else {
        return;
    };
    // Yaw moves body, pitch moves view (V9).
    body.rotation = Quat::from_axis_angle(Vec3::Y, look.yaw);
    let forward = Vec3::new(-look.yaw.sin(), 0.0, -look.yaw.cos());
    let right = Vec3::new(look.yaw.cos(), 0.0, -look.yaw.sin());
    let strafe = Vec2::new(
        (keyboard.pressed(KeyCode::KeyD) as i8 - keyboard.pressed(KeyCode::KeyA) as i8) as f32,
        (keyboard.pressed(KeyCode::KeyS) as i8 - keyboard.pressed(KeyCode::KeyW) as i8) as f32,
    );
    let wish = (right * strafe.x - forward * strafe.y).normalize_or_zero();
    let slow = keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight);
    let sprint = keyboard.pressed(KeyCode::AltLeft) || keyboard.pressed(KeyCode::AltRight);
    intent.wish_dir = wish;
    intent.target_speed = if sprint {
        tuning.sprint_speed
    } else if slow {
        tuning.walk_speed
    } else if wish.length_squared() > 0.0 {
        tuning.run_speed
    } else {
        0.0
    };
    intent.jump_pressed = keyboard.just_pressed(KeyCode::Space);
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn toggle_mode_system(
    keyboard: Res<ButtonInput<KeyCode>>,
    capture: Res<CursorCapture>,
    tuning: Res<MovementTuning>,
    context: ReadRapierContext,
    mut mode: ResMut<MoveMode>,
    mut status: ResMut<WalkEntryStatus>,
    mut intent: ResMut<WalkIntent>,
    mut look: ResMut<LookIntent>,
    camera: Query<&Transform, (With<StreamingCamera>, With<ControlledCamera>)>,
    mut player: Query<
        (Entity, &mut Transform, &Collider),
        (With<PlayerBody>, Without<StreamingCamera>),
    >,
    mut states: Query<&mut WalkState>,
    mut controllers: Query<&mut KinematicCharacterController>,
    mut commands: Commands,
) {
    if !keyboard.just_pressed(KeyCode::KeyV) || *capture != CursorCapture::Captured {
        return;
    }
    match *mode {
        MoveMode::Noclip => {
            let (Ok(camera), Ok((entity, mut body, collider)), Ok(context)) =
                (camera.single(), player.single_mut(), context.single())
            else {
                return;
            };
            match try_enter_walk(&context, &tuning, camera.translation, collider) {
                Ok(spawn) => {
                    body.translation = spawn;
                    let (yaw, _, _) = camera.rotation.to_euler(EulerRot::YXZ);
                    body.rotation = Quat::from_axis_angle(Vec3::Y, yaw);
                    look.yaw = yaw;
                    commands.entity(entity).remove::<RigidBodyDisabled>();
                    status.blocked_reason = None;
                    *mode = MoveMode::Walk;
                }
                Err(reason) => {
                    status.blocked_reason = Some(reason);
                }
            }
        }
        MoveMode::Walk => {
            let Ok((entity, _, _)) = player.single() else {
                return;
            };
            commands.entity(entity).insert(RigidBodyDisabled);
            status.blocked_reason = None;
            *mode = MoveMode::Noclip;
        }
    }
    clear_motion_state(&mut intent, &mut states, &mut controllers);
}

fn overlay_system(
    mode: Res<MoveMode>,
    status: Res<WalkEntryStatus>,
    mut overlay: Query<&mut Text, With<NoclipOverlay>>,
) {
    let Ok(mut text) = overlay.single_mut() else {
        return;
    };
    **text = noclip_overlay_text(*mode, &status);
}

#[cfg(test)]
mod noclip_tests {
    use super::*;

    #[test]
    fn pitch_clamps_inside_bounded_range() {
        assert_eq!(clamp_pitch(5.0), MAX_PITCH_RADIANS);
        assert_eq!(clamp_pitch(-5.0), -MAX_PITCH_RADIANS);
        assert_eq!(clamp_pitch(0.3), 0.3);
    }

    #[test]
    fn uncaptured_pointer_yields_no_look_intent() {
        let mut intent = LookIntent::default();
        sample_look_intent(&CursorCapture::Released, Vec2::new(40.0, 20.0), &mut intent);
        assert_eq!(intent.yaw, 0.0);
        assert_eq!(intent.pitch, 0.0);
        sample_look_intent(&CursorCapture::Captured, Vec2::new(40.0, 20.0), &mut intent);
        assert!(intent.yaw < 0.0);
        assert!(intent.pitch < 0.0);
    }

    #[test]
    fn noclip_flies_relative_to_view_with_fast_multiplier() {
        let slow = noclip_displacement(
            Vec3::NEG_Z,
            Vec3::X,
            false,
            false,
            Vec2::new(0.0, -1.0),
            false,
            1.0,
        );
        assert!((slow - Vec3::NEG_Z * NOCLIP_SPEED).length() < 0.01);
        let fast = noclip_displacement(
            Vec3::NEG_Z,
            Vec3::X,
            false,
            false,
            Vec2::new(0.0, -1.0),
            true,
            1.0,
        );
        assert!((fast.length() - NOCLIP_SPEED * NOCLIP_FAST_MULTIPLIER).abs() < 0.01);
        let rise = noclip_displacement(Vec3::NEG_Z, Vec3::X, true, false, Vec2::ZERO, false, 1.0);
        assert!((rise - Vec3::Y * NOCLIP_SPEED).length() < 0.01);
    }

    #[test]
    fn overlay_reports_mode_with_key_hint_and_block_reason() {
        let status = WalkEntryStatus::default();
        assert_eq!(
            noclip_overlay_text(MoveMode::Noclip, &status),
            "NOCLIP: ON  [V]  [T] tankard  [E] grab/drop"
        );
        assert_eq!(
            noclip_overlay_text(MoveMode::Walk, &status),
            "NOCLIP: OFF  [V]  [T] tankard  [E] grab/drop"
        );
        let blocked = WalkEntryStatus {
            blocked_reason: Some("no walkable ground below".to_owned()),
        };
        assert!(noclip_overlay_text(MoveMode::Noclip, &blocked).contains("WALK blocked"));
    }
}

/// P1 gate evidence (T4, V16): slope/wall/step behavior, toggle state
/// agreement, fixed-step determinism, and render-origin rebase alignment.
#[cfg(test)]
mod gate_tests {
    use super::headless;
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn walk_crosses_narrow_raised_floor_join() {
        let mut app = headless::fixture_app();
        for (x, width, top) in [(0.0, 12.0, 15.0), (206.0, 400.0, 30.0)] {
            app.world_mut().spawn((
                RigidBody::Fixed,
                Collider::cuboid(width * 0.5, top * 0.5, 80.0),
                world_collision_groups(),
                Transform::from_xyz(x, top * 0.5, 850.0),
            ));
        }
        headless::place_player(&mut app, Vec3::new(-150.0, 200.0, 850.0));
        for _ in 0..150 {
            app.update();
        }
        let start = headless::player_pose(&mut app).0;
        assert!(start.y < 80.0, "player never grounded: {start:?}");
        let tuning = app.world().resource::<MovementTuning>();
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: tuning.run_speed,
            jump_pressed: false,
        });
        let mut peak = start.y;
        for _ in 0..180 {
            app.update();
            peak = peak.max(headless::player_pose(&mut app).0.y);
        }
        let (end, grounded) = headless::player_pose(&mut app);
        assert!(
            end.x > 120.0 && peak > start.y + 25.0 && grounded,
            "player stuck at raised floor join: {start:?} -> {end:?}, peak={peak}, grounded={grounded}"
        );
    }

    #[test]
    fn walk_camera_pitch_uses_current_mouse_intent() {
        let mut app = headless::fixture_app();
        headless::place_player(&mut app, Vec3::new(120.0, 300.0, 120.0));
        app.world_mut().resource_mut::<LookIntent>().pitch = 0.6;
        app.update();
        let mut camera = app
            .world_mut()
            .query_filtered::<&Transform, With<ControlledCamera>>();
        let view = camera.single(app.world()).expect("controlled camera");
        let (_, pitch, _) = view.rotation.to_euler(EulerRot::YXZ);
        assert!((pitch - 0.6).abs() < 0.001, "WALK pitch was {pitch}");
        let mut body = app
            .world_mut()
            .query_filtered::<&Transform, With<PlayerBody>>();
        let pose = body.single(app.world()).expect("player body");
        assert!(pose.rotation.x.abs() < 0.001 && pose.rotation.z.abs() < 0.001);
    }

    #[test]
    fn riverwood_tankard_spawn_pickup_and_drop() {
        let mut app = headless::fixture_app();
        app.init_resource::<HeldTankard>();
        let visuals = {
            let world = app.world_mut();
            world.resource_scope(|world, mut meshes: Mut<Assets<Mesh>>| {
                tankard_visuals(
                    &mut meshes,
                    &mut world.resource_mut::<Assets<StandardMaterial>>(),
                )
            })
        };
        app.insert_resource(visuals);
        let ground = {
            let mut query = app
                .world_mut()
                .query_filtered::<(Entity, &Transform), With<FixtureArena>>();
            query
                .iter(app.world())
                .find(|(_, pose)| pose.translation.y == -20.0)
                .unwrap()
                .0
        };
        app.world_mut().entity_mut(ground).insert(TerrainCollider);
        let camera = {
            let mut query = app
                .world_mut()
                .query_filtered::<Entity, With<ControlledCamera>>();
            query.single(app.world()).unwrap()
        };
        app.world_mut()
            .entity_mut(camera)
            .get_mut::<Transform>()
            .unwrap()
            .clone_from(
                &Transform::from_xyz(0.0, 200.0, 0.0)
                    .looking_at(Vec3::new(0.0, 200.0, -1.0), Vec3::Y),
            );
        app.update();
        app.insert_resource(CursorCapture::Captured);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyT);
        let ground_hit = app
            .world_mut()
            .run_system_once(
                |context: ReadRapierContext, terrain: Query<(), With<TerrainCollider>>| {
                    let is_terrain = |entity| terrain.get(entity).is_ok();
                    context.single().unwrap().cast_ray(
                        Vec3::new(0.0, 500.0, -140.0),
                        Vec3::NEG_Y,
                        700.0,
                        true,
                        QueryFilter::default().predicate(&is_terrain),
                    )
                },
            )
            .unwrap();
        let any_hit = app
            .world_mut()
            .run_system_once(|context: ReadRapierContext| {
                context.single().unwrap().cast_ray(
                    Vec3::new(0.0, 500.0, -140.0),
                    Vec3::NEG_Y,
                    700.0,
                    true,
                    QueryFilter::default(),
                )
            })
            .unwrap();
        assert!(
            ground_hit.is_some(),
            "fixture terrain ray must hit; nearest={any_hit:?}, ground={ground:?}"
        );
        app.world_mut()
            .run_system_once(world_tankard_input)
            .unwrap();
        let spawned = {
            let mut query = app
                .world_mut()
                .query_filtered::<(Entity, &Transform), With<DebugTankard>>();
            query
                .iter(app.world())
                .find(|(_, pose)| pose.translation.z < -100.0)
                .map(|(entity, pose)| (entity, pose.translation))
                .expect("spawned tankard above terrain")
        };
        let mut query = app
            .world_mut()
            .query_filtered::<Entity, With<DebugTankard>>();
        assert_eq!(query.iter(app.world()).count(), 4);
        app.update();
        let spawned_pose = app
            .world()
            .entity(spawned.0)
            .get::<Transform>()
            .unwrap()
            .translation;
        app.world_mut()
            .entity_mut(camera)
            .get_mut::<Transform>()
            .unwrap()
            .look_at(spawned_pose, Vec3::Y);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyE);
        app.world_mut()
            .run_system_once(world_tankard_input)
            .unwrap();
        assert_eq!(app.world().resource::<HeldTankard>().0, Some(spawned.0));
        assert!(
            app.world()
                .entity(spawned.0)
                .contains::<RigidBodyDisabled>()
        );
        assert!(app.world().entity(spawned.0).contains::<ColliderDisabled>());
        app.world_mut()
            .entity_mut(camera)
            .get_mut::<Transform>()
            .unwrap()
            .translation
            .x += 100.0;
        app.world_mut().run_system_once(move_held_tankard).unwrap();
        let held_pose = app
            .world()
            .entity(spawned.0)
            .get::<Transform>()
            .unwrap()
            .translation;
        assert!(held_pose.x > 90.0, "held tankard did not follow the view");
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.release(KeyCode::KeyE);
            keys.clear();
            keys.press(KeyCode::KeyE);
        }
        app.world_mut()
            .run_system_once(world_tankard_input)
            .unwrap();
        assert_eq!(app.world().resource::<HeldTankard>().0, None);
        assert!(
            !app.world()
                .entity(spawned.0)
                .contains::<RigidBodyDisabled>()
        );
        assert!(!app.world().entity(spawned.0).contains::<ColliderDisabled>());
        app.update();
        let dropped_pose = app
            .world()
            .entity(spawned.0)
            .get::<Transform>()
            .unwrap()
            .translation;
        assert!(
            (dropped_pose - held_pose).length() < 10.0,
            "drop teleported tankard: {held_pose:?} -> {dropped_pose:?}"
        );
    }

    fn run_speed_intent(app: &mut App, wish_dir: Vec3) {
        let speed = app.world().resource::<MovementTuning>().run_speed;
        app.insert_resource(WalkIntent {
            wish_dir,
            target_speed: speed,
            jump_pressed: false,
        });
    }

    #[test]
    fn wall_blocks_step_mounts_slope_climbs() {
        let mut app = headless::fixture_app();
        // Wall face sits at x=730 (center 760, half-width 30).
        headless::place_player(&mut app, Vec3::new(500.0, 300.0, 0.0));
        for _ in 0..120 {
            app.update();
        }
        run_speed_intent(&mut app, Vec3::X);
        for _ in 0..600 {
            app.update();
        }
        let (blocked, _) = headless::player_pose(&mut app);
        let tuning = app.world().resource::<MovementTuning>().clone();
        assert!(
            blocked.x < 730.0 - tuning.capsule_radius,
            "capsule tunneled the wall: {blocked:?}"
        );

        // Step top is y=24; the 36-unit autostep budget includes it (V12).
        headless::place_player(&mut app, Vec3::new(100.0, 300.0, 240.0));
        for _ in 0..120 {
            app.update();
        }
        run_speed_intent(&mut app, Vec3::X);
        for _ in 0..600 {
            app.update();
        }
        let (stepped, step_grounded) = headless::player_pose(&mut app);
        assert!(step_grounded, "capsule never grounded near step");
        let rest = tuning.capsule_half_height() + tuning.capsule_radius;
        // Walk the step again tracking peak: autostep must lift the capsule
        // onto the 24-unit block, not stall against it (V12).
        headless::place_player(&mut app, Vec3::new(100.0, 300.0, 240.0));
        for _ in 0..120 {
            app.update();
        }
        run_speed_intent(&mut app, Vec3::X);
        let mut step_peak = 0.0f32;
        for _ in 0..600 {
            app.update();
            let (pose, _) = headless::player_pose(&mut app);
            step_peak = step_peak.max(pose.y);
        }
        assert!(
            step_peak > rest + 18.0,
            "capsule did not mount the 24-unit step: peak {step_peak}, rest {rest}, end {stepped:?}"
        );

        // Hill rises toward -x; climbing +x-to--x gains height (V12).
        // Slope is ~18 degrees, under the 50-degree climb limit.
        headless::place_player(&mut app, Vec3::new(100.0, 300.0, -500.0));
        for _ in 0..120 {
            app.update();
        }
        run_speed_intent(&mut app, Vec3::NEG_X);
        let mut peak = 0.0f32;
        for _ in 0..600 {
            app.update();
            let (pose, _) = headless::player_pose(&mut app);
            peak = peak.max(pose.y);
        }
        assert!(peak > 200.0, "capsule did not climb the hill: peak {peak}");
    }

    #[test]
    fn tankards_contact_wall_and_settle_without_penetration() {
        let mut app = headless::fixture_app();
        // Hurl one tankard at the wall; it must collide, not tunnel (V15).
        let hurled = {
            let mut query = app
                .world_mut()
                .query_filtered::<Entity, With<DebugTankard>>();
            let first = query.iter(app.world()).next().expect("tankard");
            app.world_mut().entity_mut(first).insert((
                Velocity::linear(Vec3::X * 1500.0),
                Transform::from_xyz(0.0, 300.0, 0.0),
            ));
            first
        };
        for _ in 0..900 {
            app.update();
        }
        let position = app
            .world()
            .get::<Transform>(hurled)
            .expect("tankard transform")
            .translation;
        assert!(position.is_finite());
        // Wall spans x in [730, 790]; the cup radius keeps contact outside it.
        assert!(
            position.x < 730.0 - TANKARD_CUP_RADIUS * 0.5 || position.y > 420.0,
            "tankard tunneled the wall: {position:?}"
        );
        // Every tankard rests at or above the ground pad (no sinking).
        let mut query = app
            .world_mut()
            .query_filtered::<&Transform, With<DebugTankard>>();
        for transform in query.iter(app.world()) {
            assert!(
                transform.translation.y > -TANKARD_CUP_HALF_HEIGHT,
                "tankard sank through ground: {:?}",
                transform.translation
            );
        }
    }

    #[test]
    fn toggle_clears_state_and_overlay_matches_mode() {
        let mut app = headless::fixture_app();
        headless::place_player(&mut app, Vec3::new(120.0, 300.0, 120.0));
        for _ in 0..120 {
            app.update();
        }
        // Simulate WALK->NOCLIP: stale intent must clear (V14).
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: 300.0,
            jump_pressed: true,
        });
        let _ = app.world_mut().run_system_once(
            |mut intent: ResMut<WalkIntent>,
             mut states: Query<&mut WalkState>,
             mut controllers: Query<&mut KinematicCharacterController>| {
                clear_motion_state(&mut intent, &mut states, &mut controllers);
            },
        );
        let intent = app.world().resource::<WalkIntent>();
        assert_eq!(intent.wish_dir, Vec3::ZERO);
        assert!(!intent.jump_pressed);
        // Overlay text tracks the mode resource every frame (V7).
        for _ in 0..5 {
            app.update();
        }
        let mut query = app
            .world_mut()
            .query_filtered::<&Text, With<NoclipOverlay>>();
        let text = query.single(app.world()).expect("overlay").clone();
        assert_eq!(
            text.as_str(),
            "NOCLIP: OFF  [V]  [T] tankard  [E] grab/drop"
        );
        app.insert_resource(MoveMode::Noclip);
        for _ in 0..5 {
            app.update();
        }
        let mut query = app
            .world_mut()
            .query_filtered::<&Text, With<NoclipOverlay>>();
        let text = query.single(app.world()).expect("overlay").clone();
        assert_eq!(text.as_str(), "NOCLIP: ON  [V]  [T] tankard  [E] grab/drop");
    }

    #[test]
    fn fixed_step_walk_is_frame_rate_independent() {
        // Same intents, different render cadence: 30/60/120 updates per
        // simulated second must agree within tolerance (V18). Time runs at
        // real 60 Hz fixed ticks; render-only updates add no physics.
        fn drive(frames_per_tick: u32) -> Vec3 {
            let mut app = headless::fixture_app();
            headless::place_player(&mut app, Vec3::new(120.0, 300.0, 120.0));
            for _ in 0..120 {
                app.update();
            }
            let speed = app.world().resource::<MovementTuning>().run_speed;
            for _ in 0..120 {
                app.insert_resource(WalkIntent {
                    wish_dir: Vec3::X,
                    target_speed: speed,
                    jump_pressed: false,
                });
                for _ in 0..frames_per_tick {
                    app.update();
                }
            }
            headless::player_pose(&mut app).0
        }
        // frames_per_tick scales total ticks, so normalize: 1x120 vs 2x60.
        let mut fast = headless::fixture_app();
        headless::place_player(&mut fast, Vec3::new(120.0, 300.0, 120.0));
        for _ in 0..120 {
            fast.update();
        }
        let speed = fast.world().resource::<MovementTuning>().run_speed;
        for _ in 0..240 {
            fast.insert_resource(WalkIntent {
                wish_dir: Vec3::X,
                target_speed: speed,
                jump_pressed: false,
            });
            fast.update();
        }
        let slow_pose = drive(1);
        let fast_pose = headless::player_pose(&mut fast).0;
        assert!(
            (slow_pose - fast_pose).length() < 60.0,
            "frame-rate divergence: {slow_pose:?} vs {fast_pose:?}"
        );
    }

    #[test]
    fn origin_rebase_keeps_body_camera_and_tankards_aligned() {
        use crate::streaming::RenderOrigin;
        use crate::world::components::CELL_SIZE;

        let mut app = headless::fixture_app();
        app.insert_resource(RenderOrigin(IVec2::ZERO));
        headless::place_player(&mut app, Vec3::new(120.0, 300.0, 120.0));
        for _ in 0..120 {
            app.update();
        }
        // Simulate render-origin rebase: every physics participant shifts
        // by the same cell offset, Rapier tracks Bevy transforms, velocity
        // preserved (V9).
        let shift = Vec3::new(CELL_SIZE, 0.0, CELL_SIZE);
        let before_velocity = {
            let mut query = app
                .world_mut()
                .query_filtered::<&Velocity, With<DebugTankard>>();
            query.iter(app.world()).next().cloned()
        };
        {
            let mut query = app.world_mut().query_filtered::<&mut Transform, Or<(
                With<PlayerBody>,
                With<DebugTankard>,
                With<StreamingCamera>,
                With<FixtureArena>,
            )>>();
            for mut transform in query.iter_mut(app.world_mut()) {
                transform.translation -= shift;
            }
        }
        app.world_mut().resource_mut::<RenderOrigin>().0 += IVec2::new(1, 1);
        for _ in 0..30 {
            app.update();
        }
        let (pose, grounded) = headless::player_pose(&mut app);
        assert!(grounded, "rebase ungrounded the capsule at {pose:?}");
        assert!(pose.is_finite());
        if let Some(before) = before_velocity {
            let after = {
                let mut query = app
                    .world_mut()
                    .query_filtered::<&Velocity, With<DebugTankard>>();
                query.iter(app.world()).next().cloned().unwrap_or_default()
            };
            assert!(
                (after.linear - before.linear).length() < 400.0,
                "rebase injected tankard velocity: {before:?} -> {after:?}"
            );
        }
    }
}
