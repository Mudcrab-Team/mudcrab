//! Distance dimmer, flicker and pulse of placed `LIGH` lights (#955).
//!
//! Source: the NEEDS 20 static findings, [skyrim_decomp PR 1073](https://github.com/Mudcrab-Team/skyrim_decomp/pull/1073)
//! (build 1.7.104.0). They are static: no run confirmed them, and the unit of the frame delta is
//! open. [point-lights.md](../../../docs/specs/engine/point-lights.md) section 4 states each rule.
//!
//! Each frame [`update_light_dynamics`] sets the light's `PointLight::intensity` to
//! [`retail_intensity`] of `base fade x flicker or pulse factor x distance dimmer`, and the pulse's
//! movement into the light's local translation. The colour, the radius and the base fade stay as
//! [`crate::lights::evaluate_light`] resolved them.
//!
//! - **Distance dimmer** (per light unless its reference carries "Never Fades"): with the camera
//!   distance `d` and the light radius `R`, `q = d^2 - R^2`; 1 below `begin^2`, 0 above `end^2`,
//!   linear in `q` between. `begin` and `end` come from the current room template, else the cell's
//!   pair, else `fLightLODStartFade` and `fLightLODStartFade + fLightLODRange`
//!   ([`resolve_light_fade`]).
//! - **Pulse** (`DATA` flags `0x80` or `0x100`): `P += dt x period`, wrapped once at 2 pi;
//!   `fade = base x (1 - I + I cos P)` with `I = amplitude / 2`; the translation is
//!   `movement x (cos P, sin P, sin P cos P)`.
//! - **Flicker** (flag `0x8`): three phases advance at random rates, a noise `n` in `[-0.2, 0.5333]`
//!   is built from table sines, and `fade = base x (1 - I + I n)`.
//! - Flag `0x40` (Flicker Slow) alone gets no modulation.
//! - Only lights within `fFlickeringLightDistance` of the camera advance; the others keep their
//!   last factor.
//! - A modulated fade below 0.05 turns the light off until it rises again.

use crate::{
    config::EngineConfig,
    lights::{LightEvaluation, SkyrimLight, retail_intensity},
    world::{
        components::StreamingCamera,
        environment::interior::{
            LightFade, SceneEnvironment, point_light_fade_factor, resolve_light_fade,
        },
    },
};
use bevy::prelude::*;
use core::f32::consts::TAU;

/// `LIGH` `DATA` flag: Flicker.
pub const LIGHT_FLAG_FLICKER: u32 = 0x0008;
/// `LIGH` `DATA` flag: Flicker Slow. The per-frame code gives it no modulation.
pub const LIGHT_FLAG_FLICKER_SLOW: u32 = 0x0040;
/// `LIGH` `DATA` flags: Pulse (`0x80`) and Pulse Slow (`0x100`), which use one formula.
pub const LIGHT_FLAGS_PULSE: u32 = 0x0180;
/// A modulated fade below this detaches the light until it rises again.
pub const DETACH_FADE: f32 = 0.05;

/// Which modulation a light's flags select.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightMotion {
    None,
    Flicker,
    Pulse,
}

impl LightMotion {
    /// Bit `0x8` selects flicker, else `0x80` or `0x100` select pulse. No shipped record sets
    /// both. `0x40` alone selects nothing.
    pub fn from_flags(flags: u32) -> Self {
        if flags & LIGHT_FLAG_FLICKER != 0 {
            Self::Flicker
        } else if flags & LIGHT_FLAGS_PULSE != 0 {
            Self::Pulse
        } else {
            Self::None
        }
    }
}

/// A sine from a 512-entry table: the argument is truncated to a step of `2 pi / 512`.
pub fn table_sin(angle: f32) -> f32 {
    let index = (angle / TAU * 512.0).floor().rem_euclid(512.0);
    (index * TAU / 512.0).sin()
}

/// The flicker noise `n` of the three phases, clamped to `[-1, 1]` (its range is `[-0.2, 0.5333]`).
pub fn flicker_noise(a: f32, b: f32, c: f32) -> f32 {
    let n = ((table_sin(1.3 * b + 0.65) + 1.0) / 12.0) * (table_sin(1.1 * a + 1.87) + 1.0)
        + 0.2 * table_sin(3.0 * c + 0.9);
    n.clamp(-1.0, 1.0)
}

