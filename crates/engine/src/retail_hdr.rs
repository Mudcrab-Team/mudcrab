//! Retail's HDR image-space chain for the `skyrim` visual profile (#796, #361).
//!
//! The engine rules are in [near-lighting.md](../../../docs/specs/engine/near-lighting.md).
//!
//! Before this module the `skyrim` profile still ran Bevy's TonyMcMapface and a fixed EV100 of
//! 9.7. Retail has neither. It tone maps once, with a luminance curve whose input is the lit and
//! fogged scene, it adds bloom, and it adapts to the scene brightness over time. This module is
//! that chain. `mudcrab` is unchanged and never reaches it.
//!
//! ## Where the facts come from
//!
//! Skyrim-research NEEDS 18 (`skyrim_decomp` PR 1059) with its re-verification (amendments D1 to
//! D9), NEEDS 24 (the fog composite and `k`), NEEDS 25 (the frame timer), NEEDS 26 (interiors use
//! the same chain) and NEEDS 27 (reviewed: the timer, the first frame, the adapted pair, the
//! draws after the composite, the image-space selection and the bloom). All of them are static
//! readings of 1.7.104.0. No capture confirms any value, so every number below is a static result.
//!
//! ## What retail does, in order
//!
//! [near-lighting.md](../../../docs/specs/engine/near-lighting.md), section 10 of its source.
//!
//! 1. The lit scene is composited with fog (`retail_fog.rs`). The composite's own output is
//!    **saturated** (D3). Draws after the composite (the sorted alpha passes, the first-person
//!    view, volumetrics) add light above 1, so the tonemap input can exceed 1. Only the composite
//!    output is clipped. The whole scene is not (NEEDS 27).
//! 2. A 4-tap downsample takes the scene to a quarter size. Luminance steps (4 taps first, then 16
//!    taps) divide each side by 4 until the 2 by 2 adapted pair. The average is linear.
//! 3. The adapt draw replaces the last step. For each channel and texel,
//!    `delta = target - previous`,
//!    `step = sign(delta * rate) * min(|delta|, max(|delta * rate|, 1/256))`
//!    and `new = previous + step`: at rate 0 nothing moves (SPEC-1 section 10). A NaN target keeps
//!    the previous value. The rates are
//!    `y = 1 - max(1 - 0.01 S, 0)^E` and `x = 1 - max(1 - 0.01 S / K, 0)^E`, with
//!    `S = fGlobalEyeAdaptSpeedScale * EyeAdaptSpeed`, `K = fGlobalEyeAdaptStrengthScale *
//!    EyeAdaptStrength` and `E = 30 T`. `T` is the smoothed real frame time in seconds. On the
//!    first frame, or after the targets are rebuilt, the adapt draw is skipped and the pair is
//!    the current luminance, so the ratio is 1.
//! 4. Bloom: a bright pass with a vertical blur and a horizontal blur, both at a quarter size, of
//!    the first downsample. `k = clamp(trunc(BloomBlurRadius), 1, 7)`.
//! 5. The tonemap pass, per pixel: `Ls = L * A.y / A.x`, where `A` is the adapted pair sampled
//!    bilinearly at the screen coordinate. Reinhard `Lt = Ls (1 + Ls W2) / (1 + Ls)` with
//!    `W2 = 1 / (1.1 White)^2`, or the filmic curve when `bUseFilmicCurve` is 1. Then
//!    `colour = c * Lt / L + b * saturate(ReceiveBloomThreshold - Lt)`, saturation about the
//!    colour's luminance, the tint, brightness and contrast as multipliers about `A.x` (D6),
//!    `saturate`, power `1 / fGamma`, and the fade.
//! 6. The result goes to an `R8G8B8A8_UNORM` backbuffer with no transfer function.
//!
//! The image-space values (`IMGS`) come from the weather's four day-part forms for an exterior,
//! mixed by the same day-part weights as the sky. An interior takes the cell's own link, or the
//! default form when the cell has none, unless a cell flag selects the weather block. An exterior
//! in a world space with a lighting template follows the interior rule (NEEDS 27).
//!
//! ## The input scale
//!
//! There is none. Under `skyrim` the scene is lit in retail's own unitless values
//! (`retail_lighting.rs`, [near-lighting.md](../../../docs/specs/engine/near-lighting.md),
//! SPEC-1 section 2.1) and the camera exposure is 1, so the HDR target
//! holds what retail's holds.
//!
//! ## What Mudcrab does differently from retail
//!
//! - **IMGS constructor default.** STAND-IN(#361): a record that lacks a field takes retail's
//!   constructor default for it ([`HdrBlock::IMGS_CONSTRUCTOR_DEFAULT`]). The selection report
//!   names every defaulted field.
//! - **Not applied.** IMAD modifiers and `SetHDRParam` (Mudcrab has no cinematic modifier
//!   source), room image spaces and their cross-fade, the map-menu boosts, a screen fade (the
//!   fade amount is 0) and `ClearAdaptedLight` requests from the sky, from menus and from player
//!   moves (the pair restarts only when the targets are rebuilt).
//! - **Pause menu.** Retail replaces `T` with `fConstHDRAdaptTimerForMenu` (0.3) while paused.
//!   Mudcrab has no pause menu that stops the game clock, so `T` is always the frame time.
//! - **Weather transitions.** The weather does not change, so the current-weather share `m` is 1.
//! - **Output transfer.** The pass stores the linear decode of the display value, because
//!   Mudcrab's surface encodes sRGB on write and retail's backbuffer does not.
//! - **Fog composite saturate (D3).** The fog pass is a blend and cannot clip. A Min-blend draw
//!   of 1.0 right after it does (`retail_hdr_saturate.wgsl`), before the transmissive and
//!   transparent passes, so later draws still add above 1. Only views that draw the composite are
//!   clipped. The composite runs outside and inside; without fog it is retail's record 49, which
//!   clips and applies no `k` (see `retail_fog.rs`).
//! - **Targets.** The chain targets are `Rgba16Float`, retail's `Ultra` format.

use crate::{
    color_pipeline::{SceneGradeSource, SceneGradeTarget},
    config::EngineConfig,
    retail_hdr_chain::{FrameTimer, bloom_radius, bloom_taps},
    visual_profile::VisualProfile,
    weather_sky::WeatherSky,
    world::{
        components::{CELL_SIZE, StreamingCamera},
        environment::{EnvironmentCatalog, ImageSpace, interior},
    },
};
use bevy::{
    asset::embedded_asset,
    prelude::*,
    render::{
        extract_component::{ExtractComponent, ExtractComponentPlugin, UniformComponentPlugin},
        render_resource::ShaderType,
    },
};

/// `fReinhardWhiteScale` (`SkyrimSE.exe` 1.7.104, NEEDS 18). Neither `Ultra.ini` nor
/// `Skyrim_Default.ini` sets it, so the compiled default stands.
pub const REINHARD_WHITE_SCALE: f32 = 1.1;
/// `fFilmicWhiteScale` (NEEDS 18, compiled default).
pub const FILMIC_WHITE_SCALE: f32 = 1.4;
/// `fGlobalEyeAdaptSpeedScale` (NEEDS 18, compiled default).
pub const GLOBAL_EYE_ADAPT_SPEED_SCALE: f32 = 2.0;
/// `fGlobalEyeAdaptStrengthScale` (NEEDS 18, compiled default).
pub const GLOBAL_EYE_ADAPT_STRENGTH_SCALE: f32 = 0.4;
/// `fConstHDRAdaptTimerForMenu` (NEEDS 18, compiled default). Mudcrab has no pause menu that
/// stops the game clock, so nothing selects it. It is kept for the day a pause menu does.
pub const MENU_ADAPT_TIMER: f32 = 0.3;
/// `fGamma` (NEEDS 18, compiled default). The shader's final exponent is `1 / fGamma`.
pub const GAMMA: f32 = 1.0;
/// The smallest adaptation step per frame per channel (NEEDS 18).
pub const ADAPT_STEP_FLOOR: f32 = 1.0 / 256.0;
/// The luminance weights of the shipped image-space shaders (NEEDS 18).
pub const LUMINANCE_WEIGHTS: [f32; 3] = [0.2125, 0.7154, 0.0721];

