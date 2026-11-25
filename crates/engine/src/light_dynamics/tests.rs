use super::*;
use crate::{lights::evaluate_light, world::database::LightRow};
use core::f32::consts::{PI, TAU};

fn row(flags: u32, period: f32, intensity: f32, movement: f32) -> LightRow {
    LightRow {
        radius: 512.0,
        color: [255, 200, 120],
        flags,
        fade: Some(2.0),
        fade_status: Some(1),
        falloff: None,
        reference_data: None,
        radius_status: None,
        flicker_period: Some(period),
        flicker_intensity_amplitude: Some(intensity),
        flicker_movement_amplitude: Some(movement),
        reference_never_fades: false,
    }
}

fn dynamics(flags: u32, period: f32, intensity: f32, movement: f32) -> LightDynamics {
    LightDynamics::new(
        &evaluate_light(&row(flags, period, intensity, movement), None),
        7,
    )
}

#[test]
fn flags_select_the_motion() {
    assert_eq!(LightMotion::from_flags(0x8), LightMotion::Flicker);
    assert_eq!(LightMotion::from_flags(0x2009), LightMotion::Flicker);
    assert_eq!(LightMotion::from_flags(0x80), LightMotion::Pulse);
    assert_eq!(LightMotion::from_flags(0x100), LightMotion::Pulse);
    assert_eq!(LightMotion::from_flags(0x3081), LightMotion::Pulse);
    // Flicker Slow alone gets nothing, and so does an unrelated flag such as 0x1000.
    assert_eq!(
        LightMotion::from_flags(LIGHT_FLAG_FLICKER_SLOW),
        LightMotion::None
    );
    assert_eq!(LightMotion::from_flags(0x1001), LightMotion::None);
}

#[test]
fn the_dimmer_is_one_below_begin_zero_above_end_and_linear_between() {
    let fade = LightFade::new(3500.0, 4000.0);
    let radius = 256.0;
    let at = |distance: f32| distance_dimmer(distance * distance, radius, Some(&fade), true);
    // q = d^2 - R^2 against begin^2: the light's own radius counts.
    let q_begin = (3500.0f32.powi(2) + radius * radius).sqrt();
    assert_eq!(at(100.0), 1.0);
    assert_eq!(at(q_begin - 1.0), 1.0);
    let q_end = (4000.0f32.powi(2) + radius * radius).sqrt();
    assert_eq!(at(q_end + 1.0), 0.0);
    // Halfway in q (not in distance) between begin^2 and end^2 gives 0.5.
    let halfway_q = (3500.0f32.powi(2) + 4000.0f32.powi(2)) / 2.0;
    let d = (halfway_q + radius * radius).sqrt();
    assert!((at(d) - 0.5).abs() < 1.0e-5);
}

#[test]
fn an_end_of_zero_means_no_dimming_and_none_or_never_fades_means_one() {
    let zero_end = LightFade::new(3500.0, 0.0);
    assert_eq!(distance_dimmer(1.0e12, 256.0, Some(&zero_end), true), 1.0);
    let fade = LightFade::new(3500.0, 4000.0);
    assert_eq!(
        distance_dimmer(1.0e12, 256.0, Some(&fade), false),
        1.0,
        "Never Fades"
    );
    assert_eq!(
        distance_dimmer(1.0e12, 256.0, None, true),
        1.0,
        "no lighting data"
    );
}

#[test]
fn a_stored_zero_fade_is_replaced_by_the_settings() {
    let (start, range) = (3500.0, 500.0);
    // A stored 0/0 pair reads as 3500/4000.
    let fade = resolve_light_fade(Some((0.0, 0.0)), start, range);
    assert_eq!((fade.begin, fade.end), (3500.0, 4000.0));
    // Nonzero values stay.
    let fade = resolve_light_fade(Some((1000.0, 1500.0)), start, range);
    assert_eq!((fade.begin, fade.end), (1000.0, 1500.0));
    // A nonzero begin with a zero end gets 500 + 3500, below a begin above 4000.
    let fade = resolve_light_fade(Some((5000.0, 0.0)), start, range);
    assert_eq!((fade.begin, fade.end), (5000.0, 4000.0));
    // The dimmer of that pair is a step: 1 below the begin, 0 at and above it.
    assert_eq!(
        distance_dimmer(4999.0f32.powi(2), 0.0, Some(&fade), true),
        1.0
    );
    assert_eq!(
        distance_dimmer(5000.0f32.powi(2), 0.0, Some(&fade), true),
        0.0
    );
    assert_eq!(
        distance_dimmer(4500.0f32.powi(2), 0.0, Some(&fade), true),
        1.0
    );
    // The exterior, and INI overrides.
    let fade = resolve_light_fade(None, start, range);
    assert_eq!((fade.begin, fade.end), (3500.0, 4000.0));
    let fade = resolve_light_fade(None, 1000.0, 200.0);
    assert_eq!((fade.begin, fade.end), (1000.0, 1200.0));
    let fade = resolve_light_fade(Some((0.0, 0.0)), 1000.0, 200.0);
    assert_eq!((fade.begin, fade.end), (1000.0, 1200.0));
}

