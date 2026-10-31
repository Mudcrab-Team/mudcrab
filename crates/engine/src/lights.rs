//! Point lights from Skyrim `LIGH` references: the torches, braziers, Dwemer lamps and glowing
//! fungus the game places around its spaces. `docs/specs/engine/point-lights.md` is the norm this
//! module implements.
//!
//! `streaming::spawn_cell` gives every reference whose base record has a row in the converter's
//! `lights` table a [`PointLight`] child, and only while the engine is run with `--lights`. The
//! light entity is a descendant of the cell root, so it follows the render-origin rebases and the
//! cell unloads that move the cell hierarchy, and it is despawned with the cell that placed it.
//!
//! This module owns the record-to-light conversion, the intensity scale, and the budget that bounds
//! how many lights are enabled at once.
//!
//! Authored inputs and the Bevy compatibility transform are separate. Signed XRDS is added to
//! the base radius; FNAM scales luminous power. XLIG and DATA falloff remain inspectable but
//! unsupported by this adapter. See the spec for pinned sources and unresolved native equations.
//!
//! The backend retains the baseline's intensity normalization, frozen independently of global
//! ambient. This is a compatibility fallback, not a calibrated Skyrim radiometric conversion.
//! The renderer still uses inverse-square attenuation, unshadowed point lights and sRGB colours.
//! The 64-light budget and 256-unit intensity reference cap remain backend policy.

use crate::world::{components::StreamingCamera, database::LightRow};
use bevy::prelude::*;
use std::collections::HashSet;

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

/// The sun's illuminance in the world path, in lux. With the scene's EV100 of 9.7 this puts a
/// white surface facing the sun at about 3.8 in linear radiance before tone mapping.
pub const SUN_ILLUMINANCE: f32 = 12_000.0;

/// How much brighter than the ambient cube Skyrim's `SunlightScale` makes the sun's colour
/// (the engine's lighting constant, 2.8; see `docs/research/skyrim-lighting-regression-recovery-
/// 20261010.md`, "SunlightScale remains 2.8").
pub const SUNLIGHT_SCALE: f32 = 2.8;

/// Frozen b5259d33 half-radius diffuse-radiance normalization: 12000/(PI*2.8)*10.
/// This retains the baseline input scale, rather than adding a new dimming coefficient. It is
/// explicitly a legacy fallback until native light units/attenuation are recovered (#264/#364).
/// Changing SUN_ILLUMINANCE or AMBIENT_ILLUMINANCE cannot change local lights.
const LEGACY_HALF_RADIUS_RADIANCE: f32 = 12_000.0 / (core::f32::consts::PI * 2.8) * 10.0;

/// Backend compatibility cap in Creation units; not an authored brightness/range relationship.
pub const INTENSITY_REFERENCE_RADIUS: f32 = 256.0;
const HALF_RADIUS_WINDOW: f32 = 225.0 / 256.0;

/// Backend transform identity, retained on every evaluated light reference.
pub const LIGHT_TRANSFORM: &str = "bevy-point-legacy-normalization-v1";

/// Spawn-time diagnostics. Raw inputs stay on the reference, including unsupported fields.
#[derive(Component, Debug, Clone)]
pub struct LightEvaluation {
    pub source: LightRow,
    pub radius_offset: Option<f32>,
    pub authored_radius: Option<f64>,
    pub backend_range: Option<f32>,
    pub effective_fade: f32,
    pub legacy_fade: bool,
    pub invalid_fade: bool,
    pub invalid_radius: bool,
    pub backend_limited: bool,
    pub unsupported_reference_data: bool,
    pub malformed_reference_data: bool,
    pub unsupported_falloff: bool,
    pub suppressed_flags: bool,
    pub transform: &'static str,
}

impl LightEvaluation {
    /// The actual streaming consumer uses this conversion for both immediate and deferred lights.
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
            intensity: (f64::from(intensity_for_radius(self.backend_range?))
                * f64::from(self.effective_fade))
            .min(f64::from(f32::MAX)) as f32,
            range: self.backend_range?,
            shadow_maps_enabled: false,
            ..default()
        })
    }
}

