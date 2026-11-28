//! SPEC-2 section 11, worked examples E0 to E3, through the engine's composite, bloom weights,
//! adaptation and tonemap. All values are the specification's invented examples. Example E4 (the
//! blend modes) is a test of `nif_depth::retail_blend_factors`.
#![allow(
    clippy::excessive_precision,
    reason = "the expected values are the specification's printed six decimals"
)]

use super::OUTPUT_SCALE;
use crate::{
    retail_hdr::{HdrBlock, MENU_ADAPT_TIMER, RetailHdr, RetailHdrConfig, adapt_rates, adapt_step},
    retail_hdr_chain::{bloom_radius, bloom_taps},
};
use bevy::prelude::{Vec3, Vec4, default};

const TOLERANCE: f32 = 3.0e-6;

#[track_caller]
fn close(actual: [f32; 3], expected: [f32; 3], what: &str) {
    for (a, e) in actual.into_iter().zip(expected) {
        assert!(
            (a - e).abs() <= TOLERANCE,
            "{what}: expected {expected:?}, got {actual:?}"
        );
    }
}

#[track_caller]
fn near(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= TOLERANCE,
        "{what}: expected {expected}, got {actual}"
    );
}

fn to_bytes(colour: [f32; 3]) -> [u8; 3] {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a saturated channel times 255 is in 0..=255"
    )]
    colour.map(|c| (255.0 * c.clamp(0.0, 1.0)).round() as u8)
}

/// A block from the example's `speed, radius, Tb, scale, receive, White, sun, sky, strength,
/// saturation, brightness, contrast, tint amount, tint RGB` order.
fn block(values: [f32; 16]) -> HdrBlock {
    let [
        speed,
        radius,
        threshold,
        scale,
        receive,
        white,
        sun,
        sky,
        strength,
        saturation,
        brightness,
        contrast,
        amount,
        r,
        g,
        b,
    ] = values;
    HdrBlock {
        eye_adapt_speed: speed,
        bloom_blur_radius: radius,
        bloom_threshold: threshold,
        bloom_scale: scale,
        receive_bloom_threshold: receive,
        white,
        sunlight_scale: sun,
        sky_scale: sky,
        eye_adapt_strength: strength,
        saturation,
        brightness,
        contrast,
        tint_amount: amount,
        tint_rgb: [r, g, b],
        depth_of_field: [0.0, 0.0, 0.0, 2.0],
    }
}

#[test]
#[expect(
    clippy::approx_constant,
    reason = "SPEC-2 example E0 gives the red tint as 0.301020"
)]
fn example_e0_the_weather_block_is_the_weighted_sum_of_all_fields() {
    let current_day = block([
        3.0, 5.0, 0.80, 0.90, 0.80, 1.00, 1.6, 1.0, 3.5, 1.10, 1.05, 1.10, 0.10, 1.0, 0.9, 0.8,
    ]);
    let current_sunset = block([
        3.0, 5.0, 0.70, 1.10, 0.80, 1.20, 1.9, 1.0, 3.5, 1.20, 1.00, 1.15, 0.20, 1.0, 0.8, 0.6,
    ]);
    let previous_day = block([
        2.0, 3.0, 0.90, 0.60, 0.70, 1.00, 1.4, 1.0, 3.0, 0.90, 1.10, 1.00, 0.0, 0.0, 0.0, 0.0,
    ]);
    let previous_sunset = block([
        2.0, 3.0, 0.90, 0.60, 0.70, 1.10, 1.5, 1.0, 3.0, 0.90, 1.05, 1.00, 0.0, 0.0, 0.0, 0.0,
    ]);
    let (q, t) = (0.75_f32, 0.8_f32);
    let mixed = HdrBlock::blend(&[
        (q * t, current_day),
        (q * (1.0 - t), current_sunset),
        ((1.0 - q) * t, previous_day),
        ((1.0 - q) * (1.0 - t), previous_sunset),
    ]);
    let expected = block([
        2.75, 4.5, 0.81, 0.855, 0.775, 1.035, 1.6, 1.0, 3.375, 1.065, 1.0525, 1.0825, 0.09, 0.75,
        0.66, 0.57,
    ]);
    for (actual, wanted) in [
        (mixed.eye_adapt_speed, expected.eye_adapt_speed),
        (mixed.bloom_blur_radius, expected.bloom_blur_radius),
        (mixed.bloom_threshold, expected.bloom_threshold),
        (mixed.bloom_scale, expected.bloom_scale),
        (
            mixed.receive_bloom_threshold,
            expected.receive_bloom_threshold,
        ),
        (mixed.white, expected.white),
        (mixed.sunlight_scale, expected.sunlight_scale),
        (mixed.sky_scale, expected.sky_scale),
        (mixed.eye_adapt_strength, expected.eye_adapt_strength),
        (mixed.saturation, expected.saturation),
        (mixed.brightness, expected.brightness),
        (mixed.contrast, expected.contrast),
        (mixed.tint_amount, expected.tint_amount),
    ] {
        near(actual, wanted, "blended field");
    }
    close(mixed.tint_rgb, expected.tint_rgb, "tint");
    assert_eq!(bloom_radius(mixed.bloom_blur_radius), 4);
    use crate::retail_image_space::{ImageSpaceModifier, WeightedColour, process_frame};
    let mut modifier = ImageSpaceModifier {
        weight: 0.5,
        tint: WeightedColour {
            amount: 0.4,
            rgb: Vec3::new(0.2, 0.4, 1.0),
        },
        ..default()
    };
    modifier.multiply[9] = 0.5;
    let (working, _) = process_frame(mixed, &[modifier]);
    near(working.saturation, 0.798_75, "modified saturation");
    near(working.tint_amount, 0.4, "maximum tint amount");
    close(
        working.tint_rgb,
        [0.301_020, 0.447_755, 0.921_020],
        "normalised tint",
    );
}