/// The INI keys of the chain, in `[Display]`. Every default is the compiled retail value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetailHdrConfig {
    /// `bUseFilmicCurve`.
    pub filmic: bool,
    /// `fReinhardWhiteScale`.
    pub reinhard_white_scale: f32,
    /// `fFilmicWhiteScale`.
    pub filmic_white_scale: f32,
    /// `fGlobalEyeAdaptSpeedScale`.
    pub eye_adapt_speed_scale: f32,
    /// `fGlobalEyeAdaptStrengthScale`.
    pub eye_adapt_strength_scale: f32,
    /// `fGamma`.
    pub gamma: f32,
    /// `fGlobalSaturationBoost`, added to the image space saturation.
    pub saturation_boost: f32,
    /// `fGlobalBrightnessBoost`, added to the image space brightness.
    pub brightness_boost: f32,
    /// `fGlobalContrastBoost`, added to the image space contrast.
    pub contrast_boost: f32,
    /// `fGlobalBloomThresholdBoost`, added to the image space bloom threshold.
    pub bloom_threshold_boost: f32,
    /// `bUseMultipleLuminanceReferences`: the adapted pair is 2 by 2 when set, else 1 by 1.
    pub multiple_luminance_references: bool,
}

impl Default for RetailHdrConfig {
    fn default() -> Self {
        Self {
            filmic: false,
            reinhard_white_scale: REINHARD_WHITE_SCALE,
            filmic_white_scale: FILMIC_WHITE_SCALE,
            eye_adapt_speed_scale: GLOBAL_EYE_ADAPT_SPEED_SCALE,
            eye_adapt_strength_scale: GLOBAL_EYE_ADAPT_STRENGTH_SCALE,
            gamma: GAMMA,
            saturation_boost: 0.0,
            brightness_boost: 0.0,
            contrast_boost: 0.0,
            bloom_threshold_boost: 0.0,
            multiple_luminance_references: true,
        }
    }
}

/// One image space's values: the nine `HNAM` floats, the `CNAM` triple and the `TNAM` tint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrBlock {
    pub eye_adapt_speed: f32,
    pub bloom_blur_radius: f32,
    pub bloom_threshold: f32,
    pub bloom_scale: f32,
    pub receive_bloom_threshold: f32,
    pub white: f32,
    pub sunlight_scale: f32,
    pub sky_scale: f32,
    pub eye_adapt_strength: f32,
    pub saturation: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub tint_amount: f32,
    pub tint_rgb: [f32; 3],
}

/// The names of the fields a record can lack, in [`HdrBlock`] order.
const FIELD_NAMES: [&str; 14] = [
    "eye_adaptation_speed",
    "bloom_blur_radius",
    "bloom_threshold",
    "bloom_scale",
    "receive_bloom_threshold",
    "white",
    "sunlight_scale",
    "sky_scale",
    "eye_adaptation_strength",
    "saturation",
    "brightness",
    "contrast",
    "tint_amount",
    "tint",
];

impl HdrBlock {
    /// STAND-IN(#361): the IMGS constructor default, for a record that lacks a field. The values
    /// are the compiled block defaults of NEEDS 18, Question 2: speed 3.0, blur radius 7.0,
    /// threshold 0.6, bloom scale 0.5, receive-bloom 0.15, white 0.15, SunlightScale 1.8, SkyScale
    /// 1.5, saturation 0.9, brightness 1.5, contrast 1.1 and tint 0. The block constructor sets
    /// EyeAdaptStrength 3.5 and the record initialiser sets 3.0. A record is what is missing a
    /// field, so 3.0 is used. The tint colour is not given and does not matter at tint 0, so it
    /// is white. The SunlightScale default is the same 1.8 the object LOD path uses
    /// (`streaming/lod_object_material.rs`, issue #946).
    pub const IMGS_CONSTRUCTOR_DEFAULT: Self = Self {
        eye_adapt_speed: 3.0,
        bloom_blur_radius: 7.0,
        bloom_threshold: 0.6,
        bloom_scale: 0.5,
        receive_bloom_threshold: 0.15,
        white: 0.15,
        sunlight_scale: 1.8,
        sky_scale: 1.5,
        eye_adapt_strength: 3.0,
        saturation: 0.9,
        brightness: 1.5,
        contrast: 1.1,
        tint_amount: 0.0,
        tint_rgb: [1.0, 1.0, 1.0],
    };

    /// The block of a record. A field the record lacks takes the constructor default and its name
    /// is pushed to `defaulted`. `None` (no record in the database) defaults every field.
    pub fn from_record(record: Option<&ImageSpace>, defaulted: &mut Vec<&'static str>) -> Self {
        let default = Self::IMGS_CONSTRUCTOR_DEFAULT;
        let Some(record) = record else {
            defaulted.extend(FIELD_NAMES);
            return default;
        };
        let tint = match record.tint {
            Some(rgb) if rgb.iter().all(|channel| channel.is_finite()) => rgb,
            _ => {
                defaulted.push("tint");
                default.tint_rgb
            }
        };
        let mut pick = |value: Option<f32>, fallback: f32, name: &'static str| {
            value.filter(|value| value.is_finite()).unwrap_or_else(|| {
                defaulted.push(name);
                fallback
            })
        };
        Self {
            eye_adapt_speed: pick(
                record.eye_adaptation_speed,
                default.eye_adapt_speed,
                "eye_adaptation_speed",
            ),
            bloom_blur_radius: pick(
                record.bloom_blur_radius,
                default.bloom_blur_radius,
                "bloom_blur_radius",
            ),
            bloom_threshold: pick(
                record.bloom_threshold,
                default.bloom_threshold,
                "bloom_threshold",
            ),
            bloom_scale: pick(record.bloom_scale, default.bloom_scale, "bloom_scale"),
            receive_bloom_threshold: pick(
                record.receive_bloom_threshold,
                default.receive_bloom_threshold,
                "receive_bloom_threshold",
            ),
            white: pick(record.white, default.white, "white"),
            sunlight_scale: pick(
                record.sunlight_scale,
                default.sunlight_scale,
                "sunlight_scale",
            ),
            sky_scale: pick(record.sky_scale, default.sky_scale, "sky_scale"),
            eye_adapt_strength: pick(
                record.eye_adaptation_strength,
                default.eye_adapt_strength,
                "eye_adaptation_strength",
            ),
            saturation: pick(record.saturation, default.saturation, "saturation"),
            brightness: pick(record.brightness, default.brightness, "brightness"),
            contrast: pick(record.contrast, default.contrast, "contrast"),
            tint_amount: pick(record.tint_amount, default.tint_amount, "tint_amount"),
            tint_rgb: tint,
        }
    }

    /// The weighted sum of blocks (NEEDS 18, Question 2): all 20 floats, tint included, blend
    /// linearly. The weights are the day-part weights and sum to 1.
    pub fn blend(parts: &[(f32, Self)]) -> Self {
        let mut mixed = Self {
            eye_adapt_speed: 0.0,
            bloom_blur_radius: 0.0,
            bloom_threshold: 0.0,
            bloom_scale: 0.0,
            receive_bloom_threshold: 0.0,
            white: 0.0,
            sunlight_scale: 0.0,
            sky_scale: 0.0,
            eye_adapt_strength: 0.0,
            saturation: 0.0,
            brightness: 0.0,
            contrast: 0.0,
            tint_amount: 0.0,
            tint_rgb: [0.0; 3],
        };
        for (weight, block) in parts {
            let w = *weight;
            mixed.eye_adapt_speed += w * block.eye_adapt_speed;
            mixed.bloom_blur_radius += w * block.bloom_blur_radius;
            mixed.bloom_threshold += w * block.bloom_threshold;
            mixed.bloom_scale += w * block.bloom_scale;
            mixed.receive_bloom_threshold += w * block.receive_bloom_threshold;
            mixed.white += w * block.white;
            mixed.sunlight_scale += w * block.sunlight_scale;
            mixed.sky_scale += w * block.sky_scale;
            mixed.eye_adapt_strength += w * block.eye_adapt_strength;
            mixed.saturation += w * block.saturation;
            mixed.brightness += w * block.brightness;
            mixed.contrast += w * block.contrast;
            mixed.tint_amount += w * block.tint_amount;
            for (channel, value) in mixed.tint_rgb.iter_mut().zip(block.tint_rgb) {
                *channel += w * value;
            }
        }
        mixed
    }
}

