//! Held sun-shadow geometry, independent of the continuously moving illumination direction.
//!
//! See `docs/specs/engine/sun-shadows.md` for the legacy INI meanings and engine policies.

use crate::sun::SunLight;
use bevy::{
    light::{
        CascadeShadowConfig, Cascades, DirectionalLightShadowMap, SimulationLightSystems,
        cascade::Cascade,
    },
    math::Vec3A,
    prelude::*,
};

/// Legacy sun-shadow inputs plus an explicitly engine-specific maximum hold.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SunShadowSettings {
    /// `[Display] fSunUpdateThreshold`; degrees are an engine choice (retail units unverified).
    pub threshold_degrees: f32,
    /// `[Display] fSunShadowUpdateTime`, in seconds; reserved for a future shadow cross-fade.
    pub transition_seconds: f32,
    /// `[Display] fSunShadowMaxHoldSeconds`, virtual seconds; an engine extension.
    pub max_hold_seconds: f32,
}

impl Default for SunShadowSettings {
    /// Retain the documented legacy numbers; the ten-second hold cap is an engine policy.
    fn default() -> Self {
        Self {
            threshold_degrees: 0.5,
            transition_seconds: 1.0,
            max_hold_seconds: 10.0,
        }
    }
}

/// Per-sun projection basis; the light transform remains the continuous illumination basis.
#[derive(Component, Default)]
pub(crate) struct SunShadowDirection {
    pub(crate) rotation: Quat,
    initialized: bool,
    seconds_held: f64,
    /// The sun driver's discontinuity count this sun last applied.
    pub(crate) refreshes_applied: u64,
}

impl SunShadowDirection {
    /// Snap on a threshold, moving-clock timeout, discontinuity, or first observation.
    pub(crate) fn update(
        &mut self,
        rotation: Quat,
        settings: SunShadowSettings,
        moving_seconds: f64,
        immediate: bool,
    ) {
        self.seconds_held += moving_seconds;
        // Compare directions, so quaternion twist and sign have no effect on the step trigger.
        let angle = (self.rotation * Vec3::Z).angle_between(rotation * Vec3::Z);
        if !self.initialized
            || immediate
            || angle >= settings.threshold_degrees.to_radians()
            || self.seconds_held >= f64::from(settings.max_hold_seconds)
        {
            self.rotation = rotation;
            self.initialized = true;
            self.seconds_held = 0.0;
        }
    }
}

/// Installs both the continuous sun driver and the shadow-only projection override.
pub(crate) struct SunShadowPlugin;

impl Plugin for SunShadowPlugin {
    /// Override Bevy's sun cascades before frusta, caster visibility, and render extraction.
    fn build(&self, app: &mut App) {
        app.add_systems(Update, crate::sun::update_sun).add_systems(
            PostUpdate,
            build_sun_shadow_cascades
                .after(SimulationLightSystems::UpdateDirectionalLightCascades)
                .before(SimulationLightSystems::UpdateLightFrusta),
        );
    }
}

/// Refit every active view to the held basis, retaining camera-motion texel snapping.
/// Other directional lights keep Bevy's own cascades. The sun's light transform, colour,
/// illuminance, shadow biases, and cascade distances retain their usual consumers.
fn build_sun_shadow_cascades(
    shadow_map: Res<DirectionalLightShadowMap>,
    views: Query<(Entity, &GlobalTransform, &Projection, &Camera)>,
    mut suns: Query<
        (
            &SunShadowDirection,
            &DirectionalLight,
            &CascadeShadowConfig,
            &mut Cascades,
        ),
        With<SunLight>,
    >,
) {
    for (shadow, light, config, mut cascades) in &mut suns {
        if !light.shadow_maps_enabled || !shadow.initialized {
            continue;
        }
        let world_from_light = Mat4::from_quat(shadow.rotation);
        let light_from_world = world_from_light.transpose();
        cascades.cascades.clear();
        for (view, transform, projection, camera) in &views {
            if !camera.is_active {
                continue;
            }
            let light_from_camera = light_from_world * transform.to_matrix();
            let near_bounds = [config.minimum_distance].into_iter().chain(
                config
                    .bounds
                    .iter()
                    .map(|far| (1.0 - config.overlap_proportion) * far),
            );
            let fitted = near_bounds
                .zip(&config.bounds)
                .map(|(near, far)| {
                    fit_cascade(
                        projection.get_frustum_corners(-near, -far),
                        shadow_map.size as f32,
                        world_from_light,
                        light_from_camera,
                    )
                })
                .collect();
            cascades.cascades.insert(view, fitted);
        }
    }
}

