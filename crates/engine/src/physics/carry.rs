//! Skyrim-style object carrying for the WALK player (#104 phase b).
//!
//! Hold Activate (`E`) on a movable object for `fZKeyDelay` to pick it up. The object hangs on a
//! spring at the point where it was grabbed, in front of the camera, and keeps its collider and
//! its collision group the whole time. The release that ends the grabbing hold does nothing; the
//! next press and release of `E` drops it. That latch is how spud documents the game's rule. It
//! is not verified against the retail binary: `controlmap.txt` in `Skyrim - Interface.bsa` maps
//! Activate to `E` but says nothing about holding. Left click while carrying throws it; that is
//! Mudcrab tuning (see [`THROW_BUTTON`]). The object drops by itself when it snags (the grabbed point lags too far behind the target), when the
//! player stands on it, or when it is pressed against a much heavier moving body.
//!
//! The spring is a port of the mechanism that the MIT-licensed spud engine documents
//! (chasmlol/spud, `src/physics/world.cpp` `apply_grab`, read as a description; no code is
//! copied). Constants that come from spud's description or fallbacks are marked "spud". Skyrim's
//! own GMST names are kept in [`CarryTuning::apply_gmst`]. The pack carries only `fZKeyMaxForce`
//! (100) today; the other values are the Skyrim defaults that spud uses.
//!
//! Spud's lag test compares a distance in metres with a constant in game units, so its "stuck"
//! drop never fires at play distances. Here every distance is in Creation units, and a test pins
//! that 96 units drops at 1.37 m.

use super::{
    ControlledCamera, CursorCapture, DebugTankard, DynamicClutter, MoveMode, MovementTuning,
    PHYSICS_TIMESTEP, PlayerBody,
};
use crate::{
    console::console_closed, door_crossing::DoorCrossing, world::components::StreamingCamera,
};
use bevy::{ecs::schedule::common_conditions::not, prelude::*};
use bevy_rapier3d::prelude::*;

/// Mouse button that throws the carried object. Mudcrab tuning: the retail attack button may shove
/// or throw, but that is not verified here, and the throw speed constants are ours.
pub const THROW_BUTTON: MouseButton = MouseButton::Left;

/// Creation units per metre. The converter and the movement code use 1 unit = 1/70 m, and the
/// Skyrim GMSTs that spud reads in m/s^2 are scaled by this value.
pub const UNITS_PER_METRE: f32 = 70.0;

/// Spring and drop constants. All distances are in Creation units, masses in kg.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct CarryTuning {
    /// `fActivatePickLength:Interface`, 180 units (about 2.57 m). Spud hard-codes the same value.
    pub reach: f32,
    /// `fZKeyDelay:Controls`, seconds the key must stay down before it grabs. Spud never resolves
    /// the key, so it always uses 0.2.
    pub key_delay: f32,
    /// `fZKeySpringElasticity`. The fraction of the position error that the spring asks to remove
    /// in one 60 Hz step: 0.2 per step is 12 per second (spud).
    pub spring_elasticity: f32,
    /// `fZKeySpringDamping`. The fraction of the point velocity the spring removes each step
    /// (spud).
    pub spring_damping: f32,
    /// `fZKeyObjectDamping`. Both body velocities are multiplied by this value each step (spud).
    pub object_damping: f32,
    /// `fZKeyMaxForce`. Largest spring acceleration per kilogram, in m/s^2. The pack stores 100.
    pub max_force: f32,
    /// `fGrabMaxWeightWalking`, heaviest body the player can grab, in kg (spud).
    pub grab_max_weight: f32,
    /// Distance between the grabbed point and its target that drops the object, in units.
    /// Spud's description gives 96 game units.
    pub lag_break: f32,
    /// `fZKeyMaxContactDistance`, in units. A lag above this arms the contact-mass rule.
    pub contact_distance: f32,
    /// `fZKeyMaxContactMassRatio`. The heaviest moving body that touches the held object, divided
    /// by the held mass, drops the object when it is above this value and the lag is above
    /// [`Self::contact_distance`].
    pub contact_mass_ratio: f32,
    /// Throw momentum in kg x units/s. Mudcrab tuning: neither Skyrim nor spud has a throw.
    pub throw_momentum: f32,
    /// Fastest throw, in units/s. Mudcrab tuning.
    pub throw_max_speed: f32,
}

impl Default for CarryTuning {
    fn default() -> Self {
        Self {
            reach: 180.0,
            key_delay: 0.2,
            spring_elasticity: 0.2,
            spring_damping: 0.5,
            object_damping: 0.75,
            max_force: 100.0,
            grab_max_weight: 100.0,
            lag_break: 96.0,
            contact_distance: 10.0,
            contact_mass_ratio: 4.0,
            throw_momentum: 6000.0,
            throw_max_speed: 1000.0,
        }
    }
}

impl CarryTuning {
    /// Spring gain in 1/s: the share of the error asked for per second.
    pub fn spring_gain(&self, dt: f32) -> f32 {
        self.spring_elasticity / dt
    }

    /// Largest spring acceleration in units/s^2 (100 m/s^2 is 7000 units/s^2).
    pub fn max_acceleration(&self) -> f32 {
        self.max_force * UNITS_PER_METRE
    }

    /// Closest hold distance from the camera: the player's radius plus 5 units, so the object
    /// does not rest inside the capsule.
    pub fn min_hold_distance(&self, capsule_radius: f32) -> f32 {
        capsule_radius + 5.0
    }

    /// Replace each constant that `lookup` knows by its Skyrim GMST name. Missing, non-finite and
    /// non-positive values keep the current constant. Nothing calls this until the converter
    /// publishes the carry GMSTs (#595); the pack holds only `fZKeyMaxForce` today.
    pub fn apply_gmst(&mut self, lookup: impl Fn(&str) -> Option<f32>) {
        let take = |name: &str, slot: &mut f32| {
            if let Some(value) = lookup(name).filter(|value| value.is_finite() && *value > 0.0) {
                *slot = value;
            }
        };
        take("fZKeyDelay", &mut self.key_delay);
        take("fZKeySpringElasticity", &mut self.spring_elasticity);
        take("fZKeySpringDamping", &mut self.spring_damping);
        take("fZKeyObjectDamping", &mut self.object_damping);
        take("fZKeyMaxForce", &mut self.max_force);
        take("fGrabMaxWeightWalking", &mut self.grab_max_weight);
        take("fZKeyMaxContactDistance", &mut self.contact_distance);
        take("fZKeyMaxContactMassRatio", &mut self.contact_mass_ratio);
    }
}