/// The flicker fade factor: `1 - I + I n` with `I = amplitude / 2`.
pub fn flicker_factor(noise: f32, intensity_amplitude: f32) -> f32 {
    let i = 0.5 * intensity_amplitude;
    1.0 - i + i * noise
}

/// The pulse fade factor: `1 - I + I cos P` with `I = amplitude / 2`. It runs from 1 to
/// `1 - amplitude`.
pub fn pulse_factor(phase: f32, intensity_amplitude: f32) -> f32 {
    let i = 0.5 * intensity_amplitude;
    1.0 - i + i * phase.cos()
}

/// The pulse's local translation in Creation axes: `movement x (cos P, sin P, sin P cos P)`.
pub fn pulse_offset(phase: f32, movement_amplitude: f32) -> Vec3 {
    let (sin, cos) = phase.sin_cos();
    movement_amplitude * Vec3::new(cos, sin, sin * cos)
}

/// Wraps a phase once at 2 pi, as the per-frame code does (one subtraction, not a modulo).
pub fn wrap_once(phase: f32) -> f32 {
    if phase > TAU { phase - TAU } else { phase }
}

/// The distance dimmer of a light `distance_squared` from the camera. `None` fade (a cell with no
/// lighting data) and a light that never fades give 1.
pub fn distance_dimmer(
    distance_squared: f32,
    radius: f32,
    fade: Option<&LightFade>,
    distance_fade: bool,
) -> f32 {
    match fade {
        Some(fade) if distance_fade => {
            point_light_fade_factor(distance_squared - radius * radius, fade)
        }
        _ => 1.0,
    }
}

/// Per-light modulation state and inputs, built from the evaluated record.
#[derive(Component, Debug, Clone)]
pub struct LightDynamics {
    pub motion: LightMotion,
    /// Base fade: FNAM plus the reference's XLIG fade, as [`LightEvaluation::effective_fade`].
    pub base_fade: f32,
    /// `DATA` flicker period, raw.
    pub period: f32,
    /// `DATA` intensity amplitude, raw.
    pub intensity_amplitude: f32,
    /// `DATA` movement amplitude, raw, in Creation units.
    pub movement_amplitude: f32,
    /// The distance dimmer applies (the reference does not carry "Never Fades").
    pub distance_fade: bool,
    /// Phases A, B, C of a flicker, or the pulse phase in slot 0.
    phase: [f32; 3],
    rng: u32,
    factor: f32,
    offset: Vec3,
}

impl LightDynamics {
    /// `seed` makes the random flicker rates repeatable: use the reference's form id.
    pub fn new(evaluation: &LightEvaluation, seed: u32) -> Self {
        let source = &evaluation.source;
        let usable = |value: Option<f32>| value.filter(|value| value.is_finite());
        let (period, intensity, movement) = (
            usable(source.flicker_period),
            usable(source.flicker_intensity_amplitude),
            usable(source.flicker_movement_amplitude),
        );
        // A record without its three floats (an older package) cannot be modulated.
        let motion = match (period, intensity, movement) {
            (Some(_), Some(_), Some(_)) => LightMotion::from_flags(source.flags),
            _ => LightMotion::None,
        };
        Self {
            motion,
            base_fade: evaluation.effective_fade,
            period: period.unwrap_or(0.0),
            intensity_amplitude: intensity.unwrap_or(0.0),
            movement_amplitude: movement.unwrap_or(0.0),
            distance_fade: !source.reference_never_fades,
            // Unverified: the findings do not give the initial phases (NEEDS 25), so every light
            // starts at 0 and the pulsing lights of one cell start in step.
            phase: [0.0; 3],
            rng: seed.wrapping_mul(2_654_435_761) | 1,
            factor: 1.0,
            offset: Vec3::ZERO,
        }
    }

