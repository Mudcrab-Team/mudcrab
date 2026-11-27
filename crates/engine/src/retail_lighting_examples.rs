//! SPEC-1 section 11, worked examples A to D, through the engine's own functions: the weather mix,
//! the sun path, the image-space mix, the fog composite and the HDR chain, with the pixel formulas
//! of `retail_lighting.rs`. All record values are the specification's invented examples.
//!
//! Normals and light directions are in Creation axes, as the specification states them.
#![allow(
    clippy::excessive_precision,
    reason = "the expected values are the specification's printed six decimals"
)]

use super::*;
use crate::{
    retail_hdr::{HdrBlock, RetailHdr, RetailHdrConfig, adapt_rates, adapt_step, weather_block},
    sun::{EXE_SUN_DEFAULTS, SunTiming, sun_direction},
    weather_sky::{WeatherSky, fog_day_weight, slot_weights},
    world::environment::{DirectionalAmbient, ImageSpace, TimeOfDay, Weather, WeatherFog},
};

const TOLERANCE: f32 = 2.0e-6;

#[track_caller]
fn close(actual: Vec3, expected: [f32; 3], what: &str) {
    let expected = Vec3::from_array(expected);
    assert!(
        (actual - expected).abs().max_element() <= TOLERANCE,
        "{what}: expected {expected}, got {actual}"
    );
}

#[track_caller]
fn near(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= TOLERANCE,
        "{what}: expected {expected}, got {actual}"
    );
}

fn bytes(r: u8, g: u8, b: u8) -> Vec3 {
    Vec3::new(f32::from(r), f32::from(g), f32::from(b)) / 255.0
}

fn linear(colour: Color) -> Vec3 {
    let colour = colour.to_linear();
    Vec3::new(colour.red, colour.green, colour.blue)
}

/// The common surface: diffuse (128,102,76), normal texel (128,128,255) with alpha 64 and vertex
/// colour (255,230,204), in an identity frame.
fn surface(specular: bool) -> SurfaceSample {
    let normal = (2.0 * bytes(128, 128, 255) - Vec3::ONE).normalize();
    let mask = 64.0 / 255.0;
    SurfaceSample {
        albedo: bytes(128, 102, 76) * bytes(255, 230, 204),
        normal,
        view: Vec3::Z,
        // Specular colour (1,1,1), strength 1, glossiness 30, fade 1.
        specular: specular.then_some((Vec3::splat(mask), 30.0)),
    }
}

/// Example A's interior pixel inputs.
fn interior_light(n: Vec3) -> LightSum {
    let faces = [
        bytes(40, 40, 50),
        bytes(40, 40, 50),
        bytes(30, 30, 40),
        bytes(50, 50, 60),
        bytes(20, 16, 12),
        bytes(60, 70, 90),
    ];
    // XCLL directional (80,90,110) at SunlightScale 2; XY 0 and Z 270 put the light overhead.
    let travel = crate::world::environment::interior::interior_key_travel(0, 270);
    // Runtime (x, y, z) is Creation (x, -z, y).
    let toward = Vec3::new(-travel.x, travel.z, -travel.y);
    let colour = bytes(80, 90, 110) * 2.0;
    LightSum {
        sun_colour: colour,
        sun_direction: toward,
        sun_mask: 1.0,
        points: vec![PointTerm {
            to_light: Vec3::new(0.6, 0.0, 0.8),
            distance: 128.0,
            radius: 256.0,
            // FNAM 1.5 plus offset 0.25, dimmer 1.
            colour: bytes(255, 160, 80) * (1.5 + 0.25),
        }],
        ambient: dalc_ambient(faces, n),
        emission: Vec3::ZERO,
    }
}

fn interior_fog() -> FogProperty {
    FogProperty {
        near: 100.0,
        far: 5000.0,
        power: 1.0,
        max: 0.9,
        near_colour: bytes(40, 50, 60),
        far_colour: bytes(60, 70, 80),
    }
}