/// Why a grab attempt did nothing. Kept for diagnostics and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabRefusal {
    /// The first thing on the line of sight cannot be carried (a wall, or a fixed object).
    NotCarriable,
    /// The body is heavier than `fGrabMaxWeightWalking`, or its mass is unusable.
    TooHeavy,
}

/// Why the object left the player's hands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// The player let go of Activate.
    Released,
    /// The player threw it.
    Thrown,
    /// The grabbed point lags more than [`CarryTuning::lag_break`] behind its target.
    Lag,
    /// The player stands on the held body.
    StoodOn,
    /// A heavier moving body presses on the object while it lags.
    HeavyContact,
    /// The body no longer exists (its cell unloaded).
    Gone,
    /// WALK ended, a door crossing started, or the cursor was released.
    Interrupted,
}

/// Mass check for a grab. A body at exactly the limit can be carried.
pub fn check_mass(mass: f32, tuning: &CarryTuning) -> Result<(), GrabRefusal> {
    if mass.is_finite() && mass > 0.0 && mass <= tuning.grab_max_weight {
        Ok(())
    } else {
        Err(GrabRefusal::TooHeavy)
    }
}

/// Auto-drop rule. `lag` is the distance in units between the grabbed point and its target.
/// `contact_ratio` is the heaviest touching moving body's mass over the held mass.
pub fn drop_reason(
    lag: f32,
    stood_on: bool,
    contact_ratio: f32,
    tuning: &CarryTuning,
) -> Option<DropReason> {
    if lag > tuning.lag_break {
        Some(DropReason::Lag)
    } else if stood_on {
        Some(DropReason::StoodOn)
    } else if lag > tuning.contact_distance && contact_ratio > tuning.contact_mass_ratio {
        Some(DropReason::HeavyContact)
    } else {
        None
    }
}

/// Speed given to a thrown body in units/s: the same momentum for every body, so a heavy body
/// leaves slowly, capped for light ones.
pub fn throw_speed(mass: f32, tuning: &CarryTuning) -> f32 {
    if mass.is_finite() && mass > 0.0 {
        (tuning.throw_momentum / mass).min(tuning.throw_max_speed)
    } else {
        0.0
    }
}

/// What the spring needs to know about the held body, in world space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CarryBody {
    pub mass: f32,
    pub centre: Vec3,
    /// Inverse angular inertia in world space.
    pub inv_inertia: Mat3,
    pub linear: Vec3,
    pub angular: Vec3,
}

/// New velocities of the held body after one spring step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringOutcome {
    pub linear: Vec3,
    pub angular: Vec3,
    /// Impulse applied at the anchor, after the clamp.
    pub impulse: Vec3,
}

/// Cross-product matrix: `skew(a) * b == a.cross(b)`.
pub fn skew(v: Vec3) -> Mat3 {
    Mat3::from_cols(
        Vec3::new(0.0, v.z, -v.y),
        Vec3::new(-v.z, 0.0, v.x),
        Vec3::new(v.y, -v.x, 0.0),
    )
}

/// One step of the Havok-style mouse spring on the material point `anchor` (spud
/// `apply_grab`, read as a description).
///
/// 1. Both velocities are scaled by `object_damping`.
/// 2. The point velocity is `v + w x r`.
/// 3. The wanted velocity change is `(target - anchor) * gain - point velocity * damping`.
/// 4. The impulse that gives that change at the point goes through the point's effective mass,
///    `K = I/m - [r]x W [r]x`, so an off-centre grab turns the body and does not shake it.
/// 5. The impulse is clamped to `max_acceleration * mass * dt` and applied at the point.
pub fn spring_step(
    body: &CarryBody,
    anchor: Vec3,
    target: Vec3,
    tuning: &CarryTuning,
    dt: f32,
) -> SpringOutcome {
    let mut linear = body.linear * tuning.object_damping;
    let mut angular = body.angular * tuning.object_damping;
    if !(body.mass.is_finite() && body.mass > 0.0) {
        return SpringOutcome {
            linear,
            angular,
            impulse: Vec3::ZERO,
        };
    }
    let r = anchor - body.centre;
    let point_velocity = linear + angular.cross(r);
    let wanted =
        (target - anchor) * tuning.spring_gain(dt) - point_velocity * tuning.spring_damping;
    let skew_r = skew(r);
    let response = Mat3::IDENTITY * (1.0 / body.mass) - skew_r * body.inv_inertia * skew_r;
    let determinant = response.determinant();
    let mut impulse = if determinant.is_finite() && determinant != 0.0 {
        response.inverse() * wanted
    } else {
        wanted * body.mass
    };
    impulse = impulse.clamp_length_max(tuning.max_acceleration() * body.mass * dt);
    linear += impulse / body.mass;
    angular += body.inv_inertia * r.cross(impulse);
    SpringOutcome {
        linear,
        angular,
        impulse,
    }
}

/// The held object, in the body's own frame so the pose can change under it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Held {
    pub entity: Entity,
    /// The grabbed point, relative to the body origin, in the body's frame.
    pub anchor_local: Vec3,
    /// Distance from the camera to the target, in units.
    pub distance: f32,
    pub mass: f32,
}

