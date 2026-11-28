//! Point lights from Skyrim `LIGH` references: the torches, braziers, Dwemer lamps and glowing
//! fungus the game places around its spaces. `docs/specs/engine/point-lights.md` is the norm this
//! module implements.
//!
//! `streaming::spawn_cell` gives every reference whose base record has a row in the converter's
//! `lights` table a [`PointLight`] child, unless the engine is run with `--no-lights`. The
//! light entity is a descendant of the cell root, so it follows the render-origin rebases and the
//! cell unloads that move the cell hierarchy, and it is despawned with the cell that placed it.
//!
//! This module owns the record-to-light conversion, the intensity scale, and the budget that bounds
//! how many lights are enabled at once.
//!
//! Authored inputs and the Bevy transform are separate. Signed XRDS is added to the base radius;
//! FNAM plus XLIG fade scales the light's colour. DATA falloff applies only to native shadow
//! modes, which this point-light adapter does not reproduce. See the spec for pinned sources.
//!
//! A light is shaded with retail's equations (registry rows `RF-LIGHT-GPU-004` and
//! `RF-LIGHT-GPU-008`): its contribution is `colour x fade x (1 - saturate(d / radius)^2)`, with no
//! `1 / d^2`, no exposure and no extra multiplier. The vendored `bevy_pbr` carries the falloff
//! (`RetailPointFalloff`, see `vendor/BEVY_PATCHES.md`); [`retail_intensity`] converts the fade
//! into Bevy's lumens with the one scene scale that the sun and the ambient light already share.
//! The renderer still uses unshadowed point lights and sRGB-decoded colours. The 64-light budget
//! remains backend policy.
//!
//! The budget enables the [`ENABLED_LIGHT_BUDGET`] emitting lights that rank first (wider lights
//! before tiny ones, then nearest the camera) and hides the rest. Shadows are a smaller set than
//! the budget: only the [`MAX_SHADOW_CASTING_POINT_LIGHTS`] nearest of the enabled lights whose
//! record carries a shadow flag (`DATA` flags `0x1c00`, `RF-LIGHT-GPU-009`) get a cube shadow map,
//! and only when the Point-light shadows setting is on. See that constant for the retail rule and
//! for the Metal limit the cap keeps the frame under. The shadow set is not cached with the
//! budget: it is re-ranked among the enabled lights whenever the camera has moved. A light spawns
//! unshadowed, so an unflagged light also lights the far side of the wall it is mounted on, as in
//! retail.

use crate::{
    retail_lighting::LightUnits,
    world::{components::StreamingCamera, database::LightRow},
};
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

/// `LIGH` `DATA` flag bit: the record is off until something turns it on, so a reference to it is
/// not lit by default (UESP, "Skyrim Mod:Mod File Format/LIGH").
///
/// The bit's meaning is UESP's, not measured: no `LIGH` record in the 506 of the shipped plugins
/// sets it (an unmodded install was scanned), so skipping these can never take a light out of the
/// game's own world.
pub const LIGHT_FLAG_OFF_BY_DEFAULT: u32 = 0x0000_0020;

/// `LIGH` `DATA` flag bit: the light removes light instead of adding it (UESP). Skyrim uses these
/// to darken a room; this engine has no way to subtract a clustered light, so they are skipped.
///
/// Like [`LIGHT_FLAG_OFF_BY_DEFAULT`], no shipped record sets it.
pub const LIGHT_FLAG_NEGATIVE: u32 = 0x0000_0004;

/// Global environment policy, independent of the local-light compatibility transform.
pub const AMBIENT_ILLUMINANCE: f32 = SUN_ILLUMINANCE / (core::f32::consts::PI * SUNLIGHT_SCALE);

/// The sun's illuminance in the `mudcrab` world path, in lux. With that profile's EV100 of 9.7
/// this puts a white surface facing the sun at about 3.8 in linear radiance before tone mapping.
/// The `skyrim` profile does not use it: its lights are in retail units (`retail_lighting.rs`,
/// [near-lighting.md](../../../docs/specs/engine/near-lighting.md)).
pub const SUN_ILLUMINANCE: f32 = 12_000.0;

/// How much brighter than the ambient cube Skyrim's `SunlightScale` makes the sun's colour
/// (the engine's lighting constant, 2.8; see `docs/research/skyrim-lighting-regression-recovery-
/// 20261010.md`, "SunlightScale remains 2.8").
pub const SUNLIGHT_SCALE: f32 = 2.8;

/// Backend transform identity, retained on every evaluated light reference.
pub const LIGHT_TRANSFORM: &str = "bevy-point-retail-falloff-v1";

/// Spawn-time diagnostics. Raw inputs stay on the reference, including unsupported fields.
#[derive(Component, Debug, Clone)]
pub struct LightEvaluation {
    pub source: LightRow,
    pub radius_offset: Option<f32>,
    pub authored_radius: Option<f64>,
    pub backend_range: Option<f32>,
    /// Base FNAM after legacy/malformed fallback, before the additive reference offset.
    pub base_fade: f32,
    /// Native base-plus-reference fade before the backend's nonnegative finite bound.
    pub authored_fade: f64,
    pub effective_fade: f32,
    pub reference_fade_applied: bool,
    pub invalid_reference_fade: bool,
    pub native_radius_fallback: bool,
    /// Native constructor uses DATA falloff only for flags 0x1c00, otherwise 1.
    pub native_falloff: Option<f32>,
    pub legacy_fade: bool,
    pub invalid_fade: bool,
    pub invalid_radius: bool,
    pub backend_limited: bool,
    /// An FNAM or XLIG fade equal to the `f32::MAX` sentinel meant "no fade" and was not applied.
    pub fade_sentinel: bool,
    /// A finite XRDS offset above [`MAX_RADIUS_OVERRIDE`] was ignored and the base radius kept.
    pub radius_offset_ignored: bool,
    pub unsupported_reference_data: bool,
    pub malformed_reference_data: bool,
    pub unsupported_falloff: bool,
    pub suppressed_flags: bool,
    pub transform: &'static str,
}

impl LightEvaluation {
    /// Whether the record's `DATA` flags select a shadow mode (spot `0x400`, hemisphere `0x800` or
    /// omni `0x1000`). Retail shadows no other light (`RF-LIGHT-GPU-009`).
    pub fn shadow_flagged(&self) -> bool {
        self.source.flags & LIGHT_FLAG_SHADOW_MODES != 0
    }

    /// The `mudcrab` conversion.
    pub fn point_light(&self) -> Option<PointLight> {
        if self.suppressed_flags {
            return None;
        }
        Some(PointLight {
            color: Color::srgb_u8(
                self.source.color[0],
                self.source.color[1],
                self.source.color[2],
            ),
            intensity: retail_intensity(self.effective_fade),
            range: self.backend_range?,
            shadow_maps_enabled: false,
            ..default()
        })
    }

    /// The actual streaming consumer uses this conversion for both immediate and deferred lights.
    ///
    /// Under [`LightUnits::Retail`] (SPEC-1 section 2.6) the colour is the record bytes / 255 with
    /// no sRGB decode, and the product `colour x fade` has no clamp: a Negative light (`DATA` flag
    /// `0x4`) or a negative fade gives a negative colour. `emittance` is the colour of the
    /// reference's `XEMI` source: it multiplies the record colour, and the Negative flag is then
    /// not applied again.
    ///
    /// The caller passes the source's colour for the game hour (`crate::emittance`), as its
    /// bytes / 255 with no rounding.
    ///
    /// STAND-IN(#362): only a reference with `XEMI` takes a source; retail also lists a light
    /// whose model has an external-emittance property (inferred from one run).
    pub fn point_light_in(&self, units: LightUnits, emittance: Option<Vec3>) -> Option<PointLight> {
        if !units.is_retail() {
            return self.point_light();
        }
        if self.source.flags & LIGHT_FLAG_OFF_BY_DEFAULT != 0 {
            return None;
        }
        let record = Vec3::from_array(self.source.color.map(f32::from)) / 255.0;
        let (colour, sign) = match emittance {
            Some(source) => (record * source, 1.0),
            None if self.source.flags & LIGHT_FLAG_NEGATIVE != 0 => (record, -1.0),
            None => (record, 1.0),
        };
        let colour = colour * sign * self.retail_fade().signum();
        Some(PointLight {
            color: Color::linear_rgb(colour.x, colour.y, colour.z),
            intensity: units.point_intensity(self.retail_fade().abs()),
            range: self.retail_radius()?,
            shadow_maps_enabled: false,
            ..default()
        })
    }

    /// The fade the retail path scales the colour by: `FNAM` plus the reference offset, with no
    /// clamp (SPEC-1 section 2.6, [near-lighting.md](../../../docs/specs/engine/near-lighting.md)).
    /// A negative `FNAM` stays, and the largest float in either value gives the largest float.
    ///
    /// STAND-IN(#362): the extra flag byte that stores fade 0 has no known file field.
    pub fn retail_fade(&self) -> f32 {
        let base = self
            .source
            .fade
            .filter(|fade| fade.is_finite() && self.source.fade_status != Some(2))
            // STAND-IN(#362): a record without a readable fade keeps the unit fade of
            // `evaluate_light`.
            .unwrap_or(1.0);
        let offset = self
            .source
            .reference_data
            .as_ref()
            .filter(|data| [16, 20].contains(&data.data_size))
            .and_then(|data| data.fade_offset)
            .filter(|offset| offset.is_finite())
            // retail-default: SPEC-1 section 2.6, a reference without the fade extra adds nothing.
            .unwrap_or(0.0);
        if base == f32::MAX || offset == f32::MAX {
            return f32::MAX;
        }
        (base + offset).clamp(f32::MIN, f32::MAX)
    }

    /// The radius of the retail path: the record radius plus the `XRDS` offset, `0.1` when the sum
    /// is not above 0, with no upper bound (SPEC-1 section 2.6). `None` for an unreadable input.
    pub fn retail_radius(&self) -> Option<f32> {
        // retail-default: SPEC-1 section 2.6, a reference without `XRDS` adds nothing.
        let offset = self.radius_offset.unwrap_or(0.0);
        if self.source.radius_status == Some(2)
            || !self.source.radius.is_finite()
            || !offset.is_finite()
        {
            return None;
        }
        let radius = (self.source.radius + offset).min(f32::MAX);
        Some(if radius <= 0.0 {
            NATIVE_NONPOSITIVE_RADIUS
        } else {
            radius
        })
    }
}