/// The adaptation rates of the two channels for one frame (NEEDS 18, with D1). `t` is the frame
/// time in real seconds. `E = 30 t`. A scale of speed or strength that makes `K` zero gives an
/// infinite ratio, which the `max(.., 0)` turns into rate 1, as in retail
/// (NEEDS 18, https://github.com/Mudcrab-Team/skyrim_decomp/pull/1059).
pub fn adapt_rates(block: &HdrBlock, config: &RetailHdrConfig, t: f32) -> (f32, f32) {
    let speed = config.eye_adapt_speed_scale * block.eye_adapt_speed;
    let strength = config.eye_adapt_strength_scale * block.eye_adapt_strength;
    let e = 30.0 * t;
    let y = 1.0 - (1.0 - 0.01 * speed).max(0.0).powf(e);
    let x = 1.0 - (1.0 - 0.01 * speed / strength).max(0.0).powf(e);
    (x, y)
}

/// One adaptation step of one channel: `sign(delta x rate) x min(|delta|, max(|delta x rate|,
/// 1/256))` (SPEC-1 section 10, correction C1). At rate 0 the channel does not move. A NaN target
/// keeps the previous value.
pub fn adapt_step(previous: f32, measured: f32, rate: f32) -> f32 {
    if measured.is_nan() {
        return previous;
    }
    let delta = measured - previous;
    let product = delta * rate;
    if product == 0.0 {
        return previous;
    }
    previous + product.signum() * delta.abs().min(product.abs().max(ADAPT_STEP_FLOOR))
}

/// The white-point coefficient `W2` of the selected curve (NEEDS 18). Reinhard: `1 / (scale White)^2`.
/// Filmic: `h(x_w)^-2.2` with `x_w = max(scale White - 0.004, 0)`.
pub fn white_coefficient(block: &HdrBlock, config: &RetailHdrConfig) -> f32 {
    if config.filmic {
        let x = (config.filmic_white_scale * block.white - 0.004).max(0.0);
        filmic_h(x).powf(-2.2)
    } else {
        let scaled = config.reinhard_white_scale * block.white;
        1.0 / (scaled * scaled)
    }
}

fn filmic_h(x: f32) -> f32 {
    (x * (6.2 * x + 0.5)) / (x * (6.2 * x + 1.7) + 0.06)
}

/// The curve on the scaled luminance `ls` (NEEDS 18).
pub fn tone_curve(ls: f32, w2: f32, filmic: bool) -> f32 {
    if filmic {
        let x = (ls - 0.004).max(0.0);
        filmic_h(x).powf(2.2) * w2
    } else {
        ls * (1.0 + ls * w2) / (1.0 + ls)
    }
}

fn luminance(colour: [f32; 3]) -> f32 {
    colour[0] * LUMINANCE_WEIGHTS[0]
        + colour[1] * LUMINANCE_WEIGHTS[1]
        + colour[2] * LUMINANCE_WEIGHTS[2]
}

fn saturate(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

/// The settings of the retail tonemap pass for one camera. This is the shader's uniform, field
/// for field, and the component that selects the chain for a camera. Only the `skyrim` profile
/// adds it.
#[derive(Component, Clone, Copy, Debug, PartialEq, ShaderType, ExtractComponent)]
#[extract_component_filter(With<Camera>)]
pub struct RetailHdr {
    /// x = 1 when the filmic curve is selected, y = `W2`, z = `1 / fGamma`, w = the fade amount.
    pub curve: Vec4,
    /// x = saturation, y = brightness, z = contrast (multipliers, neutral 1), w = tint amount.
    pub grade: Vec4,
    /// rgb = the tint colour.
    pub tint: Vec4,
    /// rgb = the fade colour.
    pub fade: Vec4,
    /// x = the x adaptation rate, y = the y adaptation rate, z = ReceiveBloomThreshold, w = 1
    /// when the adapted pair is 2 by 2 (`bUseMultipleLuminanceReferences`).
    pub adapt: Vec4,
    /// x = the bright-pass threshold, y = BloomScale, z = the blur radius `k`, w = `k_r`.
    pub bloom: Vec4,
    /// The blur weights of radius `k`: entry `u + 7` holds tap `u`, in x.
    pub weights: [Vec4; 15],
}

impl Default for RetailHdr {
    /// A neutral pass: Reinhard with `W2` 1, no grade, no fade, no adaptation movement.
    fn default() -> Self {
        Self {
            curve: Vec4::new(0.0, 1.0, 1.0, 0.0),
            grade: Vec4::new(1.0, 1.0, 1.0, 0.0),
            tint: Vec4::ONE,
            fade: Vec4::ZERO,
            adapt: Vec4::new(0.0, 0.0, 0.0, 1.0),
            bloom: Vec4::new(
                0.0,
                0.0,
                1.0,
                crate::output_scale::framebuffer_output_scale(true),
            ),
            weights: blur_weights(1),
        }
    }
}

fn blur_weights(radius: usize) -> [Vec4; 15] {
    bloom_taps(radius).map(|weight| Vec4::new(weight, 0.0, 0.0, 0.0))
}

impl RetailHdr {
    /// The pass settings for a blended image space. `t` is the frame time in real seconds.
    /// The fade amount is 0: Mudcrab has no screen fade yet.
    pub fn from_block(block: &HdrBlock, config: &RetailHdrConfig, t: f32) -> Self {
        let (rate_x, rate_y) = adapt_rates(block, config, t);
        let radius = bloom_radius(block.bloom_blur_radius);
        Self {
            curve: Vec4::new(
                f32::from(config.filmic),
                white_coefficient(block, config),
                1.0 / config.gamma,
                0.0,
            ),
            grade: Vec4::new(
                block.saturation + config.saturation_boost,
                block.brightness + config.brightness_boost,
                block.contrast + config.contrast_boost,
                block.tint_amount,
            ),
            tint: Vec3::from_array(block.tint_rgb).extend(0.0),
            fade: Vec4::ZERO,
            adapt: Vec4::new(
                rate_x,
                rate_y,
                block.receive_bloom_threshold,
                f32::from(u8::from(config.multiple_luminance_references)),
            ),
            bloom: Vec4::new(
                block.bloom_threshold + config.bloom_threshold_boost,
                block.bloom_scale,
                f32::from(u8::try_from(radius).unwrap_or(1)),
                crate::output_scale::framebuffer_output_scale(true),
            ),
            weights: blur_weights(radius),
        }
    }

    pub fn multiple_luminance_references(&self) -> bool {
        self.adapt.w > 0.5
    }

    /// The display value of one scene pixel: the arithmetic of `retail_hdr_tonemap.wgsl`, in the
    /// order of NEEDS 18. `adapted` is `(A.x, A.y)` and `bloom` the bloom colour, both as
    /// sampled at the pixel.
    pub fn display_value(&self, scene: [f32; 3], adapted: [f32; 2], bloom: [f32; 3]) -> [f32; 3] {
        let [a_x, a_y] = adapted;
        let l = luminance(scene).max(1.0e-5);
        // The shader's guard: retail divides by A.x with no clamp, and a zero is a NaN there.
        let ratio = if a_x > 0.0 { a_y / a_x } else { 1.0 };
        let lt = tone_curve(l * ratio, self.curve.y, self.curve.x > 0.5);
        let receive = self.adapt.z;
        let mut colour: [f32; 3] =
            std::array::from_fn(|i| scene[i] * (lt / l) + bloom[i] * saturate(receive - lt));
        let l1 = luminance(colour);
        let (saturation, brightness, contrast, tint_amount) =
            (self.grade.x, self.grade.y, self.grade.z, self.grade.w);
        colour = colour.map(|c| l1 + saturation * (c - l1));
        let tint = self.tint.truncate().to_array();
        colour = std::array::from_fn(|i| colour[i] + tint_amount * (l1 * tint[i] - colour[i]));
        colour = colour.map(|c| contrast * (brightness * c - a_x) + a_x);
        colour = colour.map(|c| saturate(c).powf(self.curve.z));
        let fade = self.curve.w;
        if fade > 0.0 {
            let fade_rgb = self.fade.truncate().to_array();
            colour = std::array::from_fn(|i| colour[i] + fade * (fade_rgb[i] - colour[i]));
        }
        colour
    }
}

/// Which image spaces the chain used, for the start-up and change log and for tests.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageSpaceReport {
    /// Where the forms came from: `weather`, `cell` or `none`.
    pub source: &'static str,
    /// The forms read, with their weights.
    pub forms: Vec<(u32, f32)>,
    /// Forms whose record is not in the database. Their whole block is the constructor default.
    pub missing_records: Vec<u32>,
    /// Fields a record lacks, which took the constructor default.
    pub defaulted_fields: Vec<&'static str>,
    /// Why the constructor default stands for the whole block, when it does.
    pub reason: Option<&'static str>,
}