/// What the player holds, and what happened last. One object at a time.
#[derive(Resource, Debug, Default)]
pub struct HeldObject {
    pub held: Option<Held>,
    pub last_drop: Option<DropReason>,
    pub last_refusal: Option<GrabRefusal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CarryRequest {
    Grab,
    Release,
    Throw,
}

/// Key state sampled each frame; the fixed step consumes the request.
#[derive(Resource, Debug, Default)]
pub struct CarryInput {
    down: bool,
    down_secs: f32,
    attempted: bool,
    /// The current press already asked for a grab, so its release must not drop anything.
    grabbed_by_press: bool,
    pending: Option<CarryRequest>,
}

/// Samples `E` and the throw button each frame. A key that stays down for `key_delay` seconds asks
/// for one grab, and the release that ends that hold does nothing. A later press and release of
/// the key drops the object. A click of [`THROW_BUTTON`] throws it.
pub fn carry_input_system(
    keyboard: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    capture: Res<CursorCapture>,
    time: Res<Time>,
    tuning: Res<CarryTuning>,
    held: Res<HeldObject>,
    mut input: ResMut<CarryInput>,
) {
    let holding = held.held.is_some();
    if *capture != CursorCapture::Captured {
        input.down = false;
        input.pending = if holding || input.pending == Some(CarryRequest::Grab) {
            Some(CarryRequest::Release)
        } else {
            input.pending
        };
        return;
    }
    if keyboard.just_pressed(KeyCode::KeyE) {
        input.down = true;
        input.down_secs = 0.0;
        input.attempted = false;
        input.grabbed_by_press = false;
    }
    if input.down && keyboard.pressed(KeyCode::KeyE) {
        input.down_secs += time.delta_secs();
        if !input.attempted && input.down_secs >= tuning.key_delay {
            input.attempted = true;
            if !holding && input.pending.is_none() {
                input.pending = Some(CarryRequest::Grab);
                input.grabbed_by_press = true;
            }
        }
    }
    if keyboard.just_released(KeyCode::KeyE) {
        input.down = false;
        // The release that ends the grabbing hold does nothing, whether or not the grab worked.
        // Any other release while carrying lets go.
        if !input.grabbed_by_press && holding && input.pending != Some(CarryRequest::Throw) {
            input.pending = Some(CarryRequest::Release);
        }
    }
    if mouse.just_pressed(THROW_BUTTON) && holding {
        input.pending = Some(CarryRequest::Throw);
    }
}

/// While the console or a menu owns the keyboard, the key counts as released.
pub fn carry_cancel_system(held: Res<HeldObject>, mut input: ResMut<CarryInput>) {
    input.down = false;
    input.attempted = false;
    input.grabbed_by_press = false;
    if held.held.is_some() || input.pending == Some(CarryRequest::Grab) {
        input.pending = Some(CarryRequest::Release);
    }
}

/// Inverse inertia in world space from a Rapier body.
fn world_inverse_inertia(body: &bevy_rapier3d::rapier::dynamics::RigidBody) -> Mat3 {
    let props = body.mass_properties().local_mprops;
    let frame = Mat3::from_quat(*body.rotation() * props.principal_inertia_local_frame);
    let inverse = props.inv_principal_inertia;
    let scaled = Mat3::from_cols(
        frame.x_axis * inverse.x,
        frame.y_axis * inverse.y,
        frame.z_axis * inverse.z,
    );
    scaled * frame.transpose()
}

/// True when the player's capsule rests on the held body.
fn player_stands_on(
    context: &RapierContext,
    player: (Entity, &Transform, &Collider),
    held: Entity,
) -> bool {
    let (entity, transform, capsule) = player;
    let is_held = |candidate: Entity| {
        context
            .colliders
            .collider_parent(context.rigidbody_set, candidate)
            == Some(held)
    };
    let filter = QueryFilter::default()
        .exclude_rigid_body(entity)
        .predicate(&is_held);
    context
        .cast_shape(
            transform.translation,
            transform.rotation,
            Vec3::NEG_Y,
            &*capsule.raw,
            ShapeCastOptions {
                target_distance: 0.0,
                stop_at_penetration: false,
                max_time_of_impact: STAND_PROBE,
                compute_impact_geometry_on_penetration: true,
            },
            filter,
        )
        .and_then(|(_, hit)| hit.details)
        // A surface that faces up holds the player. A held body against the capsule's side does not.
        .is_some_and(|details| details.normal1.y > 0.5)
}

/// How far below the capsule a held body can be and still count as the player's floor, in units.
/// The controller keeps the capsule 2 units above its support.
const STAND_PROBE: f32 = 6.0;

/// The heaviest moving body that touches the held body, over the held mass. Static and kinematic
/// bodies (world, player) do not count.
fn contact_mass_ratio(context: &RapierContext, held: Entity, held_mass: f32) -> f32 {
    let mut heaviest = 0.0f32;
    for collider in context
        .colliders
        .rigid_body_colliders(held, context.rigidbody_set)
    {
        for pair in context.contact_pairs_with(collider) {
            if !pair.has_any_active_contact() {
                continue;
            }
            let other = if pair.collider1() == Some(collider) {
                pair.collider2()
            } else {
                pair.collider1()
            };
            let Some(owner) = other.and_then(|other| {
                context
                    .colliders
                    .collider_parent(context.rigidbody_set, other)
            }) else {
                continue;
            };
            if owner == held {
                continue;
            }
            let Some(body) = context
                .entity2body()
                .get(&owner)
                .and_then(|handle| context.rigidbody_set.bodies.get(*handle))
            else {
                continue;
            };
            if body.is_dynamic() {
                heaviest = heaviest.max(body.mass());
            }
        }
    }
    heaviest / held_mass
}

/// Let go of the held object. The body keeps the velocity the spring left it with, plus the throw.
fn release(
    commands: &mut Commands,
    held: &mut HeldObject,
    velocities: &mut Query<&mut Velocity>,
    reason: DropReason,
    throw: Option<Vec3>,
    tuning: &CarryTuning,
) {
    let Some(taken) = held.held.take() else {
        return;
    };
    held.last_drop = Some(reason);
    if let Some(direction) = throw
        && let Ok(mut velocity) = velocities.get_mut(taken.entity)
    {
        velocity.linear += direction * throw_speed(taken.mass, tuning);
    }
    // Normal sleeping again. Only an entity that still exists can take the component.
    if let Ok(mut entity) = commands.get_entity(taken.entity) {
        entity.insert(Sleeping::default());
    }
    info!("carry: released {:?} ({reason:?})", taken.entity);
}

/// Bodies the player may pick up: debug tankards and authored dynamic clutter.
type CarriableFilter = Or<(With<DebugTankard>, With<DynamicClutter>)>;

type CarryCamera<'w, 's> = Query<
    'w,
    's,
    &'static Transform,
    (
        With<StreamingCamera>,
        With<ControlledCamera>,
        Without<PlayerBody>,
    ),
>;

/// Fixed-step carry: runs the pending grab, drop or throw, then the drop rules and the spring.
#[allow(clippy::too_many_arguments)]
pub fn carry_step_system(
    mode: Res<MoveMode>,
    crossing: Option<Res<DoorCrossing>>,
    tuning: Res<CarryTuning>,
    movement: Res<MovementTuning>,
    mut input: ResMut<CarryInput>,
    mut held: ResMut<HeldObject>,
    camera: CarryCamera,
    player: Query<(Entity, &Transform, &Collider), With<PlayerBody>>,
    carriable: Query<(), CarriableFilter>,
    mut velocities: Query<&mut Velocity>,
    context: ReadRapierContext,
    mut commands: Commands,
) {
    let request = input.pending.take();
    let interrupted = *mode != MoveMode::Walk
        || crossing
            .as_ref()
            .is_some_and(|crossing| crossing.is_active());
    if interrupted {
        release(
            &mut commands,
            &mut held,
            &mut velocities,
            DropReason::Interrupted,
            None,
            &tuning,
        );
        return;
    }
    let (Ok(camera), Ok(context)) = (camera.single(), context.single()) else {
        return;
    };
    let forward = camera.forward().as_vec3();
    if held.held.is_none() {
        if request == Some(CarryRequest::Grab) {
            try_grab(
                &context,
                camera,
                &tuning,
                &movement,
                &player,
                &carriable,
                &mut held,
                &mut commands,
            );
        }
        return;
    }
    let Some(taken) = held.held else { return };
    let body = context
        .entity2body()
        .get(&taken.entity)
        .and_then(|handle| context.rigidbody_set.bodies.get(*handle));
    let (Some(body), Ok(velocity)) = (body, velocities.get(taken.entity).copied()) else {
        release(
            &mut commands,
            &mut held,
            &mut velocities,
            DropReason::Gone,
            None,
            &tuning,
        );
        return;
    };
    match request {
        Some(CarryRequest::Throw) => {
            release(
                &mut commands,
                &mut held,
                &mut velocities,
                DropReason::Thrown,
                Some(forward),
                &tuning,
            );
            return;
        }
        Some(CarryRequest::Release) => {
            release(
                &mut commands,
                &mut held,
                &mut velocities,
                DropReason::Released,
                None,
                &tuning,
            );
            return;
        }
        _ => {}
    }
    let anchor = body.translation() + *body.rotation() * taken.anchor_local;
    let target = camera.translation + forward * taken.distance;
    let lag = anchor.distance(target);
    let stood_on = player
        .single()
        .is_ok_and(|player| player_stands_on(&context, player, taken.entity));
    let ratio = if lag > tuning.contact_distance {
        contact_mass_ratio(&context, taken.entity, taken.mass)
    } else {
        0.0
    };
    if let Some(reason) = drop_reason(lag, stood_on, ratio, &tuning) {
        release(
            &mut commands,
            &mut held,
            &mut velocities,
            reason,
            None,
            &tuning,
        );
        return;
    }
    let outcome = spring_step(
        &CarryBody {
            mass: taken.mass,
            centre: body.center_of_mass(),
            inv_inertia: world_inverse_inertia(body),
            linear: velocity.linear,
            angular: velocity.angular,
        },
        anchor,
        target,
        &tuning,
        PHYSICS_TIMESTEP,
    );
    if let Ok(mut velocity) = velocities.get_mut(taken.entity) {
        velocity.linear = outcome.linear;
        velocity.angular = outcome.angular;
    }
}

#[allow(clippy::too_many_arguments)]
fn try_grab(
    context: &RapierContext,
    camera: &Transform,
    tuning: &CarryTuning,
    movement: &MovementTuning,
    player: &Query<(Entity, &Transform, &Collider), With<PlayerBody>>,
    carriable: &Query<(), CarriableFilter>,
    held: &mut HeldObject,
    commands: &mut Commands,
) {
    let forward = camera.forward().as_vec3();
    let filter = match player.single() {
        Ok((entity, _, _)) => QueryFilter::default().exclude_rigid_body(entity),
        Err(_) => QueryFilter::default(),
    };
    // The nearest thing on the line of sight decides: a wall in front hides an object behind it.
    let Some((collider, distance)) =
        context.cast_ray(camera.translation, forward, tuning.reach, true, filter)
    else {
        held.last_refusal = Some(GrabRefusal::NotCarriable);
        return;
    };
    let Some(owner) = context
        .colliders
        .collider_parent(context.rigidbody_set, collider)
    else {
        held.last_refusal = Some(GrabRefusal::NotCarriable);
        return;
    };
    let body = context
        .entity2body()
        .get(&owner)
        .and_then(|handle| context.rigidbody_set.bodies.get(*handle));
    let Some(body) = body.filter(|body| body.is_dynamic() && carriable.get(owner).is_ok()) else {
        held.last_refusal = Some(GrabRefusal::NotCarriable);
        return;
    };
    let mass = body.mass();
    if let Err(refusal) = check_mass(mass, tuning) {
        held.last_refusal = Some(refusal);
        info!(
            "carry: refused {owner:?}, mass {mass} kg (limit {} kg)",
            tuning.grab_max_weight
        );
        return;
    }
    let point = camera.translation + forward * distance;
    held.held = Some(Held {
        entity: owner,
        anchor_local: body.rotation().inverse() * (point - body.translation()),
        distance: distance.max(tuning.min_hold_distance(movement.capsule_radius)),
        mass,
    });
    held.last_refusal = None;
    held.last_drop = None;
    // A held body must not fall asleep, and a body that was asleep wakes now.
    commands.entity(owner).insert(Sleeping::disabled());
    info!("carry: grabbed {owner:?}, mass {mass} kg, {distance:.0} units away");
}

/// Adds the carry systems. `E` carries, a left click throws, `T` still spawns a test tankard.
pub struct CarryPlugin;

impl Plugin for CarryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CarryTuning>()
            .init_resource::<CarryInput>()
            .init_resource::<HeldObject>()
            .add_systems(
                Update,
                (
                    carry_input_system.run_if(console_closed),
                    carry_cancel_system.run_if(not(console_closed)),
                )
                    .after(crate::settings_ui::SettingsUiSet),
            )
            .add_systems(
                FixedUpdate,
                carry_step_system.before(PhysicsSet::SyncBackend),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::{
        LookIntent, body_and_camera_for_feet, headless, mass_properties_from_body,
        spawn_dynamic_clutter, world_collision_groups,
    };
    use bevy::ecs::system::RunSystemOnce;
    use bevy::input::{
        ButtonState,
        keyboard::{Key, KeyboardInput},
        mouse::MouseButtonInput,
    };
    use shared::collision::{BodyKind, CollisionBody, HavokBodyInfo};

    const DT: f32 = PHYSICS_TIMESTEP;

    // ---- Pure spring, mass and drop rules ------------------------------------------------------

    /// A 20-unit cube, inertia about its centre.
    fn cube(mass: f32) -> CarryBody {
        let inertia = mass * (20.0 * 20.0 * 2.0) / 12.0;
        CarryBody {
            mass,
            centre: Vec3::ZERO,
            inv_inertia: Mat3::IDENTITY * (1.0 / inertia),
            linear: Vec3::ZERO,
            angular: Vec3::ZERO,
        }
    }

    #[test]
    fn the_defaults_are_twelve_per_second_and_one_hundred_metres_per_second_squared() {
        let tuning = CarryTuning::default();
        assert!((tuning.spring_gain(DT) - 12.0).abs() < 1.0e-3);
        assert!((tuning.max_acceleration() - 7000.0).abs() < 1.0e-3);
        // One 60 Hz step may change a body's velocity by at most 116.7 units/s.
        assert!((tuning.max_acceleration() * DT - 116.67).abs() < 0.01);
        assert_eq!(tuning.min_hold_distance(28.0), 33.0);
    }

    #[test]
    fn a_small_error_at_the_centre_becomes_twelve_times_the_error_as_velocity() {
        let body = cube(5.0);
        let outcome = spring_step(
            &body,
            Vec3::ZERO,
            Vec3::new(5.0, 0.0, 0.0),
            &CarryTuning::default(),
            DT,
        );
        assert!(
            outcome
                .linear
                .abs_diff_eq(Vec3::new(60.0, 0.0, 0.0), 1.0e-3),
            "{:?}",
            outcome.linear
        );
        assert!(outcome.angular.length() < 1.0e-6, "{:?}", outcome.angular);
    }

    #[test]
    fn a_large_error_is_clamped_to_the_per_mass_force_limit() {
        for mass in [1.0, 5.0, 25.0, 100.0] {
            let outcome = spring_step(
                &cube(mass),
                Vec3::ZERO,
                Vec3::new(0.0, 0.0, 140.0),
                &CarryTuning::default(),
                DT,
            );
            let change = outcome.linear.length();
            assert!(
                (change - 116.67).abs() < 116.67 * 0.05,
                "{mass} kg changed its velocity by {change}"
            );
            // The limit is per kilogram, so the impulse grows with the mass.
            assert!((outcome.impulse.length() - 116.67 * mass).abs() < 116.67 * mass * 0.05);
        }
    }

    #[test]
    fn both_velocities_are_damped_each_step() {
        let mut body = cube(5.0);
        body.linear = Vec3::new(8.0, 0.0, 0.0);
        body.angular = Vec3::new(0.0, 4.0, 0.0);
        // At the target and at the centre of mass the spring only removes velocity:
        // 0.75 (object damping), then half of the point velocity (spring damping).
        let outcome = spring_step(&body, Vec3::ZERO, Vec3::ZERO, &CarryTuning::default(), DT);
        assert!(
            outcome.linear.abs_diff_eq(Vec3::new(3.0, 0.0, 0.0), 1.0e-4),
            "{:?}",
            outcome.linear
        );
        assert!(
            outcome
                .angular
                .abs_diff_eq(Vec3::new(0.0, 3.0, 0.0), 1.0e-4),
            "{:?}",
            outcome.angular
        );
    }

    /// An off-centre grab goes through the point's effective mass: the point gets exactly the
    /// velocity change that was asked for, and the body turns.
    #[test]
    fn an_off_centre_grab_turns_the_body_and_moves_the_point_by_the_wanted_change() {
        let body = cube(5.0);
        let anchor = Vec3::new(8.0, 6.0, -9.0);
        let target = anchor + Vec3::new(0.0, 2.0, 1.0);
        let tuning = CarryTuning::default();
        let outcome = spring_step(&body, anchor, target, &tuning, DT);
        let wanted = (target - anchor) * tuning.spring_gain(DT);
        let point_velocity = outcome.linear + outcome.angular.cross(anchor - body.centre);
        assert!(
            point_velocity.abs_diff_eq(wanted, 1.0e-2),
            "point {point_velocity:?} wanted {wanted:?}"
        );
        assert!(outcome.angular.length() > 0.01, "the body did not turn");
    }

    #[test]
    fn the_mass_limit_is_inclusive_and_refuses_unusable_masses() {
        let tuning = CarryTuning::default();
        assert_eq!(check_mass(99.9, &tuning), Ok(()));
        assert_eq!(check_mass(100.0, &tuning), Ok(()));
        assert_eq!(check_mass(100.1, &tuning), Err(GrabRefusal::TooHeavy));
        for mass in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(check_mass(mass, &tuning), Err(GrabRefusal::TooHeavy));
        }
    }