/// Resolve supported inputs without modifying their stored values. XRDS is an additive signed
/// offset (xEdit wbREFRRadiusToStr). Invalid present offsets are rejected, not replaced by base
/// radius. Zero/negative effective ranges produce no light. XLIG is never a raw multiplier.
pub fn evaluate_light(light: &LightRow, radius_offset: Option<f32>) -> LightEvaluation {
    let authored_radius = (light.radius_status != Some(2)
        && light.radius.is_finite()
        && radius_offset.is_none_or(f32::is_finite))
    .then(|| f64::from(light.radius) + f64::from(radius_offset.unwrap_or(0.0)));
    let backend_range = authored_radius
        .filter(|radius| *radius > 0.0)
        .map(|radius| radius.min(f64::from(MAX_RADIUS_OVERRIDE)) as f32);
    let invalid_fade = light.fade_status == Some(2)
        || light
            .fade
            .is_some_and(|fade| !fade.is_finite() || fade < 0.0);
    let effective_fade = if invalid_fade {
        1.0
    } else {
        light
            .fade
            .filter(|fade| fade.is_finite() && *fade >= 0.0)
            .unwrap_or(1.0)
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
        effective_fade,
        legacy_fade: light.fade.is_none() || invalid_fade,
        invalid_fade,
        invalid_radius: authored_radius.is_none(),
        backend_limited: authored_radius
            .is_some_and(|radius| radius > f64::from(MAX_RADIUS_OVERRIDE))
            || f64::from(intensity_for_radius(backend_range.unwrap_or(0.0)))
                * f64::from(effective_fade)
                > f64::from(f32::MAX),
        unsupported_reference_data: light.reference_data.is_some(),
        malformed_reference_data,
        unsupported_falloff: light.falloff.is_some(),
        suppressed_flags: light.flags & (LIGHT_FLAG_NEGATIVE | LIGHT_FLAG_OFF_BY_DEFAULT) != 0,
        transform: LIGHT_TRANSFORM,
    }
}

/// Cumulative spawn-time coverage, including references with no emitted PointLight.
/// Counts are dispositions, not mutually exclusive; unload/reload counts a new evaluation.
#[derive(Resource, Default, Debug)]
pub struct LightInputCoverage {
    pub evaluated: u64,
    pub legacy_normalization: u64,
    pub legacy_fade: u64,
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
        coverage.legacy_normalization += 1;
        coverage.legacy_fade += u64::from(light.legacy_fade);
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

/// How many of the spawned lights are enabled at once. Bevy's clustered forward renderer draws
/// every enabled light in a cluster it reaches, and the game's spaces hold whole halls of `LIGH`
/// references, so the far ones are switched off rather than paid for.
pub const ENABLED_LIGHT_BUDGET: usize = 64;

/// How far the camera moves before the enabled lights are chosen again. Re-choosing is a sort of
/// every spawned light, so it is not done per frame.
pub const BUDGET_RECHOOSE_DISTANCE: f32 = 256.0;

/// Convenience entrypoint; streaming also retains the evaluation on the reference entity.
pub fn point_light(light: &LightRow, radius_offset: Option<f32>) -> Option<PointLight> {
    evaluate_light(light, radius_offset).point_light()
}

/// Maximum backend range for both base radius and signed XRDS result. Preserve the authored
/// result on LightEvaluation and clamp explicitly instead of silently substituting base radius.
pub const MAX_RADIUS_OVERRIDE: f32 = 8_192.0;

/// Bounded compatibility lumens for unit fade. The normalization is frozen at b5259d33 and is
/// not a native calibration. Bevy divides lumens by 4*PI; the diffuse BRDF adds another 1/PI.
pub fn intensity_for_radius(radius: f32) -> f32 {
    if !radius.is_finite() || radius <= 0.0 {
        return 0.0;
    }
    let reference = (radius * 0.5).min(INTENSITY_REFERENCE_RADIUS);
    4.0 * core::f32::consts::PI
        * core::f32::consts::PI
        * LEGACY_HALF_RADIUS_RADIANCE
        * reference
        * reference
        / HALF_RADIUS_WINDOW
}

/// A [`PointLight`] that came from a Skyrim `LIGH` reference.
///
/// The budget and the tests tell the game's lights from the engine's own by this marker: a
/// `PointLight` without it is never enabled or budgeted by [`budget_lights`], which keeps the
/// budget from touching a light a fixture or a future engine feature adds.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkyrimLight {
    /// The reference that carries the light (`REFR` form id).
    pub form_id: u32,
    /// The cell that reference belongs to.
    pub cell_id: u32,
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
    chosen: HashSet<Entity>,
}