/// Resolve supported inputs without modifying their stored values. Retail adds the reference's
/// extra data to the record: radius is DATA plus XRDS, and fade is FNAM plus XLIG (registry row
/// `RF-LIGHT-GPU-007`; colour times fade is `RF-LIGHT-GPU-008`). XRDS is an additive signed
/// offset (xEdit wbREFRRadiusToStr). Invalid present offsets are rejected, not replaced by base
/// radius. The native constructor replaces nonpositive sums with 0.1. XLIG fade is an additive
/// offset. An `f32::MAX` fade, in FNAM or XLIG, is the native "no fade" sentinel: this backend
/// cannot emit "unbounded power", so the sentinel is not applied and the light keeps the unit
/// (or authored base) fade instead of becoming a maximum-intensity light.
///
/// A finite XRDS offset above [`MAX_RADIUS_OVERRIDE`] is an outlier. The registered pack holds ten
/// such references: nine in retail data (Skyrim.esm and Dragonborn.esm, 15,967 up to 3,736,737
/// units) and one in a Creation Club plugin (9,505.58, ccbgssse031-advcyrus.esm). The pre-additive code ignored it and kept the record radius; that
/// is preserved so those lights do not jump to the range cap. Native 1.7.104 evidence is pinned in
/// [point-lights.md](../../../docs/specs/engine/point-lights.md).
pub fn evaluate_light(light: &LightRow, radius_offset: Option<f32>) -> LightEvaluation {
    let radius_offset_ignored =
        radius_offset.is_some_and(|offset| offset.is_finite() && offset > MAX_RADIUS_OVERRIDE);
    let applied_offset = if radius_offset_ignored {
        None
    } else {
        radius_offset
    };
    let authored_radius = (light.radius_status != Some(2)
        && light.radius.is_finite()
        && applied_offset.is_none_or(f32::is_finite))
    .then(|| f64::from(light.radius) + f64::from(applied_offset.unwrap_or(0.0)));
    let native_radius_fallback = authored_radius.is_some_and(|radius| radius <= 0.0);
    let backend_range = authored_radius.map(|radius| {
        if radius <= 0.0 {
            NATIVE_NONPOSITIVE_RADIUS
        } else {
            radius.min(f64::from(MAX_RADIUS_OVERRIDE)) as f32
        }
    });
    let base_fade_sentinel = light.fade == Some(f32::MAX);
    let invalid_fade = light.fade_status == Some(2)
        || light
            .fade
            .is_some_and(|fade| !fade.is_finite() || fade < 0.0);
    let base_fade = if invalid_fade || base_fade_sentinel {
        1.0
    } else {
        light
            .fade
            .filter(|fade| fade.is_finite() && *fade >= 0.0)
            .unwrap_or(1.0)
    };
    let reference_fade_raw = light.reference_data.as_ref().and_then(|data| {
        [16, 20]
            .contains(&data.data_size)
            .then_some(data.fade_offset)
            .flatten()
            .filter(|fade| fade.is_finite())
    });
    let reference_fade_sentinel = reference_fade_raw == Some(f32::MAX);
    let reference_fade = reference_fade_raw.filter(|fade| *fade != f32::MAX);
    let invalid_reference_fade = light.reference_data.is_some() && reference_fade_raw.is_none();
    let authored_fade = match reference_fade {
        Some(offset) => f64::from(base_fade) + f64::from(offset),
        None => f64::from(base_fade),
    };
    // Keep the authored signed result. Bevy's additive light backend cannot emit negative power.
    let effective_fade = authored_fade.clamp(0.0, f64::from(f32::MAX)) as f32;
    let shadow_mode = light.flags & LIGHT_FLAG_SHADOW_MODES != 0;
    let native_falloff = if shadow_mode {
        light.falloff.filter(|value| value.is_finite())
    } else {
        Some(1.0)
    };
    let malformed_reference_data = light.reference_data.as_ref().is_some_and(|data| {
        ![16, 20].contains(&data.data_size)
            || [
                data.fov_offset,
                data.fade_offset,
                data.end_distance_cap,
                data.shadow_depth_bias,
            ]
            .iter()
            .any(|value| value.is_none_or(|v| !v.is_finite()))
    });
    LightEvaluation {
        source: light.clone(),
        radius_offset,
        authored_radius,
        backend_range,
        base_fade,
        authored_fade,
        effective_fade,
        reference_fade_applied: reference_fade.is_some(),
        invalid_reference_fade,
        native_radius_fallback,
        native_falloff,
        legacy_fade: light.fade.is_none() || invalid_fade || base_fade_sentinel,
        invalid_fade,
        invalid_radius: authored_radius.is_none(),
        backend_limited: authored_fade < 0.0
            || authored_fade > f64::from(f32::MAX)
            || radius_offset_ignored
            || authored_radius.is_some_and(|radius| radius > f64::from(MAX_RADIUS_OVERRIDE))
            || retail_intensity_f64(effective_fade) > f64::from(f32::MAX),
        fade_sentinel: base_fade_sentinel || reference_fade_sentinel,
        radius_offset_ignored,
        // Fade is consumed. Other nonzero/invalid fields still require their own native path.
        unsupported_reference_data: light.reference_data.as_ref().is_some_and(|data| {
            [
                data.fov_offset,
                data.end_distance_cap,
                data.shadow_depth_bias,
            ]
            .iter()
            .any(|value| value.is_none_or(|value| value != 0.0))
        }),
        malformed_reference_data,
        unsupported_falloff: shadow_mode,
        suppressed_flags: light.flags & (LIGHT_FLAG_NEGATIVE | LIGHT_FLAG_OFF_BY_DEFAULT) != 0,
        transform: LIGHT_TRANSFORM,
    }
}

/// Cumulative spawn-time coverage, including references with no emitted PointLight.
/// Counts are dispositions, not mutually exclusive; unload/reload counts a new evaluation.
#[derive(Resource, Default, Debug)]
pub struct LightInputCoverage {
    pub evaluated: u64,
    pub legacy_fade: u64,
    pub reference_fade_applied: u64,
    pub invalid_reference_fade: u64,
    pub native_radius_fallback: u64,
    pub invalid_fade: u64,
    pub invalid_radius: u64,
    pub backend_limited: u64,
    pub unsupported_reference_data: u64,
    pub malformed_reference_data: u64,
    pub unsupported_falloff: u64,
    pub suppressed_flags: u64,
}

fn record_light_coverage(
    lights: Query<&LightEvaluation, Added<LightEvaluation>>,
    mut coverage: ResMut<LightInputCoverage>,
) {
    for light in &lights {
        coverage.evaluated += 1;
        coverage.legacy_fade += u64::from(light.legacy_fade);
        coverage.reference_fade_applied += u64::from(light.reference_fade_applied);
        coverage.invalid_reference_fade += u64::from(light.invalid_reference_fade);
        coverage.native_radius_fallback += u64::from(light.native_radius_fallback);
        coverage.invalid_fade += u64::from(light.invalid_fade);
        coverage.invalid_radius += u64::from(light.invalid_radius);
        coverage.backend_limited += u64::from(light.backend_limited);
        coverage.unsupported_reference_data += u64::from(light.unsupported_reference_data);
        coverage.malformed_reference_data += u64::from(light.malformed_reference_data);
        coverage.unsupported_falloff += u64::from(light.unsupported_falloff);
        coverage.suppressed_flags += u64::from(light.suppressed_flags);
    }
    if !lights.is_empty() {
        debug!(?coverage, "local light input coverage (cumulative)");
    }
}

// STAND-IN(#999): retail gives one geometry at most 7 point lights (after the sun and up to 4
// shadow lights), cut in the order of the geometry's own light list, not by distance. Bevy's
// clustered forward lights loop over the lights of a cluster, not of a mesh, and the cluster
// order is not attach order, so a per-fragment cap of 7 would cut arbitrarily and flicker as the
// camera moves. Mudcrab keeps the nearest lights instead, so it uses more lights per geometry
// than retail. Source: the NEEDS 20 findings,
// [skyrim_decomp PR 1073](https://github.com/Mudcrab-Team/skyrim_decomp/pull/1073).

/// How many of the spawned lights are enabled at once. Bevy's clustered forward renderer draws
/// every enabled light in a cluster it reaches, and the game's spaces hold whole halls of `LIGH`
/// references, so the far ones are switched off rather than paid for.
pub const ENABLED_LIGHT_BUDGET: usize = 64;

/// The most point lights that cast shadows at once: the first 4, which is retail's limit. Only
/// lights whose record carries a shadow flag (`DATA` flags `0x1c00`, [`SkyrimLight::shadow_flagged`])
/// can take one.
///
/// Retail's lighting pixel shader shadows "the first min(NumShadowLights, 4)" of the point lights
/// it draws (registry row `RF-LIGHT-GPU-004`, traced from the shader bytecode, confidence high).
/// The row gives the limit and not the order of the "first", so the engine takes the nearest to
/// the camera, which is the order of the budget. Retail also shadows only a light whose record
/// carries a shadow flag (`RF-LIGHT-GPU-009`), and this engine ranks only such lights.
///
/// The cap is also what keeps a frame under a hard GPU limit. wgpu's Metal backend refuses to open
/// a new command encoder once 4096 are outstanding and reports the device as lost
/// ([`METAL_OUTSTANDING_COMMAND_BUFFER_LIMIT`]). Bevy renders every cube face of a shadowing point
/// light as its own view, and every view opens several encoders (see
/// [`ENCODERS_PER_POINT_SHADOW_VIEW`]). By that estimate a 64-light budget with shadows on opens
/// 1,920 encoders per frame, and the encoders of earlier frames still count while the GPU holds
/// them, so entering a 71-light cell lost the device. With this cap one frame opens
/// [`shadow_encoder_estimate`]`(4)` = 120.
pub const MAX_SHADOW_CASTING_POINT_LIGHTS: usize = 4;

/// The number of views Bevy renders for one shadow-casting point light: one per cube face
/// (`CUBE_MAP_FACES`, which `bevy_pbr` 0.19.0 renders one view for each).
pub const POINT_SHADOW_VIEWS_PER_LIGHT: usize = bevy::camera::primitives::CUBE_MAP_FACES.len();

/// The command encoders Bevy 0.19 opens for one point-light shadow view in one frame.
///
/// Each cube face is a root view that runs the whole `Core3d` schedule, and every render system
/// that takes a `RenderContext` and records a command opens its own encoder. For a shadow view
/// that is five systems of `bevy_pbr` 0.19.0: `clear_indirect_parameters_metadata`,
/// `unpack_bins`, `early_gpu_preprocess`, `early_prepass_build_indirect_parameters` (all in
/// `render/gpu_preprocess.rs`) and `shared_shadow_pass` (`render/light.rs`). The count is read
/// from the code, not measured: an estimate used by the guard, which is why the guard keeps a wide
/// margin ([`SHADOW_ENCODER_WARN_ESTIMATE`]).
pub const ENCODERS_PER_POINT_SHADOW_VIEW: usize = 5;

/// The limit on outstanding command buffers in wgpu-hal's Metal backend (`wgpu_hal::metal`): the
/// 4097th is refused with "refusing to create new command buffer; 4097 outstanding command
/// buffers exceeds the limit of 4096. Treating this as device lost."
pub const METAL_OUTSTANDING_COMMAND_BUFFER_LIMIT: usize = 4096;