    #[test]
    fn the_snag_rule_drops_at_96_units_and_not_at_96_metres() {
        let tuning = CarryTuning::default();
        // 96 units is 1.37 m.
        assert!((96.0 / UNITS_PER_METRE - 1.371).abs() < 0.001);
        assert_eq!(drop_reason(90.0, false, 0.0, &tuning), None);
        assert_eq!(drop_reason(96.0, false, 0.0, &tuning), None);
        assert_eq!(
            drop_reason(100.0, false, 0.0, &tuning),
            Some(DropReason::Lag)
        );
        // Spud compares metres with 96, so it keeps holding at this lag: 2 m is 140 units.
        assert_eq!(
            drop_reason(2.0 * UNITS_PER_METRE, false, 0.0, &tuning),
            Some(DropReason::Lag)
        );
    }

    #[test]
    fn standing_on_the_object_always_drops_it() {
        let tuning = CarryTuning::default();
        assert_eq!(
            drop_reason(0.0, true, 0.0, &tuning),
            Some(DropReason::StoodOn)
        );
    }

    #[test]
    fn a_heavy_moving_body_drops_the_object_only_while_it_lags() {
        let tuning = CarryTuning::default();
        // Ratio 5 is above 4, but the object follows its target: no drop.
        assert_eq!(drop_reason(5.0, false, 5.0, &tuning), None);
        assert_eq!(
            drop_reason(11.0, false, 5.0, &tuning),
            Some(DropReason::HeavyContact)
        );
        // Ratio 3 is below 4: held at any lag below the snag limit.
        assert_eq!(drop_reason(50.0, false, 3.0, &tuning), None);
        // Static geometry is ratio 0.
        assert_eq!(drop_reason(50.0, false, 0.0, &tuning), None);
    }