#[test]
fn example_a_interior_pixel() {
    let surface_plain = surface(false);
    let light = interior_light(surface_plain.normal);
    close(
        light.sun_colour,
        [0.627_451, 0.705_882, 0.862_745],
        "cb2[1]",
    );
    close(light.sun_direction, [0.0, 0.0, 1.0], "direction");
    near(
        surface_plain.normal.dot(light.sun_direction),
        0.999_985,
        "N.L",
    );
    close(light.ambient, [0.235_447, 0.266_819, 0.350_478], "Am");
    close(
        light.points[0].colour,
        [1.75, 1.098_039, 0.549_020],
        "cb2[22]",
    );
    near(point_attenuation(128.0, 256.0), 0.75, "att");

    let (lit, spec) = light_surface(&surface_plain, &light);
    close(lit, [0.961_737, 0.589_320, 0.368_039], "L");
    assert_eq!(spec, Vec3::ZERO);

    let fog = interior_fog();
    let f = fog.factor(800.0);
    near(f, 0.142_857, "vertex F");
    let cf = fog.colour(f);
    close(cf, [0.168_067, 0.207_283, 0.246_499], "Cf");
    close(
        output_tail(lit, None, f, cf, 0.0),
        [0.961_737, 0.589_320, 0.368_039],
        "first stage O",
    );
    close(
        output_tail(lit, None, f, cf, 1.0),
        [0.706_963, 0.445_619, 0.292_230],
        "blended O",
    );

    let surface_spec = surface(true);
    let (lit, spec) = light_surface(&surface_spec, &light);
    close(spec, [0.227_906, 0.221_317, 0.238_550], "Spec");
    close(
        output_tail(lit, Some(spec), f, cf, 0.0),
        [1.189_643, 0.810_637, 0.606_589],
        "specular O",
    );
    close(
        output_tail(lit, Some(spec), f, cf, 1.0),
        [0.869_753, 0.603_703, 0.462_623],
        "specular blended O",
    );
}

/// Climate: sunrise 5:00 to 7:00, sunset 17:00 to 19:00.
const TIMING: SunTiming = SunTiming([30, 42, 102, 114]);

fn exterior_weather() -> Weather {
    let mut weather = Weather {
        editor_id: Some("Spec1ExampleB".to_owned()),
        fog: WeatherFog {
            day_near: Some(1000.0),
            day_far: Some(80_000.0),
            night_near: Some(500.0),
            night_far: Some(40_000.0),
            day_power: Some(0.5),
            night_power: Some(0.6),
            day_max: Some(0.8),
            night_max: Some(0.9),
        },
        image_spaces: [Some(1), Some(2), Some(3), Some(4)],
        ..Default::default()
    };
    // The example gives the day and sunset slots; sunrise and night have weight 0 at both hours.
    for time in TimeOfDay::ALL {
        let sunset = matches!(time, TimeOfDay::Sunset);
        let pick = |day: [u8; 3], set: [u8; 3]| if sunset { set } else { day };
        for (name, colour) in [
            ("sunlight", pick([230, 215, 190], [200, 120, 70])),
            ("fog_near", pick([150, 170, 190], [170, 120, 110])),
            ("fog_far", pick([130, 160, 200], [150, 110, 120])),
            ("sky_upper", [80, 120, 190]),
            ("sky_lower", [140, 170, 215]),
            ("horizon", [180, 200, 220]),
        ] {
            weather.colors.insert((name.to_owned(), time), colour);
        }
        let day = [
            [90, 90, 100],
            [110, 105, 100],
            [95, 95, 105],
            [105, 100, 100],
            [60, 55, 45],
            [130, 150, 180],
        ];
        let set = [
            [100, 70, 60],
            [60, 50, 60],
            [75, 60, 65],
            [80, 60, 60],
            [45, 35, 30],
            [80, 80, 120],
        ];
        weather.directional_ambient.insert(
            time,
            DirectionalAmbient {
                colors: if sunset { set } else { day },
                specular: None,
                scale: None,
            },
        );
    }
    weather
}

/// What one exterior hour of example B must give.
struct ExteriorExpected {
    hour: f64,
    weights: [f32; 4],
    sun_scale: f32,
    cb2_1: [f32; 3],
    direction: [f32; 3],
    ambient: [f32; 3],
    lit: [f32; 3],
    vertex_f: f32,
    composite_colour: [f32; 3],
    tonemap_input: [f32; 3],
    shadowed_lit: [f32; 3],
    shadowed_input: [f32; 3],
    output: [f32; 3],
    bytes: [u8; 3],
}