/// `DefaultImageSpace`: the form a weather's missing day part uses (NEEDS 18, Question 2).
pub const DEFAULT_IMAGE_SPACE_FORM: u32 = 0x167;

/// Reads the forms through `lookup` and mixes them. The weights are the mix weights.
fn blend_forms(
    source: &'static str,
    forms: &[(u32, f32)],
    lookup: &dyn Fn(u32) -> Option<ImageSpace>,
) -> (HdrBlock, ImageSpaceReport) {
    let mut report = ImageSpaceReport {
        source,
        forms: forms.to_vec(),
        missing_records: Vec::new(),
        defaulted_fields: Vec::new(),
        reason: None,
    };
    let parts: Vec<(f32, HdrBlock)> = forms
        .iter()
        .map(|(form, weight)| {
            let record = lookup(*form);
            if record.is_none() {
                report.missing_records.push(*form);
            }
            let mut defaulted = Vec::new();
            let block = HdrBlock::from_record(record.as_ref(), &mut defaulted);
            for name in defaulted {
                if !report.defaulted_fields.contains(&name) {
                    report.defaulted_fields.push(name);
                }
            }
            (*weight, block)
        })
        .collect();
    (HdrBlock::blend(&parts), report)
}

fn constructor_default(reason: &'static str) -> (HdrBlock, ImageSpaceReport) {
    (
        HdrBlock::IMGS_CONSTRUCTOR_DEFAULT,
        ImageSpaceReport {
            source: "none",
            forms: Vec::new(),
            missing_records: Vec::new(),
            defaulted_fields: FIELD_NAMES.to_vec(),
            reason: Some(reason),
        },
    )
}

/// The exterior image space at a game hour: the weather's four day-part forms mixed by the
/// day-part weights of the sky (NEEDS 18, Question 2, with the current-weather share 1). A day
/// part with no form uses [`DEFAULT_IMAGE_SPACE_FORM`].
pub fn weather_block(
    weather: &WeatherSky,
    hour: f64,
    lookup: &dyn Fn(u32) -> Option<ImageSpace>,
) -> (HdrBlock, ImageSpaceReport) {
    let weights = weather.slot_weights_at(hour);
    let forms = weather.image_space_forms();
    let used: Vec<(u32, f32)> = forms
        .iter()
        .zip(weights)
        .filter(|(_, weight)| *weight > 0.0)
        .map(|(form, weight)| {
            let form = form
                .filter(|form| *form != 0)
                .unwrap_or(DEFAULT_IMAGE_SPACE_FORM);
            (form, weight)
        })
        .collect();
    let (block, mut report) = blend_forms("weather", &used, lookup);
    if report.forms.is_empty() {
        return constructor_default("the day-part weights are all zero");
    }
    report.source = "weather";
    (block, report)
}

/// Which image space the scene asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SceneImageSpace {
    /// An exterior: the weather's.
    Exterior,
    /// An exterior in a world space with a lighting template: the interior rule (NEEDS 27). The
    /// value is the cell's image-space link, `Some(None)` when the cell names none, and `None`
    /// when the database has no row for the cell.
    TemplatedExterior(Option<Option<u32>>),
    /// An interior and its image-space selection. `None` means the database has no value for the
    /// cell (no cell flags or no XCIM column).
    Interior(Option<interior::ImageSpaceSelection>),
}

impl SceneImageSpace {
    pub(crate) fn of(scene: Option<&interior::SceneEnvironment>) -> Self {
        match scene {
            Some(scene) if scene.interior.is_some() => Self::Interior(scene.image_space),
            _ => Self::Exterior,
        }
    }
}

/// The image space of the active scene. An interior takes the cell's own link (`Xcim`, or
/// `DefaultImageSpace` when the cell has none). An interior that uses sky lighting, and every
/// exterior, takes the weather's. Anything that cannot be resolved gives the constructor default
/// with the reason in the report. Nothing is invented.
pub(crate) fn select_block(
    scene: SceneImageSpace,
    weather: Option<&WeatherSky>,
    hour: f64,
    lookup: Option<&dyn Fn(u32) -> Option<ImageSpace>>,
) -> (HdrBlock, ImageSpaceReport) {
    let Some(lookup) = lookup else {
        return constructor_default("no world database: no image-space records");
    };
    match scene {
        SceneImageSpace::TemplatedExterior(None) => {
            constructor_default("the exterior cell is not in the database")
        }
        SceneImageSpace::TemplatedExterior(Some(link)) => {
            let form = link
                .filter(|form| *form != 0)
                .unwrap_or(DEFAULT_IMAGE_SPACE_FORM);
            let (block, mut report) = blend_forms("cell", &[(form, 1.0)], lookup);
            report.source = "cell";
            (block, report)
        }
        SceneImageSpace::Interior(None) => {
            constructor_default("the cell's image-space link is not in the database")
        }
        SceneImageSpace::Interior(Some(selection))
            if selection.source != interior::ImageSpaceSource::WeatherGlobal =>
        {
            let Some(form) = selection.form_id else {
                return constructor_default("the cell's image-space selection has no form");
            };
            let (block, mut report) = blend_forms("cell", &[(form, 1.0)], lookup);
            report.source = "cell";
            (block, report)
        }
        _ => match weather {
            Some(weather) => weather_block(weather, hour, lookup),
            None => constructor_default("no weather record for the exterior image space"),
        },
    }
}

/// Adds the chain to every scene camera under the `skyrim` profile. `mudcrab` cameras never get
/// it, so they keep Bevy's tone mapping and exposure.
pub(crate) fn attach_retail_hdr(
    mut commands: Commands,
    source: Option<Res<SceneGradeSource>>,
    cameras: Query<(Entity, &SceneGradeTarget), Without<RetailHdr>>,
) {
    if source.is_none_or(|source| source.profile != VisualProfile::Skyrim) {
        return;
    }
    for (entity, target) in &cameras {
        if *target == SceneGradeTarget::Scene {
            commands.entity(entity).insert(RetailHdr::default());
        }
    }
}

/// The image-space question the scene asks. An exterior in a world space whose record names a
/// lighting template reads the cell under the camera (NEEDS 27).
fn scene_image_space(
    scene: Option<&interior::SceneEnvironment>,
    catalog: Option<&EnvironmentCatalog>,
    space: Option<&crate::streaming::ActiveSpace>,
    config: Option<&EngineConfig>,
    origin: Option<&crate::streaming::RenderOrigin>,
    eye: Option<&Transform>,
) -> SceneImageSpace {
    let selected = SceneImageSpace::of(scene);
    if selected != SceneImageSpace::Exterior {
        return selected;
    }
    let (Some(catalog), Some(space), Some(config)) = (catalog, space, config) else {
        return selected;
    };
    let world = space.exterior_worldspace(config.worldspace_id);
    if !catalog.worldspace_has_lighting_template(world) {
        return selected;
    }
    let cell = origin.zip(eye).map(|(origin, eye)| {
        let offset = crate::capture_frame::absolute_creation_position(eye.translation, origin.0);
        // A cell index is a floored quotient; positions in a loaded world are far inside i32.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "floored cell indices fit in i32"
        )]
        let grid = |value: f32| (value / CELL_SIZE).floor() as i32;
        (grid(offset[0]), grid(offset[1]))
    });
    SceneImageSpace::TemplatedExterior(
        cell.and_then(|(x, y)| catalog.exterior_cell_image_space(world, x, y)),
    )
}

/// The image-space block of the active scene (SPEC-1 section 2.4). The sun's `SunlightScale`,
/// the sky's `SkyScale` and the HDR chain all read this one block.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub(crate) struct RetailImageSpace(pub HdrBlock);

impl Default for RetailImageSpace {
    fn default() -> Self {
        Self(HdrBlock::IMGS_CONSTRUCTOR_DEFAULT)
    }
}

/// The selected block. An app without the selection system (a fixture) takes the constructor
/// block, as a scene with no image-space record does.
pub(crate) fn scene_block(selected: Option<&RetailImageSpace>) -> HdrBlock {
    selected.map_or(HdrBlock::IMGS_CONSTRUCTOR_DEFAULT, |selected| selected.0)
}