    #[test]
    fn throw_speed_falls_with_mass_and_is_capped() {
        let tuning = CarryTuning::default();
        assert_eq!(throw_speed(1.0, &tuning), tuning.throw_max_speed);
        assert!((throw_speed(20.0, &tuning) - 300.0).abs() < 1.0e-3);
        assert!(throw_speed(100.0, &tuning) < throw_speed(20.0, &tuning));
        assert_eq!(throw_speed(0.0, &tuning), 0.0);
    }

    #[test]
    fn gmst_values_replace_constants_and_bad_ones_are_ignored() {
        let mut tuning = CarryTuning::default();
        tuning.apply_gmst(|name| match name {
            "fZKeyMaxForce" => Some(150.0),
            "fGrabMaxWeightWalking" => Some(f32::NAN),
            "fZKeyDelay" => Some(-1.0),
            _ => None,
        });
        assert_eq!(tuning.max_force, 150.0);
        assert_eq!(tuning.grab_max_weight, 100.0);
        assert_eq!(tuning.key_delay, 0.2);
    }

    // ---- Rapier fixture ------------------------------------------------------------------------

    const FEET: Vec3 = Vec3::new(-300.0, 0.0, 600.0);

    fn carry_app() -> App {
        let mut app = headless::fixture_app_with(|app| {
            app.add_plugins(CarryPlugin);
        });
        app.insert_resource(CursorCapture::Captured);
        let tuning = app.world().resource::<MovementTuning>().clone();
        let (body, _) = body_and_camera_for_feet(&tuning, FEET);
        headless::place_player(&mut app, body);
        frames(&mut app, 60);
        app
    }

