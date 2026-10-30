//! Going through a load door: look at one, press E, fade to black, arrive at the other side.
//!
//! One state machine per crossing, with a single crossing at a time:
//!
//! 1. **Fade out** (0.25 s) under a full-screen black UI node.
//! 2. At full black the space switches in one step: every cell of the space left behind unloads at
//!    once (unpaced), the [`ActiveSpace`] and `CameraSpace` change, an exterior landing sets the
//!    render origin to the arrival's grid square (an interior landing leaves it alone), and the
//!    player is teleported to the door's `XTEL` arrival point.
//! 3. **Hold black** until the landing cell is resident, nothing under its root is pending (models, surfaces) and
//!    every mesh, material and texture under the cell's root has finished loading, then two more
//!    frames so render extraction and preparation run, for at most 10 s per landing. While black,
//!    the per-frame upload budget is lifted (a stutter behind black is invisible) and the configured
//!    value comes back when the fade-in starts. A landing cell that fails to load puts the player
//!    back where they were, with a warning. A destination timeout also returns to the saved space.
//!    If that return fails or times out, an ordinary crossing fades in with movement paused until
//!    the player recovers in noclip: press V with the cursor captured, or enter `tcl` in the console.
//! 4. **Fade in** (0.25 s). The player is placed on the landing point once more as it starts.
//!
//! The player's walking and noclip systems pause for the whole crossing ([`no_door_crossing`]): nothing under
//! the landing point has collision until its cell is ready, so a WALK body would fall (and WASD
//! would move it) while the screen is black.
//!
//! There is no preload: the fade simply holds until the destination is resident and uploaded.
//!

use crate::{
    doors::LoadDoor,
    physics::{
        CursorCapture, MovementTuning, PlayerBody, TeleportPlayer, body_and_camera_for_feet,
    },
    sky::CameraSpace,
    streaming::{
        ActiveSpace, PendingUnder, RenderOrigin, StreamingWorld, creation_rotation_to_bevy,
        creation_to_bevy, streaming_center, unload_all_cells_now,
    },
    swap_bridge::{
        assets::LandingAssets,
        destination::{
            DestinationKey, DestinationReports, DestinationRequest, DestinationRequests,
            DestinationState, Generations, InvalidationCause, PassFamilies, PassFamily, RequestId,
            ViewKey, WorldId,
        },
        systems::{DestinationReadinessPlugin, DestinationReportsUpdated},
    },
    world::{
        components::{CELL_SIZE, ExpectedModelBounds, StreamingCamera, WorldPosition},
        database::CellKey,
    },
};
use bevy::{
    core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass, NormalPrepass},
    prelude::*,
    render::{
        render_asset::RenderAssetBytesPerFrame, sync_world::MainEntity, view::RetainedViewEntity,
    },
};
use bevy_rapier3d::prelude::{QueryFilter, ReadRapierContext};

/// How far from the camera a door can be and still be used, in Creation units.
pub const DOOR_REACH: f32 = 200.0;
/// The box a load door is activated through when its reference carries no model bounds, in the
/// door's local space: a door-sized box centred on the origin horizontally (160 wide and deep, so
/// a door hinged at the origin is still hit across its width) that starts at the floor and reaches
/// 200 up, a person's height and a little more.
pub const FALLBACK_DOOR_MIN: Vec3 = Vec3::new(-80.0, 0.0, -80.0);
pub const FALLBACK_DOOR_MAX: Vec3 = Vec3::new(80.0, 200.0, 80.0);
/// How much nearer than a door's bounds a collider may be before it counts as blocking the view
/// of the door, in Creation units: the door's own collider and its frame sit at the bounds.
pub const DOOR_BLOCKER_SLACK: f32 = 15.0;
/// Seconds to fade out, and to fade in.
pub const FADE_SECONDS: f32 = 0.25;
/// Longest the screen stays black waiting for the destination.
pub const LANDING_TIMEOUT_SECONDS: f32 = 10.0;
/// Frames to keep waiting once everything under the landing cell has loaded.
const SETTLE_FRAMES: u32 = 2;

pub struct DoorCrossingPlugin;

impl Plugin for DoorCrossingPlugin {
    fn build(&self, app: &mut App) {
        // The destination readiness protocol the black hold waits on. The swap bridge installs it in
        // a real run; a run or test without the bridge gets it here, with no renderer to prove.
        if !app.is_plugin_added::<DestinationReadinessPlugin>() {
            app.add_plugins(DestinationReadinessPlugin);
        }
        app.init_resource::<DoorCrossing>()
            .init_resource::<CameraSpace>()
            // Install landing eye height when no walking-player plugin provided it.
            .init_resource::<MovementTuning>()
            .add_message::<TeleportPlayer>()
            // One accepted activation requests authored door playback.
            .add_message::<crate::doors::OpenDoor>()
            .add_systems(Startup, spawn_fade_overlay)
            // Apply covered space switches before propagation and history compensation.
            .add_systems(
                Update,
                (
                    refresh_destination_request
                        .run_if(|requests: Res<DestinationRequests>| !requests.is_empty()),
                    drive_door_crossing,
                )
                    .chain()
                    .after(crate::streaming::StreamingUpdate)
                    .after(crate::settings_ui::SettingsUiSet)
                    .after(DestinationReportsUpdated),
            );
    }
}

/// The full-screen black node the crossing fades.
#[derive(Component)]
pub struct FadeOverlay;

/// Where a crossing puts the player.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Landing {
    space: ActiveSpace,
    /// The cell whose residency ends the black hold.
    key: CellKey,
    /// `Some(grid)` for an exterior landing; an interior landing leaves the origin alone.
    origin: Option<IVec2>,
    camera_space: CameraSpace,
    /// FEET position in render space (relative to `origin`, or to the unchanged origin), as
    /// [`TeleportPlayer`] takes it.
    position: Vec3,
    yaw: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Stage {
    FadeOut {
        elapsed: f32,
    },
    /// Black; the switch is done and the landing cell is loading.
    Landing {
        waited: f32,
        restoring: bool,
        /// Consecutive frames the landing cell has been fully loaded; the fade-in starts after
        /// [`SETTLE_FRAMES`] more, so render extraction runs with the budget still lifted.
        ready_frames: u32,
    },
    FadeIn {
        elapsed: f32,
        recovery: bool,
    },
    Recovery,
}

/// The player's own place before a crossing.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Restore {
    space: ActiveSpace,
    camera_space: CameraSpace,
    configured_worldspace: u32,
    /// Feet position. For an exterior start: absolute render-space coordinates (the render
    /// position plus the render origin's offset), so independent of whichever origin is in force.
    /// For an interior start: the raw render position, interiors being placed absolutely with the
    /// origin unused.
    feet: Vec3,
    yaw: f32,
}

/// The absolute feet position for render-space `feet` under `origin`.
fn absolute_feet(feet: Vec3, origin: IVec2) -> Vec3 {
    feet + Vec3::new(
        origin.x as f32 * CELL_SIZE,
        0.0,
        -(origin.y as f32) * CELL_SIZE,
    )
}