/// Selects the scene's image-space block under the `skyrim` profile, after the scene is selected
/// and before the lights and the sky read it.
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy injects each resource separately"
)]
pub(crate) fn select_retail_image_space(
    config_resource: Option<Res<EngineConfig>>,
    clock: Option<Res<crate::sun::GameClock>>,
    source: Option<Res<SceneGradeSource>>,
    catalog: Option<Res<EnvironmentCatalog>>,
    scene: Option<Res<interior::SceneEnvironment>>,
    space: Option<Res<crate::streaming::ActiveSpace>>,
    origin: Option<Res<crate::streaming::RenderOrigin>>,
    eye: Query<&Transform, With<StreamingCamera>>,
    mut last: Local<Option<ImageSpaceReport>>,
    selected: Option<ResMut<RetailImageSpace>>,
) {
    let Some(mut selected) = selected else {
        return;
    };
    if source
        .as_deref()
        .is_none_or(|source| source.profile != VisualProfile::Skyrim)
    {
        return;
    }
    let hour = clock.map_or(crate::sun::RETAIL_GAME_HOUR, |clock| clock.hour);
    let weather = source.as_deref().and_then(|source| source.weather.as_ref());
    let lookup = catalog
        .as_deref()
        .map(|catalog| move |form: u32| catalog.image_space(form).copied());
    let (block, report) = select_block(
        scene_image_space(
            scene.as_deref(),
            catalog.as_deref(),
            space.as_deref(),
            config_resource.as_deref(),
            origin.as_deref(),
            eye.iter().next(),
        ),
        weather,
        hour,
        lookup
            .as_ref()
            .map(|lookup| lookup as &dyn Fn(u32) -> Option<ImageSpace>),
    );
    if last.as_ref() != Some(&report) {
        info!(
            "retail HDR image space: source {}, forms {:08X?}, missing records {:08X?}, \
             defaulted fields {:?}{}",
            report.source,
            report.forms,
            report.missing_records,
            report.defaulted_fields,
            report
                .reason
                .map_or_else(String::new, |reason| format!(", reason: {reason}")),
        );
        *last = Some(report);
    }
    selected.set_if_neq(RetailImageSpace(block));
}

/// Writes the scene's image space and the adaptation rates into every camera's pass.
pub(crate) fn drive_retail_hdr(
    config_resource: Option<Res<EngineConfig>>,
    image_space: Option<Res<RetailImageSpace>>,
    time: Res<Time<Real>>,
    mut timer: Local<FrameTimer>,
    mut cameras: Query<&mut RetailHdr>,
) {
    // The timer records every frame, as retail's does.
    let seconds = timer.push(time.delta_secs());
    if cameras.is_empty() {
        return;
    }
    // A world without the engine's configuration (a test) takes the compiled retail values.
    let config = config_resource
        .as_deref()
        .map_or_else(RetailHdrConfig::default, |config| config.retail_hdr);
    let block = scene_block(image_space.as_deref());
    // SPEC-2 amends the output stage: its inputs enter here, with the block.
    let settings = RetailHdr::from_block(&block, &config, seconds);
    for mut camera in &mut cameras {
        camera.set_if_neq(settings);
    }
}

pub struct RetailHdrPlugin;

