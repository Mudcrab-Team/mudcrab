//! Clock/climate driven directional light geometry; shading and the sky palette stay separate.
//!
//! The sun path is the retail one, traced in SkyrimSE.exe 1.7.104.0 `0x14041bd90` (registry
//! RF-SUN-001 to RF-SUN-004, `rea-20261114/q4-sun.md`). Every value is read from the pack: the five
//! sun GMSTs (RF-SUN-003) and the worldspace climate TNAM (RF-SUN-001). A GMST the pack lacks takes
//! the retail exe default (`EXE_SUN_DEFAULTS`), so the same retail formula still runs. A world with
//! no climate timing has no sunrise or sunset: it gets no sun path and `app::sun_rotation` gives its
//! sun no authored direction (identity rotation).

use crate::sun_shadow::{SunShadowDirection, SunShadowSettings};
use crate::{config::EngineConfig, streaming::ActiveSpace, world::environment::EnvironmentCatalog};
use bevy::prelude::*;

/// A single frame that advances the game clock by at least this many game seconds (15 game
/// minutes) counts as a time jump, so shadows refresh at once.
const IMMEDIATE_REFRESH_GAME_SECONDS: f64 = 900.0;

/// Retail `GameHour` (GLOB record, form id 0x38, editor id `GameHour`): 8.0 in the registered pack
/// data. It is the default start hour and the replacement for a non-finite hour. The matching
/// `TimeScale` record (GLOB 0x3A, editor id `TimeScale`) is 20.0; see `EngineConfig::timescale`.
pub const RETAIL_GAME_HOUR: f64 = 8.0;

/// GMST names in `SunSettings` order; the same names may be explicitly overridden in `[Sky]` INI
/// files (an engine extension; retail reads only the GMSTs). RF-SUN-003.
pub const SUN_KEYS: [&str; 5] = [
    "fSunXExtreme",
    "fSunYExtreme",
    "fSunZExtreme",
    "fSunDirXExtreme",
    "fSunAlphaTransTime",
];

/// The five sun GMSTs, read from the pack (RF-SUN-003):
/// `[0]` `fSunXExtreme`, the east-west half-span of the sun path (Skyrim.esm 400, GMST 0x12FC9);
/// `[1]` `fSunYExtreme`, the constant north offset of the path (40, 0x12FCB);
/// `[2]` `fSunZExtreme`, the vertical component of the light direction (-150, 0x12FCA);
/// `[3]` `fSunDirXExtreme`, the scale of the horizontal light direction (200, 0x93FA8);
/// `[4]` `fSunAlphaTransTime`, the fade window in hours around the sunrise and sunset midpoints
/// (0.5, 0x10FDC3). These are the sun path itself. The Skyrim.esm numbers above are for reference;
/// the code reads the pack and uses `EXE_SUN_DEFAULTS` only for a GMST the pack lacks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SunSettings(pub [f32; 5]);

/// The retail exe defaults of the five sun settings, used only for a GMST the pack lacks (Skyrim.esm
/// values win over them; RF-SUN-003, `rea-20261114/q4-sun.md` GMST table). The data addresses were
/// confirmed against the exe settings dump (record addresses 0x14209e940, 0x14209e958, 0x14209e970,
/// 0x14209e988, 0x14209e9a0; value at record + 8) and match the PR #214 research note
/// `skyrim-native-sun-trajectory-20261010`:
/// `fSunXExtreme` 400 (0x14209E960), `fSunYExtreme` 25 (0x14209E978), `fSunZExtreme` -100
/// (0x14209E990), `fSunDirXExtreme` 400 (0x14209E9A8), `fSunAlphaTransTime` 2.0 (0x14209E948).
pub const EXE_SUN_DEFAULTS: SunSettings = SunSettings([400.0, 25.0, -100.0, 400.0, 2.0]);

impl SunSettings {
    /// Validate each setting before accepting it from optional records or INI input.
    /// Bounds avoid zero division/degenerate vectors and leave ample room for modded paths.
    pub fn valid(index: usize, value: f32) -> bool {
        value.is_finite()
            && match index {
                0 | 2 => (0.001..=1_000_000.0).contains(&value.abs()),
                1 | 3 => value.abs() <= 1_000_000.0,
                4 => (0.0..24.0).contains(&value),
                _ => false,
            }
    }

    /// Take each value from a validated explicit override, else the pack's GMST, else the retail
    /// exe default of that setting (`EXE_SUN_DEFAULTS`).
    pub fn resolve(pack: [Option<f32>; 5], overrides: [Option<f32>; 5]) -> Self {
        let mut values = EXE_SUN_DEFAULTS.0;
        for (index, value) in values.iter_mut().enumerate() {
            if let Some(found) = overrides[index]
                .filter(|v| Self::valid(index, *v))
                .or(pack[index].filter(|v| Self::valid(index, *v)))
            {
                *value = found;
            }
        }
        Self(values)
    }
}