/// The per-frame encoder estimate at which [`warn_on_shadow_encoder_load`] logs an error: a quarter
/// of the Metal limit. The margin is wide on purpose: encoders from earlier frames stay outstanding
/// until the GPU finishes them, and [`ENCODERS_PER_POINT_SHADOW_VIEW`] is an estimate. The 64-light
/// load that lost the device (1,920) is above it; the cap (120) is far below it.
pub const SHADOW_ENCODER_WARN_ESTIMATE: usize = METAL_OUTSTANDING_COMMAND_BUFFER_LIMIT / 4;

/// The command encoders one frame opens for `lights` shadow-casting point lights, as estimated
/// from the Bevy code (see [`ENCODERS_PER_POINT_SHADOW_VIEW`]). It leaves out the camera's own
/// passes and the sun's cascades, which are a handful of encoders in total.
pub const fn shadow_encoder_estimate(lights: usize) -> usize {
    lights * POINT_SHADOW_VIEWS_PER_LIGHT * ENCODERS_PER_POINT_SHADOW_VIEW
}

// The cap must stay far below the warning level: a build that raises it into the danger zone
// fails here instead of losing the device in a 71-light cell.
const _: () = assert!(
    shadow_encoder_estimate(MAX_SHADOW_CASTING_POINT_LIGHTS) < SHADOW_ENCODER_WARN_ESTIMATE,
    "the shadow-casting light cap puts a frame too close to the Metal command buffer limit"
);

/// How far the camera moves before the enabled lights are chosen again. Re-choosing is a sort of
/// every spawned light, so it is not done per frame.
pub const BUDGET_RECHOOSE_DISTANCE: f32 = 256.0;

/// Convenience entrypoint; streaming also retains the evaluation on the reference entity.
pub fn point_light(light: &LightRow, radius_offset: Option<f32>) -> Option<PointLight> {
    evaluate_light(light, radius_offset).point_light()
}

/// Maximum backend range for a base radius or an additive XRDS result. An in-range sum is clamped
/// to this cap and its authored value kept on `LightEvaluation`. A finite XRDS offset above the cap
/// is a retail outlier and is ignored, so the light keeps its base radius (see `evaluate_light`).
pub const MAX_RADIUS_OVERRIDE: f32 = 8_192.0;

/// Native 1.7.104 CreatePointLight, RVA 0x2746d2; not an editor hint or arbitrary range knob.
pub const NATIVE_NONPOSITIVE_RADIUS: f32 = 0.1;
/// Native constructor dispatches DATA falloff only for spot/hemi/omni shadow modes.
pub const LIGHT_FLAG_SHADOW_MODES: u32 = 0x1c00;

/// Bevy lumens for a light whose retail colour scale is `fade`, shaded with the retail falloff.
/// Source: `RF-LIGHT-GPU-008` in [point-lights.md](../../../docs/specs/engine/point-lights.md) section 4.
///
/// Retail adds `albedo x colour x fade x att` per light (`RF-LIGHT-GPU-004`, `RF-LIGHT-GPU-008`),
/// with no `pi` and no exposure. The scene shades one retail unit of light as
/// [`AMBIENT_ILLUMINANCE`] nits: the interior ambient light is that brightness times the DALC mean,
/// and the key light is `pi x AMBIENT_ILLUMINANCE x SunlightScale` lux (a Lambert surface turns
/// `E` lux into `E / pi` nits). A point light with the retail falloff must use the same scale, or
/// its ratio to the ambient and to the sun is wrong. Bevy takes `intensity / (4 pi)` as the light's
/// luminous intensity and the diffuse BRDF supplies `1 / pi`, so the lumens that give
/// `albedo x colour x fade x AMBIENT_ILLUMINANCE` at the light's centre are
/// `4 pi^2 x AMBIENT_ILLUMINANCE x fade`. The result does not depend on the radius: the radius
/// only sets where the falloff reaches zero.
///
/// `AMBIENT_ILLUMINANCE` is Mudcrab's own scene scale (12,000 lux / (pi x 2.8)), not a retail
/// number, because retail shades display-encoded values with no lux. It keeps the scalar ratios
/// between point, ambient and sun light retail's, so that no light is tuned by eye. Per-channel
/// ratios differ for saturated colours, because all three colours are sRGB-decoded here and retail
/// uses the raw bytes (#706).
pub fn retail_intensity(fade: f32) -> f32 {
    retail_intensity_f64(fade).min(f64::from(f32::MAX)) as f32
}

fn retail_intensity_f64(fade: f32) -> f64 {
    if !fade.is_finite() || fade <= 0.0 {
        return 0.0;
    }
    4.0 * core::f64::consts::PI
        * core::f64::consts::PI
        * f64::from(AMBIENT_ILLUMINANCE)
        * f64::from(fade)
}

impl LightEvaluation {
    /// The distance dimmer, flicker and pulse state of this light (see [`crate::light_dynamics`]).
    /// `seed` is the reference's form id.
    pub fn dynamics(&self, seed: u32) -> crate::light_dynamics::LightDynamics {
        crate::light_dynamics::LightDynamics::new(self, seed)
    }

    /// [`Self::dynamics`] for the light [`Self::point_light_in`] builds in `units`.
    pub fn dynamics_in(
        &self,
        units: LightUnits,
        seed: u32,
    ) -> crate::light_dynamics::LightDynamics {
        match units {
            LightUnits::Mudcrab => self.dynamics(seed),
            LightUnits::Retail => crate::light_dynamics::LightDynamics::retail(self, seed),
        }
    }
}

/// A [`PointLight`] that came from a Skyrim `LIGH` reference.
///
/// The budget and the tests tell the game's lights from the engine's own by this marker: a
/// `PointLight` without it is never enabled or budgeted by [`budget_lights`], which keeps the
/// budget from touching a light a fixture or a future engine feature adds.
///
/// Every `SkyrimLight` also carries Bevy's `RetailPointFalloff`, so the renderer shades it with
/// retail's attenuation `1 - saturate(d / range)^2` (see [`retail_intensity`]).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
#[require(bevy::pbr::RetailPointFalloff)]
pub struct SkyrimLight {
    /// The reference that carries the light (`REFR` form id).
    pub form_id: u32,
    /// The cell that reference belongs to.
    pub cell_id: u32,
    /// The record carries a shadow flag (`DATA` flags `0x1c00`). Retail gives a shadow light object
    /// only to such a light (`RF-LIGHT-GPU-009`, `RF-LIGHT-GPU-015`), so only these compete for
    /// the shadow slots. Source: `RF-LIGHT-GPU-009` in [point-lights.md](../../../docs/specs/engine/point-lights.md) section 6.
    pub shadow_flagged: bool,
}

/// The lights the budget chose last, and where it chose them.
#[derive(Resource, Default)]
struct LightBudget {
    /// The camera position the current selection was made at; `None` before the first frame.
    chosen_at: Option<Vec3>,
    /// Every spawned light when the current selection was made - the whole set the ranking ran
    /// over, not only the [`ENABLED_LIGHT_BUDGET`] it enabled.
    ///
    /// The selection is re-made whenever that set changes, even while the camera stands still and
    /// even when the number of lights in the world does not: a light spawning or despawning, and a
    /// cell streaming in or out, both move lights in or out of it. A cache keyed on the *count*
    /// alone kept a room walked into and stopped in dark until the player moved another
    /// [`BUDGET_RECHOOSE_DISTANCE`].
    chosen: HashMap<Entity, LightBudgetClass>,
    /// The lights the current selection enabled, nearest first as of `chosen_at`. The shadow set
    /// is ranked among these.
    enabled: Vec<Entity>,
    /// The camera position the shadow set was last ranked at; `None` forces a new ranking.
    shadow_at: Option<Vec3>,
    /// The lights that currently have a cube shadow map, nearest first.
    shadowed: Vec<Entity>,
    /// Reused by the shadow ranking, so a camera that moves every frame allocates nothing.
    shadow_scratch: Vec<(Entity, f32)>,
}

/// Backend budget policy only. Keep native fallback ranges and authored zero power intact,
/// but do not let them displace wider emitting lights.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LightBudgetClass {
    Regular,
    Tiny,
    Dark,
}

impl LightBudgetClass {
    fn of(light: &PointLight) -> Self {
        if !light.intensity.is_finite()
            || light.intensity <= 0.0
            || !light.range.is_finite()
            || light.range <= 0.0
        {
            Self::Dark
        } else if light.range <= NATIVE_NONPOSITIVE_RADIUS {
            Self::Tiny
        } else {
            Self::Regular
        }
    }
}

/// Applies the configured light budget, preferring wider emitters before tiny ones and excluding
/// zero-power lights. The default limit is [`ENABLED_LIGHT_BUDGET`].
///
/// Add it after [`StreamingPlugin`](crate::streaming::StreamingPlugin).
pub struct LightsPlugin;

/// Converted light data retained on an unlit reference, so enabling lights works in resident cells.
#[derive(Component)]
pub(crate) struct DeferredPointLight {
    pub light: PointLight,
    pub marker: SkyrimLight,
    pub dynamics: crate::light_dynamics::LightDynamics,
}

fn activate_deferred_lights(
    config: Option<Res<crate::config::EngineConfig>>,
    references: Query<(Entity, &DeferredPointLight, Option<&Visibility>)>,
    mut commands: Commands,
) {
    if !config.is_some_and(|config| config.lights) {
        return;
    }
    for (entity, deferred, visibility) in &references {
        let mut reference = commands.entity(entity);
        if visibility.is_none() {
            reference.insert(Visibility::Inherited);
        }
        reference.remove::<DeferredPointLight>();
        reference.with_child((
            Name::new(format!("Light {:08X}", deferred.marker.form_id)),
            deferred.light,
            deferred.marker,
            deferred.dynamics.clone(),
            crate::render::light_render_layers(),
        ));
    }
}

impl Plugin for LightsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LightBudget>()
            .init_resource::<LightInputCoverage>()
            .add_systems(Update, record_light_coverage)
            .add_systems(
                Update,
                activate_deferred_lights.after(crate::settings_ui::SettingsUiSet),
            )
            .add_systems(
                Update,
                crate::light_dynamics::update_light_dynamics.after(activate_deferred_lights),
            )
            .add_systems(
                PostUpdate,
                (
                    budget_lights.after(TransformSystems::Propagate),
                    warn_on_shadow_encoder_load.after(budget_lights),
                ),
            );
    }
}