#[test]
fn pulse_runs_from_base_to_base_times_one_minus_amplitude() {
    for amplitude in [0.2, 0.5, 1.0] {
        assert!((pulse_factor(0.0, amplitude) - 1.0).abs() < 1.0e-6);
        assert!((pulse_factor(PI, amplitude) - (1.0 - amplitude)).abs() < 1.0e-6);
        assert!((pulse_factor(PI / 2.0, amplitude) - (1.0 - amplitude / 2.0)).abs() < 1.0e-6);
    }
    let offset = pulse_offset(PI / 4.0, 10.0);
    let (s, c) = (PI / 4.0).sin_cos();
    assert!((offset - 10.0 * Vec3::new(c, s, s * c)).length() < 1.0e-5);
    assert_eq!(pulse_offset(1.234, 0.0), Vec3::ZERO);
}

#[test]
fn a_pulse_advances_by_dt_times_period_and_wraps_once() {
    let mut light = dynamics(0x80, 2.0, 0.5, 4.0);
    light.advance(0.25);
    assert!((light.phase[0] - 0.5).abs() < 1.0e-6);
    // Fade = base x (1 - I + I cos P), with I = 0.25: base fade is 2.
    let expected = 2.0 * (1.0 - 0.25 + 0.25 * 0.5f32.cos());
    assert!((light.fade() - expected).abs() < 1.0e-5);
    // One subtraction of 2 pi, not a modulo.
    light.phase[0] = TAU - 0.1;
    light.advance(0.2);
    assert!((light.phase[0] - 0.3).abs() < 1.0e-5);
    // The translation follows the phase, in Creation axes.
    let creation = pulse_offset(light.phase[0], 4.0);
    assert!((light.offset() - creation).length() < 1.0e-5);
}

#[test]
fn table_sines_truncate_to_512_steps() {
    assert_eq!(table_sin(0.0), 0.0);
    let step = TAU / 512.0;
    // Inside one step the value does not change.
    assert_eq!(table_sin(0.1 * step), table_sin(0.9 * step));
    assert!((table_sin(step * 100.5) - (step * 100.0).sin()).abs() < 1.0e-6);
    // Wraps over 2 pi.
    assert!((table_sin(TAU + step * 10.2) - (step * 10.0).sin()).abs() < 1.0e-5);
}

#[test]
fn flicker_noise_stays_inside_its_range_and_the_factor_follows_it() {
    let mut low = f32::MAX;
    let mut high = f32::MIN;
    let steps = 64;
    for i in 0..steps {
        for j in 0..steps {
            for k in 0..steps {
                let n = flicker_noise(
                    TAU * i as f32 / steps as f32,
                    TAU * j as f32 / steps as f32,
                    TAU * k as f32 / steps as f32,
                );
                low = low.min(n);
                high = high.max(n);
            }
        }
    }
    assert!(
        low >= -0.2 - 1.0e-2 && high <= 0.5333 + 1.0e-2,
        "{low} {high}"
    );
    assert!(
        low < -0.15 && high > 0.5,
        "the range is reached: {low} {high}"
    );
    assert_eq!(flicker_factor(0.0, 1.0), 0.5);
    assert!((flicker_factor(0.5333, 0.5) - (1.0 - 0.25 + 0.25 * 0.5333)).abs() < 1.0e-6);
    assert_eq!(flicker_factor(0.3, 0.0), 1.0, "no amplitude, no flicker");
}

#[test]
fn a_flicker_moves_its_phases_and_keeps_its_fade_inside_the_amplitude_band() {
    let mut light = dynamics(0x8, 0.5, 0.6, 12.0);
    let mut changed = false;
    for _ in 0..600 {
        let before = light.fade();
        light.advance(1.0 / 60.0);
        changed |= (light.fade() - before).abs() > 1.0e-4;
        // base 2.0, I = 0.3, n in [-0.2, 0.5333]: factor = 1 - I + I n, in [0.64, 0.86].
        let factor = light.fade() / 2.0;
        assert!((1.0 - 0.3 - 0.3 * 0.2 - 1.0e-2..=1.0 - 0.3 + 0.3 * 0.5334).contains(&factor));
    }
    assert!(changed, "the fade flickers");
    assert_eq!(
        light.offset(),
        Vec3::ZERO,
        "STAND-IN(#1000): no flicker movement yet"
    );
}