/// Climate TNAM boundaries (sunrise begin, sunrise end, sunset begin, sunset end) in ten-minute
/// units, so one unit is 1/6 hour (RF-SUN-001, `0x141869dd8`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SunTiming(pub [u8; 4]);

impl SunTiming {
    /// Accept ordered same-day transitions; a malformed climate gets no sun path and warns at load.
    pub fn valid(self) -> bool {
        let [a, b, c, d] = self.0;
        a <= b
            && b < c
            && c <= d
            && d <= 144
            && u16::from(c) + u16::from(d) - u16::from(a) - u16::from(b) < 288
    }
}

/// The day window of the retail sun update (RF-SUN-001): sunrise and sunset midpoints, the fade
/// half-width `T/2` (`T` = `fSunAlphaTransTime`), `A = riseMid - T/2` and `B = setMid + T/2`.
struct SunDay {
    rise_mid: f64,
    set_mid: f64,
    half_trans: f64,
    a: f64,
    b: f64,
}

impl SunDay {
    fn new(timing: SunTiming, settings: SunSettings) -> Option<Self> {
        if !timing.valid() || (0..5).any(|i| !SunSettings::valid(i, settings.0[i])) {
            return None;
        }
        let [rise_begin, rise_end, set_begin, set_end] = timing.0.map(|v| f64::from(v) / 6.0);
        let rise_mid = (rise_begin + rise_end) * 0.5;
        let set_mid = (set_begin + set_end) * 0.5;
        let half_trans = f64::from(settings.0[4]) * 0.5;
        let (a, b) = (rise_mid - half_trans, set_mid + half_trans);
        // The window must be a proper part of one day.
        ((0.0..24.0).contains(&(b - a)) && b > a).then_some(Self {
            rise_mid,
            set_mid,
            half_trans,
            a,
            b,
        })
    }

    /// Hours since `A`, wrapped to `[0, 24)`.
    fn elapsed(&self, hour: f64) -> f64 {
        let hour = if hour.is_finite() {
            hour
        } else {
            RETAIL_GAME_HOUR
        };
        (hour - self.a).rem_euclid(24.0)
    }
}

/// The sun's east-west position `x` in `[-X, X]` (RF-SUN-001). By day `u` runs 0..1 from `A` to
/// `B` and `x = (1 - 2u) * X` (east to west). At night `u` runs 0..1 from `B` back to `A` and
/// `x = (2u - 1) * X` (west to east).
fn sun_x(day: &SunDay, hour: f64, x_extreme: f64) -> f64 {
    let elapsed = day.elapsed(hour);
    let length = day.b - day.a;
    if elapsed < length {
        (1.0 - 2.0 * elapsed / length) * x_extreme
    } else {
        (2.0 * (elapsed - length) / (24.0 - length) - 1.0) * x_extreme
    }
}

/// Unit vector toward the sun in engine +Y-up space; light rays travel in the opposite direction.
///
/// Retail light direction (RF-SUN-002, `0x14041c49d` to `0x14041c540`): the direction the light
/// travels is `normalize(-(DirX/X) * x, -(DirX/X) * Y, Z)`; this function returns its negation.
/// The light keeps this geometry at night, so it never drops below the sunrise elevation; the
/// night look comes from the weather's night colours, not from this vector. `None` when an input
/// is invalid. A non-finite hour counts as the retail `GameHour` record (GLOB 0x38).
pub fn sun_direction(hour: f64, timing: SunTiming, settings: SunSettings) -> Option<Vec3> {
    let day = SunDay::new(timing, settings)?;
    let [x_extreme, y_extreme, z_extreme, dir_x_extreme, _] = settings.0.map(f64::from);
    let x = sun_x(&day, hour, x_extreme);
    let scale = dir_x_extreme / x_extreme;
    let toward_sun = bevy::math::DVec3::new(scale * x, scale * y_extreme, -z_extreme);
    let direction = toward_sun.try_normalize()?;
    Some(Vec3::from_array(
        shared::coordinates::creation_to_runtime_vector(direction.as_vec3().to_array()),
    ))
}