/// Logs an error when the point lights that cast shadows would open enough command encoders in a
/// frame to approach the Metal limit ([`SHADOW_ENCODER_WARN_ESTIMATE`]), and says what to do.
///
/// [`budget_lights`] keeps its own lights under [`MAX_SHADOW_CASTING_POINT_LIGHTS`], so this fires
/// only for a shadow-casting `PointLight` that something else adds, or for a raised cap. It counts
/// every visible `PointLight` and no `SpotLight`. Without it, the
/// only trace of this failure is a wgpu warning that the engine's default log filter hides, then
/// a lost device. It logs once for each change of the count, not every frame.
fn warn_on_shadow_encoder_load(
    lights: Query<(&PointLight, &InheritedVisibility)>,
    mut reported: Local<usize>,
) {
    let casting = lights
        .iter()
        .filter(|(light, visibility)| light.shadow_maps_enabled && visibility.get())
        .count();
    let estimate = shadow_encoder_estimate(casting);
    if estimate < SHADOW_ENCODER_WARN_ESTIMATE {
        *reported = 0;
        return;
    }
    if *reported == casting {
        return;
    }
    *reported = casting;
    error!(
        casting,
        estimated_encoders_per_frame = estimate,
        limit = METAL_OUTSTANDING_COMMAND_BUFFER_LIMIT,
        cap = MAX_SHADOW_CASTING_POINT_LIGHTS,
        "lights: so many shadow-casting point lights that the Metal command buffer limit may be \
         reached and the GPU device lost; the Point-light shadows setting controls only Skyrim \
         lights, so also disable or reduce other shadow-casting lights (spot lights, and lights \
         owned by other systems)"
    );
}