    fn uniform(&mut self, low: f32, high: f32) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        low + (x as f32 / u32::MAX as f32) * (high - low)
    }

    /// Advances the modulation by `dt` seconds. The unit of retail's frame delta is open (NEEDS 20,
    /// asked as NEEDS 25), so Mudcrab uses seconds. Retail draws new flicker rates every update,
    /// so the flicker is smoother at a high frame rate. That is parity, not a defect to tune.
    pub fn advance(&mut self, dt: f32) {
        match self.motion {
            LightMotion::None => {}
            LightMotion::Flicker => {
                for (slot, (low, high)) in [(1.1, 13.1), (1.2, 13.2), (1.3, 19.3)]
                    .into_iter()
                    .enumerate()
                {
                    let rate = self.uniform(low, high);
                    self.phase[slot] = wrap_once(self.phase[slot] + dt * self.period * rate);
                }
                let [a, b, c] = self.phase;
                self.factor = flicker_factor(flicker_noise(a, b, c), self.intensity_amplitude);
                // STAND-IN(#1000): the answer gives the flicker movement as M = 0.5 x movement x
                // s1 x s2 but not the axes it moves along, so a flickering light does not move.
                self.offset = Vec3::ZERO;
            }
            LightMotion::Pulse => {
                self.phase[0] = wrap_once(self.phase[0] + dt * self.period);
                self.factor = pulse_factor(self.phase[0], self.intensity_amplitude);
                self.offset = pulse_offset(self.phase[0], self.movement_amplitude);
            }
        }
    }

    /// The modulated NiLight fade: base times the current factor, 0 below [`DETACH_FADE`] for a
    /// modulated light, never negative.
    pub fn fade(&self) -> f32 {
        let fade = (self.base_fade * self.factor).max(0.0);
        if self.motion != LightMotion::None && fade < DETACH_FADE {
            0.0
        } else {
            fade
        }
    }

    /// The local translation of the light, in Creation axes.
    pub fn offset(&self) -> Vec3 {
        self.offset
    }
}

/// Applies the dimmer, the flicker and the pulse to every spawned light. See the module doc.
pub(crate) fn update_light_dynamics(
    time: Res<Time>,
    config: Option<Res<EngineConfig>>,
    scene: Option<Res<SceneEnvironment>>,
    camera: Query<&GlobalTransform, With<StreamingCamera>>,
    mut lights: Query<
        (
            &GlobalTransform,
            &mut PointLight,
            &mut Transform,
            &mut LightDynamics,
        ),
        With<SkyrimLight>,
    >,
) {
    // STAND-IN(#955): retail measures the flicker gate and the dimmer from the player. Mudcrab
    // uses the streaming camera, so in a free or third-person camera the gate follows the camera.
    let Ok(camera) = camera.single() else {
        return;
    };
    let camera = camera.translation();
    let defaults = EngineConfig::default();
    let config = config.as_deref().unwrap_or(&defaults);
    let (start, range) = (config.light_lod_start_fade, config.light_lod_range);
    // The current room's pair, else the cell's, else the exterior's. A cell without lighting data
    // has no pair: its lights are not dimmed. Unverified: `active_render` is also `None` for an
    // interior in sky mode 3 and for a pack without the lighting table, and those scenes get the
    // exterior pair here. The findings say nothing about them.
    let fade = match scene
        .as_deref()
        .and_then(|scene| scene.active_render(Some(camera)))
    {
        Some(render) => render
            .light_fade
            .map(|pair| resolve_light_fade(Some(pair), start, range)),
        None => Some(resolve_light_fade(None, start, range)),
    };
    let flicker_distance_squared = config.flickering_light_distance.powi(2);
    let dt = time.delta_secs();
    for (global, mut light, mut transform, mut dynamics) in &mut lights {
        let distance_squared = global.translation().distance_squared(camera);
        if distance_squared <= flicker_distance_squared {
            dynamics.advance(dt);
        }
        let dimmer = distance_dimmer(
            distance_squared,
            light.range,
            fade.as_ref(),
            dynamics.distance_fade,
        );
        let intensity = retail_intensity(dynamics.fade() * dimmer);
        if light.intensity != intensity {
            light.intensity = intensity;
        }
        let offset = crate::streaming::creation_to_bevy(dynamics.offset());
        if transform.translation != offset {
            transform.translation = offset;
        }
    }
}

#[cfg(test)]
mod tests;
