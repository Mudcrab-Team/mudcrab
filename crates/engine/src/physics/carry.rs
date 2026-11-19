//! Skyrim-style object carrying for the WALK player (#104 phase b).
//!
//! Hold Activate (`E`) on a movable object for `fZKeyDelay` to pick it up. The object hangs on a
//! spring at the point where it was grabbed, in front of the camera, and keeps its collider and
//! its collision group the whole time. The release that ends the grabbing hold does nothing; the
//! next press and release of `E` drops it. The object drops by itself when it snags (the grabbed
//! point is more than 96 units from its target) or when it is pressed against a much heavier
//! moving body.
//!
//! # Where each rule and constant comes from
//!
//! The reverse-engineering analysis of retail `SkyrimSE.exe` 1.7.104.0 is the source for every rule
//! and constant here, cited by registry ID. It is a static trace (activate handler `0x1407b2660`,
//! grab start `0x140751870`, spring solver `0x140b99e20`, hold checks `0x140753580`). Effective
//! values are the compiled defaults, replaced by `Skyrim.esm` where it overrides them
//! (`RF-GMST-001`).
//!
//! - **Input, `RF-CARRY-001`.** A press held longer than `fZKeyDelay` (0.2 s) with no grab active
//!   starts one grab; a flag marks that this press started it. On release, if a grab is active and
//!   this press did not start it, the object drops; the release that ends the grabbing hold does
//!   nothing.
//! - **Weight limit, `RF-CARRY-002`.** The grab is refused when the body's mass is above
//!   `fGrabMaxWeightWalking` (100 kg); a body at exactly 100 is carried.
//! - **Spring parameters, `RF-CARRY-003`.** Damping `fZKeySpringDamping` (0.5), elasticity
//!   `fZKeySpringElasticity` (0.2) times E, object damping `fZKeyObjectDamping` (0.75) and maximum
//!   relative force `fZKeyMaxForce` times S. For a normal object E = 1 and S is the mass scale of
//!   [`CarryTuning::max_force_scale`]: 1.0 at 0 kg, falling linearly to 0.2 at
//!   `fZKeyMaxForceWeightHigh` and above. `Skyrim.esm` overrides `fZKeyMaxForce` (250 in the
//!   executable, 100 in the master) and `fZKeyMaxForceWeightHigh` (300 and 150); both are read from
//!   the pack ([`CarryTuning::from_world_database`]).
//! - **Solver, `RF-CARRY-009`.** The Havok mouse-spring action, once per 1/60 s step
//!   (`hkpMouseSpringAction::applyAction`, high confidence): multiply both velocities by 0.75, take
//!   the point velocity `v + w x r`, apply at the point the impulse
//!   `J = -K^-1 (0.5 v_p + 0.2 E e / dt)` with `K = I/m + [r]x W [r]x^T`, clamp `|J|` to
//!   `100 * S * m * dt`, and apply nothing if K is singular. [`spring_step`] does this. It was first
//!   written as a step-by-step port of the algorithm in `apply_grab` in spud
//!   (<https://github.com/chasmlol/spud>, MIT, commit `e5f2de4b`, `src/physics/world.cpp`), from an
//!   internal audit note of that source and not by copying code; the trace shows the retail solver
//!   does the same steps, so the two agree. Spud's licence notice is in `THIRD_PARTY.md`. The
//!   contact-mass scan follows spud's too.
//! - **Drops, `RF-CARRY-005`.** The lag is the distance between the grabbed point and its target in
//!   game units. Lag above 96 units (hard-coded at `0x14181ab88`) drops the object. Lag above
//!   `fZKeyMaxContactDistance` (10) with a touching body more than `fZKeyMaxContactMassRatio` (4)
//!   times the held mass drops it. A Havok metre is 69.99125 units (`RF-UNIT-001`).
//! - **Hold distance, `RF-CARRY-008`** (medium-high). The distance from the eye to the pick hit
//!   point, shortened by the fraction of any obstacle ray, and at least the player's
//!   character-controller capsule radius in Havok metres times 69.99125 plus 5 units (the constant
//!   5.0 is at `0x141b5be6c`). Mudcrab's nearest-hit pick refuses a grab with an obstacle in front.
//!   Its minimum uses Mudcrab's own capsule radius (`MovementTuning`, provisional), so the minimum
//!   is only as retail as that radius.
//! - **Heavy carry, `RF-CARRY-012`** (medium-high; supersedes `RF-CARRY-006`). Holding a body above
//!   `fZKeyHeavyWeight` (75 kg) makes the "may run" test fail, the test that over-encumbrance
//!   uses, and every direction's run speed is replaced by its walk speed. Sprint was not traced
//!   and is left unchanged (#691).
//! - **No throw, `RF-CARRY-007`.** Release removes the spring and applies no impulse; there is no
//!   throw setting for this path. The object keeps the velocity the spring left it.
//!
//! Not implemented, each with a follow-up (#691), all medium confidence: the layer-14 (TRAP) case
//! (S = 0.5, E = 0.1) and the all-contacts constraint case (S x 0.2, E = 0.1) (`RF-CARRY-004`,
//! meanings in `RF-CARRY-010`); the complex helper (`RF-CARRY-011`: `fZKeyComplexHelper*`, an extra
//! pull `(1 - m/100) * 2 * (L - 6) / L` toward the target for flagged objects and ragdolls when the
//! lag L is over 6 units); and the retail held-object collision layer. spud's stand-on drop has no
//! retail source and is not implemented either.
//!
//! Not from retail, and without constants of its own (Mudcrab engineering): the steep-look target
//! clamp (it reuses the radius plus 5 of `RF-CARRY-008`), the interrupt rules (WALK end, door
//! crossing, cursor release, console or menu open) and the disabled sleeping while held.
//!
//! The settings are read from the pack at startup; a pack without them keeps the compiled
//! defaults. [`CarryTuning::apply_gmst`] takes any other carry setting the converter publishes
//! later (#595).

use super::{
    ControlledCamera, CursorCapture, DebugTankard, DynamicClutter, MoveMode, MovementTuning,
    PHYSICS_TIMESTEP, PlayerBody,
};
use crate::{
    console::console_closed, door_crossing::DoorCrossing, world::components::StreamingCamera,
};
use bevy::{ecs::schedule::common_conditions::not, prelude::*};
use bevy_rapier3d::prelude::*;
use color_eyre::{Result, eyre::WrapErr};
use rusqlite::{Connection, OpenFlags};
use shared::records::ArchivedRecordData;
use std::{collections::HashMap, path::Path};