    fn frames(app: &mut App, count: usize) {
        for _ in 0..count {
            app.update();
        }
    }

    fn key(app: &mut App, state: ButtonState) {
        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::KeyE,
            logical_key: Key::Character("e".into()),
            state,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        app.update();
    }

    fn click(app: &mut App) {
        for state in [ButtonState::Pressed, ButtonState::Released] {
            app.world_mut().write_message(MouseButtonInput {
                button: MouseButton::Left,
                state,
                window: Entity::PLACEHOLDER,
            });
            app.update();
        }
    }

    /// Hold Activate until the delay passes and the fixed step has run the grab, then let go: the
    /// release that ends the grabbing hold does nothing, so the object stays held.
    fn hold_activate(app: &mut App) {
        key(app, ButtonState::Pressed);
        frames(app, 16);
    }

    /// A short press and release of Activate.
    fn tap_activate(app: &mut App) {
        key(app, ButtonState::Pressed);
        frames(app, 3);
        key(app, ButtonState::Released);
        frames(app, 2);
    }

    fn eye(app: &mut App) -> Transform {
        let mut query = app
            .world_mut()
            .query_filtered::<&Transform, (With<StreamingCamera>, With<ControlledCamera>)>();
        *query.single(app.world()).expect("controlled camera")
    }

    fn held(app: &App) -> Option<Held> {
        app.world().resource::<HeldObject>().held
    }

    fn box_body(mass: f32) -> CollisionBody {
        let inertia = mass * (20.0 * 20.0 * 2.0) / 12.0;
        CollisionBody {
            node: 1,
            target: "Carry".to_owned(),
            shapes: vec![0],
            kind: BodyKind::Dynamic,
            havok: HavokBodyInfo {
                motion_system: 3,
                quality_type: 4,
                deactivator_type: 1,
                collision_layer: 4,
            },
            mass,
            inertia: [inertia, 0.0, 0.0, 0.0, inertia, 0.0, 0.0, 0.0, inertia],
            center_of_mass: [0.0, 10.0, 0.0],
            linear_damping: 0.1,
            angular_damping: 0.05,
            friction: 0.6,
            restitution: 0.0,
            max_linear_velocity: 7_000.0,
            max_angular_velocity: 31.0,
            convex: true,
        }
    }

    /// A 20-unit cube of `mass` kg whose centre is at `centre`, optionally under a cell root.
    fn spawn_box(app: &mut App, mass: f32, centre: Vec3, parent: Option<Entity>) -> Entity {
        app.world_mut()
            .run_system_once(move |mut commands: Commands| {
                let origin = centre - Vec3::Y * 10.0;
                let mut entity = commands.spawn(Transform::from_translation(origin));
                if let Some(parent) = parent {
                    entity.insert(ChildOf(parent));
                }
                let entity = entity.id();
                let body = box_body(mass);
                let collider = Collider::cuboid(10.0, 10.0, 10.0);
                spawn_dynamic_clutter(
                    &mut commands,
                    entity,
                    &body,
                    [(Vec3::new(0.0, 10.0, 0.0), &collider)],
                    mass_properties_from_body(&body),
                    1.0,
                );
                entity
            })
            .unwrap()
    }

    /// A box straight ahead of the camera, `distance` units from the eye, at eye height.
    fn spawn_box_ahead(app: &mut App, mass: f32, distance: f32) -> Entity {
        let view = eye(app);
        let centre = view.translation + view.forward().as_vec3() * distance;
        spawn_box(app, mass, centre, None)
    }

    fn centre_of(app: &App, entity: Entity) -> Vec3 {
        app.world().get::<Transform>(entity).unwrap().translation + Vec3::Y * 10.0
    }

    /// Distance from the grabbed point to its target, in units.
    fn lag(app: &mut App) -> f32 {
        let taken = held(app).expect("held");
        let view = eye(app);
        let transform = *app.world().get::<Transform>(taken.entity).unwrap();
        let anchor = transform.translation + transform.rotation * taken.anchor_local;
        anchor.distance(view.translation + view.forward().as_vec3() * taken.distance)
    }

    #[test]
    fn holding_activate_grabs_the_box_and_the_next_tap_drops_it_with_collision_on() {
        let mut app = carry_app();
        let entity = spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        let taken = held(&app).expect("the box is held");
        assert_eq!(taken.entity, entity);
        assert_eq!(taken.mass, 5.0);
        // The grab distance is where the box was taken, not a fixed 110.
        assert!((taken.distance - 90.0).abs() < 2.0, "{}", taken.distance);
        frames(&mut app, 60);
        // The collider stays on for the whole hold: no disabled body, no sensor.
        let world = app.world();
        assert!(!world.entity(entity).contains::<RigidBodyDisabled>());
        assert!(!world.entity(entity).contains::<ColliderDisabled>());
        // The box's collider sits off its origin, so it is a child entity: every collider of the
        // body is present, enabled and solid.
        let mut colliders = app.world_mut().query_filtered::<(
            Entity,
            Option<&ChildOf>,
            Has<ColliderDisabled>,
            Has<Sensor>,
        ), With<Collider>>();
        let own: Vec<_> = colliders
            .iter(app.world())
            .filter(|(collider, parent, _, _)| {
                *collider == entity || parent.is_some_and(|parent| parent.parent() == entity)
            })
            .collect();
        assert!(!own.is_empty(), "the held body lost its collider");
        assert!(
            own.iter()
                .all(|(_, _, disabled, sensor)| !disabled && !sensor)
        );
        assert!(held(&app).is_some(), "the hold ended by itself");
        assert!(lag(&mut app) < 3.0, "lag {}", lag(&mut app));
        // The release that ends the grabbing hold does nothing.
        key(&mut app, ButtonState::Released);
        frames(&mut app, 5);
        assert!(
            held(&app).is_some(),
            "the grab-ending release dropped the box"
        );
        // The next press and release drops it with the velocity the spring left it.
        tap_activate(&mut app);
        assert!(held(&app).is_none());
        assert_eq!(
            app.world().resource::<HeldObject>().last_drop,
            Some(DropReason::Released)
        );
        let velocity = app.world().get::<Velocity>(entity).unwrap();
        assert!(
            velocity.linear.length() < 60.0,
            "released with {velocity:?}"
        );
    }