/// The composite of one opaque pixel: `sat(k((1 - Fc) AO O + Fc Cfc))`.
fn composite(o: [f32; 3], ao: f32, fc: f32, cfc: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| {
        crate::retail_fog::composite_saturated(o[i], ao, fc, cfc[i], OUTPUT_SCALE)
    })
}

/// The two bloom passes of a neighbourhood where `inside(u, v)` says which level-0 texels hold
/// `c`; the others are below the threshold.
fn bloom(c: [f32; 3], settings: &HdrBlock, inside: impl Fn(i32, i32) -> bool) -> [f32; 3] {
    let radius = bloom_radius(settings.bloom_blur_radius);
    let taps = bloom_taps(radius);
    let radius = i32::try_from(radius).unwrap();
    let weight = |u: i32| taps[usize::try_from(u + 7).unwrap()];
    let k = OUTPUT_SCALE;
    std::array::from_fn(|i| {
        let bright = (c[i] / k - settings.bloom_threshold).max(0.0);
        let mut sum = 0.0;
        for u in -radius..=radius {
            let mut vertical = 0.0;
            for v in -radius..=radius {
                if inside(u, v) {
                    vertical += weight(v) * bright;
                }
            }
            sum += weight(u) * k * settings.bloom_scale * vertical;
        }
        sum
    })
}