/// Enables emitting lights up to the configured budget and hides the rest. Wider lights precede
/// tiny lights; camera distance orders each class. With the Point-light shadows setting on, the
/// [`MAX_SHADOW_CASTING_POINT_LIGHTS`] nearest of the enabled lights whose record carries a shadow
/// flag also get cube shadow maps and every other light has them off. The enabled set is cached until the camera has moved
/// [`BUDGET_RECHOOSE_DISTANCE`] or the set of lights or their budget classes change; the shadow set
/// is re-ranked among the enabled lights after every camera move.
///
/// Hidden is the switch: the lights are extracted to the render world only while they are visible
/// (`bevy_pbr/src/render/light.rs`), and a light set back to `Visibility::Inherited` still goes
/// dark whenever an ancestor is hidden. A light under a reference whose model failed validation is
/// hidden by that ancestor, not by this system.
///
/// This system owns the `Visibility` of a [`SkyrimLight`]. A light that another system hides and
/// this one re-enables - `streaming::hide_partial_scene`, which hides the descendants of a
/// reference whose model failed strict validation - comes back on: the budget cannot tell that
/// hiding from its own, and a light next to a missing model is not wrong. Hiding an *ancestor*
/// needs no such care, because `Visibility::Inherited` defers to it.
fn budget_lights(
    mut budget: ResMut<LightBudget>,
    camera: Query<&GlobalTransform, With<StreamingCamera>>,
    mut lights: Query<
        (Entity, &GlobalTransform, &mut PointLight, &mut Visibility),
        With<SkyrimLight>,
    >,
    markers: Query<&SkyrimLight>,
    config: Option<Res<crate::config::EngineConfig>>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
) {
    if config.is_some_and(|config| !config.lights) {
        for (_, _, _, mut visibility) in &mut lights {
            visibility.set_if_neq(Visibility::Hidden);
        }
        budget.chosen_at = None;
        return;
    }
    if settings
        .as_ref()
        .is_some_and(|settings| settings.is_changed())
    {
        budget.chosen_at = None;
    }
    let Ok(camera) = camera.single() else {
        return;
    };
    let camera_position = camera.translation();
    // One pass over every spawned light, allocating nothing: count them and detect changed
    // membership or budget class. A light that starts emitting must be reconsidered even when
    // the entity set and camera do not change.
    //
    // The second half is what the count alone cannot answer. A swap that keeps the number of lights
    // in the world the same is invisible to a count - a cell unloading as another streams in with
    // as many lights - and the player standing still is the case the budget exists for: a room
    // walked into and stopped in has to light up.
    let mut spawned = 0usize;
    let mut joined = false;
    for (entity, _, light, _) in lights.iter() {
        spawned += 1;
        joined |= budget.chosen.get(&entity) != Some(&LightBudgetClass::of(light));
    }
    let budget_size = settings
        .as_deref()
        .map_or(ENABLED_LIGHT_BUDGET, |settings| settings.point_light_budget);
    let shadow_count = shadow_casting_count(
        budget_size,
        settings
            .as_deref()
            .is_some_and(|settings| settings.point_light_shadows),
    );
    let rechoose = joined
        || budget.chosen.len() != spawned
        || budget
            .chosen_at
            .is_none_or(|chosen_at| chosen_at.distance(camera_position) > BUDGET_RECHOOSE_DISTANCE);

    let mut nearest = None;
    if rechoose {
        let mut ranked: Vec<(Entity, LightBudgetClass, f32)> = lights
            .iter()
            .map(|(entity, transform, light, _)| {
                (
                    entity,
                    LightBudgetClass::of(light),
                    transform.translation().distance_squared(camera_position),
                )
            })
            .collect();
        sort_for_budget(&mut ranked);
        budget.enabled.clear();
        budget.enabled.extend(
            ranked
                .iter()
                .filter(|(_, class, _)| *class != LightBudgetClass::Dark)
                .take(budget_size)
                .map(|(entity, _, _)| *entity),
        );
        let enabled: HashSet<Entity> = budget.enabled.iter().copied().collect();
        for (entity, _, _, mut visibility) in &mut lights {
            let wanted = if enabled.contains(&entity) {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
            if *visibility != wanted {
                *visibility = wanted;
            }
        }
        budget.chosen_at = Some(camera_position);
        budget.chosen = ranked
            .iter()
            .map(|(entity, class, _)| (*entity, *class))
            .collect();
        // A new set of enabled lights has a new set of shadows.
        budget.shadow_at = None;
        nearest = ranked
            .iter()
            .find(|(_, class, _)| *class != LightBudgetClass::Dark)
            .map(|(entity, _, distance_squared)| (*entity, *distance_squared));
    }

    // The shadow set follows the camera on its own: it is the nearest few of the enabled lights
    // as of now, not as of the last re-choose. The budget's cache lets the camera move
    // `BUDGET_RECHOOSE_DISTANCE` before it re-ranks, and a shadow set that waited for it would
    // leave the light the player walks up to unshadowed while a light it has left behind keeps its
    // map. The ranking is a sort of at most the enabled budget, and it runs only after a move.
    if budget.shadow_at != Some(camera_position) {
        let LightBudget {
            enabled,
            shadowed,
            shadow_scratch,
            ..
        } = &mut *budget;
        shadow_scratch.clear();
        if shadow_count > 0 {
            shadow_scratch.extend(enabled.iter().filter_map(|&entity| {
                // Retail shadows only a light whose record carries a shadow flag.
                // silent-ok: an enabled entity that lost its light has nothing to shadow, and the changed light count makes the budget choose again.
                if !markers.get(entity).ok()?.shadow_flagged {
                    return None;
                }
                let (_, transform, _, _) = lights.get(entity).ok()?;
                Some((
                    entity,
                    transform.translation().distance_squared(camera_position),
                ))
            }));
            sort_by_distance(shadow_scratch);
            shadow_scratch.truncate(shadow_count);
        }
        shadowed.clear();
        shadowed.extend(shadow_scratch.iter().map(|(entity, _)| *entity));
        for (entity, _, mut light, _) in &mut lights {
            let wanted = shadowed.contains(&entity);
            if light.shadow_maps_enabled != wanted {
                light.shadow_maps_enabled = wanted;
            }
        }
        budget.shadow_at = Some(camera_position);
    }

    if let Some((nearest, distance_squared)) = nearest
        && let Ok((_, _, light, _)) = lights.get(nearest)
    {
        debug!(
            spawned,
            enabled = budget.enabled.len(),
            shadowed = budget.shadowed.len(),
            nearest_distance = distance_squared.sqrt(),
            nearest_range = light.range,
            nearest_intensity = light.intensity,
            "lights: budget re-chosen"
        );
    }
}

/// Wider emitting lights precede tiny lights. Distance orders each class, and entity identity
/// breaks ties. Dark lights stay in the cache but never consume an enabled slot.
fn sort_for_budget(ranked: &mut [(Entity, LightBudgetClass, f32)]) {
    ranked.sort_by(
        |(left_entity, left_class, left), (right_entity, right_class, right)| {
            left_class
                .cmp(right_class)
                .then_with(|| left.total_cmp(right))
                .then_with(|| left_entity.cmp(right_entity))
        },
    );
}

/// How many of the nearest lights cast shadows: none when the Point-light shadows setting is off,
/// otherwise the enabled budget, but never more than [`MAX_SHADOW_CASTING_POINT_LIGHTS`].
fn shadow_casting_count(budget_size: usize, shadows_on: bool) -> usize {
    if shadows_on {
        budget_size.min(MAX_SHADOW_CASTING_POINT_LIGHTS)
    } else {
        0
    }
}

/// Nearest first. The entity breaks ties, so two lights at the same distance are chosen between the
/// same way every frame they are re-ranked in.
fn sort_by_distance(ranked: &mut [(Entity, f32)]) {
    ranked.sort_by(|(left_entity, left), (right_entity, right)| {
        left.total_cmp(right)
            .then_with(|| left_entity.cmp(right_entity))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::schedule::SingleThreadedExecutor;
    use bevy::log::tracing_subscriber::{
        Registry,
        layer::{Layer, SubscriberExt},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tracing::{Event, Level, Subscriber};

    #[test]
    fn resident_deferred_lights_enable_disable_and_reenable_without_reloading() {
        let mut app = budget_app(Vec3::ZERO, []);
        app.insert_resource(crate::config::EngineConfig {
            lights: false,
            ..default()
        })
        .add_systems(Update, activate_deferred_lights);
        let reference = app
            .world_mut()
            .spawn((
                Transform::from_xyz(100.0, 0.0, 0.0),
                DeferredPointLight {
                    light: point_light(&light_row(512.0, 0), None).unwrap(),
                    marker: SkyrimLight {
                        form_id: 1,
                        cell_id: 2,
                        shadow_flagged: true,
                    },
                    dynamics: evaluate_light(&light_row(512.0, 0), None).dynamics(1),
                },
            ))
            .id();
        app.update();
        assert!(app.world().get::<Children>(reference).is_none());
        app.world_mut()
            .resource_mut::<crate::config::EngineConfig>()
            .lights = true;
        app.update();
        let light = app.world().get::<Children>(reference).unwrap()[0];
        assert_eq!(
            *app.world().get::<Visibility>(light).unwrap(),
            Visibility::Inherited
        );
        assert!(app.world().get::<DeferredPointLight>(reference).is_none());
        app.world_mut()
            .resource_mut::<crate::config::EngineConfig>()
            .lights = false;
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(light).unwrap(),
            Visibility::Hidden
        );
        app.world_mut()
            .resource_mut::<crate::config::EngineConfig>()
            .lights = true;
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(light).unwrap(),
            Visibility::Inherited
        );
        assert_eq!(app.world().get::<Children>(reference).unwrap().len(), 1);
    }
    use crate::world::components::CELL_SIZE;
    use bevy::{
        asset::AssetPlugin, camera::visibility::VisibilityPlugin, transform::TransformPlugin,
    };

    fn light_row(radius: f32, flags: u32) -> LightRow {
        LightRow {
            radius,
            color: [255, 200, 120],
            flags,
            fade: None,
            fade_status: None,
            falloff: None,
            reference_data: None,
            radius_status: None,
            flicker_period: None,
            flicker_intensity_amplitude: None,
            flicker_movement_amplitude: None,
            reference_never_fades: false,
        }
    }

    /// The radiance, in nits before exposure, that a white Lambert surface of albedo 1 facing a
    /// retail-falloff light receives at `distance`, written from the engine it runs in:
    /// `bevy_pbr/src/render/light.rs` divides a point light's intensity by `4*PI` (lumens to lumens
    /// per steradian) before it reaches the shader, `Fd_Burley` in
    /// `bevy_pbr/src/render/pbr_lighting.wgsl` supplies the Lambert `1/PI`, and the retail flag
    /// replaces `window(d) / d^2` with `getRetailDistanceAttenuation`, `saturate(1 - d^2 / r^2)`.
    ///
    /// Both `PI`s belong here. An earlier version of this helper carried only the Lambert one,
    /// which made the test pass while the lights delivered `4*PI` less than the constant they were
    /// aimed at.
    fn radiance(light: &PointLight, distance: f32) -> f32 {
        let attenuation = (1.0 - distance * distance / (light.range * light.range)).clamp(0.0, 1.0);
        light.intensity / (4.0 * core::f32::consts::PI * core::f32::consts::PI) * attenuation
    }

    /// Algebra-only consistency check of the input constants: the sun's illuminance over `PI`
    /// divided by `AMBIENT_ILLUMINANCE` equals `SUNLIGHT_SCALE`. This checks the stated input
    /// ratio, not the rendered output. Bevy's ambient path (environment-BRDF approximation and
    /// specular terms) is not modelled here, so this test does not validate a rendered ratio.
    #[test]
    fn the_configured_sun_to_ambient_input_ratio_matches_the_sunlight_scale() {
        let bevy_ratio = SUN_ILLUMINANCE / core::f32::consts::PI / AMBIENT_ILLUMINANCE;
        assert!(
            (bevy_ratio - SUNLIGHT_SCALE).abs() < 1.0e-3,
            "sun to ambient is {bevy_ratio}, Skyrim's sunlight scale is {SUNLIGHT_SCALE}"
        );
    }

    /// Relative luminance of a colour, the same Rec. 709 weights the pixels of a render are
    /// measured with: a light's colour scales its contribution linearly, so a warm light of a given
    /// intensity puts less luminance on a surface than a white one.
    fn luminance(color: Color) -> f32 {
        let linear = LinearRgba::from(color);
        0.2126 * linear.red + 0.7152 * linear.green + 0.0722 * linear.blue
    }

    /// Retail adds `albedo x colour x fade` at the centre of a light (`RF-LIGHT-GPU-008`). The scene
    /// shades one retail unit as `AMBIENT_ILLUMINANCE` nits, the scale of its ambient light, so a
    /// fade-1 light must put exactly that on a white surface at its centre. Source:
    /// `RF-LIGHT-GPU-008`, see [point-lights.md](../../../docs/specs/engine/point-lights.md).
    #[test]
    fn a_unit_fade_light_delivers_one_retail_unit_at_its_centre() {
        let light = point_light(&light_row(512.0, 0), None).unwrap();
        assert!((radiance(&light, 0.0) / AMBIENT_ILLUMINANCE - 1.0).abs() < 1.0e-5);
        // The same unit as the key light: sun lux / PI is the nits a Lambert surface receives, and
        // retail's sun is `colour x SunlightScale` retail units (`RF-LIGHT-GPU-006`).
        let sun_nits = SUN_ILLUMINANCE / core::f32::consts::PI;
        assert!(
            (sun_nits / radiance(&light, 0.0) - SUNLIGHT_SCALE).abs() < 1.0e-3,
            "a fade-1 point light at its centre is 1/{SUNLIGHT_SCALE} of the sun, as in retail"
        );
    }

    /// Retail's curve, from the outside: `1 - (d / r)^2`, so 0.75 at half the radius and zero at
    /// the radius, with no `1 / d^2` rise towards the light and no brightness that depends on `r`.
    #[test]
    fn the_falloff_is_retails_and_the_radius_only_places_its_end() {
        for radius in [64.0, 512.0, 4334.0] {
            let light = point_light(&light_row(radius, 0), None).unwrap();
            let centre = radiance(&light, 0.0);
            assert!((radiance(&light, radius * 0.5) / centre - 0.75).abs() < 1.0e-5);
            assert!((radiance(&light, radius * 0.25) / centre - 0.9375).abs() < 1.0e-5);
            assert_eq!(radiance(&light, radius), 0.0);
            assert_eq!(radiance(&light, radius * 2.0), 0.0);
            assert_eq!(light.range, radius);
        }
    }

    /// A light's colour scales by its FNAM fade, linearly and with nothing else: no radius term
    /// and no exposure. Zero fade is a dark light; invalid fades never become a bright one.
    #[test]
    fn intensity_is_the_retail_unit_times_the_fade_and_nothing_else() {
        let unit = retail_intensity(1.0);
        assert!(
            (unit / (4.0 * core::f32::consts::PI * core::f32::consts::PI * AMBIENT_ILLUMINANCE)
                - 1.0)
                .abs()
                < 1.0e-6
        );
        for fade in [0.0, 0.5, 1.0, 1.5, 3.0, 5.0] {
            assert!((retail_intensity(fade) - unit * fade).abs() <= unit * fade * 1.0e-6);
            let light = point_light(
                &LightRow {
                    fade: Some(fade),
                    ..light_row(768.0, 0)
                },
                None,
            )
            .unwrap();
            assert_eq!(light.intensity, retail_intensity(fade));
        }
        assert_eq!(retail_intensity(0.0), 0.0);
        assert_eq!(retail_intensity(-1.0), 0.0);
        assert_eq!(retail_intensity(f32::NAN), 0.0);
        assert_eq!(retail_intensity(f32::MAX), f32::MAX);
    }

    /// The colour is the record's and is never mixed with the intensity: a warm light of the same
    /// fade puts less luminance on a surface than a white one, at the same intensity.
    #[test]
    fn the_intensity_scale_ignores_the_lights_colour() {
        let warm = point_light(&light_row(512.0, 0), None).unwrap();
        let white = point_light(
            &LightRow {
                color: [255, 255, 255],
                ..light_row(512.0, 0)
            },
            None,
        )
        .unwrap();
        assert_eq!(warm.intensity, white.intensity);
        assert!(
            luminance(warm.color) < luminance(white.color),
            "and the warm light is the dimmer of the two on a surface"
        );
    }

    /// The light the old intensity cap existed for. `000D9051` `FalmerCityLight02NS` is Blackreach's
    /// one huge light: radius 4334, colour (216,128,39). Retail does not scale a light by its
    /// radius, so it is as bright as a torch of the same fade and only reaches further. The old
    /// formula made it 71 times a torch, which a 256-unit cap then hid. Source: `RF-LIGHT-GPU-008`,
    /// see [point-lights.md](../../../docs/specs/engine/point-lights.md).
    #[test]
    fn the_biggest_shipped_light_keeps_its_reach_at_a_torchs_brightness() {
        let falmer_city_light = LightRow {
            color: [216, 128, 39],
            ..light_row(4334.0, 0)
        };
        let big = point_light(&falmer_city_light, None).unwrap();
        let torch = point_light(&light_row(512.0, 0), None).unwrap();
        assert_eq!(big.range, 4334.0, "the record's reach is unchanged");
        assert_eq!(big.intensity, torch.intensity);
        assert_eq!(big.color, Color::srgb_u8(216, 128, 39));
    }

    /// Every spawned Skyrim light carries the marker that makes the renderer use retail's
    /// falloff; a light without it would be shaded with Bevy's inverse square and be far too
    /// bright for the lumens above.
    #[test]
    fn a_skyrim_light_requires_the_retail_falloff_marker() {
        let mut world = World::new();
        let light = world
            .spawn(SkyrimLight {
                form_id: 1,
                cell_id: 2,
                shadow_flagged: true,
            })
            .id();
        assert!(world.get::<bevy::pbr::RetailPointFalloff>(light).is_some());
        let other = world.spawn(PointLight::default()).id();
        assert!(world.get::<bevy::pbr::RetailPointFalloff>(other).is_none());
    }

    #[test]
    fn a_torch_becomes_a_range_coloured_unshadowed_light() {
        let light = point_light(&light_row(512.0, 0), None).unwrap();
        assert_eq!(light.range, 512.0);
        assert_eq!(light.color, Color::srgb_u8(255, 200, 120));
        assert!(!light.shadow_maps_enabled);
        assert_eq!(light.radius, 0.0, "no area, so no oversized specular");
        // A cell is 4096 Creation units across, about 58 metres: the range is in those units.
        assert!((CELL_SIZE / 512.0 - 8.0).abs() < 1.0e-3);
        assert_eq!(light.intensity, retail_intensity(1.0));
    }

    #[test]
    fn negative_and_off_by_default_lights_spawn_nothing() {
        assert!(point_light(&light_row(512.0, LIGHT_FLAG_NEGATIVE), None).is_none());
        assert!(point_light(&light_row(512.0, LIGHT_FLAG_OFF_BY_DEFAULT), None).is_none());
        assert!(point_light(&light_row(512.0, 0x0001 | 0x0008), None).is_some());
        assert_eq!(point_light(&light_row(0.0, 0), None).unwrap().range, 0.1);
        assert!(point_light(&light_row(f32::NAN, 0), None).is_none());
        assert_eq!(point_light(&light_row(-64.0, 0), None).unwrap().range, 0.1);
        assert!(
            point_light(&light_row(0.0, 0), Some(512.0)).is_some(),
            "an override rescues a record whose own radius is unusable"
        );
    }

    /// Signed XRDS adds to the base radius; negative offsets shrink rather than disable it.
    #[test]
    fn a_reference_radius_offset_adds_to_the_record_radius() {
        let record = light_row(256.0, 0);
        let overridden = point_light(&record, Some(850.8)).unwrap();
        assert_eq!(overridden.range, 1106.8);
        assert_eq!(
            overridden.intensity,
            retail_intensity(1.0),
            "retail's brightness does not depend on the radius that is used"
        );

        assert_eq!(point_light(&record, Some(-100.0)).unwrap().range, 156.0);
        assert_eq!(point_light(&record, Some(0.0)).unwrap().range, 256.0);
        for offset in [-256.0, -300.0] {
            assert_eq!(point_light(&record, Some(offset)).unwrap().range, 0.1);
        }
        for offset in [f32::NAN, f32::INFINITY] {
            assert!(point_light(&record, Some(offset)).is_none());
        }
    }

    /// A large radius or an in-range offset whose sum passes the cap is clamped to the cap, with the
    /// authored sum kept. An offset above the cap is a retail outlier and is ignored instead.
    #[test]
    fn backend_range_limit_clamps_a_large_sum_but_ignores_an_outlier_offset() {
        assert_eq!(
            point_light(&light_row(20_000.0, 0), None).unwrap().range,
            MAX_RADIUS_OVERRIDE
        );
        let sum = evaluate_light(&light_row(2_000.0, 0), Some(7_000.0));
        assert_eq!(sum.authored_radius, Some(9_000.0));
        assert_eq!(sum.point_light().unwrap().range, MAX_RADIUS_OVERRIDE);
        assert!(sum.backend_limited && !sum.radius_offset_ignored);
        // The cap itself is an offset that is kept, not an outlier.
        let at_cap = evaluate_light(&light_row(256.0, 0), Some(MAX_RADIUS_OVERRIDE));
        assert!(!at_cap.radius_offset_ignored);
        assert_eq!(at_cap.point_light().unwrap().range, MAX_RADIUS_OVERRIDE);

        // The ten references above the cap in the registered pack (nine retail, 15,967 up to
        // 3,736,737 units, and one Creation Club, 9,505.58) keep the record's radius, as before
        // XRDS became additive; they do not jump to the range cap.
        let unit = point_light(&light_row(256.0, 0), None).unwrap();
        for outlier in [
            9_505.58,
            15_967.0,
            25_074.0,
            456_444.0,
            844_567.0,
            3_736_737.5,
        ] {
            let evaluated = evaluate_light(&light_row(256.0, 0), Some(outlier));
            assert_eq!(evaluated.radius_offset, Some(outlier), "raw offset is kept");
            assert_eq!(evaluated.authored_radius, Some(256.0), "offset {outlier}");
            assert!(evaluated.radius_offset_ignored && evaluated.backend_limited);
            let light = evaluated.point_light().unwrap();
            assert_eq!(light.range, 256.0, "offset {outlier}");
            assert_eq!(light.intensity, unit.intensity, "offset {outlier}");
        }
        // An unusable record radius is not rescued by an ignored outlier.
        assert_eq!(
            evaluate_light(&light_row(0.0, 0), Some(25_074.0))
                .point_light()
                .unwrap()
                .range,
            NATIVE_NONPOSITIVE_RADIUS
        );
        // A non-finite offset stays invalid rather than being treated as an outlier.
        let infinite = evaluate_light(&light_row(256.0, 0), Some(f32::INFINITY));
        assert!(infinite.invalid_radius && !infinite.radius_offset_ignored);
    }

    fn reference_light_data(fade_offset: Option<f32>) -> crate::world::database::ReferenceLightRow {
        crate::world::database::ReferenceLightRow {
            data_size: 16,
            fov_offset: Some(0.0),
            fade_offset,
            end_distance_cap: Some(0.0),
            shadow_depth_bias: Some(0.0),
        }
    }

    /// `f32::MAX` in FNAM or XLIG is the "no fade" sentinel. It must not become a light at
    /// maximum intensity: it leaves the unit fade (or the authored base fade) in place.
    #[test]
    fn the_max_fade_sentinel_means_no_fade_and_never_a_maximum_light() {
        let unit = point_light(&light_row(256.0, 0), None).unwrap();

        // Absent fade is also "no fade": unit power, no sentinel disposition.
        let absent = evaluate_light(&light_row(256.0, 0), None);
        assert_eq!(absent.effective_fade, 1.0);
        assert!(!absent.fade_sentinel);

        // FNAM sentinel alone.
        let mut row = light_row(256.0, 0);
        row.fade = Some(f32::MAX);
        row.fade_status = Some(1);
        let evaluated = evaluate_light(&row, None);
        assert!(evaluated.fade_sentinel && evaluated.legacy_fade && !evaluated.invalid_fade);
        assert_eq!(evaluated.base_fade, 1.0);
        assert_eq!(evaluated.effective_fade, 1.0);
        assert_eq!(evaluated.authored_fade, 1.0);
        assert!(!evaluated.backend_limited);
        assert_eq!(evaluated.point_light().unwrap().intensity, unit.intensity);

        // XLIG sentinel keeps the authored base fade and is neither applied nor malformed.
        let mut row = light_row(256.0, 0);
        row.fade = Some(2.0);
        row.fade_status = Some(1);
        row.reference_data = Some(reference_light_data(Some(f32::MAX)));
        let evaluated = evaluate_light(&row, None);
        assert!(evaluated.fade_sentinel);
        assert!(!evaluated.reference_fade_applied && !evaluated.invalid_reference_fade);
        assert!(!evaluated.malformed_reference_data && !evaluated.unsupported_reference_data);
        assert_eq!(evaluated.authored_fade, 2.0);
        assert_eq!(evaluated.effective_fade, 2.0);
        assert!(!evaluated.backend_limited);
        let light = evaluated.point_light().unwrap();
        assert!((light.intensity / unit.intensity - 2.0).abs() < 1.0e-5);
        assert_ne!(light.intensity, f32::MAX);

        // Both sentinels together leave unit power.
        row.fade = Some(f32::MAX);
        let evaluated = evaluate_light(&row, None);
        assert!(evaluated.fade_sentinel);
        assert_eq!(evaluated.effective_fade, 1.0);
        assert_eq!(evaluated.point_light().unwrap().intensity, unit.intensity);

        // A near-sentinel finite offset is an ordinary offset, not the sentinel.
        let mut row = light_row(256.0, 0);
        row.reference_data = Some(reference_light_data(Some(f32::MAX / 2.0)));
        let evaluated = evaluate_light(&row, None);
        assert!(!evaluated.fade_sentinel && evaluated.reference_fade_applied);
        assert!(
            evaluated.backend_limited,
            "the saturated product is flagged"
        );
    }

    /// A light budgeted around a camera, as `budget_lights` sees them.
    fn budget_app(camera: Vec3, light_positions: impl IntoIterator<Item = Vec3>) -> App {
        budget_app_with(camera, light_positions, None)
    }

    /// [`budget_app`] with the rendering settings the budget reads, when a test needs them.
    fn budget_app_with(
        camera: Vec3,
        light_positions: impl IntoIterator<Item = Vec3>,
        settings: Option<crate::settings::RenderingSettings>,
    ) -> App {
        let mut app = App::new();
        if let Some(settings) = settings {
            app.insert_resource(settings);
        }
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            bevy::mesh::MeshPlugin,
            TransformPlugin,
            VisibilityPlugin,
        ))
        .init_asset::<Mesh>()
        .init_resource::<LightBudget>()
        .add_systems(PostUpdate, budget_lights.after(TransformSystems::Propagate));
        app.world_mut().spawn((
            Transform::from_translation(camera),
            GlobalTransform::from_translation(camera),
            StreamingCamera,
        ));
        for position in light_positions {
            app.world_mut().spawn((
                SkyrimLight {
                    form_id: 1,
                    cell_id: 2,
                    shadow_flagged: true,
                },
                // The real bundle: `PointLight` is what gives a light its `Transform`, its
                // `Visibility` and its `GlobalTransform`.
                point_light(&light_row(512.0, 0), None).unwrap(),
                Transform::from_translation(position),
            ));
        }
        app.update();
        app
    }

    fn enabled_lights(app: &mut App) -> Vec<Entity> {
        let mut query = app
            .world_mut()
            .query_filtered::<Entity, (With<SkyrimLight>, With<Visibility>)>();
        query
            .iter(app.world())
            .filter(|entity| {
                *app.world().entity(*entity).get::<Visibility>().unwrap() == Visibility::Inherited
            })
            .collect()
    }

    #[test]
    fn tiny_fallback_and_zero_power_lights_do_not_displace_wider_lights() {
        let mut app = budget_app(
            Vec3::ZERO,
            (0..ENABLED_LIGHT_BUDGET).map(|i| Vec3::new(1000.0 + i as f32, 0.0, 0.0)),
        );
        let regular: HashSet<_> = enabled_lights(&mut app).into_iter().collect();
        let mut tiny = HashSet::new();
        let mut dark = HashSet::new();
        for _ in 0..ENABLED_LIGHT_BUDGET + 1 {
            tiny.insert(
                app.world_mut()
                    .spawn((
                        SkyrimLight {
                            form_id: 3,
                            cell_id: 4,
                            shadow_flagged: true,
                        },
                        point_light(&light_row(0.0, 0), None).unwrap(),
                        Transform::default(),
                    ))
                    .id(),
            );
            dark.insert(
                app.world_mut()
                    .spawn((
                        SkyrimLight {
                            form_id: 5,
                            cell_id: 4,
                            shadow_flagged: true,
                        },
                        point_light(
                            &LightRow {
                                fade: Some(0.0),
                                fade_status: Some(1),
                                ..light_row(512.0, 0)
                            },
                            None,
                        )
                        .unwrap(),
                        Transform::default(),
                    ))
                    .id(),
            );
        }
        app.update();
        let enabled = enabled_lights(&mut app);
        assert_eq!(enabled.len(), ENABLED_LIGHT_BUDGET);
        assert!(enabled.iter().all(|entity| regular.contains(entity)));

        // Keep the native 0.1 light and permit it when a slot is available.
        app.world_mut()
            .entity_mut(*regular.iter().next().unwrap())
            .despawn();
        app.update();
        let enabled = enabled_lights(&mut app);
        assert_eq!(enabled.len(), ENABLED_LIGHT_BUDGET);
        assert_eq!(
            enabled
                .iter()
                .filter(|entity| tiny.contains(*entity))
                .count(),
            1
        );
        assert!(enabled.iter().all(|entity| !dark.contains(entity)));
    }

    #[test]
    fn the_budget_reconsiders_power_and_range_class_changes_without_camera_motion() {
        let mut app = budget_app(
            Vec3::ZERO,
            (0..ENABLED_LIGHT_BUDGET).map(|i| Vec3::new(1000.0 + i as f32, 0.0, 0.0)),
        );
        let regular: HashSet<_> = enabled_lights(&mut app).into_iter().collect();
        let changed = app
            .world_mut()
            .spawn((
                SkyrimLight {
                    form_id: 3,
                    cell_id: 4,
                    shadow_flagged: true,
                },
                point_light(
                    &LightRow {
                        fade: Some(0.0),
                        fade_status: Some(1),
                        ..light_row(512.0, 0)
                    },
                    None,
                )
                .unwrap(),
                Transform::default(),
            ))
            .id();
        app.update();
        assert!(!enabled_lights(&mut app).contains(&changed));

        app.world_mut()
            .get_mut::<PointLight>(changed)
            .unwrap()
            .intensity = retail_intensity(1.0);
        app.update();
        let enabled = enabled_lights(&mut app);
        assert_eq!(enabled.len(), ENABLED_LIGHT_BUDGET);
        assert!(enabled.contains(&changed));

        app.world_mut()
            .get_mut::<PointLight>(changed)
            .unwrap()
            .range = NATIVE_NONPOSITIVE_RADIUS;
        app.update();
        let enabled = enabled_lights(&mut app);
        assert_eq!(enabled.len(), ENABLED_LIGHT_BUDGET);
        assert!(enabled.iter().all(|entity| regular.contains(entity)));
    }

    fn move_camera(app: &mut App, camera: Vec3) {
        let mut query = app
            .world_mut()
            .query_filtered::<&mut Transform, With<StreamingCamera>>();
        for mut transform in query.iter_mut(app.world_mut()) {
            transform.translation = camera;
        }
        app.update();
    }

    /// Hides nothing and enables nothing else: a `PointLight` the engine's own code spawns - a
    /// fixture, a future camera lantern - is never in the ranking.
    #[test]
    fn the_budget_leaves_a_point_light_that_is_not_a_skyrim_light_alone() {
        let mut app = budget_app(
            Vec3::ZERO,
            (0..ENABLED_LIGHT_BUDGET + 4).map(|index| Vec3::new(1000.0 + index as f32, 0.0, 0.0)),
        );
        let ordinary = app
            .world_mut()
            .spawn((
                point_light(&light_row(512.0, 0), None).unwrap(),
                Transform::from_translation(Vec3::new(1_000.0, 0.0, 0.0)),
            ))
            .id();
        app.update();
        assert_eq!(
            *app.world().entity(ordinary).get::<Visibility>().unwrap(),
            Visibility::Inherited,
            "a plain `PointLight` is not the budget's to switch"
        );
        assert_eq!(enabled_lights(&mut app).len(), ENABLED_LIGHT_BUDGET);
    }

    /// The budget is the 64 nearest and no more: the far ones are hidden, nearest first.
    #[test]
    fn the_budget_enables_the_64_nearest_lights() {
        let mut app = budget_app(
            Vec3::ZERO,
            (0..ENABLED_LIGHT_BUDGET + 16).map(|index| Vec3::new(1000.0 + index as f32, 0.0, 0.0)),
        );

        let enabled = enabled_lights(&mut app);
        assert_eq!(
            enabled.len(),
            ENABLED_LIGHT_BUDGET,
            "the budget is 64 lights, not one per spawned reference"
        );
        let mut query = app
            .world_mut()
            .query_filtered::<(Entity, &Transform), With<SkyrimLight>>();
        let mut hidden: Vec<f32> = query
            .iter(app.world())
            .filter(|(entity, _)| !enabled.contains(entity))
            .map(|(_, transform)| transform.translation.x)
            .collect();
        hidden.sort_by(f32::total_cmp);
        assert_eq!(
            hidden.len(),
            16,
            "the sixteen farthest lights are the ones switched off"
        );
        assert!(
            hidden[0] >= 1000.0 + ENABLED_LIGHT_BUDGET as f32,
            "and no nearer light was switched off for a farther one: {hidden:?}"
        );

        // Moving the camera past `BUDGET_RECHOOSE_DISTANCE` re-chooses: the lights around its new
        // position come on and the ones left behind go off.
        move_camera(&mut app, Vec3::new(2000.0, 0.0, 0.0));
        let mut query = app
            .world_mut()
            .query_filtered::<(Entity, &Transform), With<SkyrimLight>>();
        let enabled = enabled_lights(&mut app);
        assert_eq!(enabled.len(), ENABLED_LIGHT_BUDGET);
        assert!(
            query
                .iter(app.world())
                .filter(|(entity, transform)| transform.translation.x < 1100.0
                    && !enabled.contains(entity))
                .count()
                > 0,
            "the lights by the camera's old position are switched off"
        );
    }

    /// The set-change re-choose, which a cache keyed on the count of lights cannot see: one cell
    /// streams out and another in with the same number of lights while the camera stands still, and
    /// the new lights have to be ranked.
    ///
    /// A cache that keyed on the count alone would leave the new light visible without hiding
    /// anything - 65 enabled lights - which is what the first assertion catches.
    #[test]
    fn the_budget_re_chooses_when_the_light_set_changes_without_the_camera_moving() {
        let mut app = budget_app(
            Vec3::ZERO,
            (0..ENABLED_LIGHT_BUDGET + 16).map(|index| Vec3::new(1000.0 + index as f32, 0.0, 0.0)),
        );
        assert_eq!(enabled_lights(&mut app).len(), ENABLED_LIGHT_BUDGET);

        // One light of a distant cell goes away and one of a cell next to the camera arrives: the
        // same count, and the camera has not moved a unit.
        let gone = {
            let mut query = app
                .world_mut()
                .query_filtered::<(Entity, &Transform), With<SkyrimLight>>();
            query
                .iter(app.world())
                .max_by(|(_, left), (_, right)| left.translation.x.total_cmp(&right.translation.x))
                .map(|(entity, _)| entity)
                .unwrap()
        };
        let arrived = app
            .world_mut()
            .spawn((
                SkyrimLight {
                    form_id: 3,
                    cell_id: 4,
                    shadow_flagged: true,
                },
                point_light(&light_row(512.0, 0), None).unwrap(),
                Transform::from_translation(Vec3::new(5.0, 0.0, 0.0)),
            ))
            .id();
        app.world_mut().entity_mut(gone).despawn();
        app.update();

        let enabled = enabled_lights(&mut app);
        assert_eq!(
            enabled.len(),
            ENABLED_LIGHT_BUDGET,
            "a light that arrives while the camera stands still is ranked like any other"
        );
        assert!(
            enabled.contains(&arrived),
            "and the nearest light of all is one of them"
        );
        let mut query = app
            .world_mut()
            .query_filtered::<(Entity, &Transform), With<SkyrimLight>>();
        let enabled_far = query
            .iter(app.world())
            .filter(|(entity, transform)| {
                enabled.contains(entity) && transform.translation.x >= 1_000.0
            })
            .count();
        assert_eq!(
            enabled_far,
            ENABLED_LIGHT_BUDGET - 1,
            "the light the newcomer displaced is the farthest one"
        );
    }

    /// The lights that have a cube shadow map, nearest first, as `(x, entity)`.
    fn shadow_casting_lights(app: &mut App) -> Vec<(f32, Entity)> {
        let mut query = app
            .world_mut()
            .query_filtered::<(Entity, &PointLight, &Transform), With<SkyrimLight>>();
        let mut casting: Vec<(f32, Entity)> = query
            .iter(app.world())
            .filter(|(_, light, _)| light.shadow_maps_enabled)
            .map(|(entity, _, transform)| (transform.translation.x, entity))
            .collect();
        casting.sort_by(|left, right| left.0.total_cmp(&right.0));
        casting
    }

    fn shadows_on() -> crate::settings::RenderingSettings {
        crate::settings::RenderingSettings {
            point_light_shadows: true,
            ..default()
        }
    }

    /// Bleak Falls Barrow has 71 placed lights. With Point-light shadows on, the budget must not give
    /// more than the retail limit of them a cube shadow map: 64 cube maps lost the Metal device.
    #[test]
    fn the_budget_never_shadows_more_lights_than_the_cap() {
        const BLEAK_FALLS_BARROW_LIGHTS: usize = 71;
        let mut app = budget_app_with(
            Vec3::ZERO,
            (0..BLEAK_FALLS_BARROW_LIGHTS).map(|index| Vec3::new(100.0 + index as f32, 0.0, 0.0)),
            Some(shadows_on()),
        );

        assert_eq!(
            enabled_lights(&mut app).len(),
            ENABLED_LIGHT_BUDGET,
            "the lighting budget is not the shadow cap"
        );
        assert_eq!(
            shadow_casting_lights(&mut app).len(),
            MAX_SHADOW_CASTING_POINT_LIGHTS,
            "exactly the cap, not one per enabled light"
        );
        assert_eq!(
            MAX_SHADOW_CASTING_POINT_LIGHTS, 4,
            "retail: RF-LIGHT-GPU-004"
        );
        assert!(
            shadow_encoder_estimate(shadow_casting_lights(&mut app).len())
                < SHADOW_ENCODER_WARN_ESTIMATE,
            "a frame stays far below the Metal command buffer limit"
        );
    }

    /// The nearest lights are the ones that cast shadows, whatever order they spawned in, and they
    /// follow the camera.
    #[test]
    fn the_budget_gives_shadows_to_the_nearest_lights_first() {
        // Spawned far to near, so spawn order is the opposite of the ranking.
        let mut app = budget_app_with(
            Vec3::ZERO,
            (0..20)
                .rev()
                .map(|index| Vec3::new(100.0 + 10.0 * index as f32, 0.0, 0.0)),
            Some(shadows_on()),
        );
        let casting: Vec<f32> = shadow_casting_lights(&mut app)
            .into_iter()
            .map(|(x, _)| x)
            .collect();
        assert_eq!(casting, vec![100.0, 110.0, 120.0, 130.0]);

        // Past `BUDGET_RECHOOSE_DISTANCE` the shadows move with the camera: the lights left
        // behind lose theirs, so the total never grows past the cap.
        move_camera(&mut app, Vec3::new(1_000.0, 0.0, 0.0));
        let casting: Vec<f32> = shadow_casting_lights(&mut app)
            .into_iter()
            .map(|(x, _)| x)
            .collect();
        assert_eq!(casting, vec![260.0, 270.0, 280.0, 290.0]);
    }

    /// The cap is a ceiling and not a quota: fewer lights than the cap all cast, and a small
    /// budget wins over the cap.
    #[test]
    fn the_shadow_cap_is_a_ceiling() {
        let mut app = budget_app_with(
            Vec3::ZERO,
            (0..2).map(|index| Vec3::new(100.0 + index as f32, 0.0, 0.0)),
            Some(shadows_on()),
        );
        assert_eq!(shadow_casting_lights(&mut app).len(), 2);

        assert_eq!(shadow_casting_count(1, true), 1);
        assert_eq!(shadow_casting_count(0, true), 0);
        assert_eq!(
            shadow_casting_count(ENABLED_LIGHT_BUDGET, true),
            MAX_SHADOW_CASTING_POINT_LIGHTS
        );
        assert_eq!(
            shadow_casting_count(256, true),
            MAX_SHADOW_CASTING_POINT_LIGHTS
        );
    }

    /// With the setting off no light casts a shadow, and switching it on and off re-chooses at
    /// once, without a camera move.
    #[test]
    fn point_light_shadows_follow_the_setting() {
        let mut app = budget_app_with(
            Vec3::ZERO,
            (0..10).map(|index| Vec3::new(100.0 + index as f32, 0.0, 0.0)),
            Some(crate::settings::RenderingSettings::default()),
        );
        assert!(shadow_casting_lights(&mut app).is_empty());

        app.world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .point_light_shadows = true;
        app.update();
        assert_eq!(
            shadow_casting_lights(&mut app).len(),
            MAX_SHADOW_CASTING_POINT_LIGHTS
        );

        app.world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .point_light_shadows = false;
        app.update();
        assert!(shadow_casting_lights(&mut app).is_empty());
    }

    /// Retail shadows only a light whose record carries a shadow flag (`DATA` flags `0x1c00`,
    /// `RF-LIGHT-GPU-009`): an unflagged light never takes one of the shadow slots, however near it
    /// is, and the slots go to the nearest flagged lights.
    #[test]
    fn only_lights_with_a_shadow_flag_take_a_shadow() {
        let mut app = budget_app_with(
            Vec3::ZERO,
            (0..10).map(|index| Vec3::new(100.0 + index as f32, 0.0, 0.0)),
            Some(shadows_on()),
        );
        let mut query = app.world_mut().query::<(Entity, &mut SkyrimLight)>();
        let lights: Vec<Entity> = query.iter(app.world()).map(|(entity, _)| entity).collect();
        for (index, entity) in lights.iter().enumerate() {
            app.world_mut()
                .get_mut::<SkyrimLight>(*entity)
                .unwrap()
                .shadow_flagged = index % 2 == 0;
        }
        // The flags are constant for a real light, so the ranking is cached with the camera.
        app.world_mut().resource_mut::<LightBudget>().shadow_at = None;
        app.update();
        let shadowed = shadow_casting_lights(&mut app);
        assert_eq!(
            shadowed.len(),
            MAX_SHADOW_CASTING_POINT_LIGHTS,
            "five flagged lights compete for the cap"
        );
        for (_, entity) in &shadowed {
            assert!(
                app.world()
                    .get::<SkyrimLight>(*entity)
                    .unwrap()
                    .shadow_flagged
            );
        }
        for entity in &lights {
            app.world_mut()
                .get_mut::<SkyrimLight>(*entity)
                .unwrap()
                .shadow_flagged = false;
        }
        app.world_mut().resource_mut::<LightBudget>().shadow_at = None;
        app.update();
        assert!(shadow_casting_lights(&mut app).is_empty());
    }

    /// A light that is not a Skyrim light keeps whatever shadow setting its owner gave it.
    #[test]
    fn the_shadow_cap_leaves_other_point_lights_alone() {
        let mut app = budget_app_with(
            Vec3::ZERO,
            (0..10).map(|index| Vec3::new(100.0 + index as f32, 0.0, 0.0)),
            Some(shadows_on()),
        );
        let fixture = app
            .world_mut()
            .spawn((
                PointLight {
                    shadow_maps_enabled: true,
                    ..default()
                },
                Transform::from_translation(Vec3::new(5_000.0, 0.0, 0.0)),
            ))
            .id();
        app.update();
        assert!(
            app.world()
                .entity(fixture)
                .get::<PointLight>()
                .unwrap()
                .shadow_maps_enabled
        );
        assert_eq!(
            shadow_casting_lights(&mut app).len(),
            MAX_SHADOW_CASTING_POINT_LIGHTS
        );
    }

    /// The guard logs for a load near the limit and is silent below it. The estimate is the
    /// arithmetic the doc comments state: 6 cube faces, 5 encoders each.
    #[test]
    fn the_encoder_estimate_flags_the_load_that_lost_the_device() {
        assert_eq!(shadow_encoder_estimate(4), 120);
        assert_eq!(shadow_encoder_estimate(64), 1_920);
        assert!(
            shadow_encoder_estimate(MAX_SHADOW_CASTING_POINT_LIGHTS) < SHADOW_ENCODER_WARN_ESTIMATE
        );
        assert!(
            shadow_encoder_estimate(ENABLED_LIGHT_BUDGET) >= SHADOW_ENCODER_WARN_ESTIMATE,
            "the 64-light load that lost the device is above the warning level"
        );
    }

    /// The camera moves less than `BUDGET_RECHOOSE_DISTANCE`, so the 64-light selection is cached
    /// and not re-made. The shadows still go to the four lights nearest the camera now: the light
    /// walked up to takes one and the one left behind gives it up.
    #[test]
    fn shadows_follow_the_camera_inside_the_budget_cache_distance() {
        let mut app = budget_app_with(
            Vec3::ZERO,
            (0..9).map(|index| Vec3::new(100.0 + 10.0 * index as f32, 0.0, 0.0)),
            Some(shadows_on()),
        );
        let xs = |app: &mut App| -> Vec<f32> {
            shadow_casting_lights(app)
                .into_iter()
                .map(|(x, _)| x)
                .collect()
        };
        assert_eq!(xs(&mut app), vec![100.0, 110.0, 120.0, 130.0]);

        move_camera(&mut app, Vec3::new(141.0, 0.0, 0.0));
        assert_eq!(
            app.world().resource::<LightBudget>().chosen_at,
            Some(Vec3::ZERO),
            "the move is inside the cache distance, so the budget was not re-made"
        );
        const { assert!(141.0 < BUDGET_RECHOOSE_DISTANCE) };
        assert_eq!(
            xs(&mut app),
            vec![130.0, 140.0, 150.0, 160.0],
            "the shadows moved to the four nearest lights"
        );
        assert_eq!(
            enabled_lights(&mut app).len(),
            9,
            "and no light was hidden by it"
        );
    }

    /// Two lights at the same distance compete for the last shadow of four, and the one with the
    /// lower entity wins every time, however the camera moves without changing that: the entity
    /// breaks the tie, so nothing flickers.
    #[test]
    fn equidistant_lights_keep_the_same_shadow_when_the_camera_moves() {
        // Three lights nearer than the rest, then a pair on either side of the camera at the same
        // distance for the fourth shadow, then one farther. Spawned with the pair first, so the
        // winner is the one spawned first.
        let mut app = budget_app_with(
            Vec3::ZERO,
            [40.0, -40.0, 10.0, 20.0, -30.0, 100.0].map(|x| Vec3::new(x, 0.0, 0.0)),
            Some(shadows_on()),
        );
        let pair: Vec<Entity> = {
            let mut query = app
                .world_mut()
                .query_filtered::<(Entity, &Transform), With<SkyrimLight>>();
            query
                .iter(app.world())
                .filter(|(_, transform)| transform.translation.x.abs() == 40.0)
                .map(|(entity, _)| entity)
                .collect()
        };
        assert_eq!(pair.len(), 2);
        let winner = *pair.iter().min().unwrap();

        for step in 0..=5 {
            // Along y the distances to the pair stay equal to each other.
            move_camera(&mut app, Vec3::new(0.0, step as f32, 0.0));
            let casting = shadow_casting_lights(&mut app);
            assert_eq!(casting.len(), MAX_SHADOW_CASTING_POINT_LIGHTS);
            let from_pair: Vec<Entity> = casting
                .iter()
                .map(|(_, entity)| *entity)
                .filter(|entity| pair.contains(entity))
                .collect();
            assert_eq!(
                from_pair,
                vec![winner],
                "the lower entity of the tied pair has the fourth shadow at step {step}"
            );
        }
    }

    /// Runs `warn_on_shadow_encoder_load` over a world of lights and counts its error logs.
    struct GuardRun {
        app: App,
        errors: Arc<AtomicUsize>,
    }

    impl GuardRun {
        fn new() -> Self {
            let mut app = App::new();
            // The log is captured per thread, so the system has to run on this one.
            app.edit_schedule(Update, |schedule| {
                schedule.set_executor(SingleThreadedExecutor::new());
            });
            app.add_systems(Update, warn_on_shadow_encoder_load);
            Self {
                app,
                errors: Arc::default(),
            }
        }

        fn spawn(&mut self, count: usize, shadows: bool, visible: bool) -> Vec<Entity> {
            (0..count)
                .map(|_| {
                    self.app
                        .world_mut()
                        .spawn((
                            PointLight {
                                shadow_maps_enabled: shadows,
                                ..default()
                            },
                            if visible {
                                InheritedVisibility::VISIBLE
                            } else {
                                InheritedVisibility::HIDDEN
                            },
                        ))
                        .id()
                })
                .collect()
        }

        /// One frame, and how many error logs of the guard it produced.
        fn frame(&mut self) -> usize {
            let before = self.errors.load(Ordering::Relaxed);
            let subscriber = Registry::default().with(ErrorCount(self.errors.clone()));
            tracing::subscriber::with_default(subscriber, || self.app.update());
            self.errors.load(Ordering::Relaxed) - before
        }
    }

    #[derive(Clone)]
    struct ErrorCount(Arc<AtomicUsize>);

    impl<S: Subscriber> Layer<S> for ErrorCount {
        fn on_event(
            &self,
            event: &Event<'_>,
            _context: bevy::log::tracing_subscriber::layer::Context<'_, S>,
        ) {
            let metadata = event.metadata();
            if *metadata.level() == Level::ERROR && metadata.target() == "engine::lights" {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// The fewest shadow-casting lights whose estimate reaches the warning level.
    fn lights_at_the_warning_level() -> usize {
        SHADOW_ENCODER_WARN_ESTIMATE.div_ceil(shadow_encoder_estimate(1))
    }

    #[test]
    fn the_guard_counts_only_visible_shadow_casting_lights() {
        let at_level = lights_at_the_warning_level();
        let mut run = GuardRun::new();
        // Plenty of lights that cannot cost an encoder: hidden ones, and visible ones without a
        // shadow map.
        run.spawn(at_level + 20, true, false);
        run.spawn(at_level + 20, false, true);
        run.spawn(at_level - 1, true, true);
        assert_eq!(run.frame(), 0, "one light below the level is silent");
        run.spawn(1, true, true);
        assert_eq!(run.frame(), 1, "the light that reaches the level warns");
    }

    #[test]
    fn the_guard_warns_once_per_crossing_and_again_after_a_drop() {
        let at_level = lights_at_the_warning_level();
        let mut run = GuardRun::new();
        let mut casting = run.spawn(at_level, true, true);

        assert_eq!(run.frame(), 1, "crossing the level warns");
        assert_eq!(
            run.frame(),
            0,
            "an unchanged count does not repeat the warning"
        );
        assert_eq!(run.frame(), 0);

        // Below the level: silent, and the guard forgets the count.
        run.app.world_mut().despawn(casting.pop().unwrap());
        assert_eq!(run.frame(), 0);
        assert_eq!(run.frame(), 0);

        // Crossing again warns again, once.
        casting.extend(run.spawn(1, true, true));
        assert_eq!(run.frame(), 1, "the second crossing warns again");
        assert_eq!(run.frame(), 0, "and only once");
    }
}