impl Plugin for RetailHdrPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/retail_hdr_chain.wgsl");
        embedded_asset!(app, "shaders/retail_hdr_tonemap.wgsl");
        embedded_asset!(app, "shaders/retail_hdr_saturate.wgsl");
        bevy::shader::load_shader_library!(app, "shaders/retail_hdr.wgsl");
        app.add_plugins((
            ExtractComponentPlugin::<RetailHdr>::default(),
            UniformComponentPlugin::<RetailHdr>::default(),
        ))
        .init_resource::<RetailImageSpace>()
        .add_systems(
            PostUpdate,
            (attach_retail_hdr, drive_retail_hdr)
                .chain()
                .after(interior::EnvironmentSet),
        );
        crate::retail_hdr_render::build(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        color_pipeline::{SceneColorPipeline, SceneGradeMode, SceneVisualPreset},
        sun::SunTiming,
        world::environment::{
            TimeOfDay, Weather, WeatherFog,
            interior::{ImageSpaceSelection, ImageSpaceSource},
        },
    };
    use bevy::core_pipeline::tonemapping::Tonemapping;

    fn close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1.0e-6,
            "{what}: expected {expected}, got {actual}"
        );
    }

    /// A block with neutral grade and `White` 1.0, the worked check of NEEDS 18 (re-verification).
    fn neutral_block() -> HdrBlock {
        HdrBlock {
            white: 1.0,
            saturation: 1.0,
            brightness: 1.0,
            contrast: 1.0,
            tint_amount: 0.0,
            ..HdrBlock::IMGS_CONSTRUCTOR_DEFAULT
        }
    }

    fn neutral() -> RetailHdr {
        RetailHdr::from_block(&neutral_block(), &RetailHdrConfig::default(), 1.0 / 60.0)
    }

    /// NEEDS 18 re-verification, worked check: Reinhard, White 1.0, A = 0.5, neutral parameters.
    #[test]
    fn the_worked_check_of_needs_18_holds() {
        let hdr = neutral();
        let grey = hdr.display_value([0.5, 0.5, 0.5], [0.5, 0.5], [0.0; 3]);
        for channel in grey {
            close(channel, 0.471_074_4, "grey");
        }
        let bright = hdr.display_value([2.0, 1.0, 0.25], [0.5, 0.5], [0.0; 3]);
        close(bright[0], 1.0, "red clips at the saturate");
        close(bright[1], 0.906_853_9, "green");
        close(bright[2], 0.226_713_5, "blue");
    }

    /// The white point is `1.1 x White` (NEEDS 18). Luminance equal to `1.1 White` maps to 1.
    #[test]
    fn the_reinhard_white_point_is_one_point_one_times_white() {
        let config = RetailHdrConfig::default();
        let block = HdrBlock {
            white: 0.8,
            ..neutral_block()
        };
        let w2 = white_coefficient(&block, &config);
        close(w2, 1.0 / (1.1 * 0.8_f32).powi(2), "W2");
        close(tone_curve(0.88, w2, false), 1.0, "white maps to 1");
    }

    /// The filmic curve is used only when the INI flag is set. Its coefficient maps the filmic
    /// white (1.4 x White) to 1 (NEEDS 18).
    #[test]
    fn filmic_is_selected_by_the_ini_flag_only() {
        let mut config = RetailHdrConfig::default();
        let block = neutral_block();
        assert_eq!(
            RetailHdr::from_block(&block, &config, 0.016).curve.x,
            0.0,
            "Reinhard is the default"
        );
        config.filmic = true;
        let hdr = RetailHdr::from_block(&block, &config, 0.016);
        assert_eq!(hdr.curve.x, 1.0);
        let white = config.filmic_white_scale * block.white;
        close(
            tone_curve(white, hdr.curve.y, true),
            1.0,
            "filmic white maps to 1",
        );
        // The hand-evaluated curve at Ls = 0.5: x = 0.496, h = x (6.2 x + 0.5) / (x (6.2 x + 1.7) + 0.06).
        let x = 0.496_f32;
        let h = x * (6.2 * x + 0.5) / (x * (6.2 * x + 1.7) + 0.06);
        close(
            tone_curve(0.5, hdr.curve.y, true),
            h.powf(2.2) * hdr.curve.y,
            "filmic at 0.5",
        );
    }

    /// The order of NEEDS 18, with every stage non-neutral: tonemap, saturation, tint, brightness
    /// and contrast about A.x, saturate, power 1 / fGamma, fade. The expected value is written out
    /// stage by stage, and a swapped order must give another value, so the test can fail.
    #[test]
    fn the_stages_run_in_the_order_of_needs_18() {
        let block = HdrBlock {
            white: 1.0,
            saturation: 1.4,
            brightness: 1.2,
            contrast: 1.3,
            tint_amount: 0.5,
            tint_rgb: [1.0, 0.5, 0.25],
            ..HdrBlock::IMGS_CONSTRUCTOR_DEFAULT
        };
        let config = RetailHdrConfig {
            gamma: 2.0,
            ..RetailHdrConfig::default()
        };
        let mut hdr = RetailHdr::from_block(&block, &config, 1.0 / 60.0);
        hdr.fade = Vec4::new(0.0, 0.0, 1.0, 0.0);
        hdr.curve.w = 0.25;
        let scene = [0.6_f32, 0.3, 0.1];
        let adapted = [0.4_f32, 0.4];
        let actual = hdr.display_value(scene, adapted, [0.0; 3]);

        // Stage 1: the luminance curve.
        let w2 = 1.0 / (1.1_f32 * 1.0).powi(2);
        let l = luminance(scene);
        let lt = l * (1.0 + l * w2) / (1.0 + l);
        let toned = scene.map(|c| c * lt / l);
        // Stage 2: saturation about the colour's own luminance.
        let l1 = luminance(toned);
        let saturated = toned.map(|c| l1 + 1.4 * (c - l1));
        // Stage 3: tint toward the luminance times the tint colour, with the same l1.
        let tint = [1.0_f32, 0.5, 0.25];
        let tinted: [f32; 3] =
            std::array::from_fn(|i| saturated[i] + 0.5 * (l1 * tint[i] - saturated[i]));
        // Stage 4: brightness and contrast about A.x.
        let graded = tinted.map(|c| 1.3 * (1.2 * c - 0.4) + 0.4);
        // Stage 5 and 6: saturate, then the power 1 / fGamma.
        let powered = graded.map(|c| c.clamp(0.0, 1.0).powf(0.5));
        // Stage 7: the fade.
        let fade_rgb = [0.0_f32, 0.0, 1.0];
        let expected: [f32; 3] =
            std::array::from_fn(|i| powered[i] + 0.25 * (fade_rgb[i] - powered[i]));
        for i in 0..3 {
            close(actual[i], expected[i], "full chain");
        }

        // The same stages with the tint taken before the saturation give another value.
        let swapped_tint: [f32; 3] =
            std::array::from_fn(|i| toned[i] + 0.5 * (l1 * tint[i] - toned[i]));
        let swapped_l = luminance(swapped_tint);
        let swapped = swapped_tint.map(|c| swapped_l + 1.4 * (c - swapped_l));
        assert!(
            (0..3).any(|i| (swapped[i] - saturated[i]).abs() > 1.0e-3
                || (swapped[i] - tinted[i]).abs() > 1.0e-3),
            "a swapped tint and saturation would show"
        );
        // The saturate comes before the gamma: a value above 1 clips first, then the power.
        let bright = RetailHdr {
            curve: Vec4::new(0.0, w2, 0.5, 0.0),
            ..neutral()
        };
        let clipped = bright.display_value([4.0, 4.0, 4.0], [1.0, 1.0], [0.0; 3]);
        for channel in clipped {
            close(channel, 1.0, "saturate before the gamma power");
        }
    }

    /// Retail's contrast pivot is the adapted x value, not a fixed pivot (D6): at A.x equal to the
    /// toned colour a pixel does not move, and it moves away from the pivot with contrast above 1.
    #[test]
    fn the_contrast_pivot_is_the_adapted_x_value() {
        let hdr = RetailHdr {
            grade: Vec4::new(1.0, 1.0, 2.0, 0.0),
            ..neutral()
        };
        let scene = [0.4_f32, 0.4, 0.4];
        let toned = neutral().display_value(scene, [0.4, 0.4], [0.0; 3])[0];
        close(
            hdr.display_value(scene, [toned, toned], [0.0; 3])[0],
            toned,
            "pivot at A.x",
        );
        let above = hdr.display_value(scene, [toned * 0.5, toned * 0.5], [0.0; 3])[0];
        assert!(
            above > toned,
            "a lower pivot raises the pixel with contrast 2"
        );
    }

    /// Brightness and contrast are multipliers: the neutral value is 1 (D6).
    #[test]
    fn brightness_and_contrast_are_multipliers_with_neutral_one() {
        let hdr = neutral();
        let scene = [0.2_f32, 0.3, 0.4];
        let a = hdr.display_value(scene, [0.3, 0.3], [0.0; 3]);
        let b = hdr.display_value(scene, [0.9, 0.9], [0.0; 3]);
        for i in 0..3 {
            close(a[i], b[i], "the pivot does not move a neutral pixel");
        }
    }

    /// NEEDS 18, with D1: the rates depend on the real frame time through `E = 30 T`. Two frames
    /// of 1/60 s move as far as one frame of 1/30 s while the base is above zero.
    #[test]
    fn the_adaptation_rate_is_independent_of_the_frame_rate() {
        let block = HdrBlock {
            eye_adapt_speed: 3.0,
            eye_adapt_strength: 3.0,
            ..neutral_block()
        };
        let config = RetailHdrConfig::default();
        let (x60, y60) = adapt_rates(&block, &config, 1.0 / 60.0);
        let (x30, y30) = adapt_rates(&block, &config, 1.0 / 30.0);
        // S = 2.0 * 3.0 = 6, K = 0.4 * 3.0 = 1.2. y base 0.94, x base 1 - 0.06 / 1.2 = 0.95.
        close(y60, 1.0 - 0.94_f32.powf(0.5), "y rate at 60 fps");
        close(y30, 1.0 - 0.94_f32.powf(1.0), "y rate at 30 fps");
        close(x60, 1.0 - 0.95_f32.powf(0.5), "x rate at 60 fps");
        close(x30, 1.0 - 0.95_f32.powf(1.0), "x rate at 30 fps");
        close(
            (1.0 - y60) * (1.0 - y60),
            1.0 - y30,
            "two 60 fps frames equal one 30 fps frame (y)",
        );
        close(
            (1.0 - x60) * (1.0 - x60),
            1.0 - x30,
            "two 60 fps frames equal one 30 fps frame (x)",
        );
        // Zero time moves nothing.
        assert_eq!(adapt_rates(&block, &config, 0.0), (0.0, 0.0));
    }

    /// The step of SPEC-1 section 10 (correction C1):
    /// `sign(delta * rate) * min(|delta|, max(|delta * rate|, 1/256))`, and a NaN target keeps the
    /// previous value.
    #[test]
    fn the_adaptation_step_has_a_floor_and_a_bound() {
        // A large delta moves by delta times the rate.
        close(adapt_step(0.0, 1.0, 0.25), 0.25, "rate step");
        close(adapt_step(1.0, 0.0, 0.25), 0.75, "downward");
        // A small delta moves by the floor 1/256 at least.
        close(
            adapt_step(0.5, 0.5 + 0.1, 0.001),
            0.5 + 1.0 / 256.0,
            "floor",
        );
        close(
            adapt_step(0.5, 0.5 - 0.1, 0.001),
            0.5 - 1.0 / 256.0,
            "floor down",
        );
        // The step never passes the target.
        close(adapt_step(0.5, 0.5 + 0.001, 0.001), 0.501, "bound");
        // At rate 0 nothing moves: the sign comes from `delta * rate`.
        assert_eq!(adapt_step(0.5, 0.9, 0.0), 0.5);
        assert_eq!(adapt_step(0.5, 0.5, 0.9), 0.5);
        assert_eq!(adapt_step(0.7, f32::NAN, 0.9), 0.7);
    }

    // ----- the IMGS selection ---------------------------------------------------------------

    /// A record whose fields all equal `seed` plus the field index, so a swapped field shows.
    fn record(seed: f32) -> ImageSpace {
        ImageSpace {
            eye_adaptation_speed: Some(seed),
            bloom_blur_radius: Some(seed + 1.0),
            bloom_threshold: Some(seed + 2.0),
            bloom_scale: Some(seed + 3.0),
            receive_bloom_threshold: Some(seed + 4.0),
            white: Some(seed + 5.0),
            sunlight_scale: Some(seed + 6.0),
            sky_scale: Some(seed + 7.0),
            eye_adaptation_strength: Some(seed + 8.0),
            saturation: Some(seed + 9.0),
            brightness: Some(seed + 10.0),
            contrast: Some(seed + 11.0),
            tint_amount: Some(seed + 12.0),
            tint: Some([seed + 13.0, seed + 14.0, seed + 15.0]),
        }
    }

    const TIMING: SunTiming = SunTiming([33, 60, 96, 123]);

    fn weather(forms: [Option<u32>; 4]) -> WeatherSky {
        let mut weather = Weather {
            editor_id: Some("TestWeather".to_owned()),
            image_spaces: forms,
            fog: WeatherFog::default(),
            ..Default::default()
        };
        for time in TimeOfDay::ALL {
            for name in ["sky_upper", "sky_lower", "horizon", "fog_near", "fog_far"] {
                weather.colors.insert((name.to_owned(), time), [1, 2, 3]);
            }
        }
        WeatherSky::from_weather(0x81A, &weather, TIMING).unwrap()
    }

    fn database(form: u32) -> Option<ImageSpace> {
        match form {
            0x10 => Some(record(100.0)),
            0x11 => Some(record(200.0)),
            0x12 => Some(record(300.0)),
            0x13 => Some(record(400.0)),
            DEFAULT_IMAGE_SPACE_FORM => Some(record(900.0)),
            0x77 => Some(record(700.0)),
            _ => None,
        }
    }

    #[test]
    fn an_exterior_takes_the_weather_form_of_the_day_part() {
        let sky = weather([Some(0x10), Some(0x11), Some(0x12), Some(0x13)]);
        let lookup: &dyn Fn(u32) -> Option<ImageSpace> = &database;
        // Noon is day only.
        let (block, report) =
            select_block(SceneImageSpace::Exterior, Some(&sky), 12.0, Some(lookup));
        assert_eq!(report.source, "weather");
        assert_eq!(report.forms, vec![(0x11, 1.0)]);
        assert!(report.missing_records.is_empty() && report.defaulted_fields.is_empty());
        assert_eq!(block.white, 205.0);
        assert_eq!(block.eye_adapt_strength, 208.0);
        assert_eq!(block.tint_rgb, [213.0, 214.0, 215.0]);
        // Midnight is night only.
        let (night, _) = select_block(SceneImageSpace::Exterior, Some(&sky), 0.0, Some(lookup));
        assert_eq!(night.white, 405.0);
        // Hour 18.5 is the sunset peak (the middle of 16.0 to 21.0).
        let (sunset, report) =
            select_block(SceneImageSpace::Exterior, Some(&sky), 18.5, Some(lookup));
        assert_eq!(report.forms, vec![(0x12, 1.0)]);
        assert_eq!(sunset.white, 305.0);
    }

    /// Between two day parts the block is the weighted sum of the two forms, all 20 floats.
    #[test]
    fn between_day_parts_the_block_is_the_weighted_sum() {
        let sky = weather([Some(0x10), Some(0x11), Some(0x12), Some(0x13)]);
        let lookup: &dyn Fn(u32) -> Option<ImageSpace> = &database;
        // 8.75 is halfway from the sunrise peak (7.5) to day: weights 0.5 sunrise and 0.5 day.
        let (block, report) =
            select_block(SceneImageSpace::Exterior, Some(&sky), 8.75, Some(lookup));
        assert_eq!(report.forms.len(), 2);
        close(block.white, 0.5 * 105.0 + 0.5 * 205.0, "white");
        close(block.contrast, 0.5 * 111.0 + 0.5 * 211.0, "contrast");
        close(block.tint_rgb[2], 0.5 * 115.0 + 0.5 * 215.0, "tint blue");
        let weight_sum: f32 = report.forms.iter().map(|(_, weight)| weight).sum();
        close(weight_sum, 1.0, "weights sum to 1");
    }

    /// A day part with no form uses `DefaultImageSpace` (0x167) (NEEDS 18,
    /// https://github.com/Mudcrab-Team/skyrim_decomp/pull/1059).
    #[test]
    fn a_missing_day_part_form_uses_the_default_image_space() {
        let sky = weather([Some(0x10), None, Some(0x12), Some(0x13)]);
        let lookup: &dyn Fn(u32) -> Option<ImageSpace> = &database;
        let (block, report) =
            select_block(SceneImageSpace::Exterior, Some(&sky), 12.0, Some(lookup));
        assert_eq!(report.forms, vec![(DEFAULT_IMAGE_SPACE_FORM, 1.0)]);
        assert_eq!(block.white, 905.0);
    }

    /// An interior with its own image space ignores the weather and takes the cell's form.
    #[test]
    fn an_interior_takes_its_cells_image_space_not_the_weather() {
        let sky = weather([Some(0x10), Some(0x11), Some(0x12), Some(0x13)]);
        let lookup: &dyn Fn(u32) -> Option<ImageSpace> = &database;
        let cell = SceneImageSpace::Interior(Some(ImageSpaceSelection {
            source: ImageSpaceSource::Xcim,
            form_id: Some(0x77),
        }));
        let (block, report) = select_block(cell, Some(&sky), 12.0, Some(lookup));
        assert_eq!(report.source, "cell");
        assert_eq!(report.forms, vec![(0x77, 1.0)]);
        assert_eq!(block.white, 705.0);
        // A cell with no XCIM uses the default image space, which the selection already names.
        let default = SceneImageSpace::Interior(Some(ImageSpaceSelection {
            source: ImageSpaceSource::DefaultImageSpace,
            form_id: Some(DEFAULT_IMAGE_SPACE_FORM),
        }));
        assert_eq!(
            select_block(default, Some(&sky), 12.0, Some(lookup))
                .0
                .white,
            905.0
        );
        // An interior that uses sky lighting takes the weather's block.
        let sky_lit = SceneImageSpace::Interior(Some(ImageSpaceSelection {
            source: ImageSpaceSource::WeatherGlobal,
            form_id: None,
        }));
        assert_eq!(
            select_block(sky_lit, Some(&sky), 12.0, Some(lookup))
                .0
                .white,
            205.0
        );
    }

    /// Nothing is invented: a missing record, a missing field and an unknown cell link all give
    /// the labelled constructor default and are reported.
    #[test]
    fn a_missing_record_or_field_takes_the_constructor_default_and_is_reported() {
        let defaults = HdrBlock::IMGS_CONSTRUCTOR_DEFAULT;
        let sky = weather([Some(0x10), Some(0x99), Some(0x12), Some(0x13)]);
        let lookup: &dyn Fn(u32) -> Option<ImageSpace> = &database;
        let (block, report) =
            select_block(SceneImageSpace::Exterior, Some(&sky), 12.0, Some(lookup));
        assert_eq!(report.missing_records, vec![0x99]);
        assert_eq!(block, defaults);
        assert_eq!(report.defaulted_fields.len(), FIELD_NAMES.len());

        // One field of one record is missing.
        let partial = |form: u32| {
            database(form).map(|mut record| {
                record.sunlight_scale = None;
                record
            })
        };
        let lookup: &dyn Fn(u32) -> Option<ImageSpace> = &partial;
        let sky = weather([Some(0x10), Some(0x11), Some(0x12), Some(0x13)]);
        let (block, report) =
            select_block(SceneImageSpace::Exterior, Some(&sky), 12.0, Some(lookup));
        assert_eq!(report.defaulted_fields, vec!["sunlight_scale"]);
        assert_eq!(block.sunlight_scale, defaults.sunlight_scale);
        assert_eq!(
            block.white, 205.0,
            "the other fields keep the record's values"
        );

        // An interior whose link is not in the database, an exterior with no weather and a run
        // with no database.
        let (block, report) = select_block(
            SceneImageSpace::Interior(None),
            Some(&sky),
            12.0,
            Some(lookup),
        );
        assert_eq!(block, defaults);
        assert!(report.reason.is_some());
        let (block, report) = select_block(SceneImageSpace::Exterior, None, 12.0, Some(lookup));
        assert_eq!(block, defaults);
        assert!(report.reason.is_some());
        let (block, report) = select_block(SceneImageSpace::Exterior, Some(&sky), 12.0, None);
        assert_eq!(block, defaults);
        assert!(report.reason.is_some());
    }

    /// The constructor default of the SunlightScale is the 1.8 the object LOD path already uses.
    #[test]
    fn the_constructor_default_keeps_the_object_lod_sunlight_scale() {
        assert_eq!(HdrBlock::IMGS_CONSTRUCTOR_DEFAULT.sunlight_scale, 1.8);
    }

    // ----- the profile gate -----------------------------------------------------------------

    fn gate_app(profile: VisualProfile) -> (App, Entity) {
        let mut app = App::new();
        app.insert_resource(SceneGradeSource::default().with_weather(profile, None))
            .init_resource::<SceneGradeMode>()
            .init_resource::<SceneVisualPreset>()
            .add_systems(
                Update,
                (
                    attach_retail_hdr,
                    crate::color_pipeline::apply_scene_grade_mode,
                ),
            );
        let camera = app.world_mut().spawn(SceneColorPipeline::default()).id();
        app.update();
        (app, camera)
    }

    /// `mudcrab` keeps Bevy's tonemapping and gets no retail pass. `skyrim` gets the pass and no
    /// Bevy transform.
    #[test]
    fn only_the_skyrim_profile_gets_the_retail_chain() {
        let (mudcrab, camera) = gate_app(VisualProfile::Mudcrab);
        assert!(mudcrab.world().get::<RetailHdr>(camera).is_none());
        assert_eq!(
            *mudcrab.world().get::<Tonemapping>(camera).unwrap(),
            Tonemapping::TonyMcMapface
        );
        let (skyrim, camera) = gate_app(VisualProfile::Skyrim);
        assert!(skyrim.world().get::<RetailHdr>(camera).is_some());
        assert_eq!(
            *skyrim.world().get::<Tonemapping>(camera).unwrap(),
            Tonemapping::None
        );
    }

    #[test]
    fn a_reflection_camera_never_gets_the_retail_pass() {
        let mut app = App::new();
        app.insert_resource(SceneGradeSource::default().with_weather(VisualProfile::Skyrim, None))
            .add_systems(Update, attach_retail_hdr);
        let reflection = app.world_mut().spawn(SceneColorPipeline::reflection()).id();
        app.update();
        assert!(app.world().get::<RetailHdr>(reflection).is_none());
    }

    #[test]
    fn the_profile_picks_the_display_transform_and_the_exposure() {
        let selected = Tonemapping::AcesFitted;
        assert_eq!(
            VisualProfile::Mudcrab.display_tonemapping(selected),
            selected
        );
        assert_eq!(
            VisualProfile::Skyrim.display_tonemapping(selected),
            Tonemapping::None
        );
        assert_eq!(VisualProfile::Mudcrab.scene_exposure_ev100(8.5), 8.5);
        assert_eq!(
            VisualProfile::Skyrim.scene_exposure_ev100(8.5),
            crate::retail_lighting::UNIT_EXPOSURE_EV100
        );
    }

    // ----- the uniform and the render state --------------------------------------------------

    #[test]
    fn the_shader_uniform_matches_the_component_layout() {
        let library = include_str!("shaders/retail_hdr.wgsl")
            .replace("#define_import_path engine::retail_hdr", "");
        let module = wgpu::naga::front::wgsl::parse_str(&library).unwrap();
        let uniform = module
            .types
            .iter()
            .find_map(|(_, ty)| (ty.name.as_deref() == Some("RetailHdr")).then_some(&ty.inner))
            .unwrap();
        let wgpu::naga::TypeInner::Struct { members, span } = uniform else {
            panic!("the retail HDR uniform must be a struct");
        };
        assert_eq!(u64::from(*span), RetailHdr::min_size().get());
        assert_eq!(
            members
                .iter()
                .map(|member| member.offset)
                .collect::<Vec<_>>(),
            [0, 16, 32, 48, 64, 80, 96]
        );
    }

    /// The saturate draw is one Min blend of 1.0 on the colour channels, so the target becomes
    /// min(1, target). Alpha is left as it is (D3).
    #[test]
    fn the_fog_composite_is_saturated_by_a_min_blend() {
        use bevy::render::render_resource::{BlendFactor, BlendOperation};
        assert_eq!(
            crate::retail_hdr_render::SATURATE_BLEND.color.operation,
            BlendOperation::Min
        );
        assert_eq!(
            crate::retail_hdr_render::SATURATE_BLEND.color.src_factor,
            BlendFactor::One
        );
        assert_eq!(
            crate::retail_hdr_render::SATURATE_BLEND.color.dst_factor,
            BlendFactor::One
        );
        let shader = include_str!("shaders/retail_hdr_saturate.wgsl");
        assert!(shader.contains("return vec4<f32>(1.0);"));
    }

    /// The tonemap input of an exterior pixel is the composite output, which is saturated:
    /// `retail_fog::composite_saturated` is the same clip the Min blend draws.
    #[test]
    fn the_composite_output_is_clipped_before_the_tonemap() {
        let k = crate::output_scale::framebuffer_output_scale(true);
        let over = crate::retail_fog::composite(3.0, 1.0, 0.0, 0.6, k);
        assert!(over > 1.0, "the blend alone leaves {over} above 1");
        assert_eq!(over.clamp(0.0, 1.0), 1.0);
        assert_eq!(
            crate::retail_fog::composite_saturated(3.0, 1.0, 0.0, 0.6, k),
            1.0
        );
    }

    /// The bloom enters the tonemap where the toned luminance is below the receive threshold
    /// (`b * saturate(ReceiveBloomThreshold - Lt)`).
    #[test]
    fn the_bloom_is_added_below_the_receive_threshold() {
        let block = HdrBlock {
            receive_bloom_threshold: 0.8,
            ..neutral_block()
        };
        let hdr = RetailHdr::from_block(&block, &RetailHdrConfig::default(), 1.0 / 60.0);
        let scene = [0.2_f32, 0.2, 0.2];
        let without = hdr.display_value(scene, [0.5, 0.5], [0.0; 3]);
        let with = hdr.display_value(scene, [0.5, 0.5], [0.1, 0.2, 0.3]);
        let w2 = 1.0 / (1.1_f32 * 1.1);
        let l = 0.2_f32;
        let lt = tone_curve(l, w2, false);
        for (i, bloom) in [0.1_f32, 0.2, 0.3].into_iter().enumerate() {
            close(with[i] - without[i], bloom * (0.8 - lt), "bloom share");
        }
        // A pixel whose toned luminance is above the threshold receives none.
        let bright = [2.0_f32, 2.0, 2.0];
        assert_eq!(
            hdr.display_value(bright, [0.5, 0.5], [0.5; 3]),
            hdr.display_value(bright, [0.5, 0.5], [0.0; 3])
        );
    }

    /// The pass settings carry the bloom threshold, scale, radius, `k_r` and the weight table.
    #[test]
    fn the_settings_carry_the_bloom_parameters_of_the_image_space() {
        let block = HdrBlock {
            bloom_threshold: 0.8,
            bloom_scale: 4.0,
            bloom_blur_radius: 7.0,
            receive_bloom_threshold: 0.9,
            ..neutral_block()
        };
        let config = RetailHdrConfig {
            bloom_threshold_boost: 0.05,
            ..RetailHdrConfig::default()
        };
        let hdr = RetailHdr::from_block(&block, &config, 0.0);
        close(hdr.bloom.x, 0.85, "threshold plus the global boost");
        assert_eq!(hdr.bloom.y, 4.0);
        assert_eq!(hdr.bloom.z, 7.0);
        assert_eq!(
            hdr.bloom.w,
            crate::output_scale::framebuffer_output_scale(true)
        );
        assert_eq!(hdr.adapt.z, 0.9);
        assert!(hdr.multiple_luminance_references());
        assert_eq!(
            hdr.weights[7].x,
            crate::retail_hdr_chain::BLOOM_WEIGHTS[6][0]
        );
        assert_eq!(
            hdr.weights[0].x,
            crate::retail_hdr_chain::BLOOM_WEIGHTS[6][7]
        );
        assert_eq!(
            hdr.weights[14].x,
            crate::retail_hdr_chain::BLOOM_WEIGHTS[6][7]
        );
        let one = RetailHdr::from_block(
            &HdrBlock {
                bloom_blur_radius: 1.0,
                ..block
            },
            &config,
            0.0,
        );
        assert_eq!(one.bloom.z, 1.0);
        assert_eq!(one.weights[5].x, 0.0);
    }

    /// A world space with a lighting template reads the cell under the camera (NEEDS 27).
    #[test]
    fn a_templated_exterior_takes_the_cells_image_space() {
        let sky = weather([Some(0x10), Some(0x11), Some(0x12), Some(0x13)]);
        let lookup: &dyn Fn(u32) -> Option<ImageSpace> = &database;
        let run = |scene| select_block(scene, Some(&sky), 12.0, Some(lookup));
        let (block, report) = run(SceneImageSpace::TemplatedExterior(Some(Some(0x77))));
        assert_eq!(report.source, "cell");
        assert_eq!(block.white, 705.0);
        let (block, report) = run(SceneImageSpace::TemplatedExterior(Some(None)));
        assert_eq!(report.forms, vec![(DEFAULT_IMAGE_SPACE_FORM, 1.0)]);
        assert_eq!(block.white, 905.0);
        let (block, report) = run(SceneImageSpace::TemplatedExterior(None));
        assert_eq!(block, HdrBlock::IMGS_CONSTRUCTOR_DEFAULT);
        assert!(report.reason.is_some());
    }

    #[test]
    fn the_ini_keys_default_to_retail_and_validate() {
        let config = crate::config::EngineConfig::default();
        assert_eq!(config.retail_hdr, RetailHdrConfig::default());
        assert!(!config.retail_hdr.filmic);
        assert_eq!(config.retail_hdr.reinhard_white_scale, 1.1);
        assert_eq!(config.retail_hdr.filmic_white_scale, 1.4);
        assert_eq!(config.retail_hdr.eye_adapt_speed_scale, 2.0);
        assert_eq!(config.retail_hdr.eye_adapt_strength_scale, 0.4);
        assert_eq!(config.retail_hdr.gamma, 1.0);
    }
}