/// Sun visibility alpha (RF-SUN-004, `0x14041bd90`): 0 outside `[A, B]`, a linear ramp from 0 to 1
/// over `riseMid` +- `T/2`, 1 between the ramps, and a ramp from 1 to 0 over `setMid` +- `T/2`.
/// The visible sun disc and its glare (alpha times the weather byte at `+0x668` over 255) are not
/// part of this change, so nothing consumes this value yet.
pub fn sun_alpha(hour: f64, timing: SunTiming, settings: SunSettings) -> Option<f32> {
    let day = SunDay::new(timing, settings)?;
    let elapsed = day.elapsed(hour);
    let length = day.b - day.a;
    if elapsed > length {
        return Some(0.0);
    }
    let hour = day.a + elapsed;
    let width = 2.0 * day.half_trans;
    let alpha = if width <= 0.0 {
        1.0
    } else if hour < day.rise_mid + day.half_trans {
        (hour - (day.rise_mid - day.half_trans)) / width
    } else if hour > day.set_mid - day.half_trans {
        ((day.set_mid + day.half_trans) - hour) / width
    } else {
        1.0
    };
    Some(alpha.clamp(0.0, 1.0) as f32)
}

/// Game hour and game seconds per real second. Capture/benchmark runs freeze this at launch.
#[derive(Resource, Debug, Clone, Copy)]
pub struct GameClock {
    pub hour: f64,
    pub timescale: f64,
}

impl GameClock {
    /// Start at the requested hour; automated captures must not drift while streaming settles.
    pub fn from_config(config: &EngineConfig) -> Self {
        let frozen = config.shots.is_some()
            || config.acceptance_screenshot.is_some()
            || config.is_benchmark_run()
            || config.benchmark_only
            || config.matched_route.is_some();
        Self {
            hour: config.game_hour,
            timescale: if frozen { 0.0 } else { config.timescale },
        }
    }

    /// Advance smoothly using double precision, including a clean midnight wrap.
    pub fn advance(&mut self, seconds: f64) {
        self.hour = (self.hour + seconds * self.timescale / 3600.0).rem_euclid(24.0);
    }
}

/// Marks the world's sun so fixture and other directional lights are never rotated accidentally.
#[derive(Component)]
#[require(SunShadowDirection)]
pub struct SunLight;

/// The sun rotation of a world that has no sun path: no authored direction exists, so the light
/// keeps Bevy's own orientation and nothing is invented. Retail worlds always carry climate
/// timing, so this is not retail behaviour (a fixture and benchmark path). Setup and the runtime
/// update both use it, so lighting never depends on travel history.
pub const NO_SUN_ROTATION: Quat = Quat::IDENTITY;

/// The climate form id, timing and settings that decide the active world's sun path: the pack's
/// five GMSTs (retail exe defaults for a missing GMST) with explicit INI keys applied. `None` means
/// the world has no climate timing, so no sunrise or sunset exists.
fn world_sun_inputs(
    config: &EngineConfig,
    catalog: Option<&EnvironmentCatalog>,
    worldspace: u32,
) -> Option<(u32, SunTiming, SunSettings)> {
    let catalog = catalog?;
    let climate = catalog.worldspace_climate(worldspace)?;
    let timing = catalog.sun_timing(climate)?;
    let settings = SunSettings::resolve(catalog.sun_settings(), config.sun_settings_overrides);
    Some((climate, timing, settings))
}

/// Resolve the active world's sun direction. `None` means no sun path applies: the world has no
/// climate timing, or `sun_path_problem` reports an incompatible fade.
pub fn world_sun_direction(
    config: &EngineConfig,
    hour: f64,
    catalog: Option<&EnvironmentCatalog>,
    worldspace: u32,
) -> Option<Vec3> {
    let (_, timing, settings) = world_sun_inputs(config, catalog, worldspace)?;
    sun_direction(hour, timing, settings)
}

/// A climate whose timing cannot hold the resolved `fSunAlphaTransTime`: the fade window
/// `A = riseMid - T/2` to `B = setMid + T/2` must be shorter than 24 hours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SunPathProblem {
    pub climate: u32,
    pub transition: f32,
    pub window_hours: f64,
}

/// Report why a world with climate timing gets no sun path. No recovery value is invented: the
/// world keeps `NO_SUN_ROTATION` and the caller warns.
pub fn sun_path_problem(
    config: &EngineConfig,
    catalog: Option<&EnvironmentCatalog>,
    worldspace: u32,
) -> Option<SunPathProblem> {
    let (climate, timing, settings) = world_sun_inputs(config, catalog, worldspace)?;
    if SunDay::new(timing, settings).is_some() {
        return None;
    }
    let [rise_begin, rise_end, set_begin, set_end] = timing.0.map(|v| f64::from(v) / 6.0);
    let transition = settings.0[4];
    Some(SunPathProblem {
        climate,
        transition,
        window_hours: (set_begin + set_end) * 0.5 - (rise_begin + rise_end) * 0.5
            + f64::from(transition),
    })
}