/// Creation units per metre: the constant the retail grab code multiplies metres by (`RF-UNIT-001`,
/// `0x1417f423c`).
pub const UNITS_PER_METRE: f32 = 69.99125;

/// Spring and drop constants. All distances are in Creation units, masses in kg.
///
/// Each default below is the compiled default in the retail executable, cited by registry ID.
/// `fZKeyMaxForce` and `fZKeyMaxForceWeightHigh`, which `Skyrim.esm` overrides (`RF-GMST-001`), are
/// read from the pack by [`Self::from_world_database`].
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct CarryTuning {
    /// `fActivatePickLength:Interface`, 180 units (`RF-SET-001`, record `0x14209e4c0`; reader function
    /// not traced; the value is high confidence). It is an INI-style setting, not a GMST:
    /// it is not in `Skyrim.esm`, the pack or `Skyrim.ini`, so no master or pack overrides it.
    pub reach: f32,
    /// `fZKeyDelay:Controls`, seconds the key must stay down before it grabs (`RF-CARRY-001`, strict
    /// `>`): 0.2 s (`RF-SET-002`, record `0x1420b8638`; reader function not traced; the value is high
    /// confidence). Not in `Skyrim.ini`.
    pub key_delay: f32,
    /// `fZKeySpringElasticity` (`RF-CARRY-003`): the fraction of the position error that the spring asks to remove
    /// in one 60 Hz step, 0.2 (12 per second). Retail executable default.
    pub spring_elasticity: f32,
    /// `fZKeySpringDamping` (`RF-CARRY-003`): the fraction of the point velocity the spring removes each step, 0.5.
    /// Retail executable default.
    pub spring_damping: f32,
    /// `fZKeyObjectDamping` (`RF-CARRY-003`): both body velocities are multiplied by this each step, 0.75. Retail
    /// executable default.
    pub object_damping: f32,
    /// `fZKeyMaxForce`: largest spring acceleration per kilogram, in m/s^2, before the weight
    /// factor (`RF-CARRY-003`, solver clamp `RF-CARRY-009`). `Skyrim.esm` sets 100 (`RF-GMST-001`) (the executable default is 250); read from
    /// the pack.
    pub max_force: f32,
    /// `fZKeyMaxForceWeightLow` (`RF-CARRY-003`), kg: bodies this light or lighter get `max_force_scale_high`.
    /// Retail executable default 0 (not overridden by the master).
    pub max_force_weight_low: f32,
    /// `fZKeyMaxForceWeightHigh`, kg: bodies this heavy or heavier get `max_force_scale_low`
    /// (`RF-CARRY-003`). `Skyrim.esm` sets 150 (`RF-GMST-001`; the executable default is 300); read
    /// from the pack.
    pub max_force_weight_high: f32,
    /// `fZKeyMaxForceScaleLow` (`RF-CARRY-003`): the force factor for the heaviest bodies. Retail executable
    /// default 0.2.
    pub max_force_scale_low: f32,
    /// `fZKeyMaxForceScaleHigh` (`RF-CARRY-003`): the force factor for the lightest bodies. Retail executable
    /// default 1.0.
    pub max_force_scale_high: f32,
    /// `fGrabMaxWeightWalking` (`RF-CARRY-002`), heaviest body the player can grab, in kg. Retail executable
    /// default 100. (`fGrabMaxWeightRunning`, 50, is not used: running is not modelled.)
    pub grab_max_weight: f32,
    /// Distance between the grabbed point and its target that drops the object, in units. The
    /// retail hold check compares the lag, in units, with the hard-coded constant 96.0 at
    /// `0x14181ab88` (`RF-CARRY-005`).
    pub lag_break: f32,
    /// `fZKeyHeavyWeight`, kg: holding a body heavier than this makes the "may run" test fail, so
    /// the run speed is replaced by the walk speed (`RF-CARRY-012`, which supersedes
    /// `RF-CARRY-006`). Retail executable default 75.
    pub heavy_weight: f32,
    /// `fZKeyMaxContactDistance` (`RF-CARRY-005`), in units. A lag above this arms the contact-mass rule. Retail
    /// executable default 10.
    pub contact_distance: f32,
    /// `fZKeyMaxContactMassRatio` (`RF-CARRY-005`). The heaviest moving body that touches the held object, divided
    /// by the held mass, drops the object when it is above this value and the lag is above
    /// [`Self::contact_distance`]. Retail executable default 4.
    pub contact_mass_ratio: f32,
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
            max_force_weight_low: 0.0,
            max_force_weight_high: 150.0,
            max_force_scale_low: 0.2,
            max_force_scale_high: 1.0,
            grab_max_weight: 100.0,
            heavy_weight: 75.0,
            lag_break: 96.0,
            contact_distance: 10.0,
            contact_mass_ratio: 4.0,
        }
    }
}

impl CarryTuning {
    /// Spring gain in 1/s: the share of the error asked for per second.
    pub fn spring_gain(&self, dt: f32) -> f32 {
        self.spring_elasticity / dt
    }

    /// The factor that scales `fZKeyMaxForce` for a body of `mass` kg, as the retail grab code
    /// computes it: with `t = (high - clamp(mass, low, high)) / (high - low)`, the factor is
    /// `scale_low + (scale_high - scale_low) * t`. A body at or below the low weight gets
    /// `scale_high` (1.0); a body at or above the high weight gets `scale_low` (0.2). Degenerate
    /// weights (high not above low) keep full force.
    pub fn max_force_scale(&self, mass: f32) -> f32 {
        let span = self.max_force_weight_high - self.max_force_weight_low;
        if !(span.is_finite() && span > 0.0 && mass.is_finite()) {
            return self.max_force_scale_high;
        }
        let clamped = mass.clamp(self.max_force_weight_low, self.max_force_weight_high);
        let light = (self.max_force_weight_high - clamped) / span;
        self.max_force_scale_low + (self.max_force_scale_high - self.max_force_scale_low) * light
    }

    /// Largest spring acceleration in units/s^2 for a body of `mass` kg: `fZKeyMaxForce` (m/s^2
    /// per kg) times [`Self::max_force_scale`], in units.
    pub fn max_acceleration_for(&self, mass: f32) -> f32 {
        self.max_force * self.max_force_scale(mass) * UNITS_PER_METRE
    }

    /// True while holding a body of `held_mass` kg blocks running: a mass above `fZKeyHeavyWeight`
    /// (`RF-CARRY-012`). The walk intent then uses the walk speed in place of the run speed.
    pub fn blocks_running(&self, held_mass: f32) -> bool {
        held_mass.is_finite() && held_mass > self.heavy_weight
    }