    #[test]
    fn a_tap_shorter_than_the_key_delay_grabs_nothing() {
        let mut app = carry_app();
        spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        key(&mut app, ButtonState::Pressed);
        frames(&mut app, 4);
        key(&mut app, ButtonState::Released);
        frames(&mut app, 30);
        assert!(held(&app).is_none());
        assert_eq!(app.world().resource::<HeldObject>().last_refusal, None);
    }

    #[test]
    fn the_box_follows_the_view_when_the_player_looks_up() {
        let mut app = carry_app();
        let entity = spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        frames(&mut app, 60);
        let level = centre_of(&app, entity);
        app.world_mut().resource_mut::<LookIntent>().pitch = 0.4;
        frames(&mut app, 90);
        let raised = centre_of(&app, entity);
        assert!(held(&app).is_some());
        assert!(
            raised.y > level.y + 20.0,
            "the box did not rise with the view: {level:?} -> {raised:?}"
        );
        assert!(lag(&mut app) < 3.0, "lag {}", lag(&mut app));
    }

    /// Spud's check: the grabbed point must not shake once the hold has settled. The body itself
    /// keeps creeping toward the pose that hangs from the point, under heavy rotational damping, so
    /// the body centre gets two checks of its own: a bound on the total creep, and a strict limit
    /// once it has settled. Measured: the point stays about 1 unit (1.4 cm) under its target and
    /// moves under 0.5 units; the centre creeps about 1 unit over ticks 44 to 104 and under 0.1
    /// units over the last 30 of them.
    #[test]
    fn a_hold_has_no_visible_jitter() {
        let mut app = carry_app();
        let entity = spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        frames(&mut app, 44);
        let taken = held(&app).expect("held");
        let spread = |points: &[Vec3]| {
            let low = points.iter().fold(Vec3::splat(f32::MAX), |a, b| a.min(*b));
            let high = points.iter().fold(Vec3::splat(f32::MIN), |a, b| a.max(*b));
            (high - low).max_element()
        };
        let (mut points, mut centres) = (Vec::new(), Vec::new());
        for _ in 0..60 {
            app.update();
            let transform = *app.world().get::<Transform>(entity).unwrap();
            points.push(transform.translation + transform.rotation * taken.anchor_local);
            centres.push(centre_of(&app, entity));
        }
        let point = spread(&points);
        assert!(
            point < 0.5,
            "the grabbed point moved {point} units while held"
        );
        let total = spread(&centres);
        assert!(
            total < 2.0,
            "the body centre moved {total} units while held"
        );
        let settled = spread(&centres[30..]);
        assert!(
            settled < 0.5,
            "the body centre still moved {settled} units in the last 30 ticks"
        );
    }