fn check_exterior(expected: &ExteriorExpected) {
    let hour = expected.hour;
    let weather = exterior_weather();
    let sky = WeatherSky::from_weather(0x81A, &weather, TIMING).unwrap();
    for (actual, wanted) in slot_weights(hour, TIMING).iter().zip(expected.weights) {
        near(*actual, wanted, "day-part weight");
    }
    let lookup = |form: u32| {
        Some(ImageSpace {
            sunlight_scale: Some(if form == 3 { 1.9 } else { 1.6 }),
            ..Default::default()
        })
    };
    let (block, _) = weather_block(&sky, hour, &lookup);
    near(block.sunlight_scale, expected.sun_scale, "SunlightScale");

    let lighting = sky.lighting_at(hour).unwrap();
    let sun_colour = linear(raw_colour(lighting.sunlight)) * block.sunlight_scale;
    close(sun_colour, expected.cb2_1, "cb2[1]");
    let toward = sun_direction(hour, TIMING, EXE_SUN_DEFAULTS).unwrap();
    let toward = Vec3::new(toward.x, -toward.z, toward.y);
    close(toward, expected.direction, "Ls");
    let cube = lighting.directional_ambient();
    let surface = surface(false);
    let n = surface.normal;
    // The engine's cube takes a runtime-axes normal: Creation (x, y, z) is runtime (x, z, -y).
    let ambient = cube.evaluate(Vec3::new(n.x, n.z, -n.y));
    close(ambient, expected.ambient, "Am");

    let mut light = LightSum {
        sun_colour,
        sun_direction: toward,
        sun_mask: 1.0,
        points: Vec::new(),
        ambient,
        emission: Vec3::ZERO,
    };
    let (lit, _) = light_surface(&surface, &light);
    close(lit, expected.lit, "L");

    let palette = sky.palette_at(hour);
    let ramp = palette.fog_ramp.unwrap();
    assert!(fog_day_weight(hour, TIMING).is_some());
    let fog = FogProperty {
        near: ramp.near,
        far: ramp.far,
        power: ramp.power,
        max: ramp.max,
        near_colour: linear(raw_colour(palette.fog_near)),
        far_colour: linear(raw_colour(palette.fog_far)),
    };
    let f = fog.factor(5000.0);
    near(f, expected.vertex_f, "vertex F");
    // The first stage leaves a colour with `k M <= B` unchanged.
    let o = output_tail(lit, None, f, fog.colour(f), 0.0);
    close(o, expected.lit, "first stage O");

    let composite = |o: Vec3| {
        let fc = fog.factor(5000.0);
        let cfc = fog.colour(fc);
        Vec3::from_array(std::array::from_fn(|i| {
            crate::retail_fog::composite_saturated(o[i], 1.0, fc, cfc[i], OUTPUT_SCALE)
        }))
    };
    let blended = composite(o) / OUTPUT_SCALE;
    close(blended, expected.composite_colour, "(1-Fc) O + Fc Cfc");
    let input = composite(o);
    close(input, expected.tonemap_input, "tonemap input");

    light.sun_mask = 0.0;
    let (shadowed, _) = light_surface(&surface, &light);
    close(shadowed, expected.shadowed_lit, "shadowed L");
    close(
        composite(shadowed),
        expected.shadowed_input,
        "shadowed tonemap input",
    );

    // Example only: White 1.0, neutral grade, no tint, no bloom, steady x = y = 0.30,
    // ReceiveBloomThreshold 0.5.
    let tonemap = RetailHdr::from_block(
        &HdrBlock {
            white: 1.0,
            receive_bloom_threshold: 0.5,
            saturation: 1.0,
            brightness: 1.0,
            contrast: 1.0,
            tint_amount: 0.0,
            ..HdrBlock::IMGS_CONSTRUCTOR_DEFAULT
        },
        &RetailHdrConfig::default(),
        1.0 / 60.0,
    );
    let output = tonemap.display_value(input.to_array(), [0.3, 0.3], [0.0; 3]);
    close(Vec3::from_array(output), expected.output, "output");
    assert_eq!(to_bytes(output), expected.bytes);
}

fn to_bytes(colour: [f32; 3]) -> [u8; 3] {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a saturated channel times 255 is in 0..=255"
    )]
    colour.map(|c| (255.0 * c.clamp(0.0, 1.0)).round() as u8)
}

#[test]
fn example_b_exterior_noon() {
    check_exterior(&ExteriorExpected {
        hour: 12.0,
        weights: [0.0, 1.0, 0.0, 0.0],
        sun_scale: 1.6,
        cb2_1: [1.443_137, 1.349_020, 1.192_157],
        direction: [0.0, 0.242_536, 0.970_143],
        ambient: [0.523_104, 0.575_314, 0.676_428],
        lit: [0.966_026, 0.680_193, 0.437_308],
        vertex_f: 0.225_018,
        composite_colour: [0.877_045, 0.675_164, 0.508_552],
        tonemap_input: [0.730_871, 0.562_637, 0.423_793],
        shadowed_lit: [0.262_578, 0.207_564, 0.161_282],
        shadowed_input: [0.276_571, 0.257_404, 0.245_530],
        output: [0.683_884, 0.526_465, 0.396_548],
        bytes: [174, 134, 101],
    });
}

#[test]
fn example_b_exterior_hour_18() {
    check_exterior(&ExteriorExpected {
        hour: 18.0,
        weights: [0.0, 0.2, 0.8, 0.0],
        sun_scale: 1.84,
        cb2_1: [1.486_431, 1.002_980, 0.678_275],
        direction: [-0.957_657, 0.069_829, 0.279_317],
        ambient: [0.389_372, 0.371_148, 0.482_965],
        lit: [0.401_255, 0.233_716, 0.159_762],
        vertex_f: 0.231_296,
        composite_colour: [0.454_820, 0.295_476, 0.239_195],
        tonemap_input: [0.379_016, 0.246_230, 0.199_329],
        shadowed_lit: [0.195_450, 0.133_904, 0.115_154],
        shadowed_input: [0.247_180, 0.182_292, 0.170_754],
        output: [0.364_988, 0.237_117, 0.191_952],
        bytes: [93, 60, 49],
    });
}