#[test]
fn example_e1_an_exterior_noon_pixel_to_bytes() {
    let settings = block([
        3.0, 4.5, 0.8, 0.8, 1.2, 1.0, 1.0, 1.0, 3.5, 1.1, 1.0, 1.05, 0.1, 1.0, 0.9, 0.8,
    ]);
    let config = RetailHdrConfig::default();
    let c0 = composite(
        [0.966_026, 0.680_193, 0.437_308],
        0.9,
        0.225_018,
        [0.570_587, 0.657_842, 0.753_922],
    );
    close(c0, [0.668_483, 0.518_708, 0.395_551], "composite");
    // One additive draw, mode 2.
    assert_eq!(crate::nif_depth::retail_blend_factors([6, 0]), [6, 0]);
    let source = Vec3::new(0.4, 0.35, 0.25);
    let c = (Vec3::from_array(c0) + source * 0.5).to_array();
    close(c, [0.868_483, 0.693_708, 0.520_551], "after the draw");
    let flat = bloom(c, &settings, |_, _| true);
    close(flat, [0.161_453, 0.021_633, 0.0], "flat bloom");
    close(
        bloom(c, &settings, |u, v| u == 0 && v == 0),
        [0.006_730, 0.000_902, 0.0],
        "single-texel bloom",
    );

    let t = 1.0 / 60.0;
    let (rx, ry) = adapt_rates(&settings, &config, t);
    near(rx, 0.021_663, "r_x");
    near(ry, 0.030_464, "r_y");
    let mut pair = [0.40_f32, 0.40];
    let mut first = pair;
    for frame in 1..=60 {
        pair = [adapt_step(pair[0], 0.55, rx), adapt_step(pair[1], 0.55, ry)];
        if frame == 1 {
            first = pair;
            near(pair[0], 0.403_906, "x takes the floor");
            near(pair[1], 0.404_570, "y");
        }
        let ratio = pair[1] / pair[0];
        match frame {
            1 => near(ratio, 1.001_642, "ratio 1"),
            10 => near(ratio, 1.004_498, "ratio 10"),
            30 => near(ratio, 1.003_819, "ratio 30"),
            60 => near(ratio, 1.0, "ratio 60"),
            _ => {}
        }
    }

    let pass = |config: &RetailHdrConfig| RetailHdr::from_block(&settings, config, t);
    let frame_1 = pass(&config).display_value(c, first, flat);
    close(frame_1, [0.915_095, 0.661_623, 0.475_159], "frame 1");
    assert_eq!(to_bytes(frame_1), [233, 169, 121]);
    let steady = [0.55, 0.55];
    let converged = pass(&config).display_value(c, steady, flat);
    close(converged, [0.906_642, 0.653_293, 0.467_078], "converged");
    assert_eq!(to_bytes(converged), [231, 167, 119]);
    let filmic = RetailHdrConfig {
        filmic: true,
        ..config
    };
    let filmic = pass(&filmic).display_value(c, steady, flat);
    close(filmic, [1.0, 0.779_790, 0.562_889], "filmic");
    assert_eq!(to_bytes(filmic), [255, 199, 144]);

    let slider = RetailHdrConfig {
        gamma: RetailHdrConfig::default().gamma_for_slider(0.75),
        ..config
    };
    let slider = pass(&slider).display_value(c, steady, flat);
    close(slider, [0.921_574, 0.701_332, 0.530_263], "fGamma 1.2");
    assert_eq!(to_bytes(slider), [235, 179, 135]);
    let mut fading = pass(&config);
    fading.curve.w = 0.25;
    fading.fade = Vec4::ZERO;
    let fading = fading.display_value(c, steady, flat);
    assert_eq!(to_bytes(fading), [173, 125, 89]);
    let plain = pass(&config).display_value(c0, steady, [0.0; 3]);
    close(
        plain,
        [0.630_356, 0.478_818, 0.353_262],
        "no draw, no bloom",
    );
    assert_eq!(to_bytes(plain), [161, 122, 90]);
    let sky = pass(&config).display_value([0.526_144, 0.640_523, 0.828_431], steady, [0.0; 3]);
    close(sky, [0.488_917, 0.593_671, 0.769_733], "sky pixel");
    assert_eq!(to_bytes(sky), [125, 151, 196]);
}

#[test]
fn example_e2_an_interior_pixel_to_bytes() {
    let settings = block([
        3.0, 4.0, 0.6, 0.5, 0.6, 0.8, 1.0, 1.0, 3.5, 1.0, 1.0, 1.2, 0.0, 0.0, 0.0, 0.0,
    ]);
    let config = RetailHdrConfig::default();
    let c = composite(
        [0.961_737, 0.589_320, 0.368_039],
        0.9,
        0.142_857,
        [0.168_067, 0.207_283, 0.246_499],
    );
    close(c, [0.638_268, 0.403_525, 0.265_942], "composite");
    let single = bloom(c, &settings, |u, v| u == 0 && v == 0);
    close(single, [0.002_882, 0.0, 0.0], "single-texel bloom");
    let t = 1.0 / 60.0;
    let (rx, ry) = adapt_rates(&settings, &config, t);
    let pair = [adapt_step(0.2, 0.3, rx), adapt_step(0.2, 0.3, ry)];
    near(pair[0], 0.203_906, "x");
    near(pair[1], 0.203_906, "y");
    let pass = RetailHdr::from_block(&settings, &config, t);
    let output = pass.display_value(c, pair, single);
    close(output, [0.794_097, 0.486_789, 0.306_912], "output");
    assert_eq!(to_bytes(output), [202, 124, 78]);
    // The first frame: the pair is the current average.
    let first = pass.display_value(c, [0.3, 0.3], single);
    close(first, [0.774_878, 0.467_571, 0.287_693], "first frame");
    assert_eq!(to_bytes(first), [198, 119, 73]);
}

#[test]
fn example_e3_the_menu_pause_step() {
    let settings = HdrBlock {
        eye_adapt_speed: 3.0,
        eye_adapt_strength: 3.5,
        ..HdrBlock::IMGS_CONSTRUCTOR_DEFAULT
    };
    let (rx, ry) = adapt_rates(&settings, &RetailHdrConfig::default(), MENU_ADAPT_TIMER);
    near(rx, 0.325_797, "r_x");
    near(ry, 0.427_005, "r_y");
    near(adapt_step(0.40, 0.55, rx), 0.448_870, "x");
    near(adapt_step(0.40, 0.55, ry), 0.464_051, "y");
}