    #[test]
    fn a_body_over_the_weight_limit_is_refused() {
        let mut app = carry_app();
        spawn_box_ahead(&mut app, 150.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        assert!(held(&app).is_none());
        assert_eq!(
            app.world().resource::<HeldObject>().last_refusal,
            Some(GrabRefusal::TooHeavy)
        );
    }

    #[test]
    fn a_fixed_object_and_a_wall_in_front_are_not_carried() {
        let mut app = carry_app();
        let view = eye(&mut app);
        let ahead = view.translation + view.forward().as_vec3() * 60.0;
        // A static wall 60 units ahead, with a movable box behind it.
        app.world_mut().spawn((
            Transform::from_translation(ahead),
            Collider::cuboid(40.0, 40.0, 5.0),
            RigidBody::Fixed,
            world_collision_groups(),
        ));
        spawn_box_ahead(&mut app, 5.0, 120.0);
        frames(&mut app, 3);
        hold_activate(&mut app);
        assert!(held(&app).is_none());
        assert_eq!(
            app.world().resource::<HeldObject>().last_refusal,
            Some(GrabRefusal::NotCarriable)
        );
    }

    #[test]
    fn a_box_beyond_the_reach_is_not_grabbed() {
        let mut app = carry_app();
        spawn_box_ahead(&mut app, 5.0, 220.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        assert!(held(&app).is_none());
    }

    #[test]
    fn the_object_drops_when_it_snags_and_lags_past_the_limit() {
        let mut app = carry_app();
        let entity = spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        frames(&mut app, 30);
        assert!(held(&app).is_some());
        // The player jumps 150 units sideways at once, as a snag does when the object stays
        // behind a wall while the player walks on.
        let player = app
            .world_mut()
            .query_filtered::<Entity, With<PlayerBody>>()
            .single(app.world())
            .unwrap();
        app.world_mut()
            .get_mut::<Transform>(player)
            .unwrap()
            .translation
            .x += 150.0;
        frames(&mut app, 3);
        assert!(held(&app).is_none());
        assert_eq!(
            app.world().resource::<HeldObject>().last_drop,
            Some(DropReason::Lag)
        );
        assert!(app.world().get::<Transform>(entity).is_some());
    }

    #[test]
    fn a_small_lag_below_the_limit_keeps_the_hold() {
        let mut app = carry_app();
        spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        frames(&mut app, 30);
        let player = app
            .world_mut()
            .query_filtered::<Entity, With<PlayerBody>>()
            .single(app.world())
            .unwrap();
        app.world_mut()
            .get_mut::<Transform>(player)
            .unwrap()
            .translation
            .x += 60.0;
        frames(&mut app, 60);
        assert!(held(&app).is_some(), "a 60 unit lag dropped the box");
    }

    #[test]
    fn standing_on_the_held_box_drops_it_even_without_lag() {
        let mut app = carry_app();
        // Switch the snag rule off, so only the stand-on rule can end the hold.
        app.world_mut().resource_mut::<CarryTuning>().lag_break = 1.0e9;
        let entity = spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        frames(&mut app, 30);
        assert!(held(&app).is_some());
        let top = centre_of(&app, entity) + Vec3::Y * 10.0;
        let tuning = app.world().resource::<MovementTuning>().clone();
        let body = top + Vec3::Y * (tuning.capsule_half_height() + tuning.capsule_radius + 2.0);
        headless::place_player(&mut app, body);
        frames(&mut app, 2);
        assert!(held(&app).is_none());
        assert_eq!(
            app.world().resource::<HeldObject>().last_drop,
            Some(DropReason::StoodOn)
        );
    }

    /// The contact rule's input: the heaviest touching moving body, over the held mass.
    fn touching_ratio(app: &mut App, held: Entity, mass: f32) -> f32 {
        app.world_mut()
            .run_system_once(move |context: ReadRapierContext| {
                contact_mass_ratio(&context.single().expect("rapier context"), held, mass)
            })
            .unwrap()
    }

    #[test]
    fn contact_ratio_counts_only_moving_bodies() {
        let mut app = carry_app();
        // The 5 kg box rests on the fixture ground (static), next to a heavier box.
        let light = spawn_box(&mut app, 5.0, Vec3::new(0.0, 10.5, 0.0), None);
        // A body that only touches the static ground has ratio 0.
        app.world_mut()
            .entity_mut(light)
            .insert(Sleeping::disabled());
        frames(&mut app, 30);
        assert_eq!(touching_ratio(&mut app, light, 5.0), 0.0);
        let heavy = spawn_box(&mut app, 25.0, Vec3::new(19.8, 10.5, 0.0), None);
        app.world_mut()
            .entity_mut(heavy)
            .insert(Sleeping::disabled());
        frames(&mut app, 30);
        let ratio = touching_ratio(&mut app, light, 5.0);
        assert!((ratio - 5.0).abs() < 1.0e-3, "ratio {ratio}");
        assert!(
            drop_reason(30.0, false, ratio, &CarryTuning::default())
                == Some(DropReason::HeavyContact)
        );
    }

    #[test]
    fn a_box_three_times_the_mass_is_not_heavy_enough_to_drop_the_object() {
        let mut app = carry_app();
        let light = spawn_box(&mut app, 5.0, Vec3::new(0.0, 10.5, 0.0), None);
        let medium = spawn_box(&mut app, 15.0, Vec3::new(19.8, 10.5, 0.0), None);
        for entity in [light, medium] {
            app.world_mut()
                .entity_mut(entity)
                .insert(Sleeping::disabled());
        }
        frames(&mut app, 30);
        let ratio = touching_ratio(&mut app, light, 5.0);
        assert!((ratio - 3.0).abs() < 1.0e-3, "ratio {ratio}");
        assert_eq!(
            drop_reason(30.0, false, ratio, &CarryTuning::default()),
            None
        );
    }

    #[test]
    fn a_left_click_throws_the_object_forward() {
        let mut app = carry_app();
        let entity = spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        frames(&mut app, 30);
        click(&mut app);
        frames(&mut app, 1);
        assert!(held(&app).is_none());
        assert_eq!(
            app.world().resource::<HeldObject>().last_drop,
            Some(DropReason::Thrown)
        );
        let velocity = app.world().get::<Velocity>(entity).unwrap().linear;
        let forward = eye(&mut app).forward().as_vec3();
        // 5 kg gets the capped speed, along the view (the fixture camera looks slightly down).
        assert!(velocity.length() > 900.0, "thrown with {velocity:?}");
        assert!(
            velocity.normalize().dot(forward) > 0.97,
            "thrown {velocity:?} along {forward:?}"
        );
    }

    #[test]
    fn a_click_without_a_held_object_does_nothing() {
        let mut app = carry_app();
        let entity = spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        click(&mut app);
        frames(&mut app, 5);
        assert!(held(&app).is_none());
        assert_eq!(app.world().resource::<HeldObject>().last_drop, None);
        assert_eq!(
            app.world().get::<Velocity>(entity).unwrap().linear,
            Vec3::ZERO
        );
    }

    #[test]
    fn leaving_walk_or_releasing_the_cursor_drops_the_object() {
        for case in ["noclip", "cursor"] {
            let mut app = carry_app();
            spawn_box_ahead(&mut app, 5.0, 100.0);
            frames(&mut app, 2);
            hold_activate(&mut app);
            assert!(held(&app).is_some(), "{case}: no hold");
            match case {
                "noclip" => app.insert_resource(MoveMode::Noclip),
                _ => app.insert_resource(CursorCapture::Released),
            };
            frames(&mut app, 3);
            assert!(held(&app).is_none(), "{case}: still held");
        }
    }

    #[test]
    fn an_unloaded_body_ends_the_hold() {
        let mut app = carry_app();
        let entity = spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        assert!(held(&app).is_some());
        app.world_mut().despawn(entity);
        frames(&mut app, 3);
        assert!(held(&app).is_none());
        assert_eq!(
            app.world().resource::<HeldObject>().last_drop,
            Some(DropReason::Gone)
        );
    }

    #[test]
    fn a_body_under_a_cell_root_is_carried_in_world_space() {
        let mut app = carry_app();
        let view = eye(&mut app);
        let centre = view.translation + view.forward().as_vec3() * 100.0;
        // The cell root sits away from the origin, as a streamed cell does.
        let root = app
            .world_mut()
            .spawn((
                Transform::from_xyz(500.0, 0.0, -200.0),
                Visibility::default(),
            ))
            .id();
        let local = centre - Vec3::new(500.0, 0.0, -200.0);
        let entity = spawn_box(&mut app, 5.0, local, Some(root));
        frames(&mut app, 3);
        hold_activate(&mut app);
        assert_eq!(held(&app).map(|taken| taken.entity), Some(entity));
        frames(&mut app, 60);
        let world = app.world().get::<GlobalTransform>(entity).unwrap();
        let target = view.translation + view.forward().as_vec3() * held(&app).unwrap().distance;
        assert!(
            (world.translation() + Vec3::Y * 10.0).distance(target) < 12.0,
            "{:?} vs {target:?}",
            world.translation()
        );
        assert!(held(&app).is_some());
    }

    #[test]
    fn a_dropped_body_sleeps_normally_again() {
        let mut app = carry_app();
        let entity = spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        assert_eq!(
            *app.world().get::<Sleeping>(entity).unwrap(),
            Sleeping::disabled()
        );
        key(&mut app, ButtonState::Released);
        tap_activate(&mut app);
        assert_eq!(
            *app.world().get::<Sleeping>(entity).unwrap(),
            Sleeping::default()
        );
    }
}