    /// Closest hold distance from the camera, in units (`RF-CARRY-008`): retail takes the player's
    /// character-controller capsule radius in Havok metres times 69.99125 plus 5.0 (the constant at
    /// `0x141b5be6c`). The caller passes Mudcrab's own capsule radius, which is provisional and not
    /// traced to retail, so this minimum is only as retail as that radius.
    pub fn min_hold_distance(&self, capsule_radius: f32) -> f32 {
        capsule_radius + 5.0
    }

    /// The compiled defaults, with every carry setting that the pack's GMST records hold
    /// replacing them. The pack carries `fZKeyMaxForce` (100) and `fZKeyMaxForceWeightHigh` (150),
    /// the two that `Skyrim.esm` overrides. A pack without GMST records, or with undecodable ones,
    /// keeps the defaults. Read-only.
    pub fn from_world_database(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .wrap_err_with(|| format!("opening carry settings in {}", path.display()))?;
        let mut statement = connection
            .prepare("SELECT data FROM records WHERE record_type='GMST'")
            .wrap_err("reading GMST records")?;
        let mut values: HashMap<String, f32> = HashMap::new();
        for row in statement.query_map([], |row| row.get::<_, Vec<u8>>(0))? {
            if let Some((name, value)) = decode_carry_gmst(&row?) {
                values.insert(name, value);
            }
        }
        let mut tuning = Self::default();
        tuning.apply_gmst(|name| values.get(&name.to_ascii_lowercase()).copied());
        info!(
            "carry settings: {} carry GMSTs from the pack; fZKeyMaxForce={} fZKeyMaxForceWeightHigh={}",
            values.len(),
            tuning.max_force,
            tuning.max_force_weight_high
        );
        Ok(tuning)
    }

    /// Replace each constant that `lookup` knows by its Skyrim GMST name. Missing, non-finite and
    /// non-positive values keep the current constant.
    pub fn apply_gmst(&mut self, lookup: impl Fn(&str) -> Option<f32>) {
        let take = |name: &str, slot: &mut f32| {
            if let Some(value) = lookup(name).filter(|value| value.is_finite() && *value > 0.0) {
                *slot = value;
            }
        };
        take("fZKeyDelay", &mut self.key_delay);
        take("fZKeyDelay:Controls", &mut self.key_delay);
        take("fActivatePickLength:Interface", &mut self.reach);
        take("fZKeySpringElasticity", &mut self.spring_elasticity);
        take("fZKeySpringDamping", &mut self.spring_damping);
        take("fZKeyObjectDamping", &mut self.object_damping);
        take("fZKeyMaxForce", &mut self.max_force);
        take("fZKeyMaxForceWeightHigh", &mut self.max_force_weight_high);
        take("fZKeyMaxForceScaleLow", &mut self.max_force_scale_low);
        take("fZKeyMaxForceScaleHigh", &mut self.max_force_scale_high);
        take("fGrabMaxWeightWalking", &mut self.grab_max_weight);
        take("fZKeyHeavyWeight", &mut self.heavy_weight);
        take("fZKeyMaxContactDistance", &mut self.contact_distance);
        take("fZKeyMaxContactMassRatio", &mut self.contact_mass_ratio);
        // `fZKeyMaxForceWeightLow` is 0 in the retail executable, and `apply_gmst` skips zero
        // values, so it is only replaced by a positive master value.
        take("fZKeyMaxForceWeightLow", &mut self.max_force_weight_low);
    }
}