/// Fit and texel-snap a cascade with Bevy 0.19's reverse-Z construction.
/// Adapted from Bevy's `bevy_light/src/cascade.rs::calculate_cascade` under its MIT license,
/// <https://github.com/bevyengine/bevy/blob/v0.19.0/crates/bevy_light/src/cascade.rs>.
/// The permission notice is retained in `docs/specs/engine/sun-shadows.md`.
/// Keep this construction aligned with Bevy when upgrading the renderer.
fn fit_cascade(
    corners: [Vec3A; 8],
    map_size: f32,
    world_from_light: Mat4,
    light_from_camera: Mat4,
) -> Cascade {
    let mut min = Vec3A::splat(f32::MAX);
    let mut max = Vec3A::splat(f32::MIN);
    for corner in corners {
        let point = light_from_camera.transform_point3a(corner);
        min = min.min(point);
        max = max.max(point);
    }
    let diameter = (corners[0] - corners[6])
        .length_squared()
        .max((corners[4] - corners[6]).length_squared())
        .sqrt()
        .ceil();
    let texel_size = diameter / map_size;
    let centre = Vec3A::new(
        (0.5 * (min.x + max.x) / texel_size).floor() * texel_size,
        (0.5 * (min.y + max.y) / texel_size).floor() * texel_size,
        max.z,
    );
    let inverse = world_from_light.transpose();
    let cascade_from_world = Mat4::from_cols(
        inverse.x_axis,
        inverse.y_axis,
        inverse.z_axis,
        (-centre).extend(1.0),
    );
    let world_from_cascade = Mat4::from_cols(
        world_from_light.x_axis,
        world_from_light.y_axis,
        world_from_light.z_axis,
        world_from_light * centre.extend(1.0),
    );
    let clip_from_cascade = Mat4::from_cols(
        Vec4::new(2.0 / diameter, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 / diameter, 0.0, 0.0),
        Vec4::new(0.0, 0.0, (max.z - min.z).recip(), 0.0),
        Vec4::new(0.0, 0.0, 1.0, 1.0),
    );
    Cascade {
        world_from_cascade,
        clip_from_cascade,
        clip_from_world: clip_from_cascade * cascade_from_world,
        texel_size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::EngineConfig,
        sun::{GameClock, SunTiming},
        world::environment::EnvironmentCatalog,
    };
    use bevy::{
        camera::{primitives::CascadesFrusta, visibility::ViewVisibility},
        light::{cascade::build_directional_light_cascades, update_directional_light_frusta},
        transform::{TransformPlugin, TransformSystems},
    };
    use std::time::Duration;

    /// Build the production plugin with Bevy's actual cascade/frustum systems, without a GPU.
    fn fixture(config: EngineConfig) -> (App, Entity, Entity) {
        let mut app = App::new();
        let environment = EnvironmentCatalog::with_sun_for_tests(
            config.worldspace_id,
            1,
            SunTiming([33, 60, 96, 123]),
            [
                Some(400.0),
                Some(40.0),
                Some(-150.0),
                Some(200.0),
                Some(0.5),
            ],
        );
        app.insert_resource(Time::<()>::default())
            .insert_resource(GameClock::from_config(&config))
            .insert_resource(environment)
            .insert_resource(config)
            .insert_resource(DirectionalLightShadowMap { size: 2048 })
            .add_plugins((TransformPlugin, SunShadowPlugin))
            .add_systems(
                PostUpdate,
                (
                    build_directional_light_cascades
                        .in_set(SimulationLightSystems::UpdateDirectionalLightCascades)
                        .after(TransformSystems::Propagate),
                    update_directional_light_frusta
                        .in_set(SimulationLightSystems::UpdateLightFrusta),
                ),
            );
        let sun = app
            .world_mut()
            .spawn((
                SunLight,
                DirectionalLight {
                    shadow_maps_enabled: true,
                    ..default()
                },
                ViewVisibility::VISIBLE,
            ))
            .id();
        let camera = app
            .world_mut()
            .spawn((
                Camera::default(),
                Projection::Perspective(PerspectiveProjection::default()),
                Transform::from_xyz(127.0, 91.0, -53.0),
            ))
            .id();
        app.update();
        (app, sun, camera)
    }

    /// Advance the same virtual time resource used by the production sun driver.
    fn advance(app: &mut App, seconds: f64) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f64(seconds));
        app.update();
    }

    /// Read the independent held basis rather than the continuous light transform.
    fn shadow_rotation(app: &App, sun: Entity) -> Quat {
        app.world().get::<SunShadowDirection>(sun).unwrap().rotation
    }

    /// Read all extracted shadow projections for a view, including its far cascades.
    fn matrices(app: &App, sun: Entity, camera: Entity) -> Vec<Mat4> {
        app.world().get::<Cascades>(sun).unwrap().cascades[&camera]
            .iter()
            .map(|cascade| cascade.clip_from_world)
            .collect()
    }

    /// The original failure: a stationary camera's grid must stay fixed while the light moves.
    #[test]
    fn below_threshold_holds_shadow_rotation_and_all_cascades_while_lighting_moves() {
        let (mut app, sun, camera) = fixture(EngineConfig::default());
        let shadow = shadow_rotation(&app, sun);
        let before = matrices(&app, sun, camera);
        assert_eq!(before.len(), 4);
        let transform = app.world().get::<Transform>(sun).unwrap().rotation;
        for _ in 0..10 {
            advance(&mut app, 0.1);
            assert_eq!(shadow_rotation(&app, sun), shadow);
            assert_eq!(matrices(&app, sun, camera), before);
        }
        let light_rotation = app.world().get::<Transform>(sun).unwrap().rotation;
        assert_ne!(light_rotation, transform);
        assert!(
            app.world()
                .get::<GlobalTransform>(sun)
                .unwrap()
                .rotation()
                .abs_diff_eq(light_rotation, 1e-6)
        );
        let frusta = app.world().get::<CascadesFrusta>(sun).unwrap();
        assert_eq!(frusta.frusta[&camera].len(), 4);
    }

    /// A threshold crossing snaps immediately even with a long reserved transition duration.
    #[test]
    fn crossing_threshold_snaps_without_waiting_for_transition_time() {
        let config = EngineConfig {
            sun_shadow: SunShadowSettings {
                threshold_degrees: 0.1,
                transition_seconds: 500.0,
                max_hold_seconds: 1000.0,
            },
            ..default()
        };
        let (mut app, sun, camera) = fixture(config);
        let before = matrices(&app, sun, camera);
        let held = shadow_rotation(&app, sun);
        advance(&mut app, 0.1);
        assert_eq!(shadow_rotation(&app, sun), held);
        assert_eq!(matrices(&app, sun, camera), before);
        advance(&mut app, 4.0);
        assert_ne!(shadow_rotation(&app, sun), held);
        assert_ne!(matrices(&app, sun, camera), before);
        assert_eq!(
            shadow_rotation(&app, sun),
            app.world().get::<Transform>(sun).unwrap().rotation
        );
    }

    /// The engine-specific safety interval bounds lag even on a slowly moving path.
    #[test]
    fn maximum_hold_updates_below_the_angular_threshold() {
        let config = EngineConfig {
            sun_shadow: SunShadowSettings {
                threshold_degrees: 180.0,
                max_hold_seconds: 2.0,
                ..default()
            },
            ..default()
        };
        let (mut app, sun, _) = fixture(config);
        let held = shadow_rotation(&app, sun);
        advance(&mut app, 1.0);
        assert_eq!(shadow_rotation(&app, sun), held);
        advance(&mut app, 1.0);
        assert_ne!(shadow_rotation(&app, sun), held);
    }

    /// Explicit edits in either time direction and large clock advances bypass both holds.
    #[test]
    fn game_hour_changes_and_large_advances_snap_immediately() {
        let config = EngineConfig {
            sun_shadow: SunShadowSettings {
                threshold_degrees: 180.0,
                max_hold_seconds: 1000.0,
                ..default()
            },
            ..default()
        };
        let (mut app, sun, _) = fixture(config);
        for hour in [17.0, 9.0, 9.0001] {
            app.world_mut().resource_mut::<GameClock>().hour = hour;
            advance(&mut app, 0.0);
            assert_eq!(
                shadow_rotation(&app, sun),
                app.world().get::<Transform>(sun).unwrap().rotation
            );
        }
        app.world_mut().resource_mut::<GameClock>().timescale = 3600.0;
        let held = shadow_rotation(&app, sun);
        advance(&mut app, 0.5);
        assert_ne!(shadow_rotation(&app, sun), held);
        assert_eq!(
            shadow_rotation(&app, sun),
            app.world().get::<Transform>(sun).unwrap().rotation
        );
    }

    /// An hour change while the interior lighting holds the sun still refreshes its shadows once
    /// the sun returns, even with frozen time and a threshold that never triggers.
    #[test]
    fn a_change_while_the_sun_is_held_indoors_refreshes_on_return() {
        use crate::world::environment::interior::ExteriorSunState;
        let config = EngineConfig {
            sun_shadow: SunShadowSettings {
                threshold_degrees: 180.0,
                max_hold_seconds: 1000.0,
                ..default()
            },
            ..default()
        };
        let (mut app, sun, _) = fixture(config);
        app.world_mut().resource_mut::<GameClock>().hour = 8.0;
        advance(&mut app, 0.0);
        let morning = shadow_rotation(&app, sun);

        app.world_mut()
            .entity_mut(sun)
            .insert(ExteriorSunState::held_for_tests());
        app.world_mut().resource_mut::<GameClock>().hour = 17.0;
        advance(&mut app, 0.0);
        assert_eq!(
            shadow_rotation(&app, sun),
            morning,
            "a held sun keeps its basis"
        );

        app.world_mut().entity_mut(sun).remove::<ExteriorSunState>();
        advance(&mut app, 0.0);
        let evening = app.world().get::<Transform>(sun).unwrap().rotation;
        assert_ne!(evening, morning);
        assert_eq!(
            shadow_rotation(&app, sun),
            evening,
            "the returning sun applies the change made while it was held",
        );
    }

    /// Frozen shots have the same basis and matrices regardless of settle-frame count or deltas.
    #[test]
    fn captures_remain_deterministic_with_different_settle_times() {
        let config = EngineConfig {
            shots: Some("synthetic-shots.json".into()),
            ..default()
        };
        let (mut first, first_sun, first_camera) = fixture(config.clone());
        let (mut second, second_sun, second_camera) = fixture(config);
        for _ in 0..20 {
            advance(&mut first, 0.1);
        }
        advance(&mut second, 600.0);
        assert_eq!(first.world().resource::<GameClock>().hour, 8.0);
        assert_eq!(second.world().resource::<GameClock>().hour, 8.0);
        assert_eq!(
            shadow_rotation(&first, first_sun),
            shadow_rotation(&second, second_sun)
        );
        assert_eq!(
            matrices(&first, first_sun, first_camera),
            matrices(&second, second_sun, second_camera)
        );
    }

    /// Freezing an already moving clock keeps the held projection through arbitrarily long waits.
    #[test]
    fn freezing_time_preserves_an_existing_held_direction() {
        let (mut app, sun, camera) = fixture(EngineConfig::default());
        advance(&mut app, 0.1);
        let before = matrices(&app, sun, camera);
        app.world_mut().resource_mut::<GameClock>().timescale = 0.0;
        advance(&mut app, 1000.0);
        assert_eq!(matrices(&app, sun, camera), before);
    }

    /// Travel, a changed climate/GMST, and the untimed-world fallback bypass angular holds.
    #[test]
    fn changed_world_or_sun_path_refreshes_shadows_immediately() {
        let config = EngineConfig {
            sun_shadow: SunShadowSettings {
                threshold_degrees: 180.0,
                max_hold_seconds: 1000.0,
                ..default()
            },
            ..default()
        };
        let (mut app, sun, _) = fixture(config);
        let before = shadow_rotation(&app, sun);
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .sun_settings_overrides[1] = Some(-40.0);
        advance(&mut app, 0.0);
        assert_ne!(shadow_rotation(&app, sun), before);
        app.world_mut().resource_mut::<EngineConfig>().worldspace_id = 0x123;
        advance(&mut app, 0.0);
        assert_eq!(shadow_rotation(&app, sun), Quat::IDENTITY);
    }

    /// Colour, intensity, biases, camera tracking, and unmarked lights keep their own behavior.
    #[test]
    fn camera_motion_and_light_settings_remain_live() {
        let (mut app, sun, camera) = fixture(EngineConfig::default());
        let other = app
            .world_mut()
            .spawn(DirectionalLight {
                shadow_maps_enabled: true,
                ..default()
            })
            .id();
        let mut light = app.world_mut().get_mut::<DirectionalLight>(sun).unwrap();
        light.color = Color::srgb(0.2, 0.4, 0.8);
        light.illuminance = 321.0;
        let depth_bias = light.shadow_depth_bias;
        let normal_bias = light.shadow_normal_bias;
        let before = matrices(&app, sun, camera);
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation
            .x += 100.0;
        advance(&mut app, 0.1);
        assert_ne!(matrices(&app, sun, camera), before);
        let light = app.world().get::<DirectionalLight>(sun).unwrap();
        assert_eq!(light.color, Color::srgb(0.2, 0.4, 0.8));
        assert_eq!(light.illuminance, 321.0);
        assert_eq!(light.shadow_depth_bias, depth_bias);
        assert_eq!(light.shadow_normal_bias, normal_bias);
        assert_eq!(
            app.world().get::<Transform>(other).unwrap().rotation,
            Quat::IDENTITY
        );
        assert_eq!(matrices(&app, other, camera).len(), 4);
        app.world_mut().get_mut::<Camera>(camera).unwrap().is_active = false;
        advance(&mut app, 0.1);
        assert!(
            app.world()
                .get::<Cascades>(sun)
                .unwrap()
                .cascades
                .is_empty()
        );
    }

    /// The adapted projection math matches Bevy exactly when both bases are identical.
    #[test]
    fn fitted_cascades_match_bevy_for_the_same_direction() {
        let (mut app, sun, camera) = fixture(EngineConfig::default());
        // Supply the exact same quaternion to both builders. Bevy decomposes GlobalTransform,
        // whose matrix round-trip can round differently from the original local quaternion.
        let basis = app.world().get::<GlobalTransform>(sun).unwrap().rotation();
        app.world_mut()
            .get_mut::<SunShadowDirection>(sun)
            .unwrap()
            .rotation = basis;
        advance(&mut app, 0.0);
        let expected = matrices(&app, sun, camera);
        // Removing the marker leaves Bevy as the sole cascade writer for this light.
        app.world_mut().entity_mut(sun).remove::<SunLight>();
        advance(&mut app, 0.0);
        assert_eq!(matrices(&app, sun, camera), expected);
    }

    /// Reflection views and resized cascades refit with the same held shadow basis.
    #[test]
    fn every_active_view_and_changed_cascade_configuration_are_refitted() {
        let (mut app, sun, camera) = fixture(EngineConfig::default());
        let reflection = app
            .world_mut()
            .spawn((
                Camera::default(),
                Projection::Perspective(PerspectiveProjection::default()),
                Transform::from_xyz(127.0, -91.0, -53.0),
            ))
            .id();
        let held = shadow_rotation(&app, sun);
        app.world_mut()
            .insert_resource(DirectionalLightShadowMap { size: 4096 });
        app.world_mut().entity_mut(sun).insert(
            bevy::light::CascadeShadowConfigBuilder {
                num_cascades: 2,
                ..default()
            }
            .build(),
        );
        advance(&mut app, 0.1);
        assert_eq!(shadow_rotation(&app, sun), held);
        assert_eq!(matrices(&app, sun, camera).len(), 2);
        assert_eq!(matrices(&app, sun, reflection).len(), 2);
        assert_ne!(matrices(&app, sun, camera), matrices(&app, sun, reflection));
    }
}