#[test]
fn example_c_interior_composite_adaptation_and_tonemap() {
    let light = interior_light(surface(false).normal);
    let fog = interior_fog();
    let f = fog.factor(800.0);
    let cf = fog.colour(f);
    let composite = |o: Vec3| {
        Vec3::from_array(std::array::from_fn(|i| {
            crate::retail_fog::composite_saturated(o[i], 0.9, f, cf[i], OUTPUT_SCALE)
        }))
    };
    let (lit, _) = light_surface(&surface(false), &light);
    let o = output_tail(lit, None, f, cf, 0.0);
    close(
        composite(o) / OUTPUT_SCALE,
        [0.765_921, 0.484_230, 0.319_130],
        "(1-Fc) AO O + Fc Cfc",
    );
    let input = composite(o);
    close(input, [0.638_267, 0.403_525, 0.265_942], "tonemap input");
    // A pixel at the cleared depth takes `sat(AO x O)`: no fog and no `k`.
    close(
        (0.9 * o).clamp(Vec3::ZERO, Vec3::ONE),
        [0.865_563, 0.530_388, 0.331_235],
        "d >= 0.999999",
    );
    let (lit, spec) = light_surface(&surface(true), &light);
    let o = output_tail(lit, Some(spec), f, cf, 0.0);
    close(
        composite(o),
        [0.784_779, 0.545_800, 0.419_295],
        "specular tonemap input",
    );

    // One adaptation step from (0.20, 0.20) toward 0.30, EyeAdaptSpeed 3, strength 3.5.
    let block = HdrBlock {
        eye_adapt_speed: 3.0,
        eye_adapt_strength: 3.5,
        white: 1.0,
        receive_bloom_threshold: 0.5,
        saturation: 1.1,
        brightness: 1.0,
        contrast: 1.1,
        tint_amount: 0.1,
        tint_rgb: [1.0, 0.9, 0.8],
        ..HdrBlock::IMGS_CONSTRUCTOR_DEFAULT
    };
    let config = RetailHdrConfig::default();
    let step = |t: f32| {
        let (rx, ry) = adapt_rates(&block, &config, t);
        (
            (rx, ry),
            [adapt_step(0.2, 0.3, rx), adapt_step(0.2, 0.3, ry)],
        )
    };
    let ((rx, ry), pair) = step(1.0 / 60.0);
    near(rx, 0.021_663, "r_x");
    near(ry, 0.030_464, "r_y");
    near(pair[0], 0.203_906, "x takes the floor");
    near(pair[1], 0.203_906, "y takes the floor");
    let ((rx, ry), still) = step(0.0);
    assert_eq!((rx, ry), (0.0, 0.0));
    assert_eq!(still, [0.2, 0.2], "at rate 0 nothing moves");
    let ((rx, ry), long) = step(0.3);
    near(rx, 0.325_797, "r_x");
    near(ry, 0.427_005, "r_y");
    near(long[0], 0.232_580, "x");
    near(long[1], 0.242_701, "y");

    let tonemap = RetailHdr::from_block(&block, &config, 1.0 / 60.0);
    let output = tonemap.display_value(input.to_array(), pair, [0.010, 0.008, 0.006]);
    close(
        Vec3::from_array(output),
        [0.643_119, 0.396_315, 0.249_676],
        "output",
    );
    assert_eq!(to_bytes(output), [164, 101, 64]);
}

#[test]
fn example_d_sky_pixel_and_byte_decode() {
    // Rows 8, 7 and 0 weighted by the dome vertex colour (0, 0.5, 0.5).
    let rows = 0.0 * bytes(180, 200, 220) + 0.5 * bytes(140, 170, 215) + 0.5 * bytes(80, 120, 190);
    close(OUTPUT_SCALE * rows, [0.359_477, 0.473_856, 0.661_765], "v");
    near(OUTPUT_SCALE * 0.2, 0.166_667, "y");
    close(
        sky_pixel(rows, 0.2),
        [0.526_144, 0.640_523, 0.828_431],
        "stored",
    );
    for (byte, retail, decoded) in [
        (64, 0.250_980, 0.051_269),
        (128, 0.501_961, 0.215_861),
        (192, 0.752_941, 0.527_115),
    ] {
        near(linear(raw_bytes([byte; 3])).x, retail, "retail byte decode");
        near(
            Color::srgb_u8(byte, byte, byte).to_linear().red,
            decoded,
            "sRGB decode, not retail",
        );
    }
}