#[test]
fn the_flicker_is_repeatable_for_one_seed_and_differs_between_seeds() {
    let run = |seed: u32| {
        let mut light = LightDynamics::new(&evaluate_light(&row(0x8, 0.5, 0.8, 0.0), None), seed);
        (0..30)
            .map(|_| {
                light.advance(0.016);
                light.fade()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(run(5), run(5));
    assert_ne!(run(5), run(6));
}

#[test]
fn a_modulated_fade_below_005_detaches_the_light_and_never_goes_negative() {
    let mut light = dynamics(0x80, 1.0, 1.0, 0.0);
    // Amplitude 1 reaches 0 at P = pi.
    light.phase[0] = PI - 0.001;
    light.advance(0.001);
    assert_eq!(light.fade(), 0.0);
    // Amplitude above 1 goes negative in the cycle: the backend clamps to 0.
    let mut light = dynamics(0x80, 1.0, 2.0, 0.0);
    light.phase[0] = PI - 0.001;
    light.advance(0.001);
    assert_eq!(light.fade(), 0.0);
    // An unmodulated light with a small authored fade stays (the rule is for modulated lights).
    let mut still = LightDynamics::new(
        &evaluate_light(
            &LightRow {
                fade: Some(0.01),
                ..row(0x1, 1.0, 0.0, 0.0)
            },
            None,
        ),
        1,
    );
    still.advance(0.1);
    assert!((still.fade() - 0.01).abs() < 1.0e-6);
}

#[test]
fn a_record_without_its_flicker_floats_or_with_slow_flicker_alone_does_not_move() {
    let mut old = LightDynamics::new(
        &evaluate_light(
            &LightRow {
                flicker_period: None,
                ..row(0x8, 1.0, 1.0, 1.0)
            },
            None,
        ),
        1,
    );
    old.advance(1.0);
    assert_eq!(old.fade(), 2.0);
    let mut slow = dynamics(LIGHT_FLAG_FLICKER_SLOW, 1.0, 1.0, 1.0);
    slow.advance(1.0);
    assert_eq!(slow.fade(), 2.0);
}

mod system {
    use super::*;
    use crate::lights::SkyrimLight;

    fn app_with(config: EngineConfig, camera: Vec3) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(config)
            .add_systems(Update, update_light_dynamics);
        app.world_mut().spawn((
            StreamingCamera,
            GlobalTransform::from(Transform::from_translation(camera)),
        ));
        app
    }

    fn spawn_light(app: &mut App, position: Vec3, flags: u32) -> Entity {
        let evaluation = evaluate_light(&row(flags, 1.0, 0.5, 8.0), None);
        let light = evaluation.point_light().unwrap();
        app.world_mut()
            .spawn((
                light,
                SkyrimLight {
                    form_id: 1,
                    cell_id: 2,
                    shadow_flagged: false,
                },
                LightDynamics::new(&evaluation, 1),
                Transform::default(),
                GlobalTransform::from(Transform::from_translation(position)),
            ))
            .id()
    }

    #[test]
    fn the_exterior_dimmer_uses_the_ini_settings_and_follows_the_camera() {
        // Start 1000, range 1000: begin 1000, end 2000. R = 512.
        let config = EngineConfig {
            light_lod_start_fade: 1000.0,
            light_lod_range: 1000.0,
            ..default()
        };
        let mut app = app_with(config, Vec3::ZERO);
        let near = spawn_light(&mut app, Vec3::new(100.0, 0.0, 0.0), 0x1);
        let far = spawn_light(&mut app, Vec3::new(5000.0, 0.0, 0.0), 0x1);
        let middle = spawn_light(&mut app, Vec3::new(1500.0, 0.0, 0.0), 0x1);
        app.update();
        let unit = retail_intensity(2.0);
        let intensity = |app: &App, e: Entity| app.world().get::<PointLight>(e).unwrap().intensity;
        assert_eq!(intensity(&app, near), unit);
        assert_eq!(intensity(&app, far), 0.0);
        let q = 1500.0f32.powi(2) - 512.0f32.powi(2);
        let expected = unit * (1.0 - (q - 1.0e6) / (4.0e6 - 1.0e6));
        assert!((intensity(&app, middle) - expected).abs() < expected * 1.0e-4);
    }

    #[test]
    fn flicker_updates_only_within_the_flickering_distance() {
        let config = EngineConfig {
            flickering_light_distance: 1024.0,
            ..default()
        };
        let mut app = app_with(config, Vec3::ZERO);
        let near = spawn_light(&mut app, Vec3::new(500.0, 0.0, 0.0), 0x8);
        let far = spawn_light(&mut app, Vec3::new(1500.0, 0.0, 0.0), 0x8);
        for _ in 0..30 {
            app.update();
        }
        let phase = |e: Entity| app.world().get::<LightDynamics>(e).unwrap().phase;
        assert!(
            phase(near).iter().any(|p| *p != 0.0),
            "the near light flickers"
        );
        assert_eq!(phase(far), [0.0; 3], "the far light keeps its last state");
    }

    #[test]
    fn a_pulse_moves_the_light_and_flicker_does_not() {
        let mut app = app_with(EngineConfig::default(), Vec3::ZERO);
        let pulse = spawn_light(&mut app, Vec3::new(100.0, 0.0, 0.0), 0x80);
        let flicker = spawn_light(&mut app, Vec3::new(100.0, 0.0, 0.0), 0x8);
        for _ in 0..10 {
            app.update();
        }
        assert_ne!(
            app.world().get::<Transform>(pulse).unwrap().translation,
            Vec3::ZERO
        );
        assert_eq!(
            app.world().get::<Transform>(flicker).unwrap().translation,
            Vec3::ZERO
        );
    }

    #[test]
    fn an_unmodulated_light_in_range_keeps_the_retail_unit() {
        let mut app = app_with(EngineConfig::default(), Vec3::ZERO);
        let steady = spawn_light(&mut app, Vec3::new(100.0, 0.0, 0.0), 0x1);
        app.update();
        let intensity = app.world().get::<PointLight>(steady).unwrap().intensity;
        assert_eq!(intensity, retail_intensity(2.0));
    }
}

#[test]
fn a_pulse_reaches_the_same_phase_at_any_frame_rate() {
    let mut slow = dynamics(0x80, 0.7, 0.5, 0.0);
    let mut fast = slow.clone();
    for _ in 0..60 {
        slow.advance(1.0 / 60.0);
    }
    for _ in 0..240 {
        fast.advance(1.0 / 240.0);
    }
    assert!((slow.phase[0] - 0.7).abs() < 1.0e-3, "{}", slow.phase[0]);
    assert!((slow.phase[0] - fast.phase[0]).abs() < 1.0e-3);
}

#[test]
fn a_wrap_keeps_what_is_left_over_one_turn() {
    assert!((wrap_once(TAU + 0.5) - 0.5).abs() < 1.0e-6);
    // One subtraction only: two turns leave one turn.
    assert!((wrap_once(2.0 * TAU + 0.5) - (TAU + 0.5)).abs() < 1.0e-5);
    assert_eq!(wrap_once(TAU), TAU);
}

#[test]
fn each_flicker_phase_advances_inside_its_rate_range() {
    let (dt, period) = (0.01, 0.5);
    let mut light = dynamics(0x8, period, 0.6, 0.0);
    let ranges = [(1.1, 13.1), (1.2, 13.2), (1.3, 19.3)];
    let mut highest = [0.0f32; 3];
    for _ in 0..2000 {
        let before = light.phase;
        light.advance(dt);
        for (slot, (low, high)) in ranges.into_iter().enumerate() {
            let rate = (light.phase[slot] - before[slot]).rem_euclid(TAU) / (dt * period);
            assert!(
                (low - 1.0e-3..=high + 1.0e-3).contains(&rate),
                "slot {slot}: {rate}"
            );
            highest[slot] = highest[slot].max(rate);
        }
    }
    // The third phase reaches rates the other two cannot.
    assert!(highest[2] > 14.0, "{highest:?}");
}

#[test]
fn flicker_noise_has_its_table_sine_values() {
    // Hand values from n = ((sin(1.3 B + 0.65) + 1) / 12)(sin(1.1 A + 1.87) + 1) + 0.2 sin(3 C + 0.9)
    // with each argument truncated to 2 pi / 512 steps.
    assert!((flicker_noise(0.0, 0.0, 0.0) - 0.416_371_5).abs() < 1.0e-5);
    assert!((flicker_noise(1.0, 2.0, 3.0) - -0.001_906_6).abs() < 1.0e-5);
}

#[test]
fn a_never_fades_reference_is_not_dimmed() {
    let mut source = row(0, 0.0, 0.0, 0.0);
    source.reference_never_fades = true;
    let light = LightDynamics::new(&evaluate_light(&source, None), 7);
    assert!(!light.distance_fade);
    assert!(dynamics(0, 0.0, 0.0, 0.0).distance_fade);
}