/// A carry GMST (`fZKey*` or `fGrab*`) from a `records.data` archive: its editor ID, lowercased,
/// and float value. The converter treats GMST names without regard to case and keeps the winning
/// spelling, so the name is compared and keyed in lowercase. `None` for any other record, and for
/// a record that does not decode.
fn decode_carry_gmst(bytes: &[u8]) -> Option<(String, f32)> {
    // SQLite gives no alignment guarantee for the blob, and the archive check needs one.
    let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(bytes.len());
    aligned.extend_from_slice(bytes);
    let record = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&aligned).ok()?;
    let field = |tag: &[u8; 4]| {
        record
            .subrecords
            .iter()
            .find(|sub| &sub.tag == tag)
            .map(|sub| sub.data.as_slice())
    };
    let name = String::from_utf8_lossy(field(b"EDID")?)
        .trim_matches('\0')
        .to_ascii_lowercase();
    if !(name.starts_with("fzkey") || name.starts_with("fgrab")) {
        return None;
    }
    let data: [u8; 4] = field(b"DATA")?.try_into().ok()?;
    Some((name, f32::from_le_bytes(data)))
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
    /// The grabbed point lags more than [`CarryTuning::lag_break`] behind its target.
    Lag,
    /// A heavier moving body presses on the object while it lags.
    HeavyContact,
    /// The body no longer exists (its cell unloaded).
    Gone,
    /// WALK ended, a door crossing started, the cursor was released, or the console or a menu
    /// opened.
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
pub fn drop_reason(lag: f32, contact_ratio: f32, tuning: &CarryTuning) -> Option<DropReason> {
    if lag > tuning.lag_break {
        Some(DropReason::Lag)
    } else if lag > tuning.contact_distance && contact_ratio > tuning.contact_mass_ratio {
        Some(DropReason::HeavyContact)
    } else {
        None
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

/// One step of the Havok-style mouse spring on the material point `anchor`: a step-by-step port of
/// the algorithm in spud's `apply_grab` (MIT, commit `e5f2de4b`; see the module docs and
/// `THIRD_PARTY.md`). Retail passes its spring parameters to Havok (presumably the mouse spring).
///
/// 1. Both velocities are scaled by `object_damping`.
/// 2. The point velocity is `v + w x r`.
/// 3. The wanted velocity change is `(target - anchor) * gain - point velocity * damping`.
/// 4. The impulse that gives that change at the point goes through the point's effective mass,
///    `K = I/m - [r]x W [r]x`, so an off-centre grab turns the body and does not shake it.
/// 5. The impulse is clamped to `max_acceleration_for(mass) * mass * dt` (the retail force limit,
///    scaled by the body's weight) and applied at the point.
///
/// If any input or result is not finite, the step only damps the finite velocities and applies no
/// impulse, so a degenerate inertia cannot poison the body's velocity.
pub fn spring_step(
    body: &CarryBody,
    anchor: Vec3,
    target: Vec3,
    tuning: &CarryTuning,
    dt: f32,
) -> SpringOutcome {
    let finite_or_zero = |v: Vec3| if v.is_finite() { v } else { Vec3::ZERO };
    let mut linear = finite_or_zero(body.linear * tuning.object_damping);
    let mut angular = finite_or_zero(body.angular * tuning.object_damping);
    let damped_only = SpringOutcome {
        linear,
        angular,
        impulse: Vec3::ZERO,
    };
    if !(body.mass.is_finite() && body.mass > 0.0)
        || !(anchor.is_finite() && target.is_finite() && body.centre.is_finite())
        || !body.inv_inertia.is_finite()
    {
        return damped_only;
    }
    let r = anchor - body.centre;
    let point_velocity = linear + angular.cross(r);
    let wanted =
        (target - anchor) * tuning.spring_gain(dt) - point_velocity * tuning.spring_damping;
    let skew_r = skew(r);
    let response = Mat3::IDENTITY * (1.0 / body.mass) - skew_r * body.inv_inertia * skew_r;
    // Retail applies nothing in a step where K is singular (`RF-CARRY-009`).
    let determinant = response.determinant();
    if !(determinant.is_finite() && determinant != 0.0) {
        return damped_only;
    }
    let mut impulse = response.inverse() * wanted;
    impulse = impulse.clamp_length_max(tuning.max_acceleration_for(body.mass) * body.mass * dt);
    linear += impulse / body.mass;
    angular += body.inv_inertia * r.cross(impulse);
    if !(impulse.is_finite() && linear.is_finite() && angular.is_finite()) {
        return damped_only;
    }
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

/// Samples `E` each frame. A key that stays down for `key_delay` seconds asks
/// for one grab, and the release that ends that hold does nothing. A later press and release of
/// the key drops the object.
pub fn carry_input_system(
    keyboard: Res<ButtonInput<KeyCode>>,
    capture: Res<CursorCapture>,
    time: Res<Time>,
    tuning: Res<CarryTuning>,
    mut held: ResMut<HeldObject>,
    mut input: ResMut<CarryInput>,
    mut commands: Commands,
) {
    if *capture != CursorCapture::Captured {
        // The cursor was released (focus loss, F3): let go now, in this schedule.
        cancel_carry(&mut input, &mut held, &mut commands);
        return;
    }
    let holding = held.held.is_some();
    if keyboard.just_pressed(KeyCode::KeyE) {
        input.down = true;
        input.down_secs = 0.0;
        input.attempted = false;
        input.grabbed_by_press = false;
    }
    if input.down && keyboard.pressed(KeyCode::KeyE) {
        input.down_secs += time.delta_secs();
        if !input.attempted && input.down_secs > tuning.key_delay {
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
        if !input.grabbed_by_press && holding {
            input.pending = Some(CarryRequest::Release);
        }
    }
}

/// While the console or a menu owns the keyboard, the key counts as released and the object is
/// dropped right here. A menu pauses virtual time, so no fixed step runs while it is open; the
/// drop must not wait for the first step after it closes.
pub fn carry_cancel_system(
    mut held: ResMut<HeldObject>,
    mut input: ResMut<CarryInput>,
    mut commands: Commands,
) {
    cancel_carry(&mut input, &mut held, &mut commands);
}

/// Forget the key and any pending request, and drop a held object immediately.
fn cancel_carry(input: &mut CarryInput, held: &mut HeldObject, commands: &mut Commands) {
    input.down = false;
    input.attempted = false;
    input.grabbed_by_press = false;
    input.pending = None;
    release(commands, held, DropReason::Interrupted);
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

/// Let go of the held object. The body keeps the velocity the spring left it with.
fn release(commands: &mut Commands, held: &mut HeldObject, reason: DropReason) {
    let Some(taken) = held.held.take() else {
        return;
    };
    held.last_drop = Some(reason);
    // Normal sleeping again. Only an entity that still exists can take the component.
    if let Ok(mut entity) = commands.get_entity(taken.entity) {
        entity.insert(Sleeping::default());
    }
    info!("carry: released {:?} ({reason:?})", taken.entity);
}

/// Bodies the player may pick up: debug tankards and authored dynamic clutter.
type CarriableFilter = Or<(With<DebugTankard>, With<DynamicClutter>)>;

/// Keeps the hold target out of the player's capsule: its horizontal distance from the capsule
/// axis is at least `min_horizontal`. Looking steeply down puts the plain target
/// (`eye + forward * distance`) on the axis, inside the capsule, and the held body and the capsule
/// would then fight. The target moves out along the view's horizontal direction (the body's facing
/// when the view is straight down). Mudcrab engineering: retail probably avoids this with the
/// `ZKeyObject` collision layer (#691).
pub fn clamp_target_outside_body(
    target: Vec3,
    body: Vec3,
    view_forward: Vec3,
    min_horizontal: f32,
) -> Vec3 {
    let offset = Vec2::new(target.x - body.x, target.z - body.z);
    if offset.length() >= min_horizontal {
        return target;
    }
    let facing = Vec2::new(view_forward.x, view_forward.z);
    let direction = if facing.length() > 1.0e-4 {
        facing.normalize()
    } else if offset.length() > 1.0e-4 {
        offset.normalize()
    } else {
        Vec2::NEG_Y
    };
    Vec3::new(
        body.x + direction.x * min_horizontal,
        target.y,
        body.z + direction.y * min_horizontal,
    )
}

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

/// Fixed-step carry: runs the pending grab or drop, then the drop rules and the spring.
#[allow(clippy::too_many_arguments)]
pub fn carry_step_system(
    mode: Res<MoveMode>,
    crossing: Option<Res<DoorCrossing>>,
    tuning: Res<CarryTuning>,
    movement: Res<MovementTuning>,
    mut input: ResMut<CarryInput>,
    mut held: ResMut<HeldObject>,
    camera: CarryCamera,
    player: Query<(Entity, &Transform), With<PlayerBody>>,
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
        release(&mut commands, &mut held, DropReason::Interrupted);
        return;
    }
    let (Ok(camera), Ok(context)) = (camera.single(), context.single()) else {
        return;
    };
    let forward = camera.forward().as_vec3();
    // The shown camera `Transform` is a presentation pose: it is blended between ticks
    // (`WalkViewPose`) and carries the follow spring's lag. The hold point and the grab ray use
    // the eye of the body at this tick, so they do not depend on the display's frame rate.
    let eye = player
        .single()
        .map(|(_, body)| body.translation + Vec3::Y * movement.eye_height)
        .unwrap_or(camera.translation);
    if held.held.is_none() {
        if request == Some(CarryRequest::Grab) {
            try_grab(
                &context,
                eye,
                forward,
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
        release(&mut commands, &mut held, DropReason::Gone);
        return;
    };
    if request == Some(CarryRequest::Release) {
        release(&mut commands, &mut held, DropReason::Released);
        return;
    }
    let anchor = body.translation() + *body.rotation() * taken.anchor_local;
    let mut target = eye + forward * taken.distance;
    if let Ok((_, body_pose)) = player.single() {
        target = clamp_target_outside_body(
            target,
            body_pose.translation,
            forward,
            tuning.min_hold_distance(movement.capsule_radius),
        );
    }
    let lag = anchor.distance(target);
    let ratio = if lag > tuning.contact_distance {
        contact_mass_ratio(&context, taken.entity, taken.mass)
    } else {
        0.0
    };
    if let Some(reason) = drop_reason(lag, ratio, &tuning) {
        release(&mut commands, &mut held, reason);
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
    eye: Vec3,
    forward: Vec3,
    tuning: &CarryTuning,
    movement: &MovementTuning,
    player: &Query<(Entity, &Transform), With<PlayerBody>>,
    carriable: &Query<(), CarriableFilter>,
    held: &mut HeldObject,
    commands: &mut Commands,
) {
    let filter = match player.single() {
        Ok((entity, _)) => QueryFilter::default().exclude_rigid_body(entity),
        Err(_) => QueryFilter::default(),
    };
    // The nearest thing on the line of sight decides: a wall in front hides an object behind it.
    let Some((collider, distance)) = context.cast_ray(eye, forward, tuning.reach, true, filter)
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
    let point = eye + forward * distance;
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

/// Adds the carry systems. `E` carries; `T` still spawns a test tankard.
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
    };
    use shared::collision::{BodyKind, CollisionBody, HavokBodyInfo};
    use shared::records::ArchivedRecordData;

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
    fn the_defaults_are_the_retail_values() {
        let tuning = CarryTuning::default();
        // Spring: 0.2 per step is 12 per second (fZKeySpringElasticity).
        assert!((tuning.spring_gain(DT) - 12.0).abs() < 1.0e-3);
        assert_eq!(tuning.spring_damping, 0.5);
        assert_eq!(tuning.object_damping, 0.75);
        // fZKeyMaxForce is 100 in Skyrim.esm (250 in the executable): 100 m/s^2 for a light body,
        // in units with the retail 69.99125 units per metre.
        assert!((tuning.max_acceleration_for(0.0) - 6999.125).abs() < 0.01);
        assert_eq!(tuning.grab_max_weight, 100.0);
        assert_eq!(tuning.heavy_weight, 75.0);
        assert_eq!(tuning.lag_break, 96.0);
        assert_eq!(tuning.contact_distance, 10.0);
        assert_eq!(tuning.contact_mass_ratio, 4.0);
        assert_eq!(tuning.reach, 180.0);
        assert_eq!(tuning.key_delay, 0.2);
        assert_eq!(tuning.min_hold_distance(28.0), 33.0);
    }

    /// The force factor follows the retail formula with the Skyrim.esm weights (0 and 150 kg) and
    /// the executable's scales (1.0 and 0.2).
    #[test]
    fn the_force_limit_falls_with_the_weight_of_the_body() {
        let tuning = CarryTuning::default();
        assert_eq!(tuning.max_force_scale(0.0), 1.0);
        assert!((tuning.max_force_scale(75.0) - 0.6).abs() < 1.0e-6);
        assert!((tuning.max_force_scale(150.0) - 0.2).abs() < 1.0e-6);
        // Past the high weight and below the low weight the factor stops changing.
        assert!((tuning.max_force_scale(900.0) - 0.2).abs() < 1.0e-6);
        assert_eq!(tuning.max_force_scale(-5.0), 1.0);
        // 100 kg, the heaviest body that can be grabbed, still gets 46.7 m/s^2, above gravity.
        let limit = tuning.max_acceleration_for(100.0) / UNITS_PER_METRE;
        assert!((limit - 46.667).abs() < 0.01, "{limit}");
        // Degenerate weights keep full force instead of dividing by zero.
        let flat = CarryTuning {
            max_force_weight_high: 0.0,
            ..CarryTuning::default()
        };
        assert_eq!(flat.max_force_scale(10.0), 1.0);
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
    fn a_large_error_is_clamped_to_the_weight_scaled_force_limit() {
        let tuning = CarryTuning::default();
        for mass in [1.0, 5.0, 25.0, 100.0] {
            let outcome = spring_step(
                &cube(mass),
                Vec3::ZERO,
                Vec3::new(0.0, 0.0, 140.0),
                &tuning,
                DT,
            );
            let limit = tuning.max_acceleration_for(mass) * DT;
            let change = outcome.linear.length();
            assert!(
                (change - limit).abs() < limit * 0.01,
                "{mass} kg changed its velocity by {change}, limit {limit}"
            );
            // The limit is per kilogram, so the impulse grows with the mass.
            assert!((outcome.impulse.length() - limit * mass).abs() < limit * mass * 0.01);
        }
        // A 5 kg body: 100 m/s^2 times the weight factor, about 116 units/s in one step.
        let light = tuning.max_acceleration_for(5.0) * DT;
        assert!((light - 113.0).abs() < 1.5, "{light}");
    }

    #[test]
    fn a_degenerate_body_never_writes_a_non_finite_velocity() {
        let tuning = CarryTuning::default();
        let mut body = cube(5.0);
        body.linear = Vec3::new(8.0, 0.0, 0.0);
        let cases = [
            // NaN inertia, NaN anchor, NaN target, NaN mass, infinite velocity.
            CarryBody {
                inv_inertia: Mat3::from_diagonal(Vec3::splat(f32::NAN)),
                ..body
            },
            CarryBody {
                mass: f32::NAN,
                ..body
            },
            CarryBody {
                linear: Vec3::splat(f32::INFINITY),
                ..body
            },
        ];
        for body in cases {
            let outcome = spring_step(&body, Vec3::ONE, Vec3::new(5.0, 0.0, 0.0), &tuning, DT);
            assert!(outcome.linear.is_finite(), "{outcome:?}");
            assert!(outcome.angular.is_finite(), "{outcome:?}");
            assert!(outcome.impulse.is_finite(), "{outcome:?}");
        }
        for (anchor, target) in [
            (Vec3::splat(f32::NAN), Vec3::ZERO),
            (Vec3::ZERO, Vec3::splat(f32::NAN)),
        ] {
            let outcome = spring_step(&body, anchor, target, &tuning, DT);
            assert!(outcome.linear.is_finite() && outcome.angular.is_finite());
            // Nothing was applied: the step only damped the old velocity.
            assert!(
                outcome.linear.abs_diff_eq(Vec3::new(6.0, 0.0, 0.0), 1.0e-4),
                "{outcome:?}"
            );
            assert_eq!(outcome.impulse, Vec3::ZERO);
        }
    }

    /// `RF-CARRY-012`: a held mass over `fZKeyHeavyWeight` (75 kg) blocks running.
    #[test]
    fn a_held_mass_over_75_kg_blocks_running() {
        let tuning = CarryTuning::default();
        assert!(!tuning.blocks_running(74.9));
        assert!(!tuning.blocks_running(75.0));
        assert!(tuning.blocks_running(75.1));
        assert!(tuning.blocks_running(100.0));
        assert!(!tuning.blocks_running(f32::NAN));
    }

    #[test]
    fn the_hold_target_is_kept_outside_the_capsule() {
        let body = Vec3::new(100.0, 50.0, 200.0);
        let min = 33.0;
        // Looking level: the target is far enough already and does not move.
        let level = Vec3::new(100.0, 140.0, 110.0);
        assert_eq!(
            clamp_target_outside_body(level, body, Vec3::NEG_Z, min),
            level
        );
        // Looking straight down: the target is on the axis and moves out along the facing.
        let down = Vec3::new(100.0, 107.0, 200.0);
        let moved = clamp_target_outside_body(down, body, Vec3::new(0.0, -1.0, -0.01), min);
        assert!((Vec2::new(moved.x - body.x, moved.z - body.z).length() - min).abs() < 1.0e-3);
        assert!(moved.z < body.z, "moved the wrong way: {moved:?}");
        assert_eq!(moved.y, down.y);
        // Exactly straight down and on the axis: falls back to a fixed direction, never NaN.
        let fallback = clamp_target_outside_body(down, body, Vec3::NEG_Y, min);
        assert!(fallback.is_finite());
        assert!(
            (Vec2::new(fallback.x - body.x, fallback.z - body.z).length() - min).abs() < 1.0e-3
        );
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
        assert_eq!(drop_reason(90.0, 0.0, &tuning), None);
        assert_eq!(drop_reason(96.0, 0.0, &tuning), None);
        assert_eq!(drop_reason(100.0, 0.0, &tuning), Some(DropReason::Lag));
        // The retail code compares units (metres times 69.99125) with 96.0, so a 2 m lag, 140
        // units, drops it. A lag of 2 against a constant of 96 would not.
        assert_eq!(
            drop_reason(2.0 * UNITS_PER_METRE, 0.0, &tuning),
            Some(DropReason::Lag)
        );
    }

    #[test]
    fn a_heavy_moving_body_drops_the_object_only_while_it_lags() {
        let tuning = CarryTuning::default();
        // Ratio 5 is above 4, but the object follows its target: no drop.
        assert_eq!(drop_reason(5.0, 5.0, &tuning), None);
        assert_eq!(
            drop_reason(11.0, 5.0, &tuning),
            Some(DropReason::HeavyContact)
        );
        // Ratio 3 is below 4: held at any lag below the snag limit.
        assert_eq!(drop_reason(50.0, 3.0, &tuning), None);
        // Static geometry is ratio 0.
        assert_eq!(drop_reason(50.0, 0.0, &tuning), None);
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

    // ---- Settings from the pack ----------------------------------------------------------------

    /// A `records` row for a GMST, as the converter archives it.
    fn gmst_row(name: &str, value: f32) -> Vec<u8> {
        let record = ArchivedRecordData {
            subrecords: vec![
                shared::records::ArchivedSubRecord {
                    tag: *b"EDID",
                    data: format!("{name}\0").into_bytes(),
                },
                shared::records::ArchivedSubRecord {
                    tag: *b"DATA",
                    data: value.to_le_bytes().to_vec(),
                },
            ],
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&record)
            .unwrap()
            .to_vec()
    }

    fn pack_with(rows: &[(u32, &str, Vec<u8>)]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE records (form_id INTEGER PRIMARY KEY, record_type TEXT NOT NULL, \
                 cell_id INTEGER, worldspace_id INTEGER, load_order INTEGER NOT NULL, \
                 data BLOB NOT NULL)",
            )
            .unwrap();
        for (form_id, record_type, data) in rows {
            connection
                .execute(
                    "INSERT INTO records(form_id, record_type, load_order, data) VALUES (?1, ?2, 0, ?3)",
                    rusqlite::params![form_id, record_type, data],
                )
                .unwrap();
        }
        directory
    }

    /// `RF-CARRY-003`: `Skyrim.esm` overrides `fZKeyMaxForce` (250 in the executable) with 100 and
    /// `fZKeyMaxForceWeightHigh` (300) with 150. The pack's values replace the compiled ones.
    #[test]
    fn the_pack_gmsts_replace_the_compiled_defaults() {
        let pack = pack_with(&[
            (0x108B7B, "GMST", gmst_row("fZKeyMaxForce", 100.0)),
            (0x108B7C, "GMST", gmst_row("fZKeyMaxForceWeightHigh", 150.0)),
            (0x1, "GMST", gmst_row("fMoveCharWalkBase", 100.0)),
            (0x2, "STAT", gmst_row("fZKeyDelay", 9.0)),
        ]);
        let tuning =
            CarryTuning::from_world_database(&pack.path().join("skyrim_world.db")).unwrap();
        assert_eq!(tuning.max_force, 100.0);
        assert_eq!(tuning.max_force_weight_high, 150.0);
        // Only GMST records count, and only carry settings: nothing else moved.
        assert_eq!(tuning, CarryTuning::default());
        // Other values in the pack are used too, for example the executable's own 250 and 300.
        let pack = pack_with(&[
            (1, "GMST", gmst_row("fZKeyMaxForce", 250.0)),
            (2, "GMST", gmst_row("fZKeyMaxForceWeightHigh", 300.0)),
        ]);
        let tuning =
            CarryTuning::from_world_database(&pack.path().join("skyrim_world.db")).unwrap();
        assert_eq!(tuning.max_force, 250.0);
        assert_eq!(tuning.max_force_weight_high, 300.0);
        // The mass scale follows the setting: 150 kg is the heavy end at 150, not at 300.
        assert!((CarryTuning::default().max_force_scale(150.0) - 0.2).abs() < 1.0e-6);
        assert!((tuning.max_force_scale(150.0) - 0.6).abs() < 1.0e-6);
    }

    /// The converter treats GMST names without regard to case and keeps the winning spelling, so a
    /// pack can hold `fzkeymaxforce` or `FZKeyMaxForceWeightHigh`.
    #[test]
    fn gmst_names_match_without_regard_to_case() {
        let pack = pack_with(&[
            (1, "GMST", gmst_row("fzkeymaxforce", 250.0)),
            (2, "GMST", gmst_row("FZKEYMAXFORCEWEIGHTHIGH", 300.0)),
            (3, "GMST", gmst_row("FGrabMaxWeightWalking", 90.0)),
            (4, "GMST", gmst_row("fZkEyHeavyWeight", 60.0)),
        ]);
        let tuning =
            CarryTuning::from_world_database(&pack.path().join("skyrim_world.db")).unwrap();
        assert_eq!(tuning.max_force, 250.0);
        assert_eq!(tuning.max_force_weight_high, 300.0);
        assert_eq!(tuning.grab_max_weight, 90.0);
        assert_eq!(tuning.heavy_weight, 60.0);
    }

    #[test]
    fn a_pack_without_gmst_records_keeps_the_defaults() {
        let pack = pack_with(&[(1, "STAT", vec![1, 2, 3])]);
        let tuning =
            CarryTuning::from_world_database(&pack.path().join("skyrim_world.db")).unwrap();
        assert_eq!(tuning, CarryTuning::default());
        // A GMST record that does not decode is skipped, not fatal.
        let pack = pack_with(&[(1, "GMST", vec![1, 2, 3])]);
        let tuning =
            CarryTuning::from_world_database(&pack.path().join("skyrim_world.db")).unwrap();
        assert_eq!(tuning, CarryTuning::default());
    }

    /// Reads the real pack. Set `MUDCRAB_CARRY_PACK` to a pack's `skyrim_world.db` and run with
    /// `--ignored`: the pack must give the traced values 100 and 150 (`RF-CARRY-003`).
    #[test]
    #[ignore = "needs a converted pack: set MUDCRAB_CARRY_PACK to its skyrim_world.db"]
    fn the_real_pack_gives_the_traced_force_settings() {
        let path = std::env::var("MUDCRAB_CARRY_PACK").expect("MUDCRAB_CARRY_PACK");
        let tuning = CarryTuning::from_world_database(Path::new(&path)).unwrap();
        assert_eq!(tuning.max_force, 100.0);
        assert_eq!(tuning.max_force_weight_high, 150.0);
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

    /// Distance from the grabbed point to its target (kept outside the capsule), in units: the
    /// same target the carry step aims at.
    fn lag(app: &mut App) -> f32 {
        let taken = held(app).expect("held");
        let view = eye(app);
        let forward = view.forward().as_vec3();
        let player = app
            .world_mut()
            .query_filtered::<&Transform, With<PlayerBody>>()
            .single(app.world())
            .unwrap()
            .translation;
        let radius = app.world().resource::<MovementTuning>().capsule_radius;
        let target = clamp_target_outside_body(
            view.translation + forward * taken.distance,
            player,
            forward,
            CarryTuning::default().min_hold_distance(radius),
        );
        let transform = *app.world().get::<Transform>(taken.entity).unwrap();
        let anchor = transform.translation + transform.rotation * taken.anchor_local;
        anchor.distance(target)
    }

    /// A jump while a held object is carried: the launch, the air state and the single landing
    /// are the same as without one (#689 and the walking support rule, RF-CC-013, RF-CC-015).
    #[test]
    fn a_jump_while_carrying_launches_and_lands_once() {
        let mut app = carry_app();
        spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        let taken = held(&app).expect("the box is held before the jump").entity;
        let ground = app
            .world_mut()
            .query_filtered::<&Transform, With<PlayerBody>>()
            .single(app.world())
            .unwrap()
            .translation
            .y;
        app.insert_resource(crate::physics::WalkIntent {
            wish_dir: Vec3::ZERO,
            target_speed: 0.0,
            jump_pressed: true,
        });
        let (mut apex, mut launched, mut landings, mut air_ticks) = (0.0_f32, false, 0, 0);
        for _ in 0..240 {
            app.update();
            assert_eq!(
                held(&app).map(|held| held.entity),
                Some(taken),
                "the jump dropped the held object"
            );
            let mut query = app
                .world_mut()
                .query_filtered::<(&Transform, &crate::physics::WalkState), With<PlayerBody>>();
            let (body, state) = query.single(app.world()).unwrap();
            apex = apex.max(body.translation.y - ground);
            launched |= state.jump_active();
            landings += usize::from(state.landed);
            air_ticks += usize::from(state.airborne);
        }
        assert!(
            launched && apex > 40.0,
            "the jump did not launch: apex {apex}"
        );
        assert!(
            air_ticks > 10,
            "the jump was not an air state: {air_ticks} ticks"
        );
        assert_eq!(landings, 1, "exactly one landing for the jump");
        let state = app
            .world_mut()
            .query::<&crate::physics::WalkState>()
            .single(app.world())
            .unwrap();
        assert!(
            !state.airborne && state.grounded(),
            "standing again: {state:?}"
        );
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

    fn press_w(app: &mut App) {
        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::KeyW,
            logical_key: Key::Character("w".into()),
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        app.update();
    }

    /// `RF-CARRY-012`: holding 50 kg leaves the run speed; holding 90 kg (over 75) makes the run
    /// speed equal the walk speed.
    #[test]
    fn a_heavy_hold_walks_and_a_light_hold_runs() {
        let target_speed = |mass: f32| {
            let mut app = carry_app();
            spawn_box_ahead(&mut app, mass, 100.0);
            frames(&mut app, 2);
            hold_activate(&mut app);
            assert!(held(&app).is_some(), "{mass} kg: not held");
            press_w(&mut app);
            frames(&mut app, 2);
            app.world()
                .resource::<crate::physics::WalkIntent>()
                .target_speed
        };
        let tuning = MovementTuning::default();
        let walk = tuning.walk_speed;
        let run = tuning.run_speed;
        assert!(run > walk);
        assert!((target_speed(50.0) - run).abs() < 1.0e-3, "light hold");
        assert!((target_speed(90.0) - walk).abs() < 1.0e-3, "heavy hold");
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

    /// A paused world never moves or retargets the held object (#704). The spring runs only in the
    /// fixed step, which does not run while virtual time is paused, so a view change made during
    /// the pause is not followed until the world resumes. The object stays held across the pause
    /// (the cursor is still captured here), then rises with the view.
    #[test]
    fn a_paused_world_freezes_the_held_object_until_resume() {
        let mut app = carry_app();
        let entity = spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        frames(&mut app, 60);
        let held_before = held(&app).expect("held");
        let level = centre_of(&app, entity);
        app.world_mut().resource_mut::<Time<Virtual>>().pause();
        app.world_mut().resource_mut::<LookIntent>().pitch = 0.4;
        frames(&mut app, 90);
        assert_eq!(
            held(&app),
            Some(held_before),
            "the hold changed while paused"
        );
        assert_eq!(
            centre_of(&app, entity),
            level,
            "the held object moved while the world was paused"
        );
        app.world_mut().resource_mut::<Time<Virtual>>().unpause();
        frames(&mut app, 90);
        assert!(held(&app).is_some());
        assert!(
            centre_of(&app, entity).y > level.y + 20.0,
            "the held object did not follow the view after resume"
        );
        assert!(lag(&mut app) < 3.0, "lag {}", lag(&mut app));
    }

    /// The hold must not shake once it has settled. The 0.5-unit limit on the grabbed point is the
    /// jitter limit from the audit's phase-1 acceptance numbers (see "Acceptance numbers" in
    /// `docs/specs/engine/object-carrying.md`; spud's own shake before its effective-mass fix was
    /// 16 mm, about 1.1 units). A shake also reverses the point's direction every few ticks, so
    /// reversals are counted too.
    ///
    /// The body itself keeps creeping after the grab, so its centre gets two checks of its own: a
    /// bound on the total creep and a strict limit once it has settled. Measured: the point stays
    /// about 1 unit (1.4 cm) under its target and moves under 0.5 units; the centre creeps about 1
    /// unit over ticks 44 to 104 and under 0.1 units over the last 30 of them. The cause of the
    /// creep is not proven. Pure object damping (0.75 per tick) would be gone in under 40 ticks, so
    /// the creep is driven: gravity pulls the centre of mass around the grabbed point and the
    /// spring's effective-mass response lets the body turn slowly toward hanging under the point.
    /// Whether retail does the same is unverified (#691).
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
        // A shake reverses the point's direction on every axis; a settling creep does not.
        for axis in 0..3 {
            let steps: Vec<f32> = points
                .windows(2)
                .map(|pair| pair[1][axis] - pair[0][axis])
                .collect();
            let reversals = steps
                .windows(2)
                .filter(|pair| {
                    pair[0].abs() > 0.01 && pair[1].abs() > 0.01 && pair[0] * pair[1] < 0.0
                })
                .count();
            assert!(
                reversals <= 2,
                "axis {axis}: the point reversed {reversals} times"
            );
        }
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

    /// The stand-on drop rule is not implemented (it has no retail source, #691), and this test
    /// shows why that matters. A held box that the spring pulls upward lifts the player standing on
    /// it: measured 6.3 units (from 159.9 to 166.3) before it slides away. The limit stays strict
    /// (under 1 unit) and the test is ignored until the held body leaves the player's collision
    /// group the way retail's `ZKeyObject` layer probably does (#691). The cause of the lift is not
    /// verified.
    #[test]
    #[ignore = "a held box lifts the player standing on it by about 6 units; needs the retail held-object layer (#691)"]
    fn standing_on_the_held_box_does_not_launch_the_player() {
        let mut app = carry_app();
        // Keep the hold, so the spring keeps pulling on the box under the player.
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
        let mut highest = body.y;
        for _ in 0..180 {
            app.update();
            let (pose, _) = headless::player_pose(&mut app);
            assert!(pose.is_finite(), "player pose {pose:?}");
            highest = highest.max(pose.y);
        }
        assert!(
            highest < body.y + 1.0,
            "the held box lifted the player from {} to {highest}",
            body.y
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
            drop_reason(30.0, ratio, &CarryTuning::default()) == Some(DropReason::HeavyContact)
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
        assert_eq!(drop_reason(30.0, ratio, &CarryTuning::default()), None);
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

    /// Opening the console (or a menu) drops the object and swallows the key: no grab starts while
    /// it is open. What retail does with a held object when a menu opens is unknown (#691).
    #[test]
    fn opening_the_console_drops_the_object_and_blocks_a_new_grab() {
        let mut app = carry_app();
        spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        assert!(held(&app).is_some());
        // Opening the console pauses virtual time, so no fixed step runs: the drop must happen in
        // the Update that sees the console open, with no step after it.
        app.world_mut().resource_mut::<Time<Virtual>>().pause();
        app.insert_resource(crate::console::ConsoleState {
            open: true,
            ..default()
        });
        app.update();
        assert!(held(&app).is_none(), "still held with the console open");
        // Pressing and holding the key while the console is open grabs nothing.
        key(&mut app, ButtonState::Released);
        key(&mut app, ButtonState::Pressed);
        frames(&mut app, 16);
        assert!(held(&app).is_none(), "grabbed with the console open");
    }

    #[test]
    fn losing_window_focus_drops_the_object() {
        let mut app = carry_app();
        spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        assert!(held(&app).is_some());
        // An unfocused primary window releases the cursor, which ends the hold.
        app.world_mut().spawn((
            bevy::window::Window {
                focused: false,
                ..default()
            },
            bevy::window::PrimaryWindow,
        ));
        frames(&mut app, 3);
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Released
        );
        assert!(held(&app).is_none(), "still held after focus loss");
    }

    /// Looking straight down puts the plain target inside the capsule. The clamp keeps it out, so
    /// the box stays held, reaches its target and does not rest inside the player.
    #[test]
    fn looking_straight_down_keeps_the_held_box_out_of_the_capsule() {
        let mut app = carry_app();
        spawn_box_ahead(&mut app, 5.0, 100.0);
        frames(&mut app, 2);
        hold_activate(&mut app);
        frames(&mut app, 30);
        app.world_mut().resource_mut::<LookIntent>().pitch = -crate::physics::MAX_PITCH_RADIANS;
        frames(&mut app, 150);
        let taken = held(&app).expect("the hold ended while looking down");
        let radius = app.world().resource::<MovementTuning>().capsule_radius;
        let player = app
            .world_mut()
            .query_filtered::<&Transform, With<PlayerBody>>()
            .single(app.world())
            .unwrap()
            .translation;
        let transform = *app.world().get::<Transform>(taken.entity).unwrap();
        let point = transform.translation + transform.rotation * taken.anchor_local;
        let away = Vec2::new(point.x - player.x, point.z - player.z).length();
        assert!(
            away > radius,
            "the grabbed point is {away} units from the player axis, inside the radius {radius}"
        );
        assert!(lag(&mut app) < 6.0, "lag {}", lag(&mut app));
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