/// Keeps the number of enabled [`SkyrimLight`]s to [`ENABLED_LIGHT_BUDGET`] nearest the camera.
///
/// Add it after [`StreamingPlugin`](crate::streaming::StreamingPlugin).
pub struct LightsPlugin;

/// Converted light data retained on an unlit reference, so enabling lights works in resident cells.
#[derive(Component)]
pub(crate) struct DeferredPointLight {
    pub light: PointLight,
    pub marker: SkyrimLight,
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
            .add_systems(PostUpdate, budget_lights.after(TransformSystems::Propagate));
    }
}

/// Enables the [`ENABLED_LIGHT_BUDGET`] lights nearest the camera, and hides the rest.
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
    if let Some(settings) = settings.as_deref() {
        for (_, _, mut light, _) in &mut lights {
            if light.shadow_maps_enabled != settings.point_light_shadows {
                light.shadow_maps_enabled = settings.point_light_shadows;
            }
        }
    }
    let Ok(camera) = camera.single() else {
        return;
    };
    let camera_position = camera.translation();
    // One pass over every spawned light, allocating nothing: how many there are, and whether any of
    // them is not in the set the current selection was made from.
    //
    // The second half is what the count alone cannot answer. A swap that keeps the number of lights
    // in the world the same is invisible to a count - a cell unloading as another streams in with
    // as many lights - and the player standing still is the case the budget exists for: a room
    // walked into and stopped in has to light up.
    let mut spawned = 0usize;
    let mut joined = false;
    for (entity, _, _, _) in lights.iter() {
        spawned += 1;
        joined |= !budget.chosen.contains(&entity);
    }
    if !joined
        && let Some(chosen_at) = budget.chosen_at
        && budget.chosen.len() == spawned
        && chosen_at.distance(camera_position) <= BUDGET_RECHOOSE_DISTANCE
    {
        return;
    }

    let mut ranked: Vec<(Entity, f32)> = lights
        .iter()
        .map(|(entity, transform, _, _)| {
            (
                entity,
                transform.translation().distance_squared(camera_position),
            )
        })
        .collect();
    sort_by_distance(&mut ranked);
    let enabled: HashSet<Entity> = ranked
        .iter()
        .take(
            settings
                .as_deref()
                .map_or(ENABLED_LIGHT_BUDGET, |settings| settings.point_light_budget),
        )
        .map(|(entity, _)| *entity)
        .collect();

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
    budget.chosen = ranked.iter().map(|(entity, _)| *entity).collect();
    if let Some((nearest, distance_squared)) = ranked.first()
        && let Ok((_, _, light, _)) = lights.get(*nearest)
    {
        debug!(
            spawned,
            enabled = enabled.len(),
            nearest_distance = distance_squared.sqrt(),
            nearest_range = light.range,
            nearest_intensity = light.intensity,
            "lights: budget re-chosen"
        );
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

    #[test]
    fn resident_deferred_lights_enable_disable_and_reenable_without_reloading() {
        let mut app = budget_app(Vec3::ZERO, []);
        app.insert_resource(crate::config::EngineConfig::default())
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
                    },
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
        }
    }

    /// Bevy's own point light attenuation, written out from the engine it runs in rather than from
    /// this module's derivation: `bevy_pbr/src/render/light.rs` divides a point light's intensity
    /// by `4*PI` (lumens to lumens per steradian) before it reaches the shader, and
    /// `Fd_Burley` in `bevy_pbr/src/render/pbr_lighting.wgsl` supplies the Lambert `1/PI`. The
    /// range window is `getRangeFalloff`'s.
    ///
    /// Both `PI`s belong here. This helper - and the derivation it restated - once carried only
    /// the Lambert one, which made the test pass while the lights delivered `4*PI` less than the
    /// constant they were aimed at.
    fn illuminance(light: &PointLight, distance: f32) -> f32 {
        let factor = distance * distance / (light.range * light.range);
        let window = (1.0 - factor * factor).max(0.0);
        light.intensity * window * window
            / (4.0 * core::f32::consts::PI * core::f32::consts::PI * distance * distance)
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

    #[test]
    fn compatibility_normalization_is_frozen_independently_of_environment_policy() {
        // Baseline b5259d33: independent numeric regression oracle, not the ambient constant.
        let light = point_light(&light_row(512.0, 0), None).unwrap();
        assert!((light.intensity / 40_157_850_311.0 - 1.0).abs() < 1e-6);
        assert!((illuminance(&light, 256.0) / 13_641.851 - 1.0).abs() < 1e-6);
    }

    /// The scale is per radius, so the surface brightness at half a light's radius does not depend
    /// on how big the record is: four times the radius is sixteen times the intensity.
    /// The light's own colour sits on the light's side of the comparison, exactly as the ambient's
    /// colour sits on its own: the intensity scale is colour-free, and a warm light of the same
    /// intensity puts less luminance on a surface than a white one of the same radius would.
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
        assert!(
            (warm.intensity - white.intensity).abs() < 1.0,
            "the scale is per radius, not per colour: {} and {}",
            warm.intensity,
            white.intensity
        );
        assert!(
            luminance(warm.color) < luminance(white.color),
            "and the warm light is the dimmer of the two on a surface"
        );
    }

    /// The inverse-square law, from the outside: twice the radius is four times the intensity, so a
    /// surface at half of either radius receives the same illuminance. It stops at the cap.
    #[test]
    fn intensity_follows_the_square_of_the_radius() {
        let quarter = intensity_for_radius(256.0);
        let half = intensity_for_radius(512.0);
        assert!((half / quarter - 4.0).abs() < 1.0e-4, "{quarter} -> {half}");
        assert!(
            intensity_for_radius(512.0) > 1.0e7,
            "a metre-scale intensity would not reach across a 512-unit room: {}",
            intensity_for_radius(512.0)
        );
        // And it stops at the cap: twice the radius is no longer four times the intensity.
        assert_eq!(intensity_for_radius(1024.0), intensity_for_radius(512.0));
    }

    /// The cap has to be invisible to every light up to the widest common radius: at or below
    /// `2 * INTENSITY_REFERENCE_RADIUS` the intensity is exactly the uncapped formula, to the bit,
    /// so no torch, lamp or brazier moves. Above it the light keeps the intensity of the largest
    /// uncapped one.
    #[test]
    fn the_reference_cap_leaves_every_radius_up_to_512_exactly_as_it_was() {
        let uncapped = |radius: f32| {
            let half_radius = radius * 0.5;
            (4.0 * core::f32::consts::PI * core::f32::consts::PI * LEGACY_HALF_RADIUS_RADIANCE)
                * half_radius
                * half_radius
                / HALF_RADIUS_WINDOW
        };
        for radius in [0.5, 64.0, 128.0, 147.7, 256.0, 330.0, 511.0, 512.0] {
            assert_eq!(
                intensity_for_radius(radius),
                uncapped(radius),
                "a {radius}-unit light is byte-identical to the uncapped formula"
            );
        }
        assert_eq!(
            INTENSITY_REFERENCE_RADIUS * 2.0,
            512.0,
            "the cap is half of the widest common radius, which is what makes the line above cover \
             every light that ever moved"
        );
        for radius in [512.5, 1024.0, 3300.0, 4334.0] {
            assert_eq!(
                intensity_for_radius(radius),
                intensity_for_radius(512.0),
                "a {radius}-unit light is lit like the largest uncapped one"
            );
            assert!(intensity_for_radius(radius) < uncapped(radius));
        }
    }

    /// The light the cap exists for. `000D9051` `FalmerCityLight02NS` is Blackreach's one huge
    /// light - radius 4334 at (2122,9014,4668), colour (216,128,39) - and with the intensity sized
    /// from its own radius it comes out 71 times a 512-unit torch, which washes out the cavern
    /// ceiling. It keeps its reach, and its colour is untouched: what it loses is the 71x.
    #[test]
    fn the_biggest_shipped_light_keeps_its_reach_without_the_brightness() {
        let falmer_city_light = LightRow {
            radius: 4334.0,
            color: [216, 128, 39],
            flags: 0,
            ..light_row(4334.0, 0)
        };
        let big = point_light(&falmer_city_light, None).unwrap();
        let torch = point_light(&light_row(512.0, 0), None).unwrap();
        assert_eq!(big.range, 4334.0, "the record's reach is unchanged");
        assert_eq!(
            big.intensity, torch.intensity,
            "and it is lit at a torch's intensity, not 71 of them"
        );
        assert!(
            big.range > torch.range * 8.0,
            "while still reaching eight times further than a torch: {}",
            big.range
        );
        assert_eq!(
            big.color,
            Color::srgb_u8(216, 128, 39),
            "its colour is the record's, orange as it is: what overwhelmed the space was the \
             intensity, not the colour"
        );
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
        // A metre-scale intensity would not reach across a 512-unit room.
        assert!(
            (light.intensity - intensity_for_radius(512.0)).abs() < 1.0,
            "{}",
            light.intensity
        );
    }

    #[test]
    fn negative_and_off_by_default_lights_spawn_nothing() {
        assert!(point_light(&light_row(512.0, LIGHT_FLAG_NEGATIVE), None).is_none());
        assert!(point_light(&light_row(512.0, LIGHT_FLAG_OFF_BY_DEFAULT), None).is_none());
        assert!(point_light(&light_row(512.0, 0x0001 | 0x0008), None).is_some());
        assert!(
            point_light(&light_row(0.0, 0), None).is_none(),
            "a light with no radius lights nothing"
        );
        assert!(point_light(&light_row(f32::NAN, 0), None).is_none());
        assert!(point_light(&light_row(-64.0, 0), None).is_none());
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
        assert!(
            (overridden.intensity / intensity_for_radius(1106.8) - 1.0).abs() < 1.0e-5,
            "the intensity follows the radius that is used: {}",
            overridden.intensity
        );

        assert_eq!(point_light(&record, Some(-100.0)).unwrap().range, 156.0);
        assert_eq!(point_light(&record, Some(0.0)).unwrap().range, 256.0);
        for offset in [-256.0, -300.0, f32::NAN, f32::INFINITY] {
            assert!(point_light(&record, Some(offset)).is_none());
        }
    }

    #[test]
    fn backend_range_limit_preserves_authored_radius_and_never_substitutes_base() {
        let record = light_row(256.0, 0);
        let evaluated = evaluate_light(&record, Some(25_074.0));
        assert_eq!(evaluated.authored_radius, Some(25_330.0));
        assert_eq!(evaluated.point_light().unwrap().range, MAX_RADIUS_OVERRIDE);
        assert!(evaluated.backend_limited);
        assert_eq!(
            point_light(&light_row(20_000.0, 0), None).unwrap().range,
            MAX_RADIUS_OVERRIDE
        );
    }

    /// A light budgeted around a camera, as `budget_lights` sees them.
    fn budget_app(camera: Vec3, light_positions: impl IntoIterator<Item = Vec3>) -> App {
        let mut app = App::new();
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
}