/// Last clock and path inputs, used to recognize travel and explicit time changes immediately.
struct SunFrame {
    hour: f64,
    worldspace: u32,
    path: Option<(u32, SunTiming, SunSettings)>,
    shadow_settings: SunShadowSettings,
}

/// Suns the game clock turns. Sky modes 1 and 2 hold the sun with the cell key light
/// (RF-LIGHT-CPU-011), so a sun with `ExteriorSunState` keeps its interior rotation
/// (`world::environment::interior`).
type ClockDrivenSun = (
    With<SunLight>,
    Without<crate::world::environment::interior::ExteriorSunState>,
);

/// Per-driver history and one-time diagnostics.
#[derive(Default)]
pub(crate) struct SunUpdateState {
    problem: Option<SunPathProblem>,
    previous: Option<SunFrame>,
    transition_reported: bool,
    /// Counts discontinuities (time, world or path changes). Each sun records the count it last
    /// applied, so a sun the interior lighting held during a change refreshes when it returns.
    refreshes: u64,
}

/// Advance continuous illumination and the independent stepped shadow basis before rendering.
/// No allocations or database access occur per frame.
///
/// The clock advances by the delta of `Res<Time>`, which in `Update` is virtual time. Bevy clamps
/// a virtual-time delta to its default maximum (250 ms) and no production code changes that limit,
/// so a frame slower than 4 FPS advances the clock slower than real time. A pause of virtual time
/// (the console, the settings menu) stops the clock. Retail behaviour is not established.
pub(crate) fn update_sun(
    time: Res<Time>,
    mut clock: ResMut<GameClock>,
    config: Res<EngineConfig>,
    catalog: Option<Res<EnvironmentCatalog>>,
    space: Option<Res<ActiveSpace>>,
    mut sun: Query<(&mut Transform, &mut SunShadowDirection), ClockDrivenSun>,
    mut state: Local<SunUpdateState>,
) {
    let before = clock.hour;
    clock.advance(time.delta_secs_f64());
    if !state.transition_reported {
        info!(
            "sun shadows: fSunUpdateThreshold={} degrees (engine units), \
             fSunShadowUpdateTime={} seconds, fSunShadowMaxHoldSeconds={} (engine extension)",
            config.sun_shadow.threshold_degrees,
            config.sun_shadow.transition_seconds,
            config.sun_shadow.max_hold_seconds
        );
        if config.sun_shadow.transition_seconds > 0.0 {
            // The default value is expected, so it is only informational. A value the user set
            // has no effect yet, so it is worth a warning.
            let message = "sun shadows: fSunShadowUpdateTime is reserved for cross-fading; \
                           shadow transitions are not implemented yet, so shadows snap";
            if config.sun_shadow.transition_seconds
                == SunShadowSettings::default().transition_seconds
            {
                info!("{message}");
            } else {
                warn!("{message}");
            }
        }
        state.transition_reported = true;
    }
    if config.sun_position.is_some() {
        for (transform, mut shadow) in &mut sun {
            shadow.update(transform.rotation, config.sun_shadow, 0.0, true);
        }
        state.previous = None;
        return;
    }
    let worldspace = space.as_deref().map_or(config.worldspace_id, |s| {
        s.exterior_worldspace(config.worldspace_id)
    });
    let path = world_sun_inputs(&config, catalog.as_deref(), worldspace);
    let direction =
        path.and_then(|(_, timing, settings)| sun_direction(clock.hour, timing, settings));
    let rotation = match direction {
        Some(direction) => {
            state.problem = None;
            Quat::from_rotation_arc(Vec3::Z, direction)
        }
        None => {
            // Report an incompatible fade once per distinct problem, never every frame.
            let problem = sun_path_problem(&config, catalog.as_deref(), worldspace);
            if problem != state.problem {
                if let Some(problem) = problem {
                    warn!(
                        "sun: fSunAlphaTransTime={} gives a {:.2}-hour day window with climate {:#x}; \
                         the window must be shorter than 24 hours, so this world gets no sun path \
                         (no recovery value is invented)",
                        problem.transition, problem.window_hours, problem.climate
                    );
                }
                state.problem = problem;
            }
            NO_SUN_ROTATION
        }
    };
    let immediate = state.previous.as_ref().is_none_or(|previous| {
        previous.hour != before
            || previous.worldspace != worldspace
            || previous.path != path
            || previous.shadow_settings.threshold_degrees != config.sun_shadow.threshold_degrees
            || previous.shadow_settings.max_hold_seconds != config.sun_shadow.max_hold_seconds
    }) || (time.delta_secs_f64() * clock.timescale).abs()
        >= IMMEDIATE_REFRESH_GAME_SECONDS;
    if immediate {
        state.refreshes = state.refreshes.wrapping_add(1);
    }
    let moving_seconds = if before == clock.hour {
        0.0
    } else {
        time.delta_secs_f64()
    };
    for (mut transform, mut shadow) in &mut sun {
        let refresh = shadow.refreshes_applied != state.refreshes;
        shadow.refreshes_applied = state.refreshes;
        shadow.update(rotation, config.sun_shadow, moving_seconds, refresh);
        if transform.rotation != rotation {
            transform.rotation = rotation;
        }
    }
    state.previous = Some(SunFrame {
        hour: clock.hour,
        worldspace,
        path,
        shadow_settings: config.sun_shadow,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Skyrim.esm sun GMSTs (RF-SUN-003; GMST 0x12FC9, 0x12FCB, 0x12FCA, 0x93FA8, 0x10FDC3) and the
    /// `DefaultClimate`/`SkyrimClimate` TNAM (CLMT 0x15F, 0x812). Test data only: the code under
    /// test takes both from the pack.
    const ESM: SunSettings = SunSettings([400.0, 40.0, -150.0, 200.0, 0.5]);
    const CLIMATE: SunTiming = SunTiming([33, 60, 96, 123]);
    /// `A = riseMid - T/2` and `B = setMid + T/2` for the data above (RF-SUN-001).
    const A: f64 = 7.5;
    const B: f64 = 18.5;

    fn elevation_degrees(direction: Vec3) -> f32 {
        direction.y.asin().to_degrees()
    }

    fn catalog(settings: [Option<f32>; 5]) -> EnvironmentCatalog {
        EnvironmentCatalog::with_sun_for_tests(
            EngineConfig::default().worldspace_id,
            1,
            CLIMATE,
            settings,
        )
    }

    const ESM_VALUES: [Option<f32>; 5] = [
        Some(400.0),
        Some(40.0),
        Some(-150.0),
        Some(200.0),
        Some(0.5),
    ];

    /// Exercise the scheduled clock advance rather than only the standalone arithmetic helper.
    #[test]
    fn runtime_driver_advances_hour_across_midnight() {
        let mut app = App::new();
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_secs_f64(3.6));
        app.insert_resource(time)
            .insert_resource(EngineConfig::default())
            .insert_resource(catalog(ESM_VALUES))
            .insert_resource(GameClock {
                hour: 23.99,
                timescale: 20.0,
            })
            .add_systems(Update, update_sun);
        let light = app.world_mut().spawn((SunLight, Transform::default())).id();
        app.update();
        assert!((app.world().resource::<GameClock>().hour - 0.01).abs() < 1e-10);
        let direction = app.world().get::<Transform>(light).unwrap().back();
        assert!(direction.is_finite());
    }

    /// The actual scheduled system rotates only the marked sun and respects explicit overrides.
    #[test]
    fn runtime_driver_tracks_clock_and_preserves_override() {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(EngineConfig::default())
            .insert_resource(catalog(ESM_VALUES))
            .insert_resource(GameClock {
                hour: 9.0,
                timescale: 0.0,
            })
            .add_systems(Update, update_sun);
        let light = app.world_mut().spawn((SunLight, Transform::default())).id();
        let other = app.world_mut().spawn(Transform::default()).id();
        app.update();
        let at_nine = app.world().get::<Transform>(light).unwrap().rotation;
        app.world_mut().resource_mut::<GameClock>().hour = 17.0;
        app.update();
        let at_seventeen = app.world().get::<Transform>(light).unwrap().rotation;
        assert!((at_nine * Vec3::Z).distance(at_seventeen * Vec3::Z) > 1.0);
        assert_eq!(
            app.world().get::<Transform>(other).unwrap().rotation,
            Quat::IDENTITY
        );
        app.world_mut().resource_mut::<EngineConfig>().sun_position = Some((61.0, 74.0));
        app.world_mut().resource_mut::<GameClock>().hour = 12.0;
        app.update();
        assert_eq!(
            app.world().get::<Transform>(light).unwrap().rotation,
            at_seventeen
        );
    }

    /// A pack with a climate but without the sun GMSTs runs the same formula with the retail exe
    /// defaults. Reproduced by hand at A (6.75 h: `riseMid` 7.75 - T/2 with T = 2): `x = 400`,
    /// `scale = 400 / 400 = 1`, so the toward-sun vector is `(400, 25, 100) / 413.07` in Creation
    /// space and `(x, z, -y)` = (0.96836, 0.24209, -0.06052) in engine axes.
    #[test]
    fn a_pack_without_the_gmsts_runs_the_exe_default_path() {
        let direction = sun_direction(6.75, CLIMATE, EXE_SUN_DEFAULTS).unwrap();
        let length = (400.0_f32.powi(2) + 25.0_f32.powi(2) + 100.0_f32.powi(2)).sqrt();
        assert!(direction.distance(Vec3::new(400.0, 100.0, -25.0) / length) < 1e-6);
        assert!(direction.distance(Vec3::new(0.968_36, 0.242_09, -0.060_52)) < 1e-4);
        assert_eq!(SunSettings::resolve([None; 5], [None; 5]), EXE_SUN_DEFAULTS);
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(EngineConfig::default())
            .insert_resource(catalog([None; 5]))
            .insert_resource(GameClock {
                hour: 6.75,
                timescale: 0.0,
            })
            .add_systems(Update, update_sun);
        let light = app.world_mut().spawn((SunLight, Transform::default())).id();
        app.update();
        let rotation = app.world().get::<Transform>(light).unwrap().rotation;
        assert!((rotation * Vec3::Z).distance(direction) < 1e-5);
    }

    /// A world with no climate timing has no sunrise or sunset: no sun path applies and the sun
    /// gets the no-direction rotation, whatever rotation it had before.
    #[test]
    fn a_world_without_a_climate_timing_gets_the_no_direction_rotation() {
        for environment in [None, Some(EnvironmentCatalog::default())] {
            let mut app = App::new();
            app.insert_resource(Time::<()>::default())
                .insert_resource(EngineConfig::default())
                .insert_resource(GameClock {
                    hour: 9.0,
                    timescale: 0.0,
                })
                .add_systems(Update, update_sun);
            if let Some(environment) = environment {
                app.insert_resource(environment);
            }
            let start = Quat::from_rotation_y(0.7);
            let light = app
                .world_mut()
                .spawn((SunLight, Transform::from_rotation(start)))
                .id();
            app.update();
            assert_eq!(
                app.world().get::<Transform>(light).unwrap().rotation,
                NO_SUN_ROTATION
            );
        }
    }

    /// Leaving a timed world for an untimed one gives the same no-direction rotation as starting
    /// there, so lighting does not depend on travel history. An explicit `sun_position` still wins.
    #[test]
    fn entering_a_world_without_timing_resets_the_sun_rotation() {
        let timed = EngineConfig::default().worldspace_id;
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(EngineConfig::default())
            .insert_resource(catalog(ESM_VALUES))
            .insert_resource(GameClock {
                hour: 9.0,
                timescale: 0.0,
            })
            .add_systems(Update, update_sun);
        let light = app.world_mut().spawn((SunLight, Transform::default())).id();
        app.update();
        let rotation = |app: &App| app.world().get::<Transform>(light).unwrap().rotation;
        assert_ne!(rotation(&app), NO_SUN_ROTATION);
        app.world_mut().resource_mut::<EngineConfig>().worldspace_id = timed + 1;
        app.update();
        assert_eq!(rotation(&app), NO_SUN_ROTATION);
        app.world_mut().resource_mut::<EngineConfig>().worldspace_id = timed;
        app.update();
        assert_ne!(rotation(&app), NO_SUN_ROTATION);
        let explicit = &mut app.world_mut().resource_mut::<EngineConfig>();
        explicit.worldspace_id = timed + 1;
        explicit.sun_position = Some((61.0, 74.0));
        let before = rotation(&app);
        app.update();
        assert_eq!(rotation(&app), before);
    }

    /// With the climate timing 33/60/96/123, `fSunAlphaTransTime = 14` gives a 24.5-hour window
    /// (`18.25 - 7.75 + 14`). It passes the per-value check, so the world-level check reports it
    /// and the world gets no sun path; a compatible value reports nothing.
    #[test]
    fn an_incompatible_fade_is_reported_not_silently_dropped() {
        let environment = catalog(ESM_VALUES);
        let mut config = EngineConfig::default();
        config.sun_settings_overrides[4] = Some(14.0);
        assert!(SunSettings::valid(4, 14.0));
        let worldspace = config.worldspace_id;
        let problem = sun_path_problem(&config, Some(&environment), worldspace).unwrap();
        assert_eq!(problem.transition, 14.0);
        assert!((problem.window_hours - 24.5).abs() < 1e-9);
        assert!(world_sun_direction(&config, 12.0, Some(&environment), worldspace).is_none());
        let config = EngineConfig::default();
        assert!(sun_path_problem(&config, Some(&environment), worldspace).is_none());
        assert!(world_sun_direction(&config, 12.0, Some(&environment), worldspace).is_some());
    }

    /// RF-SUN-002: elevation is 36.7 degrees at A and at B and 82.4 degrees at the 13:00 midpoint.
    /// RF-SUN-001: `x = 400` at `u = 0` (east, toward +X) and `x = -400` at `u = 1`.
    #[test]
    fn retail_elevation_at_the_window_ends_and_at_the_midpoint() {
        let at_a = sun_direction(A, CLIMATE, ESM).unwrap();
        let at_b = sun_direction(B, CLIMATE, ESM).unwrap();
        let at_noon = sun_direction(13.0, CLIMATE, ESM).unwrap();
        assert!((elevation_degrees(at_a) - 36.7).abs() < 0.05, "{at_a:?}");
        assert!((elevation_degrees(at_b) - 36.7).abs() < 0.05, "{at_b:?}");
        assert!(
            (elevation_degrees(at_noon) - 82.4).abs() < 0.05,
            "{at_noon:?}"
        );
        // Runtime X is east: the sun rises in the east and sets in the west.
        assert!(at_a.x > 0.0 && at_b.x < 0.0 && at_noon.x.abs() < 1e-6);
        let day = SunDay::new(CLIMATE, ESM).unwrap();
        assert!((day.a - A).abs() < 1e-9 && (day.b - B).abs() < 1e-9);
        assert!((sun_x(&day, A, 400.0) - 400.0).abs() < 1e-9);
        assert!((sun_x(&day, B, 400.0) + 400.0).abs() < 1e-9);
        assert!(sun_x(&day, 13.0, 400.0).abs() < 1e-9);
    }

    /// RF-SUN-001: the path has a constant north offset of `fSunYExtreme` (positive: north).
    /// Creation +Y is north and maps to runtime -Z.
    #[test]
    fn the_path_tilts_north() {
        let at_noon = sun_direction(13.0, CLIMATE, ESM).unwrap();
        // normalize(0, 20, 150): 7.6 degrees to the north.
        assert!(at_noon.z < 0.0);
        assert!((at_noon.z + 20.0 / (20.0_f32.powi(2) + 150.0_f32.powi(2)).sqrt()).abs() < 1e-6);
        let mirrored = SunSettings([400.0, -40.0, -150.0, 200.0, 0.5]);
        assert!(sun_direction(13.0, CLIMATE, mirrored).unwrap().z > 0.0);
    }

    /// RF-SUN-002 at 12:00 (elevation 74.5 degrees, from the trace).
    #[test]
    fn noon_matches_the_traced_elevation_and_axis_mapping() {
        let direction = sun_direction(12.0, CLIMATE, ESM).unwrap();
        assert!((elevation_degrees(direction) - 74.5).abs() < 0.05);
        assert!(direction.distance(Vec3::new(0.233_646_62, 0.963_792_3, -0.128_505_65)) < 1e-6);
    }

    /// RF-SUN-001/-002: at night the same light runs from west to east and never drops below the
    /// 36.7 degree sunrise elevation.
    #[test]
    fn the_night_light_runs_west_to_east_above_the_sunrise_elevation() {
        let mut previous = f32::NEG_INFINITY;
        // From B (18.5 h) through midnight to A (7.5 h).
        for step in 0..=130 {
            let hour = B + f64::from(step) * 0.1;
            let direction = sun_direction(hour, CLIMATE, ESM).unwrap();
            assert!(direction.x >= previous - 1e-6, "x not increasing at {hour}");
            previous = direction.x;
            assert!(
                elevation_degrees(direction) >= 36.7 - 0.05,
                "below 36.7 at {hour}"
            );
        }
        assert!(sun_direction(B + 0.01, CLIMATE, ESM).unwrap().x < 0.0);
        assert!(sun_direction(A - 0.01, CLIMATE, ESM).unwrap().x > 0.0);
    }

    /// RF-SUN-004: alpha is 0 outside `[A, B]`, ramps over 0.25 h on each side of the sunrise and
    /// sunset midpoints (7.75 h and 18.25 h) and is 1 between.
    #[test]
    fn sun_alpha_ramps_over_a_quarter_hour_each_side_of_sunrise_and_sunset() {
        for (hour, expected) in [
            (3.0, 0.0),
            (7.5, 0.0),
            (7.625, 0.25),
            (7.75, 0.5),
            (7.875, 0.75),
            (8.0, 1.0),
            (13.0, 1.0),
            (18.0, 1.0),
            (18.125, 0.75),
            (18.25, 0.5),
            (18.375, 0.25),
            (18.5, 0.0),
            (23.0, 0.0),
        ] {
            let alpha = sun_alpha(hour, CLIMATE, ESM).unwrap();
            assert!(
                (alpha - expected).abs() < 1e-5,
                "alpha at {hour} was {alpha}"
            );
        }
    }

    /// An asymmetric synthetic climate/settings pair detects sign and midpoint mistakes.
    #[test]
    fn synthetic_day_sweep_has_independent_endpoints_and_midpoint() {
        let timing = SunTiming([18, 42, 108, 132]); // midpoints 5 and 20; expanded to 4 and 21
        let settings = SunSettings([200.0, -60.0, -120.0, 100.0, 2.0]);
        for (hour, expected) in [
            (4.0, Vec3::new(100.0, 120.0, 30.0)),
            (12.5, Vec3::new(0.0, 120.0, 30.0)),
            (21.0, Vec3::new(-100.0, 120.0, 30.0)),
            (0.5, Vec3::new(0.0, 120.0, 30.0)),
        ] {
            let direction = sun_direction(hour, timing, settings).unwrap();
            assert!(direction.distance(expected.normalize()) < 1e-6);
        }
    }

    /// Midnight and both day-window boundaries must be continuous; arbitrary hours wrap.
    #[test]
    fn path_is_continuous_and_periodic() {
        for timing in [
            CLIMATE,
            SunTiming([0, 6, 132, 143]),
            SunTiming([6, 12, 138, 144]),
        ] {
            for hour in [0.0, 5.0, 6.0, 12.0, 19.0, 23.999] {
                let a = sun_direction(hour, timing, ESM).unwrap();
                assert!(a.distance(sun_direction(hour + 24.0, timing, ESM).unwrap()) < 1e-6);
                assert!(a.distance(sun_direction(hour + 0.00001, timing, ESM).unwrap()) < 1e-4);
                assert!((a.length() - 1.0).abs() < 1e-6);
            }
        }
    }

    /// Override beats pack beats exe default; an invalid value is skipped for the next source.
    #[test]
    fn settings_resolve_override_then_pack_then_exe_default() {
        assert_eq!(SunSettings::resolve(ESM_VALUES, [None; 5]), ESM);
        let mut overrides = [None; 5];
        overrides[1] = Some(80.0);
        assert_eq!(SunSettings::resolve(ESM_VALUES, overrides).0[1], 80.0);
        let mut missing = ESM_VALUES;
        missing[0] = None;
        let resolved = SunSettings::resolve(missing, [None; 5]);
        assert_eq!(resolved.0[0], EXE_SUN_DEFAULTS.0[0]);
        assert_eq!(resolved.0[1], 40.0);
        let mut invalid = ESM_VALUES;
        invalid[4] = Some(24.0);
        let resolved = SunSettings::resolve(invalid, [None; 5]);
        assert_eq!(resolved.0[4], EXE_SUN_DEFAULTS.0[4]);
    }

    /// Malformed input never produces NaN or a degenerate rotation: it gives no direction.
    #[test]
    fn invalid_inputs_give_no_direction_and_a_bad_hour_stays_finite() {
        let bad = SunSettings([0.0, f32::NAN, 0.0, f32::MAX, 23.9]);
        for timing in [
            SunTiming([255, 0, 0, 255]),
            SunTiming([0, 1, 140, 143]),
            CLIMATE,
        ] {
            assert!(sun_direction(12.0, timing, bad).is_none());
        }
        assert!(sun_direction(12.0, SunTiming([255, 0, 0, 255]), ESM).is_none());
        for hour in [f64::NAN, f64::INFINITY, -23.0] {
            assert!(sun_direction(hour, CLIMATE, ESM).unwrap().is_normalized());
        }
    }

    /// A fractional tick survives midnight and captures cannot advance during settling.
    #[test]
    fn clock_advances_and_capture_runs_freeze() {
        let mut clock = GameClock {
            hour: 23.99,
            timescale: 20.0,
        };
        clock.advance(3.6);
        assert!((clock.hour - 0.01).abs() < 1e-10);
        let config = EngineConfig {
            shots: Some("synthetic.json".into()),
            game_hour: 9.25,
            ..Default::default()
        };
        let mut clock = GameClock::from_config(&config);
        clock.advance(1234.0);
        assert_eq!(clock.hour, 9.25);
    }

    /// `--benchmark-only` alone is not a benchmark run (no frame or duration limit), but it
    /// still renders the synthetic sun, so it must freeze the clock too.
    #[test]
    fn benchmark_only_alone_freezes_the_clock() {
        let config = EngineConfig {
            benchmark_only: true,
            game_hour: 9.25,
            timescale: 20.0,
            ..Default::default()
        };
        assert!(!config.is_benchmark_run());
        let mut clock = GameClock::from_config(&config);
        clock.advance(1234.0);
        assert_eq!(clock.hour, 9.25);
    }
}