impl Restore {
    fn landing(&self) -> Landing {
        match self.space.interior {
            Some(cell_id) => Landing {
                space: self.space,
                key: CellKey::Interior(cell_id),
                origin: None,
                camera_space: self.camera_space,
                position: self.feet,
                yaw: self.yaw,
            },
            None => {
                let grid = streaming_center(self.feet, IVec2::ZERO);
                Landing {
                    space: self.space,
                    key: CellKey::Exterior {
                        worldspace_id: self.space.exterior_worldspace(self.configured_worldspace),
                        grid_x: grid.x,
                        grid_y: grid.y,
                    },
                    origin: Some(grid),
                    camera_space: self.camera_space,
                    position: absolute_feet(self.feet, -grid),
                    yaw: self.yaw,
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Crossing {
    door: LoadDoor,
    target: Landing,
    /// Where the player was, to put them back if the target cannot load. Kept in world terms (the
    /// absolute feet position, see [`absolute_feet`]) so a render-origin rebase before the switch
    /// cannot move it; it becomes a [`Landing`] only when it is used.
    restore: Restore,
    /// Seconds since E was pressed.
    since_press: f32,
    stage: Stage,
    /// The destination the black hold waits for, made by [`switch_space`] for the space the player is
    /// put in (the target, or the restore after a failure) and withdrawn when the hold ends.
    request: Option<(RequestId, DestinationKey)>,
}

/// The crossing in progress, if any.
#[derive(Resource, Default, Debug)]
pub struct DoorCrossing {
    active: Option<Crossing>,
}

impl DoorCrossing {
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Whether the failed return is visible and the player can explicitly recover in noclip.
    pub(crate) fn needs_recovery(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| matches!(active.stage, Stage::Recovery))
    }

    /// Release the movement guard after the caller has disabled the walking body.
    pub(crate) fn finish_recovery(&mut self) {
        debug_assert!(self.needs_recovery());
        self.active = None;
    }
}

/// Run condition for the player's movement systems: true unless a door crossing is in progress (a
/// run without the crossing plugin never has one).
pub fn no_door_crossing(crossing: Option<Res<DoorCrossing>>) -> bool {
    crossing.is_none_or(|crossing| !crossing.is_active())
}

#[cfg(test)]
impl DoorCrossing {
    pub(crate) fn recovery_for_test() -> Self {
        let mut crossing = Self::holding_for_test();
        crossing.active.as_mut().unwrap().stage = Stage::Recovery;
        crossing
    }

    /// A crossing held black on its landing, for tests of systems that must pause during one.
    pub(crate) fn holding_for_test() -> Self {
        let door = LoadDoor {
            ref_id: 1,
            destination: crate::doors::DoorDestination {
                destination_ref_id: 2,
                interior_cell_id: Some(3),
                worldspace_id: None,
                arrival_position: [0.0; 3],
                arrival_rotation: [0.0; 3],
            },
        };
        let space = ActiveSpace::default();
        let target = landing_for(&door, &space, 0);
        Self {
            active: Some(Crossing {
                door,
                target,
                restore: Restore {
                    space,
                    camera_space: CameraSpace::default(),
                    configured_worldspace: 0,
                    feet: Vec3::ZERO,
                    yaw: 0.0,
                },
                since_press: 0.0,
                request: None,
                stage: Stage::Landing {
                    waited: 0.0,
                    restoring: false,
                    ready_frames: 0,
                },
            }),
        }
    }
}

fn spawn_fade_overlay(mut commands: Commands) {
    commands.spawn((
        Name::new("Door fade"),
        FadeOverlay,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
        GlobalZIndex(i32::MAX - 1),
    ));
}

fn fallback_bounds() -> ExpectedModelBounds {
    ExpectedModelBounds {
        min: FALLBACK_DOOR_MIN,
        max: FALLBACK_DOOR_MAX,
    }
}

/// The destination as the crossing's INFO line prints it: the interior cell id, or the
/// worldspace and grid square.
fn describe_destination(key: &CellKey) -> String {
    match key {
        CellKey::Interior(cell_id) => format!("interior cell {cell_id}"),
        CellKey::Exterior {
            worldspace_id,
            grid_x,
            grid_y,
        } => format!("worldspace {worldspace_id} grid ({grid_x}, {grid_y})"),
    }
}

/// The distance along a ray (unit `direction`) to a door's model bounds, if within `reach`.
///
/// The bounds are the converter's model-space box; the ray is taken into the door's local space
/// so a rotated or scaled door is tested as the oriented box it is.
fn ray_hits_door(
    origin: Vec3,
    direction: Vec3,
    door: &GlobalTransform,
    bounds: &ExpectedModelBounds,
    reach: f32,
) -> Option<f32> {
    let inverse = door.affine().inverse();
    let local_origin = inverse.transform_point3(origin);
    // Not renormalised, so the ray parameter stays a world distance.
    let local_direction = inverse.transform_vector3(direction);
    let mut near = 0.0f32;
    let mut far = reach;
    for axis in 0..3 {
        let (start, step) = (local_origin[axis], local_direction[axis]);
        if step.abs() < 1e-9 {
            if start < bounds.min[axis] || start > bounds.max[axis] {
                return None;
            }
            continue;
        }
        let (a, b) = (
            (bounds.min[axis] - start) / step,
            (bounds.max[axis] - start) / step,
        );
        near = near.max(a.min(b));
        far = far.min(a.max(b));
        if near > far {
            return None;
        }
    }
    Some(near)
}

/// Whether a collider the camera ray met first hides a door `door_distance` away: it must be
/// nearer than the door by more than [`DOOR_BLOCKER_SLACK`]. No collider (`None`) never blocks.
fn door_view_blocked(nearest_collider: Option<f32>, door_distance: f32) -> bool {
    nearest_collider.is_some_and(|hit| hit < door_distance - DOOR_BLOCKER_SLACK)
}

/// Whether a collider hides the door the camera ray met `distance` away: the same camera-ray test
/// the tankard grab uses, so a door behind a wall is not reachable. Without a physics world (a run
/// with no colliders) nothing can block.
fn door_blocked(
    physics: &ReadRapierContext,
    player: &Query<Entity, With<PlayerBody>>,
    view: &Transform,
    direction: Vec3,
    distance: f32,
) -> bool {
    let Ok(physics) = physics.single() else {
        return false;
    };
    let filter = match player.single() {
        Ok(player) => QueryFilter::default().exclude_rigid_body(player),
        Err(_) => QueryFilter::default(),
    };
    let nearest = physics
        .cast_ray(view.translation, direction, DOOR_REACH, true, filter)
        .map(|(_, distance)| distance);
    door_view_blocked(nearest, distance)
}

/// The landing a door's destination describes, from the space the player is in now.
fn landing_for(door: &LoadDoor, current: &ActiveSpace, configured_worldspace: u32) -> Landing {
    let destination = &door.destination;
    let creation = Vec3::from_array(destination.arrival_position);
    let yaw = creation_rotation_to_bevy(destination.arrival_rotation)
        .to_euler(EulerRot::YXZ)
        .0;
    if let Some(cell_id) = destination.interior_cell_id {
        // Interiors are placed in absolute coordinates and the origin stays as it is.
        return Landing {
            space: ActiveSpace {
                worldspace_id: Some(current.exterior_worldspace(configured_worldspace)),
                interior: Some(cell_id),
            },
            key: CellKey::Interior(cell_id),
            origin: None,
            camera_space: CameraSpace::Interior,
            position: creation_to_bevy(creation),
            yaw,
        };
    }
    let worldspace_id = destination
        .worldspace_id
        .unwrap_or_else(|| current.exterior_worldspace(configured_worldspace));
    let at = WorldPosition::from_creation_units(creation);
    Landing {
        space: ActiveSpace {
            worldspace_id: Some(worldspace_id),
            interior: None,
        },
        key: CellKey::Exterior {
            worldspace_id,
            grid_x: at.grid.x,
            grid_y: at.grid.y,
        },
        origin: Some(at.grid),
        camera_space: CameraSpace::Exterior,
        position: creation_to_bevy(at.relative_to(at.grid)),
        yaw,
    }
}

/// The destination world identity of a space, for the readiness protocol: a worldspace's exterior is
/// its form id, an interior cell is its id with the top bit set, so the two cannot meet.
fn world_id(space: &ActiveSpace, configured_worldspace: u32) -> WorldId {
    match space.interior {
        Some(cell_id) => WorldId::interior(cell_id),
        None => WorldId::exterior(space.exterior_worldspace(configured_worldspace)),
    }
}

/// Switches the world to `landing` in one step. Runs as a command, so it applies before the next
/// system that reads the cells and the old space is gone the same frame.
fn switch_space(world: &mut World, landing: Landing) {
    unload_all_cells_now(world);
    let left = *world.resource::<ActiveSpace>();
    *world.resource_mut::<ActiveSpace>() = landing.space;
    if let Some(grid) = landing.origin {
        world.resource_mut::<RenderOrigin>().0 = grid;
    }
    *world.resource_mut::<CameraSpace>() = landing.camera_space;
    let (_, eye) = body_and_camera_for_feet(world.resource::<MovementTuning>(), landing.position);
    // Move the camera now as well as through the message: the planner runs before the teleport is
    // applied and must already see the player in the new space.
    let mut cameras = world.query_filtered::<&mut Transform, With<StreamingCamera>>();
    for mut camera in cameras.iter_mut(world) {
        camera.translation = eye;
    }
    world.write_message(TeleportPlayer {
        position: landing.position,
        sampled_ground_y: None,
        yaw: landing.yaw,
    });
    request_destination(world, &left, &landing);
}

/// Makes the destination request the black hold waits on: the space left is invalidated (what
/// was proven about it is gone), the space entered gets a new generation, and a request for the main
/// camera's view of the landing cell is stored on the crossing. The passes are the ones the camera
/// and the lights run: forward always, each prepass the camera carries, shadows when a light casts
/// them (a destination with no shadow view is vacuous for that family).
fn request_destination(world: &mut World, left: &ActiveSpace, landing: &Landing) {
    let configured = world
        .resource::<crate::config::EngineConfig>()
        .worldspace_id;
    let entered = world_id(&landing.space, configured);
    let generation = {
        let mut generations = world.resource_mut::<Generations>();
        generations.bump(world_id(left, configured), InvalidationCause::Composition);
        generations.bump(entered, InvalidationCause::Composition)
    };
    let camera = world
        .query_filtered::<Entity, With<StreamingCamera>>()
        .iter(world)
        .next();
    let Some(camera) = camera else {
        return;
    };
    let passes = destination_passes(world, camera);
    let key = DestinationKey {
        view: ViewKey {
            view: RetainedViewEntity::new(MainEntity::from(camera), None, 0),
            incarnation: 0,
        },
        world: entered,
        generation,
    };
    let id = world
        .resource_mut::<DestinationRequests>()
        .request(DestinationRequest {
            key,
            passes,
            coverage: std::sync::Arc::from(vec![landing.key]),
        });
    if let Some(active) = world.resource_mut::<DoorCrossing>().active.as_mut() {
        if let Some((old, old_key)) = active.request.replace((id, key)) {
            world.resource_mut::<DestinationRequests>().withdraw(old);
            world.resource_mut::<DestinationReports>().remove(&old_key);
        }
    } else {
        // No crossing owns it (a test calling the switch directly): nothing would withdraw it.
        world.resource_mut::<DestinationRequests>().withdraw(id);
    }
}

/// Read the passes actually enabled for this camera and the current lights.
/// Creation and held-request refresh use the same scope discovery.
fn destination_passes(world: &mut World, camera: Entity) -> PassFamilies {
    let mut passes = PassFamilies::NONE.with(PassFamily::Forward);
    let entity = world.entity(camera);
    if entity.contains::<DepthPrepass>() {
        passes = passes.with(PassFamily::DepthPrepass);
    }
    if entity.contains::<NormalPrepass>() {
        passes = passes.with(PassFamily::NormalPrepass);
    }
    if entity.contains::<MotionVectorPrepass>() {
        passes = passes.with(PassFamily::MotionPrepass);
    }
    if world
        .query::<&DirectionalLight>()
        .iter(world)
        .any(|light| light.shadow_maps_enabled)
        || world
            .query::<&PointLight>()
            .iter(world)
            .any(|light| light.shadow_maps_enabled)
        || world
            .query::<&SpotLight>()
            .iter(world)
            .any(|light| light.shadow_maps_enabled)
    {
        passes = passes.with(PassFamily::Shadow);
    }
    passes
}

/// A held crossing must not accept an earlier, weaker render proof after settings change.
/// The request owner preserves coverage while invalidating the old scope.
fn refresh_destination_request(world: &mut World) {
    let held = world
        .resource::<DoorCrossing>()
        .active
        .as_ref()
        .and_then(|active| {
            matches!(active.stage, Stage::Landing { .. })
                .then_some(active.request)
                .flatten()
        });
    let Some((id, previous)) = held else {
        return;
    };
    let Some(request) = world
        .resource::<DestinationRequests>()
        .iter()
        .find_map(|(held, request)| (held == id).then(|| request.clone()))
    else {
        return;
    };
    let Some(camera) = world
        .query_filtered::<Entity, With<StreamingCamera>>()
        .iter(world)
        .next()
    else {
        return;
    };
    let passes = destination_passes(world, camera);
    let view = ViewKey {
        view: RetainedViewEntity::new(MainEntity::from(camera), None, 0),
        incarnation: 0,
    };
    let current = world.resource_scope(|world, mut requests: Mut<DestinationRequests>| {
        requests.update(
            id,
            &mut world.resource_mut::<Generations>(),
            view,
            passes,
            request.coverage,
        )
    });
    if current != Some(previous) {
        world.resource_mut::<DestinationReports>().remove(&previous);
        if let Some(active) = world.resource_mut::<DoorCrossing>().active.as_mut() {
            active.request = current.map(|key| (id, key));
            // Settled frames for the previous scope cannot count toward this one.
            if let Stage::Landing { ready_frames, .. } = &mut active.stage {
                *ready_frames = 0;
            }
        }
    }
}

/// Places the player on `landing` again as the fade-in starts. The walking systems paused during
/// the crossing, so this only undoes anything else that moved the body or the camera while black.
fn place_again(commands: &mut Commands, landing: &Landing) {
    let landing = *landing;
    commands.queue(move |world: &mut World| {
        // A saved exterior pose belongs to its landing origin, which may have changed during
        // the hold. Interiors use absolute coordinates and do not follow the render origin.
        let position = match landing.origin {
            Some(saved) => {
                absolute_feet(landing.position, saved - world.resource::<RenderOrigin>().0)
            }
            None => landing.position,
        };
        world.write_message(TeleportPlayer {
            position,
            sampled_ground_y: None,
            yaw: landing.yaw,
        });
    });
}

/// Sets the per-frame render-asset upload budget, if the engine has one.
fn set_upload_budget(
    budget: &mut Option<ResMut<RenderAssetBytesPerFrame>>,
    max_bytes: Option<usize>,
) {
    if let Some(budget) = budget {
        budget.max_bytes = max_bytes;
    }
}

fn set_alpha(overlay: &mut Query<&mut BackgroundColor, With<FadeOverlay>>, alpha: f32) {
    for mut colour in overlay.iter_mut() {
        colour.0 = Color::srgba(0.0, 0.0, 0.0, alpha.clamp(0.0, 1.0));
    }
}

type TargetLoadDoors<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static LoadDoor,
        &'static GlobalTransform,
        Option<&'static ExpectedModelBounds>,
    ),
>;

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn drive_door_crossing(
    (keyboard, capture, mut open): (
        Res<ButtonInput<KeyCode>>,
        Option<Res<CursorCapture>>,
        MessageWriter<crate::doors::OpenDoor>,
    ),
    time: Res<Time>,
    config: Res<crate::config::EngineConfig>,
    tuning: Res<MovementTuning>,
    space: Res<ActiveSpace>,
    origin: Res<RenderOrigin>,
    camera_space: Res<CameraSpace>,
    streaming: Res<StreamingWorld>,
    camera: Query<&Transform, With<StreamingCamera>>,
    doors: TargetLoadDoors,
    (physics, player, transforms): (
        ReadRapierContext,
        Query<Entity, With<PlayerBody>>,
        bevy::transform::helper::TransformHelper,
    ),
    mut crossing: ResMut<DoorCrossing>,
    mut overlay: Query<&mut BackgroundColor, With<FadeOverlay>>,
    (mut budget, landing_assets, pending_under, mut requests, mut reports, generations): (
        Option<ResMut<RenderAssetBytesPerFrame>>,
        LandingAssets,
        PendingUnder,
        ResMut<DestinationRequests>,
        ResMut<DestinationReports>,
        Res<Generations>,
    ),
    mut commands: Commands,
) {
    let delta = time.delta_secs();
    let Some(active) = crossing.active.as_mut() else {
        let Ok(view) = camera.single() else {
            return;
        };
        if !keyboard.just_pressed(KeyCode::KeyE)
            || !capture.is_some_and(|capture| *capture == CursorCapture::Captured)
        {
            return;
        }
        let direction = view.forward().as_vec3();
        let Some((door_entity, door, distance)) = doors
            .iter()
            .filter_map(|(entity, door, _, bounds)| {
                let transform = transforms.compute_global_transform(entity).ok()?;
                let bounds = bounds.copied().unwrap_or_else(fallback_bounds);
                ray_hits_door(view.translation, direction, &transform, &bounds, DOOR_REACH)
                    .map(|distance| (entity, door, distance))
            })
            .min_by(|a, b| a.2.total_cmp(&b.2))
        else {
            return;
        };
        if door_blocked(&physics, &player, view, direction, distance) {
            return;
        }
        let (yaw, _, _) = view.rotation.to_euler(EulerRot::YXZ);
        // The camera sits an eye height above the body, and the body a capsule half-extent above
        // the feet.
        let (body_offset, _) = body_and_camera_for_feet(&tuning, Vec3::ZERO);
        let feet = view.translation - Vec3::Y * tuning.eye_height - body_offset;
        let restore = Restore {
            space: *space,
            camera_space: *camera_space,
            configured_worldspace: config.worldspace_id,
            feet: if space.interior.is_some() {
                feet
            } else {
                absolute_feet(feet, origin.0)
            },
            yaw,
        };
        let target = landing_for(door, &space, config.worldspace_id);
        debug!(
            door = format_args!("{:08X}", door.ref_id),
            "door crossing started"
        );
        open.write(crate::doors::OpenDoor { door: door_entity });
        crossing.active = Some(Crossing {
            door: door.clone(),
            target,
            restore,
            since_press: 0.0,
            request: None,
            stage: Stage::FadeOut { elapsed: 0.0 },
        });
        set_alpha(&mut overlay, 0.0);
        return;
    };
    active.since_press += delta;
    match active.stage {
        Stage::FadeOut { elapsed } => {
            let elapsed = elapsed + delta;
            set_alpha(&mut overlay, elapsed / FADE_SECONDS);
            if elapsed >= FADE_SECONDS {
                let target = active.target;
                commands.queue(move |world: &mut World| switch_space(world, target));
                // Behind black a stutter is invisible: lift the upload budget until the fade-in.
                set_upload_budget(&mut budget, None);
                active.stage = Stage::Landing {
                    waited: 0.0,
                    restoring: false,
                    ready_frames: 0,
                };
            } else {
                active.stage = Stage::FadeOut { elapsed };
            }
        }
        Stage::Landing {
            waited,
            restoring,
            ready_frames,
        } => {
            set_alpha(&mut overlay, 1.0);
            let waited = waited + delta;
            let landing = if restoring {
                active.restore.landing()
            } else {
                active.target
            };
            // A failed or timed-out landing returns to the saved space. An ordinary restore
            // keeps movement paused if it cannot finish.

            let key = if let Some((id, previous)) = active.request {
                let current = requests.rebase(id, &generations);
                if current != Some(previous) {
                    reports.remove(&previous);
                    active.request = current.map(|key| (id, key));
                }
                current
            } else {
                None
            };
            let destination_failed = key.is_some_and(|key| {
                reports
                    .get(&key)
                    .is_some_and(|report| matches!(report.state, DestinationState::Failed(_)))
            });
            if streaming.is_failed(landing.key) || destination_failed {
                if restoring {
                    warn!(
                        door = format_args!("{:08X}", active.door.ref_id),
                        "door crossing: the player's own space could not be reloaded; movement stays paused; press V or use tcl to recover in noclip"
                    );
                    info!(
                        door = format_args!("{:08X}", active.door.ref_id),
                        destination = %describe_destination(&landing.key),
                        restored = true,
                        milliseconds = (active.since_press * 1000.0) as u32,
                        "door crossing: return failed; fade-in starts with movement paused"
                    );
                    set_upload_budget(&mut budget, config.max_upload_bytes_per_frame());
                    place_again(&mut commands, &landing);
                    if let Some((id, key)) = active.request.take() {
                        requests.withdraw(id);
                        reports.remove(&key);
                    }
                    active.stage = Stage::FadeIn {
                        elapsed: 0.0,
                        recovery: true,
                    };
                } else {
                    warn!(
                        door = format_args!("{:08X}", active.door.ref_id),
                        destination = %describe_destination(&active.target.key),
                        "door crossing: the destination could not be loaded; putting the player back"
                    );
                    let restore = active.restore.landing();
                    commands.queue(move |world: &mut World| switch_space(world, restore));
                    active.stage = Stage::Landing {
                        waited: 0.0,
                        restoring: true,
                        ready_frames: 0,
                    };
                }
                return;
            }
            // Only the landing cell's own work counts: other cells of a stream window may stay
            // busy for a long time without affecting what the player sees.
            let root = streaming.resident_root(landing.key);
            let pending = root.map_or(0, |root| pending_under.count(root));
            let report = root.map(|root| landing_assets.inspect_under(root));
            // What the hold waits for is the destination report for the main view of the landing
            // cell (`crate::swap_bridge::destination`): residency, assets, geometry and material in
            // the render world and a queued pipeline in each pass the view runs. The request
            // follows the world's generation, so a change under it (a role flip, a candidate) asks
            // again instead of leaving an old report to answer; the report describes the render
            // frame it was evaluated in, one extract ago.
            let loaded = key.is_some_and(|key| reports.is_ready(&key, &generations));
            // Once loaded, a few more frames let extraction and preparation upload with the budget
            // still lifted: the fade-in starts on the SETTLE_FRAMES-th loaded frame.
            let ready_frames = if loaded { ready_frames + 1 } else { 0 };
            let ready = ready_frames >= SETTLE_FRAMES;
            // A destination that is not ready in time puts the player back. An ordinary restore
            // that times out fades in with movement paused.
            if !ready && waited >= LANDING_TIMEOUT_SECONDS && !restoring {
                let destination =
                    key.map_or_else(|| "no request".to_owned(), |key| reports.describe(&key));

                warn!(
                    door = format_args!("{:08X}", active.door.ref_id),
                    waited_seconds = waited,
                    %destination,
                    "door crossing: the destination was not ready in time; putting the player back"
                );
                let restore = active.restore.landing();
                commands.queue(move |world: &mut World| switch_space(world, restore));
                active.stage = Stage::Landing {
                    waited: 0.0,
                    restoring: true,
                    ready_frames: 0,
                };

                return;
            }
            if ready || waited >= LANDING_TIMEOUT_SECONDS {
                if !ready {
                    warn!(
                        door = format_args!("{:08X}", active.door.ref_id),
                        waited_seconds = waited,
                        resident = root.is_some(),
                        pending_components = pending,
                        destination = %key.map_or_else(
                            || "no request".to_owned(),
                            |key| reports.describe(&key)
                        ),
                        assets_loading = report.as_ref().map_or(0, |report| report.waiting),
                        first_not_loaded = report
                            .as_ref()
                            .and_then(|report| report.first_waiting.as_deref())
                            .unwrap_or("none"),
                        "door crossing: the return was not ready in time; movement stays paused; press V or use tcl to recover in noclip"
                    );
                }
                if let Some(report) = report.as_ref().filter(|report| !report.failed.is_empty()) {
                    warn!(
                        door = format_args!("{:08X}", active.door.ref_id),
                        failed_assets = report.failed.len(),
                        "door crossing: some assets under the landing cell failed to load"
                    );
                }
                info!(
                    door = format_args!("{:08X}", active.door.ref_id),
                    destination = %describe_destination(&landing.key),
                    restored = restoring,
                    milliseconds = (active.since_press * 1000.0) as u32,
                    "door crossing: {} fade-in starts",
                    if !ready {
                        "return timed out with movement paused;"
                    } else if restoring {
                        "restored the player's own place;"
                    } else {
                        "arrived;"
                    }
                );
                set_upload_budget(&mut budget, config.max_upload_bytes_per_frame());
                place_again(&mut commands, &landing);
                if let Some((id, key)) = active.request.take() {
                    requests.withdraw(id);
                    reports.remove(&key);
                }
                active.stage = Stage::FadeIn {
                    elapsed: 0.0,
                    recovery: !ready,
                };
            } else {
                active.stage = Stage::Landing {
                    waited,
                    restoring,
                    ready_frames,
                };
            }
        }
        Stage::FadeIn { elapsed, recovery } => {
            let elapsed = elapsed + delta;
            set_alpha(&mut overlay, 1.0 - elapsed / FADE_SECONDS);
            if elapsed >= FADE_SECONDS {
                if recovery {
                    active.stage = Stage::Recovery;
                } else {
                    crossing.active = None;
                }
            } else {
                active.stage = Stage::FadeIn { elapsed, recovery };
            }
        }
        Stage::Recovery => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nif_material::{EffectPaletteDepthSource, EffectPaletteMaterial};
    use crate::{
        doors::DoorDestination,
        physics::MoveMode,
        profiling::ProfilingState,
        streaming::{StreamingMetrics, StreamingWorld, pending_marker_for_test},
    };
    use bevy::ecs::{message::Messages, system::RunSystemOnce};
    use bevy::time::TimeUpdateStrategy;
    use std::time::Duration;

    const DT: f32 = 0.05;

    #[derive(Resource, Default)]
    struct Teleports(Vec<TeleportPlayer>);

    fn collect(mut reader: MessageReader<TeleportPlayer>, mut out: ResMut<Teleports>) {
        out.0.extend(reader.read().copied());
    }

    fn door(destination: DoorDestination) -> LoadDoor {
        LoadDoor {
            ref_id: 0x10,
            destination,
        }
    }

    fn interior_door() -> LoadDoor {
        door(DoorDestination {
            destination_ref_id: 0x20,
            interior_cell_id: Some(77),
            worldspace_id: None,
            arrival_position: [100.0, 200.0, 300.0],
            arrival_rotation: [0.0, 0.0, 0.0],
        })
    }

    fn exterior_door() -> LoadDoor {
        door(DoorDestination {
            destination_ref_id: 0x21,
            interior_cell_id: None,
            worldspace_id: Some(60),
            // Grid (3, -2) in Creation units, 100 into the cell on each axis.
            arrival_position: [3.0 * 4096.0 + 100.0, -2.0 * 4096.0 + 100.0, 50.0],
            arrival_rotation: [0.0, 0.0, 0.0],
        })
    }

    /// A headless world with the crossing registered, the camera 100 units in front of a door
    /// whose bounds are a 100-unit cube, and one resident old-space cell.
    fn app_with(door: LoadDoor) -> (App, Entity) {
        app_with_physics(door, false)
    }

    fn app_with_physics(door: LoadDoor, physics: bool) -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, TransformPlugin, AssetPlugin::default()));
        if physics {
            app.add_plugins(bevy_rapier3d::prelude::RapierPhysicsPlugin::<
                bevy_rapier3d::prelude::NoUserData,
            >::default());
        }
        app.init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>()
            .init_asset::<EffectPaletteMaterial>()
            .insert_resource(RenderAssetBytesPerFrame {
                max_bytes: crate::config::EngineConfig::default().max_upload_bytes_per_frame(),
            });
        app.init_resource::<ProfilingState>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<StreamingWorld>()
            .init_resource::<crate::streaming::TerrainContinuity>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ActiveSpace>()
            .init_resource::<MovementTuning>()
            .init_resource::<Teleports>()
            .insert_resource(MoveMode::Noclip)
            .insert_resource(CursorCapture::Captured)
            .insert_resource(RenderOrigin(IVec2::new(1, 1)))
            .insert_resource(crate::config::EngineConfig::default())
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                DT,
            )))
            .add_plugins(DoorCrossingPlugin)
            .add_systems(Last, collect);
        app.world_mut().spawn((
            StreamingCamera,
            Transform::from_xyz(0.0, 0.0, 0.0).looking_to(Vec3::NEG_Z, Vec3::Y),
            GlobalTransform::default(),
        ));
        app.world_mut().spawn((
            door,
            ExpectedModelBounds::new(Vec3::splat(-50.0), Vec3::splat(50.0)).unwrap(),
            Transform::from_xyz(0.0, 0.0, -150.0),
            GlobalTransform::default(),
        ));
        let old_root = app.world_mut().spawn_empty().id();
        let old = CellKey::Exterior {
            worldspace_id: 1,
            grid_x: 1,
            grid_y: 1,
        };
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(old, old_root);
        app.finish();
        app.update();
        (app, old_root)
    }

    #[test]
    fn e_opens_the_nearest_door_in_walk_and_noclip() {
        for mode in [MoveMode::Walk, MoveMode::Noclip] {
            let (mut app, _) = app_with(interior_door());
            app.insert_resource(mode);
            let mut nearer = interior_door();
            nearer.ref_id = 91;
            let near = app
                .world_mut()
                .spawn((
                    nearer,
                    ExpectedModelBounds::new(Vec3::splat(-20.0), Vec3::splat(20.0)).unwrap(),
                    Transform::from_xyz(0.0, 0.0, -70.0),
                ))
                .id();
            app.update();
            let mut opens = app
                .world()
                .resource::<Messages<crate::doors::OpenDoor>>()
                .get_cursor_current();
            press_e(&mut app);
            let opened: Vec<_> = opens
                .read(app.world().resource::<Messages<crate::doors::OpenDoor>>())
                .map(|message| message.door)
                .collect();
            assert_eq!(opened, vec![near]);
            assert_eq!(
                app.world()
                    .resource::<DoorCrossing>()
                    .active
                    .as_ref()
                    .unwrap()
                    .door
                    .ref_id,
                91
            );
        }
    }

    #[test]
    fn released_or_missing_cursor_prevents_e_activation() {
        for missing in [false, true] {
            let (mut app, _) = app_with(interior_door());

            if missing {
                app.world_mut().remove_resource::<CursorCapture>();
            } else {
                app.insert_resource(CursorCapture::Released);
            }
            press_e(&mut app);

            assert!(!app.world().resource::<DoorCrossing>().is_active());
        }
    }

    #[test]
    fn enabled_wall_blocks_e_but_disabled_wall_does_not() {
        let (mut app, _) = app_with_physics(interior_door(), true);
        let wall = app
            .world_mut()
            .spawn((
                bevy_rapier3d::prelude::Collider::cuboid(50.0, 50.0, 5.0),
                Transform::from_xyz(0.0, 0.0, -35.0),
            ))
            .id();
        for _ in 0..3 {
            app.update();
        }

        press_e(&mut app);
        assert!(!app.world().resource::<DoorCrossing>().is_active());
        app.world_mut()
            .entity_mut(wall)
            .insert(bevy_rapier3d::prelude::ColliderDisabled);
        for _ in 0..3 {
            app.update();
        }

        press_e(&mut app);
        assert!(app.world().resource::<DoorCrossing>().is_active());
    }

    #[test]
    fn e_uses_current_ancestor_transforms_before_propagation() {
        let (mut app, _) = app_with(interior_door());
        let door = app
            .world_mut()
            .query_filtered::<Entity, With<LoadDoor>>()
            .single(app.world())
            .unwrap();
        let parent = app
            .world_mut()
            .spawn(Transform::from_xyz(10_000.0, 0.0, 0.0))
            .id();
        app.world_mut().entity_mut(door).insert(ChildOf(parent));
        app.update();
        assert!(
            app.world()
                .get::<GlobalTransform>(door)
                .unwrap()
                .translation()
                .x
                > 9_000.0
        );
        app.world_mut()
            .get_mut::<Transform>(parent)
            .unwrap()
            .translation = Vec3::ZERO;
        // Invoke selection before any schedule can refresh the cached global.
        assert!(
            app.world()
                .get::<GlobalTransform>(door)
                .unwrap()
                .translation()
                .x
                > 9_000.0
        );
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyE);
        app.world_mut()
            .run_system_once(drive_door_crossing)
            .unwrap();
        assert!(
            app.world()
                .get::<GlobalTransform>(door)
                .unwrap()
                .translation()
                .x
                > 9_000.0
        );
        assert!(app.world().resource::<DoorCrossing>().is_active());
    }

    fn press_e(app: &mut App) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyE);
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset(KeyCode::KeyE);
    }

    fn alpha(app: &mut App) -> f32 {
        let mut query = app
            .world_mut()
            .query_filtered::<&BackgroundColor, With<FadeOverlay>>();
        query.single(app.world()).unwrap().0.to_srgba().alpha
    }

    fn run_until_black(app: &mut App) {
        for _ in 0..20 {
            if app.world().resource::<ActiveSpace>() != &ActiveSpace::default() {
                return;
            }
            app.update();
        }
        panic!("the space never switched");
    }

    #[test]
    fn e_at_a_door_starts_a_crossing_and_e_elsewhere_does_not() {
        let (mut app, _) = app_with(interior_door());
        // Turn away from the door: nothing starts.
        {
            let mut query = app
                .world_mut()
                .query_filtered::<&mut Transform, With<StreamingCamera>>();
            let mut camera = query.single_mut(app.world_mut()).unwrap();
            camera.look_to(Vec3::Z, Vec3::Y);
        }
        press_e(&mut app);
        assert!(!app.world().resource::<DoorCrossing>().is_active());
        // Face it, but from beyond reach.
        {
            let mut query = app
                .world_mut()
                .query_filtered::<&mut Transform, With<StreamingCamera>>();
            let mut camera = query.single_mut(app.world_mut()).unwrap();
            camera.translation = Vec3::new(0.0, 0.0, 400.0);
            camera.look_to(Vec3::NEG_Z, Vec3::Y);
        }
        press_e(&mut app);
        assert!(!app.world().resource::<DoorCrossing>().is_active());
        {
            let mut query = app
                .world_mut()
                .query_filtered::<&mut Transform, With<StreamingCamera>>();
            query.single_mut(app.world_mut()).unwrap().translation = Vec3::ZERO;
        }
        press_e(&mut app);
        assert!(app.world().resource::<DoorCrossing>().is_active());
        // E while it runs does not start another.
        press_e(&mut app);
        assert!(app.world().resource::<DoorCrossing>().is_active());
    }

    #[test]
    fn the_space_switches_at_full_black_and_the_old_cells_go_the_same_frame() {
        let (mut app, old_root) = app_with(interior_door());
        press_e(&mut app);
        assert!(alpha(&mut app) < 1.0);
        run_until_black(&mut app);
        assert!(alpha(&mut app) >= 0.99, "the switch happens at full black");
        let space = *app.world().resource::<ActiveSpace>();
        assert_eq!(space.interior, Some(77));
        assert!(app.world().get_entity(old_root).is_err());
        assert_eq!(
            app.world()
                .resource::<StreamingWorld>()
                .cell_count_for_test(),
            0
        );
        assert_eq!(
            *app.world().resource::<CameraSpace>(),
            CameraSpace::Interior
        );
        // An interior landing leaves the render origin alone.
        assert_eq!(app.world().resource::<RenderOrigin>().0, IVec2::new(1, 1));
        let teleports = &app.world().resource::<Teleports>().0;
        assert_eq!(teleports.len(), 1);
        assert_eq!(
            teleports[0].position,
            creation_to_bevy(Vec3::new(100.0, 200.0, 300.0))
        );
    }

    #[test]
    fn an_exterior_landing_sets_the_render_origin_to_the_arrival_grid() {
        let (mut app, _) = app_with(exterior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        assert_eq!(app.world().resource::<RenderOrigin>().0, IVec2::new(3, -2));
        let space = *app.world().resource::<ActiveSpace>();
        assert_eq!(space.worldspace_id, Some(60));
        assert_eq!(space.interior, None);
        let teleports = &app.world().resource::<Teleports>().0;
        // 100 units into the cell on each axis, relative to the new origin: no cell offset.
        assert_eq!(
            teleports[0].position,
            creation_to_bevy(Vec3::new(100.0, 100.0, 50.0))
        );
    }

    #[test]
    fn the_fade_in_waits_for_the_landing_cell_and_starts_within_a_frame_of_it() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        for _ in 0..30 {
            app.update();
        }
        assert!(
            alpha(&mut app) >= 0.99,
            "still black while nothing is resident"
        );
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(77), root);
        // Pending work under the landing root keeps it black.
        let marker = app
            .world_mut()
            .spawn((pending_marker_for_test(), ChildOf(root)))
            .id();
        for _ in 0..4 {
            app.update();
        }
        assert!(alpha(&mut app) >= 0.99, "pending work under the root holds");
        app.world_mut().entity_mut(marker).despawn();
        for _ in 0..4 {
            app.update();
        }
        assert!(alpha(&mut app) < 1.0, "the fade-in starts once it is ready");
        for _ in 0..10 {
            app.update();
        }
        assert!(!app.world().resource::<DoorCrossing>().is_active());
        assert_eq!(alpha(&mut app), 0.0);
    }

    #[test]
    fn the_fade_in_places_the_player_on_the_landing_point_again() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(77), root);
        for _ in 0..4 {
            app.update();
        }
        assert!(alpha(&mut app) < 1.0, "the fade-in started");
        let teleports = &app.world().resource::<Teleports>().0;
        assert_eq!(teleports.len(), 2, "{teleports:?}");
        assert_eq!(teleports[1], teleports[0], "the same landing point twice");
    }

    #[test]
    fn exterior_landing_and_restore_survive_a_rebase_during_the_black_hold() {
        for restoring in [false, true] {
            let (mut app, _) = app_with(exterior_door());
            press_e(&mut app);
            run_until_black(&mut app);
            if restoring {
                let target = app
                    .world()
                    .resource::<DoorCrossing>()
                    .active
                    .as_ref()
                    .unwrap()
                    .target;
                app.world_mut()
                    .resource_mut::<StreamingWorld>()
                    .set_failed_for_test(target.key);
                app.update();
            }
            let crossing = app
                .world()
                .resource::<DoorCrossing>()
                .active
                .as_ref()
                .unwrap();
            let landing = if restoring {
                crossing.restore.landing()
            } else {
                crossing.target
            };
            let saved_origin = landing.origin.unwrap();
            let rebased = saved_origin + IVec2::new(2, -1);
            app.world_mut().resource_mut::<RenderOrigin>().0 = rebased;
            let root = app.world_mut().spawn_empty().id();
            app.world_mut()
                .resource_mut::<StreamingWorld>()
                .set_resident_for_test(landing.key, root);
            for _ in 0..4 {
                app.update();
            }
            assert!(
                alpha(&mut app) < 1.0,
                "restoring={restoring}: fade-in starts"
            );
            let teleports = &app.world().resource::<Teleports>().0;
            let final_pose = teleports.last().unwrap();
            assert_eq!(
                absolute_feet(final_pose.position, rebased),
                absolute_feet(landing.position, saved_origin)
            );
            assert_eq!(final_pose.yaw, landing.yaw);
        }
    }

    #[test]
    fn walking_pauses_only_while_a_crossing_is_in_progress() {
        let mut world = World::new();
        assert!(world.run_system_once(no_door_crossing).unwrap());
        world.insert_resource(DoorCrossing::default());
        assert!(world.run_system_once(no_door_crossing).unwrap());
        world.insert_resource(DoorCrossing::holding_for_test());
        assert!(!world.run_system_once(no_door_crossing).unwrap());
    }

    #[test]
    fn pending_work_in_another_cell_does_not_hold_the_fade_in() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(77), root);
        // The global counts and a marker under some other cell's root are not the landing cell's.
        {
            let mut metrics = app.world_mut().resource_mut::<StreamingMetrics>();
            metrics.pending_asset_instances = 50;
            metrics.pending_surface_instances = 50;
        }
        let other_root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .spawn((pending_marker_for_test(), ChildOf(other_root)));
        for _ in 0..4 {
            app.update();
        }
        assert!(alpha(&mut app) < 1.0, "the fade-in started");
    }

    fn budget(app: &App) -> Option<usize> {
        app.world().resource::<RenderAssetBytesPerFrame>().max_bytes
    }

    fn configured_budget() -> Option<usize> {
        crate::config::EngineConfig::default().max_upload_bytes_per_frame()
    }

    #[test]
    fn the_upload_budget_is_lifted_at_the_switch_and_restored_at_the_fade_in() {
        let (mut app, _) = app_with(interior_door());
        assert!(configured_budget().is_some());
        press_e(&mut app);
        assert_eq!(budget(&app), configured_budget(), "kept while fading out");
        run_until_black(&mut app);
        assert_eq!(budget(&app), None, "lifted from the switch");
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(77), root);
        app.update();
        assert_eq!(budget(&app), None, "still lifted while the frames settle");
        for _ in 0..10 {
            app.update();
        }
        assert!(alpha(&mut app) < 1.0);
        assert_eq!(budget(&app), configured_budget());
    }

    #[test]
    fn the_upload_budget_is_restored_when_the_destination_fails() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        assert_eq!(budget(&app), None);
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(CellKey::Interior(77));
        app.update();
        app.update();
        assert_eq!(
            budget(&app),
            None,
            "still black, still lifted while restoring"
        );
        // The player's own space fails too: fade in with movement still guarded.
        let own = CellKey::Exterior {
            worldspace_id: crate::config::EngineConfig::default().worldspace_id,
            grid_x: 1,
            grid_y: 1,
        };
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(own);
        app.update();
        app.update();
        assert_eq!(budget(&app), configured_budget());
    }

    #[test]
    fn the_fade_in_waits_for_a_landing_texture_and_a_mesh_then_settles_two_frames() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        let texture = app.world().resource::<Assets<Image>>().reserve_handle();
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color_texture: Some(texture.clone()),
                ..default()
            });
        let mesh = app.world().resource::<Assets<Mesh>>().reserve_handle();
        let root = app.world_mut().spawn_empty().id();
        app.world_mut().spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material),
            ChildOf(root),
        ));
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(77), root);
        for _ in 0..20 {
            app.update();
        }
        assert!(
            alpha(&mut app) >= 0.99,
            "the texture and mesh are still loading"
        );
        assert_eq!(budget(&app), None);
        app.world_mut()
            .resource_mut::<Assets<Image>>()
            .insert(texture.id(), Image::default())
            .unwrap();
        for _ in 0..20 {
            app.update();
        }
        assert!(alpha(&mut app) >= 0.99, "the mesh is still loading");
        app.world_mut()
            .resource_mut::<Assets<Mesh>>()
            .insert(
                mesh.id(),
                Mesh::new(
                    bevy::mesh::PrimitiveTopology::TriangleList,
                    bevy::asset::RenderAssetUsages::default(),
                ),
            )
            .unwrap();
        // Loaded on the first of these frames; the fade-in starts on the SETTLE_FRAMES-th loaded
        // frame (the second), and the fade itself shows from the frame after.
        app.update();
        assert!(alpha(&mut app) >= 0.99, "settling");
        assert_eq!(budget(&app), None);
        app.update();
        assert_eq!(budget(&app), configured_budget(), "the fade-in started");
        app.update();
        assert!(alpha(&mut app) < 1.0, "the fade shows");
    }

    /// A landing whose assets are all loaded still holds the black while a surface a camera can see
    /// has not been confirmed drawable by the render world, and fades in once it has.
    #[test]
    fn the_fade_in_waits_for_a_visible_surface_the_render_world_has_not_drawn() {
        let (mut app, _) = app_with(interior_door());
        // Protocol fixture: model the renderer and its completed native proof explicitly.
        // The surface's waiting marker remains the one unfinished readiness fact.
        app.add_systems(
            First,
            |requests: Res<DestinationRequests>,
             mut results: ResMut<crate::swap_bridge::systems::RenderResults>| {
                results.renderer = true;
                for (id, request) in requests.iter() {
                    let frame = results.frame;
                    results.insert_for_test(
                        id,
                        crate::swap_bridge::systems::EvaluatedProof {
                            key: request.key,
                            frame,
                            extracted_generation: request.key.generation,
                            proof: crate::swap_bridge::destination::RenderProof::default(),
                        },
                    );
                }
            },
        );
        press_e(&mut app);
        run_until_black(&mut app);
        let root = app.world_mut().spawn_empty().id();
        let surface = app
            .world_mut()
            .spawn((
                crate::swap_bridge::SwapWaiting,
                crate::swap_bridge::SwapInCameraView,
                ChildOf(root),
            ))
            .id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(77), root);
        for _ in 0..20 {
            app.update();
        }
        assert!(
            alpha(&mut app) >= 0.99,
            "a visible surface is still waiting on the render world"
        );
        assert_eq!(budget(&app), None);
        // The render world confirms it drawn: the waiting marker leaves.
        app.world_mut()
            .entity_mut(surface)
            .remove::<crate::swap_bridge::SwapWaiting>();
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(budget(&app), configured_budget(), "the fade-in started");
    }

    /// The hold waits on one destination request for the landing cell, and withdraws it, with its
    /// report, when the hold ends.
    #[test]
    fn the_hold_requests_the_destination_and_withdraws_it_when_it_ends() {
        use crate::swap_bridge::destination::DestinationRequests;
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        for _ in 0..3 {
            app.update();
        }
        {
            let requests = app.world().resource::<DestinationRequests>();
            let (_, request) = requests.iter().next().expect("the hold made a request");
            assert_eq!(request.coverage.as_ref(), &[CellKey::Interior(77)]);
            assert_eq!(requests.iter().count(), 1);
        }
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(77), root);
        for _ in 0..8 {
            app.update();
        }
        assert_eq!(budget(&app), configured_budget(), "the fade-in started");
        assert!(
            app.world().resource::<DestinationRequests>().is_empty(),
            "the request left with the hold"
        );
    }

    #[test]
    fn ordinary_destination_hold_creates_a_request() {
        use crate::swap_bridge::destination::DestinationRequests;

        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        for _ in 0..3 {
            app.update();
        }
        let requests = app.world().resource::<DestinationRequests>();
        requests.iter().next().expect("the hold made a request");
    }

    #[test]
    fn ordinary_restore_failure_fades_in_with_movement_paused() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        let restore_key = app
            .world()
            .resource::<DoorCrossing>()
            .active
            .as_ref()
            .expect("a crossing is active")
            .restore
            .landing()
            .key;
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(CellKey::Interior(77));
        // The switch back unloads every cell (and so clears failure marks), so the restore's
        // own cell fails only once the player has been put back.
        for _ in 0..3 {
            app.update();
        }
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(restore_key);
        for _ in 0..60 {
            app.update();
        }
        let still_black = alpha(&mut app) >= 0.99;

        assert!(
            !still_black && app.world().resource::<DoorCrossing>().needs_recovery(),
            "stock: the failed restore fades in with movement paused"
        );
    }

    #[test]
    fn the_fade_in_waits_for_an_effect_palette_materials_source_texture_and_palette() {
        fade_in_waits_for_effect_palette_images(false);
    }

    #[test]
    fn the_fade_in_waits_for_a_depth_wrapped_effect_palette_through_its_source() {
        fade_in_waits_for_effect_palette_images(true);
    }

    /// `depth_wrapped`: the surface carries only the wrapper's source, as a palette surface
    /// under an authored depth override does; the wrapper itself is not inspected.
    fn fade_in_waits_for_effect_palette_images(depth_wrapped: bool) {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        let source = app.world().resource::<Assets<Image>>().reserve_handle();
        let palette = app.world().resource::<Assets<Image>>().reserve_handle();
        let material = app
            .world_mut()
            .resource_mut::<Assets<EffectPaletteMaterial>>()
            .add(EffectPaletteMaterial {
                base: StandardMaterial {
                    base_color_texture: Some(source.clone()),
                    ..default()
                },
                extension: crate::nif_material::EffectPalette::new(
                    palette.clone(),
                    true,
                    true,
                    1.0,
                ),
            });
        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Mesh::new(
                bevy::mesh::PrimitiveTopology::TriangleList,
                bevy::asset::RenderAssetUsages::default(),
            ));
        let root = app.world_mut().spawn_empty().id();
        let part = app.world_mut().spawn((Mesh3d(mesh), ChildOf(root))).id();
        if depth_wrapped {
            app.world_mut()
                .entity_mut(part)
                .insert(EffectPaletteDepthSource(material));
        } else {
            app.world_mut()
                .entity_mut(part)
                .insert(MeshMaterial3d(material));
        }
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(77), root);
        for (image, waiting) in [(source, "the palette"), (palette, "")] {
            for _ in 0..20 {
                app.update();
            }
            assert!(alpha(&mut app) >= 0.99, "an image is still loading");
            app.world_mut()
                .resource_mut::<Assets<Image>>()
                .insert(image.id(), Image::default())
                .unwrap();
            if !waiting.is_empty() {
                for _ in 0..20 {
                    app.update();
                }
                assert!(alpha(&mut app) >= 0.99, "{waiting} is still loading");
            }
        }
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(budget(&app), configured_budget(), "the fade-in started");
    }

    #[test]
    fn a_destination_timeout_returns_to_the_saved_space_before_resuming() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        let frames = (LANDING_TIMEOUT_SECONDS / DT) as usize;
        for _ in 0..frames - 5 {
            app.update();
        }
        assert!(alpha(&mut app) >= 0.99);
        for _ in 0..20 {
            app.update();
        }
        assert!(alpha(&mut app) >= 0.99, "the return is still loading");
        assert_eq!(
            *app.world().resource::<ActiveSpace>(),
            ActiveSpace::default()
        );
        assert!(!app.world_mut().run_system_once(no_door_crossing).unwrap());
        let own = app
            .world()
            .resource::<DoorCrossing>()
            .active
            .as_ref()
            .unwrap()
            .restore
            .landing()
            .key;
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(own, root);
        for _ in 0..20 {
            app.update();
        }
        assert!(alpha(&mut app) < 0.01, "the loaded return fades in");
        assert!(app.world_mut().run_system_once(no_door_crossing).unwrap());
        assert_eq!(budget(&app), configured_budget());
    }

    #[test]
    fn a_return_timeout_fades_in_without_releasing_the_movement_guard() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        for _ in 0..((2.0 * LANDING_TIMEOUT_SECONDS + 1.0) / DT) as usize {
            app.update();
        }
        assert!(
            alpha(&mut app) < 0.01,
            "failure does not leave a black screen"
        );
        assert!(app.world().resource::<DoorCrossing>().needs_recovery());
        assert!(!app.world_mut().run_system_once(no_door_crossing).unwrap());
        assert_eq!(budget(&app), configured_budget());
    }

    #[test]
    fn a_failed_return_stays_guarded_after_the_fade_in() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        let own = app
            .world()
            .resource::<DoorCrossing>()
            .active
            .as_ref()
            .unwrap()
            .restore
            .landing()
            .key;
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(CellKey::Interior(77));
        app.update();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(own);
        for _ in 0..20 {
            app.update();
        }
        assert!(alpha(&mut app) < 0.01);
        assert!(app.world().resource::<DoorCrossing>().needs_recovery());
        assert!(!app.world_mut().run_system_once(no_door_crossing).unwrap());
        assert_eq!(budget(&app), configured_budget());
    }

    #[test]
    fn a_destination_that_fails_puts_the_player_back_where_they_were() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(CellKey::Interior(77));
        app.update();
        app.update();
        let space = *app.world().resource::<ActiveSpace>();
        assert_eq!(space, ActiveSpace::default());
        assert_eq!(app.world().resource::<RenderOrigin>().0, IVec2::new(1, 1));
        let teleports = &app.world().resource::<Teleports>().0;
        assert_eq!(teleports.len(), 2);
        let back = teleports[1].position;
        assert!(back.x.abs() < 0.01 && back.z.abs() < 0.01);
    }

    #[test]
    fn the_restore_pose_survives_a_render_origin_rebase_during_the_fade_out() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        // A rebase while fading out: the origin moves, the stored pose must not.
        app.world_mut().resource_mut::<RenderOrigin>().0 = IVec2::new(5, 5);
        run_until_black(&mut app);
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(CellKey::Interior(77));
        app.update();
        app.update();
        let origin = app.world().resource::<RenderOrigin>().0;
        let teleports = &app.world().resource::<Teleports>().0;
        let back = teleports.last().unwrap().position;
        // The player stood at render (0, _, 0) under origin (1, 1): the same world place now.
        let world_x = back.x + origin.x as f32 * CELL_SIZE;
        let world_z = back.z - origin.y as f32 * CELL_SIZE;
        assert!((world_x - CELL_SIZE).abs() < 0.01, "x {world_x}");
        assert!((world_z + CELL_SIZE).abs() < 0.01, "z {world_z}");
        let tuning = MovementTuning::default();
        let feet_y = -tuning.eye_height - (tuning.capsule_half_height() + tuning.capsule_radius);
        assert!((back.y - feet_y).abs() < 0.01);
    }

    #[test]
    fn a_failed_crossing_from_an_interior_lands_where_the_player_stood() {
        // Start inside interior 5 with a non-zero render origin that interiors do not use.
        let (mut app, _) = app_with(exterior_door());
        *app.world_mut().resource_mut::<ActiveSpace>() = ActiveSpace {
            worldspace_id: Some(60),
            interior: Some(5),
        };
        *app.world_mut().resource_mut::<CameraSpace>() = CameraSpace::Interior;
        app.world_mut().resource_mut::<RenderOrigin>().0 = IVec2::new(7, -3);
        {
            let mut query = app
                .world_mut()
                .query_filtered::<&mut Transform, With<StreamingCamera>>();
            query.single_mut(app.world_mut()).unwrap().translation = Vec3::new(10.0, 0.0, 0.0);
        }
        press_e(&mut app);
        let door_target = CellKey::Exterior {
            worldspace_id: 60,
            grid_x: 3,
            grid_y: -2,
        };
        for _ in 0..20 {
            if app.world().resource::<ActiveSpace>().interior.is_none() {
                break;
            }
            app.update();
        }
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(door_target);
        app.update();
        app.update();
        let space = *app.world().resource::<ActiveSpace>();
        assert_eq!(space.interior, Some(5), "back in the interior");
        let tuning = MovementTuning::default();
        let (body_offset, _) = body_and_camera_for_feet(&tuning, Vec3::ZERO);
        let expected = Vec3::new(10.0, 0.0, 0.0) - Vec3::Y * tuning.eye_height - body_offset;
        let back = app
            .world()
            .resource::<Teleports>()
            .0
            .last()
            .unwrap()
            .position;
        assert!(
            (back - expected).length() < 0.01,
            "restored to {back}, expected {expected}"
        );
    }

    #[test]
    fn a_door_without_model_bounds_can_still_be_activated() {
        let (mut app, _) = app_with(interior_door());
        let mut doors = app.world_mut().query_filtered::<Entity, With<LoadDoor>>();
        let door = doors.single(app.world()).unwrap();
        app.world_mut()
            .entity_mut(door)
            .remove::<ExpectedModelBounds>();
        press_e(&mut app);
        assert!(app.world().resource::<DoorCrossing>().is_active());
    }

    #[test]
    fn the_teleport_target_is_the_arrival_feet_position() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        let tuning = MovementTuning::default();
        let (_, eye) =
            body_and_camera_for_feet(&tuning, creation_to_bevy(Vec3::new(100.0, 200.0, 300.0)));
        let mut view = app
            .world_mut()
            .query_filtered::<&Transform, With<StreamingCamera>>();
        assert!((view.single(app.world()).unwrap().translation - eye).length() < 1e-3);
    }

    #[test]
    fn a_nearer_collider_blocks_the_view_of_a_door() {
        // A wall well in front of the door hides it.
        assert!(door_view_blocked(Some(40.0), 100.0));
        // The door's own collider or frame at its bounds does not.
        assert!(!door_view_blocked(Some(100.0), 100.0));
        assert!(!door_view_blocked(Some(90.0), 100.0));
        // Something behind the door, or nothing at all, does not either.
        assert!(!door_view_blocked(Some(180.0), 100.0));
        assert!(!door_view_blocked(None, 100.0));
    }

    #[test]
    fn a_ray_tests_the_oriented_bounds() {
        let bounds =
            ExpectedModelBounds::new(Vec3::new(-10.0, 0.0, -2.0), Vec3::new(10.0, 40.0, 2.0))
                .unwrap();
        let door = GlobalTransform::from(
            Transform::from_xyz(0.0, 0.0, -100.0).with_rotation(Quat::from_rotation_y(0.5)),
        );
        let hit = ray_hits_door(
            Vec3::new(0.0, 20.0, 0.0),
            Vec3::NEG_Z,
            &door,
            &bounds,
            200.0,
        );
        assert!(hit.is_some_and(|distance| (distance - 98.0).abs() < 5.0));
        assert!(ray_hits_door(Vec3::new(0.0, 20.0, 0.0), Vec3::Z, &door, &bounds, 200.0).is_none());
        assert!(
            ray_hits_door(Vec3::new(0.0, 20.0, 0.0), Vec3::NEG_Z, &door, &bounds, 50.0).is_none()
        );
    }

    #[test]
    fn the_timeout_fades_in_anyway() {
        // The timeout may clear the screen only after restoring, and never releases movement
        // into an unproved destination.
        a_return_timeout_fades_in_without_releasing_the_movement_guard();
    }
    #[test]
    fn an_unspecified_destination_world_uses_active_then_configured_exterior() {
        let mut door = exterior_door();
        door.destination.worldspace_id = None;
        for (active, expected) in [(None, 42), (Some(60), 60)] {
            let space = ActiveSpace {
                worldspace_id: active,
                interior: Some(77),
            };
            let landing = landing_for(&door, &space, 42);
            assert_eq!(landing.space.worldspace_id, Some(expected));
            assert_eq!(landing.space.interior, None);
            assert!(
                matches!(landing.key, CellKey::Exterior { worldspace_id, .. } if worldspace_id == expected)
            );
            assert_eq!(world_id(&landing.space, 42), WorldId::exterior(expected));
            let restore = Restore {
                space,
                camera_space: CameraSpace::Interior,
                configured_worldspace: 42,
                feet: Vec3::new(1.0, 2.0, 3.0),
                yaw: 0.25,
            };
            let returned = restore.landing();
            assert_eq!(returned.space, space);
            assert_eq!(returned.key, CellKey::Interior(77));
            assert_eq!(returned.position, restore.feet);
        }
    }

    #[test]
    fn a_rebased_hold_withdraws_its_current_report_and_drops_the_old_one() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        let (id, old) = app
            .world()
            .resource::<DoorCrossing>()
            .active
            .as_ref()
            .unwrap()
            .request
            .unwrap();
        app.world_mut().resource_mut::<DestinationReports>().set(
            crate::swap_bridge::destination::DestinationReport {
                key: old,
                evaluated_render_frame: crate::swap_bridge::destination::RenderFrame(1),
                state: DestinationState::Ready,
            },
        );
        app.world_mut()
            .resource_mut::<Generations>()
            .bump(old.world, InvalidationCause::Candidate);
        app.update();
        let (held, current) = app
            .world()
            .resource::<DoorCrossing>()
            .active
            .as_ref()
            .unwrap()
            .request
            .unwrap();
        assert_eq!(held, id);
        assert_ne!(current, old);
        assert!(
            app.world()
                .resource::<DestinationReports>()
                .get(&old)
                .is_none()
        );
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(77), root);
        for _ in 0..10 {
            app.update();
        }
        assert!(app.world().resource::<DestinationRequests>().is_empty());
        assert!(
            app.world()
                .resource::<DestinationReports>()
                .get(&current)
                .is_none()
        );
    }

    #[test]
    fn space_switch_leaves_camera_rotation_to_the_teleport_consumer() {
        let (mut app, _) = app_with(interior_door());
        let camera = {
            let mut cameras = app
                .world_mut()
                .query_filtered::<Entity, With<StreamingCamera>>();
            cameras.single(app.world()).unwrap()
        };
        let mut players = app.world_mut().query_filtered::<(), With<PlayerBody>>();
        assert!(
            players.iter(app.world()).next().is_none(),
            "this fixture exercises the camera-only path"
        );
        let original_rotation = Quat::from_rotation_y(0.1);
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .rotation = original_rotation;
        let space = *app.world().resource::<ActiveSpace>();
        let configured_worldspace = app
            .world()
            .resource::<crate::config::EngineConfig>()
            .worldspace_id;
        let landing = landing_for(&interior_door(), &space, configured_worldspace);
        let mut teleports = app
            .world()
            .resource::<Messages<TeleportPlayer>>()
            .get_cursor_current();
        switch_space(app.world_mut(), landing);
        assert_eq!(
            app.world().get::<Transform>(camera).unwrap().rotation,
            original_rotation,
            "the space switch must not apply heading behind the teleport consumer's back"
        );
        let headings: Vec<_> = teleports
            .read(app.world().resource::<Messages<TeleportPlayer>>())
            .map(|message| message.yaw)
            .collect();
        assert_eq!(
            headings,
            vec![landing.yaw],
            "authored heading is still delivered to its owner"
        );
    }

    #[test]
    fn ordinary_failed_landing_asset_stays_hidden_without_refusing_or_restoring() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        let root = app.world_mut().spawn_empty().id();
        let failed = app
            .world_mut()
            .spawn((
                crate::streaming::AssetFailed,
                Visibility::Hidden,
                ChildOf(root),
            ))
            .id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(CellKey::Interior(77), root);
        for _ in 0..SETTLE_FRAMES + 1 {
            app.update();
        }
        assert_eq!(
            app.world().resource::<ActiveSpace>().interior,
            Some(77),
            "an ordinary hidden asset failure must not send the player back"
        );
        assert_eq!(
            app.world().get::<Visibility>(failed),
            Some(&Visibility::Hidden)
        );
        assert!(
            app.world()
                .get::<crate::streaming::AssetFailed>(failed)
                .is_some(),
            "the explicit failed-asset diagnostic remains available"
        );
        assert!(
            !app.world()
                .contains_resource::<crate::streaming::failure::LoadingRefusal>()
        );
        let crossing = app.world().resource::<DoorCrossing>();
        assert!(!crossing.needs_recovery());
        assert!(matches!(
            crossing.active.as_ref().map(|active| active.stage),
            Some(Stage::FadeIn {
                recovery: false,
                ..
            }) | None
        ));
    }

    #[test]
    fn destination_requests_include_camera_prepasses_and_each_stock_light_family() {
        for light_kind in 0..3 {
            let (mut app, _) = app_with(interior_door());
            let camera = app
                .world_mut()
                .query_filtered::<Entity, With<StreamingCamera>>()
                .single(app.world())
                .unwrap();
            app.world_mut().entity_mut(camera).insert((
                DepthPrepass,
                NormalPrepass,
                MotionVectorPrepass,
            ));
            match light_kind {
                0 => {
                    app.world_mut().spawn(DirectionalLight {
                        shadow_maps_enabled: true,
                        ..default()
                    });
                }
                1 => {
                    app.world_mut().spawn(PointLight {
                        shadow_maps_enabled: true,
                        ..default()
                    });
                }
                _ => {
                    app.world_mut().spawn(SpotLight {
                        shadow_maps_enabled: true,
                        ..default()
                    });
                }
            }
            press_e(&mut app);
            run_until_black(&mut app);
            let requests = app.world().resource::<DestinationRequests>();
            let (_, request) = requests.iter().next().unwrap();
            assert_eq!(request.key.world, WorldId::interior(77));
            assert_eq!(request.passes.iter().collect::<Vec<_>>(), PassFamily::ALL);
        }
    }

    #[test]
    fn late_camera_and_light_passes_rekey_the_hold_before_any_old_proof_can_release_it() {
        use crate::swap_bridge::{
            destination::{RenderFrame, RenderProof},
            systems::{EvaluatedProof, RenderResults},
        };
        for family in [
            PassFamily::DepthPrepass,
            PassFamily::NormalPrepass,
            PassFamily::MotionPrepass,
            PassFamily::Shadow,
        ] {
            let (mut app, _) = app_with(interior_door());
            press_e(&mut app);
            run_until_black(&mut app);
            let (id, old) = app
                .world()
                .resource::<DoorCrossing>()
                .active
                .as_ref()
                .unwrap()
                .request
                .unwrap();
            assert!(
                !app.world()
                    .resource::<DestinationRequests>()
                    .iter()
                    .find(|(held, _)| *held == id)
                    .unwrap()
                    .1
                    .passes
                    .contains(family)
            );
            let camera = app
                .world_mut()
                .query_filtered::<Entity, With<StreamingCamera>>()
                .single(app.world())
                .unwrap();
            match family {
                PassFamily::DepthPrepass => {
                    app.world_mut().entity_mut(camera).insert(DepthPrepass);
                }
                PassFamily::NormalPrepass => {
                    app.world_mut().entity_mut(camera).insert(NormalPrepass);
                }
                PassFamily::MotionPrepass => {
                    app.world_mut()
                        .entity_mut(camera)
                        .insert(MotionVectorPrepass);
                }
                PassFamily::Shadow => {
                    app.world_mut().spawn(PointLight {
                        shadow_maps_enabled: true,
                        ..default()
                    });
                }
                PassFamily::Forward => unreachable!(),
            }
            let root = app.world_mut().spawn_empty().id();
            app.world_mut()
                .resource_mut::<StreamingWorld>()
                .set_resident_for_test(CellKey::Interior(77), root);
            let old_proof = EvaluatedProof {
                key: old,
                frame: RenderFrame(10),
                extracted_generation: old.generation,
                proof: RenderProof::default(),
            };
            {
                let mut results = app.world_mut().resource_mut::<RenderResults>();
                results.renderer = true;
                results.insert_for_test(id, old_proof.clone());
            }
            app.update();
            let (held, current) = app
                .world()
                .resource::<DoorCrossing>()
                .active
                .as_ref()
                .unwrap()
                .request
                .unwrap();
            assert_eq!(held, id);
            assert!(
                app.world()
                    .resource::<DestinationRequests>()
                    .iter()
                    .find(|(held, _)| *held == id)
                    .unwrap()
                    .1
                    .passes
                    .contains(family),
                "newly enabled pass escaped the held request: {family:?}"
            );
            assert_ne!(current, old, "a weaker old proof must have a different key");
            assert!(
                app.world()
                    .resource::<DestinationReports>()
                    .get(&old)
                    .is_none()
            );
            assert!(alpha(&mut app) >= 0.99);
            let mut current_proof = old_proof;
            current_proof.key = current;
            current_proof.extracted_generation = current.generation;
            current_proof.proof.pass_missing = Some(family);
            app.world_mut()
                .resource_mut::<RenderResults>()
                .insert_for_test(id, current_proof.clone());
            for _ in 0..SETTLE_FRAMES + 2 {
                app.update();
            }
            assert!(
                alpha(&mut app) >= 0.99,
                "a missing new pass released the cover"
            );
            assert_eq!(budget(&app), None);
            current_proof.proof.pass_missing = None;
            app.world_mut()
                .resource_mut::<RenderResults>()
                .insert_for_test(id, current_proof);
            for _ in 0..SETTLE_FRAMES + 2 {
                app.update();
            }
            assert_eq!(
                budget(&app),
                configured_budget(),
                "matching complete proof must release the hold"
            );
            assert!(app.world().resource::<DestinationRequests>().is_empty());
        }
    }
}
