//! Shared scene-linear composition for world views and diagnostic fixtures.
//!
//! HDR moves the display transform after sky, fog and transparent composition. The exposure
//! and tone curve below are pinned Bevy baselines. The experimental grade uses reported
//! Skyrim image-space (`IMGS`) values with Bevy saturation and an sRGB-domain post pass.
//! Its appearance was approved in Riverwood; its mapping is not verified retail parity.

use crate::console::{AppConsoleExt, ConsoleCommand};
use crate::sky::SkyPalette;
use crate::tone_mapping::{SceneToneMapMode, SceneToneMapSource};
use crate::world::imagespace::{LoadedImageSpace, load_image_space};
use bevy::{
    asset::embedded_asset,
    camera::{CompositingSpace, Exposure, Hdr},
    core_pipeline::{
        Core3dSystems,
        fullscreen_material::{FullscreenMaterial, FullscreenMaterialPlugin},
        tonemapping::{Tonemapping, tonemapping as tonemapping_system},
    },
    ecs::{schedule::ScheduleConfigs, system::BoxedSystem},
    prelude::*,
    render::{
        extract_component::ExtractComponent,
        render_resource::ShaderType,
        view::{ColorGrading, ColorGradingGlobal, ColorGradingSection},
    },
    shader::ShaderRef,
};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Diagnostic selection; exposure, tone mapping, lighting and fog stay fixed.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SceneGradeMode {
    Off,
    Saturation,
    #[default]
    Full,
    SnapshotSrgb,
    SnapshotLinear,
    RecordSrgb,
}

impl SceneGradeMode {
    pub fn index(self) -> usize {
        match self {
            Self::Off => 0,
            Self::Saturation => 1,
            Self::Full => 2,
            Self::SnapshotSrgb => 3,
            Self::SnapshotLinear => 4,
            Self::RecordSrgb => 5,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Some(match index {
            0 => Self::Off,
            1 => Self::Saturation,
            2 => Self::Full,
            3 => Self::SnapshotSrgb,
            4 => Self::SnapshotLinear,
            5 => Self::RecordSrgb,
            _ => return None,
        })
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "saturation" => Some(Self::Saturation),
            "full" => Some(Self::Full),
            "snapshot-srgb" => Some(Self::SnapshotSrgb),
            "snapshot-linear" => Some(Self::SnapshotLinear),
            "record-srgb" => Some(Self::RecordSrgb),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Saturation => "saturation",
            Self::Full => "full",
            Self::SnapshotSrgb => "snapshot-srgb",
            Self::SnapshotLinear => "snapshot-linear",
            Self::RecordSrgb => "record-srgb",
        }
    }

    /// Resolve the screenshot's grade independently of the selected sky preset.
    pub fn grade(self, preset: SceneVisualPreset) -> SkyrimCinematicGrade {
        match self {
            Self::SnapshotSrgb => ApprovedLook::builtin().grade,
            Self::SnapshotLinear => SkyrimCinematicGrade::SLEEPING_GIANT_SNAPSHOT,
            Self::RecordSrgb => {
                panic!("record-srgb requires a loaded image space; use SceneGradeSource::resolve")
            }
            _ => preset.grade(),
        }
    }

    pub fn bevy_saturation(self, preset: SceneVisualPreset) -> f32 {
        match self {
            Self::Off | Self::SnapshotSrgb | Self::SnapshotLinear | Self::RecordSrgb => 1.0,
            Self::Saturation | Self::Full => preset.grade().saturation,
        }
    }

    pub fn post(self, preset: SceneVisualPreset) -> SkyrimCinematicPost {
        let grade = self.grade(preset);
        let mut post = grade.post();
        match self {
            Self::Off | Self::Saturation => post.enabled = 0.0,
            Self::Full => {}
            Self::SnapshotSrgb | Self::SnapshotLinear | Self::RecordSrgb => {
                post.saturation = grade.saturation;
                post.linear_domain = u32::from(self == Self::SnapshotLinear);
            }
        }
        post
    }
}

/// User-selected field overrides layered over the currently resolved grade source.
/// `None` preserves the approved, record-derived, or legacy source value.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneGradeOverrides {
    pub saturation: Option<f32>,
    pub brightness: Option<f32>,
    pub contrast: Option<f32>,
    pub tint_amount: Option<f32>,
    pub tint_red: Option<f32>,
    pub tint_green: Option<f32>,
    pub tint_blue: Option<f32>,
    pub pivot: Option<f32>,
}

impl SceneGradeOverrides {
    pub fn validate(self) -> Result<(), String> {
        for (name, value, min, max) in [
            ("saturation", self.saturation, 0.0, 3.0),
            ("brightness", self.brightness, 0.0, 3.0),
            ("contrast", self.contrast, 0.0, 3.0),
            ("tint amount", self.tint_amount, 0.0, 1.0),
            ("tint red", self.tint_red, 0.0, 1.0),
            ("tint green", self.tint_green, 0.0, 1.0),
            ("tint blue", self.tint_blue, 0.0, 1.0),
            ("pivot", self.pivot, 0.01, 0.99),
        ] {
            if value.is_some_and(|value| !value.is_finite() || !(min..=max).contains(&value)) {
                return Err(format!(
                    "grade {name} override must be finite and between {min} and {max}"
                ));
            }
        }
        Ok(())
    }

    fn apply(self, grade: &mut SkyrimCinematicGrade) {
        if let Some(value) = self.saturation {
            grade.saturation = value;
        }
        if let Some(value) = self.brightness {
            grade.brightness = value;
        }
        if let Some(value) = self.contrast {
            grade.contrast = value;
        }
        if let Some(value) = self.tint_amount {
            grade.tint_amount = value;
        }
        if let Some(value) = self.tint_red {
            grade.tint_rgb[0] = value;
        }
        if let Some(value) = self.tint_green {
            grade.tint_rgb[1] = value;
        }
        if let Some(value) = self.tint_blue {
            grade.tint_rgb[2] = value;
        }
    }
}

pub const SCENE_GRADE_VALUES: &str =
    "off, saturation, full, snapshot-srgb, snapshot-linear or record-srgb";
const SCENE_GRADE_USAGE: &str =
    "SceneGrade [off|saturation|full|snapshot-srgb|snapshot-linear|record-srgb]";

/// Explicit artistic override kept separate from authored database records.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedLook {
    name: String,
    source: String,
    raw_snapshot: SkyrimCinematicGrade,
    grade: SkyrimCinematicGrade,
    sky: ApprovedSky,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedSky {
    upper_srgb_u8: [u8; 3],
    lower_srgb_u8: [u8; 3],
    horizon_srgb_u8: [u8; 3],
    fog_far_srgb_u8: [u8; 3],
    fog_density_scale: f32,
}

impl ApprovedLook {
    fn parse(json: &str) -> color_eyre::Result<Self> {
        let look: Self = serde_json::from_str(json)?;
        color_eyre::eyre::ensure!(!look.name.trim().is_empty(), "grade preset needs a name");
        for grade in [look.raw_snapshot, look.grade] {
            color_eyre::eyre::ensure!(
                [grade.saturation, grade.brightness, grade.contrast]
                    .into_iter()
                    .all(|v| v.is_finite() && v >= 0.0)
                    && grade.tint_amount.is_finite()
                    && (0.0..=1.0).contains(&grade.tint_amount)
                    && grade
                        .tint_rgb
                        .into_iter()
                        .all(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
                "grade preset values must be finite, nonnegative, with tint in 0..1"
            );
        }
        color_eyre::eyre::ensure!(
            look.sky.fog_density_scale.is_finite() && look.sky.fog_density_scale > 0.0,
            "fog density scale must be finite and positive"
        );
        Ok(look)
    }

    fn builtin() -> &'static Self {
        static LOOK: OnceLock<ApprovedLook> = OnceLock::new();
        LOOK.get_or_init(|| {
            Self::parse(include_str!("presets/approved-snapshot.json"))
                .expect("valid built-in approved look")
        })
    }

    fn sky_palette(&self) -> SkyPalette {
        let rgb = |[r, g, b]: [u8; 3]| Color::srgb_u8(r, g, b);
        SkyPalette {
            upper: rgb(self.sky.upper_srgb_u8),
            lower: rgb(self.sky.lower_srgb_u8),
            horizon: rgb(self.sky.horizon_srgb_u8),
            fog_far: rgb(self.sky.fog_far_srgb_u8),
            fog_density_scale: self.sky.fog_density_scale,
            ..SkyPalette::SKYRIM_CLEAR_DAY
        }
    }
}

/// Loaded source data and its provenance; never substitutes a guessed image space.
#[derive(Resource, Clone, Debug)]
pub struct SceneGradeSource {
    approved: ApprovedLook,
    approved_path: String,
    pub record: Option<LoadedImageSpace>,
}

impl Default for SceneGradeSource {
    fn default() -> Self {
        Self {
            approved: ApprovedLook::builtin().clone(),
            approved_path: "builtin:presets/approved-snapshot.json".into(),
            record: None,
        }
    }
}

pub struct ResolvedSceneGrade {
    pub grade: SkyrimCinematicGrade,
    pub post: SkyrimCinematicPost,
    pub bevy_saturation: f32,
    pub tonemapping: Tonemapping,
}

impl SceneGradeSource {
    pub fn load(
        config: &crate::config::EngineConfig,
        retail_world: bool,
    ) -> color_eyre::Result<Self> {
        let mut source = Self::default();
        if let Some(path) = &config.grade_preset {
            source.approved = ApprovedLook::parse(&std::fs::read_to_string(path)?)?;
            source.approved_path = path.display().to_string();
        }
        if let Some(selection) = config.image_space_selection()? {
            color_eyre::eyre::ensure!(
                retail_world,
                "image-space selection requires a converted world database; fixture and synthetic runs stay independent"
            );
            source.record = Some(load_image_space(
                &config.assets_dir.join("skyrim_world.db"),
                selection,
            )?);
        }
        source
            .resolve(config.color_grade, config.visual_preset)
            .map_err(color_eyre::eyre::Error::msg)?;
        Ok(source)
    }

    pub fn sky_palette(&self, preset: SceneVisualPreset) -> SkyPalette {
        match preset {
            SceneVisualPreset::CoolGray => self.approved.sky_palette(),
            SceneVisualPreset::Vivid => SkyPalette::SKYRIM_CLEAR_DAY,
        }
    }

    pub fn resolve(
        &self,
        mode: SceneGradeMode,
        preset: SceneVisualPreset,
    ) -> Result<ResolvedSceneGrade, String> {
        self.resolve_with_overrides(mode, preset, SceneGradeOverrides::default())
    }

    pub fn resolve_with_overrides(
        &self,
        mode: SceneGradeMode,
        preset: SceneVisualPreset,
        overrides: SceneGradeOverrides,
    ) -> Result<ResolvedSceneGrade, String> {
        overrides.validate()?;
        let grade = match mode {
            SceneGradeMode::SnapshotSrgb => self.approved.grade,
            SceneGradeMode::RecordSrgb => {
                let record = self.record.as_ref().ok_or("record-srgb requires --image-space <FormID> or --image-space-weather <FormID> with --image-space-slot <slot>")?;
                SkyrimCinematicGrade {
                    saturation: record.cinematic.saturation,
                    brightness: record.cinematic.brightness,
                    contrast: record.cinematic.contrast,
                    tint_amount: record.tint.amount,
                    tint_rgb: record.tint.rgb,
                }
            }
            _ => mode.grade(preset),
        };
        let mut grade = grade;
        overrides.apply(&mut grade);
        let mut post = grade.post();
        if let Some(pivot) = overrides.pivot {
            post.pivot = pivot;
        }
        match mode {
            SceneGradeMode::Off | SceneGradeMode::Saturation => post.enabled = 0.0,
            SceneGradeMode::SnapshotSrgb
            | SceneGradeMode::SnapshotLinear
            | SceneGradeMode::RecordSrgb => {
                post.saturation = grade.saturation;
                post.linear_domain = u32::from(mode == SceneGradeMode::SnapshotLinear);
            }
            SceneGradeMode::Full => {}
        }
        Ok(ResolvedSceneGrade {
            grade,
            post,
            bevy_saturation: if matches!(mode, SceneGradeMode::Saturation | SceneGradeMode::Full) {
                grade.saturation
            } else {
                mode.bevy_saturation(preset)
            },
            tonemapping: Tonemapping::TonyMcMapface,
        })
    }

    pub fn evidence(&self, mode: SceneGradeMode, preset: SceneVisualPreset) -> serde_json::Value {
        self.evidence_with_overrides(mode, preset, SceneGradeOverrides::default())
    }

    pub fn evidence_with_overrides(
        &self,
        mode: SceneGradeMode,
        preset: SceneVisualPreset,
        overrides: SceneGradeOverrides,
    ) -> serde_json::Value {
        let resolved = self.resolve_with_overrides(mode, preset, overrides);
        let base = self.resolve(mode, preset);
        serde_json::json!({
            "mode": mode.name(),
            "source_kind": if mode == SceneGradeMode::RecordSrgb { "authored-image-space-record" } else if mode == SceneGradeMode::SnapshotSrgb { "approved-artistic-override" } else { "legacy-preset" },
            "grade_base_identity": if mode == SceneGradeMode::RecordSrgb { "loaded-image-space-record" } else if mode == SceneGradeMode::SnapshotSrgb { self.approved_path.as_str() } else { preset.name() },
            "approved_override": { "path": self.approved_path, "data": self.approved, "applied_to_grade": mode == SceneGradeMode::SnapshotSrgb, "applied_to_sky": preset == SceneVisualPreset::CoolGray },
            "loaded_record": self.record,
            "base_grade": base.as_ref().ok().map(|resolved| resolved.grade),
            "grade_overrides": overrides,
            "hnam_applied": false,
            "effective_grade": resolved.as_ref().ok().map(|r| r.grade),
            "effective_stages": resolved.as_ref().ok().map(|r| serde_json::json!({
                "bevy_post_saturation": r.bevy_saturation,
                "post_enabled": r.post.enabled,
                "post_saturation": r.post.saturation,
                "post_linear_domain": r.post.linear_domain,
                "pivot": r.post.pivot,
            })),
            "pivot": resolved.as_ref().ok().map(|r| r.post.pivot),
            "default_pivot": CINEMATIC_PIVOT,
            "resolution_error": resolved.err(),
            "tonemapping": "TonyMcMapface", "exposure_ev100": DEFAULT_SCENE_EV100,
            "limits": "CNAM/TNAM experiment only; authored slot is not effective runtime weather; HNAM, adaptation, native tone curve, bloom and modifier composition remain unapplied"
        })
    }
}

/// Identifies shared scene cameras without treating reflections as graded views.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum SceneGradeTarget {
    Scene,
    Reflection,
}

/// Named artistic looks. Vivid preserves the approved clear-day configuration.
/// Exposure, tone mapping, lights and shadows are shared.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SceneVisualPreset {
    #[default]
    Vivid,
    CoolGray,
}

impl SceneVisualPreset {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "vivid" => Some(Self::Vivid),
            "cool-gray" => Some(Self::CoolGray),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Vivid => "vivid",
            Self::CoolGray => "cool-gray",
        }
    }

    pub fn grade(self) -> SkyrimCinematicGrade {
        match self {
            Self::Vivid => SkyrimCinematicGrade::SKYRIM_CLEAR_DAY,
            Self::CoolGray => SkyrimCinematicGrade {
                saturation: 1.25,
                ..SkyrimCinematicGrade::SKYRIM_CLEAR_DAY
            },
        }
    }

    /// Authored sky and fog settings, before tone mapping and grading.
    pub fn sky_palette(self) -> SkyPalette {
        match self {
            Self::Vivid => SkyPalette::SKYRIM_CLEAR_DAY,
            Self::CoolGray => ApprovedLook::builtin().sky_palette(),
        }
    }
}

fn current_grade_overrides(world: &World) -> SceneGradeOverrides {
    world
        .get_resource::<SceneGradeOverrides>()
        .copied()
        .or_else(|| {
            world
                .get_resource::<crate::settings::RenderingSettings>()
                .map(crate::settings::RenderingSettings::grade_overrides)
        })
        .unwrap_or_default()
}

fn scene_preset_command() -> ConsoleCommand {
    ConsoleCommand {
        name: "ScenePreset",
        short: None,
        usage: "ScenePreset [vivid|cool-gray|toggle]",
        help: "inspect or switch the sky, fog and color grading preset",
        handler: Box::new(|world, args| {
            let current = *world.resource::<SceneVisualPreset>();
            let next = match args {
                [] => None,
                [value] if value.eq_ignore_ascii_case("toggle") => Some(match current {
                    SceneVisualPreset::Vivid => SceneVisualPreset::CoolGray,
                    SceneVisualPreset::CoolGray => SceneVisualPreset::Vivid,
                }),
                [value] => Some(
                    SceneVisualPreset::parse(value)
                        .ok_or("usage: ScenePreset [vivid|cool-gray|toggle]")?,
                ),
                _ => return Err("usage: ScenePreset [vivid|cool-gray|toggle]".into()),
            };
            if let Some(next) = next {
                let source = world
                    .get_resource::<SceneGradeSource>()
                    .cloned()
                    .unwrap_or_default();
                let tone = world
                    .get_resource::<SceneToneMapSource>()
                    .cloned()
                    .unwrap_or_default();
                let overrides = current_grade_overrides(world);
                tone.resolve_with_overrides(
                    &source,
                    *world.resource::<SceneGradeMode>(),
                    next,
                    overrides,
                )?;
                *world.resource_mut::<SceneVisualPreset>() = next;
                let next_palette = world
                    .get_resource::<SceneGradeSource>()
                    .map_or_else(|| next.sky_palette(), |source| source.sky_palette(next));
                if let Some(mut palette) = world.get_resource_mut::<SkyPalette>() {
                    *palette = next_palette;
                }
                sync_settings_from_console(world);
            }
            Ok(format!(
                "scene preset: {} (grade: {})",
                world.resource::<SceneVisualPreset>().name(),
                world.resource::<SceneGradeMode>().name()
            ))
        }),
    }
}

pub(crate) fn scene_grade_command() -> ConsoleCommand {
    ConsoleCommand {
        name: "SceneGrade",
        short: None,
        usage: SCENE_GRADE_USAGE,
        help: "inspect or change color grading without changing exposure, lighting or fog",
        handler: Box::new(|world, args| {
            match args {
                [] => {}
                [value] => {
                    let mode = SceneGradeMode::parse(value)
                        .ok_or_else(|| format!("usage: {SCENE_GRADE_USAGE}"))?;
                    let source = world
                        .get_resource::<SceneGradeSource>()
                        .cloned()
                        .unwrap_or_default();
                    let tone = world
                        .get_resource::<SceneToneMapSource>()
                        .cloned()
                        .unwrap_or_default();
                    let overrides = current_grade_overrides(world);
                    tone.resolve_with_overrides(
                        &source,
                        mode,
                        *world.resource::<SceneVisualPreset>(),
                        overrides,
                    )?;
                    if let Some(mut file) =
                        world.get_resource_mut::<crate::settings::SettingsFile>()
                    {
                        file.note_grade_mode_selection();
                    }
                    *world.resource_mut::<SceneGradeMode>() = mode;
                    sync_settings_from_console(world);
                }
                _ => return Err(format!("usage: {SCENE_GRADE_USAGE}")),
            }
            Ok(format!(
                "scene grade: {}",
                world.resource::<SceneGradeMode>().name()
            ))
        }),
    }
}

fn scene_tone_map_command() -> ConsoleCommand {
    ConsoleCommand {
        name: "SceneToneMap",
        short: None,
        usage: "SceneToneMap [tony|retail-curve]",
        help: "inspect or select the approved tone mapper or explicit retail-curve experiment",
        handler: Box::new(|world, args| {
            let mut candidate = world
                .get_resource::<SceneToneMapSource>()
                .cloned()
                .unwrap_or_default();
            match args {
                [] => {}
                [value] => {
                    candidate.mode = SceneToneMapMode::parse(value)
                        .ok_or("usage: SceneToneMap [tony|retail-curve]")?;
                    let source = world
                        .get_resource::<SceneGradeSource>()
                        .cloned()
                        .unwrap_or_default();
                    candidate.resolve_with_overrides(
                        &source,
                        *world.resource::<SceneGradeMode>(),
                        *world.resource::<SceneVisualPreset>(),
                        current_grade_overrides(world),
                    )?;
                    world.insert_resource(candidate.clone());
                }
                _ => return Err("usage: SceneToneMap [tony|retail-curve]".into()),
            }
            Ok(format!("scene tone map: {}", candidate.mode.name()))
        }),
    }
}

fn sync_settings_from_console(world: &mut World) {
    if !world.contains_resource::<crate::settings::RenderingSettings>() {
        return;
    }
    let mode = *world.resource::<SceneGradeMode>();
    let preset = *world.resource::<SceneVisualPreset>();
    let source = world
        .get_resource::<SceneGradeSource>()
        .cloned()
        .unwrap_or_default();
    let tone = world
        .get_resource::<SceneToneMapSource>()
        .cloned()
        .unwrap_or_default();
    let overrides = current_grade_overrides(world);
    let Ok(resolved) = tone.resolve_with_overrides(&source, mode, preset, overrides) else {
        return;
    };
    let mut settings = world.resource_mut::<crate::settings::RenderingSettings>();
    settings.color_grade = mode.index();
    settings.visual_preset = usize::from(preset == SceneVisualPreset::CoolGray);
    settings.show_resolved_grade(resolved.grade, resolved.post.pivot);
}

// Bevy injects these resources and the camera query independently.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_scene_grade_mode(
    mode: Res<SceneGradeMode>,
    preset: Res<SceneVisualPreset>,
    source: Option<Res<SceneGradeSource>>,
    fallback: Local<SceneGradeSource>,
    tone_source: Option<Res<SceneToneMapSource>>,
    tone_fallback: Local<SceneToneMapSource>,
    overrides: Option<Res<SceneGradeOverrides>>,
    mut cameras: Query<(
        Ref<SceneGradeTarget>,
        &mut ColorGrading,
        &mut SkyrimCinematicPost,
        &mut Tonemapping,
    )>,
) {
    let source_changed = source.as_ref().is_some_and(|source| source.is_changed());
    let tone_changed = tone_source
        .as_ref()
        .is_some_and(|source| source.is_changed());
    let overrides_changed = overrides
        .as_ref()
        .is_some_and(|overrides| overrides.is_changed());
    let source = source.as_deref().unwrap_or(&fallback);
    let tone = tone_source.as_deref().unwrap_or(&tone_fallback);
    let overrides = overrides.as_deref().copied().unwrap_or_default();
    let resolved = tone
        .resolve_with_overrides(source, *mode, *preset, overrides)
        .expect("validated scene grade source");
    for (target, mut grading, mut post, mut tonemapping) in &mut cameras {
        if *target == SceneGradeTarget::Reflection
            || !(mode.is_changed()
                || preset.is_changed()
                || source_changed
                || tone_changed
                || overrides_changed
                || target.is_added())
        {
            continue;
        }
        grading.global.post_saturation = resolved.bevy_saturation;
        *post = resolved.post;
        *tonemapping = resolved.tonemapping;
    }
}

/// Pinned Bevy exposure baseline; not a calibrated Skyrim image-space value.
pub const DEFAULT_SCENE_EV100: f32 = 9.7;

/// Artistic sRGB-encoded pivot for the experimental brightness/contrast pass.
/// This is not a measured Skyrim adapted luminance. Auto-exposure integration
/// and the relationship to the record's `HNAM` fields remain unresolved.
pub const CINEMATIC_PIVOT: f32 = 0.18;

/// One weather's cinematic grade: the `CNAM` triple and `TNAM` tint of its image
/// space (`IMGS` reached through the weather's `IMSP` slot), in record order.
///
/// Skyrim splits the image space across subrecords (xEdit `wbDefinitionsTES5`, UESP
/// `Skyrim Mod:Mod File Format/IMGS`): `CNAM` holds Saturation, Brightness and Contrast,
/// `TNAM` holds a tint amount followed by RGB, and `HNAM` holds the nine HDR fields (Eye
/// Adapt Speed, Bloom Blur Radius, Bloom Threshold, Bloom Scale, Receive Bloom Threshold,
/// White, Sunlight Scale, Sky Scale, Eye Adapt Strength). The `HNAM` block is not
/// carried here; see [`SkyrimCinematicGrade::color_grading`] for what maps and what
/// does not.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkyrimCinematicGrade {
    pub saturation: f32,
    pub brightness: f32,
    pub contrast: f32,
    /// Experimental interpretation of `TNAM`: mix toward luminance tinted by
    /// [`SkyrimCinematicGrade::tint_rgb`].
    pub tint_amount: f32,
    /// `TNAM` tint colour, RGB in 0..1.
    pub tint_rgb: [f32; 3],
}

impl SkyrimCinematicGrade {
    /// Reported grade for `SkyrimClear`'s day slot, read from the winning records in the
    /// local game data (`Skyrim.esm` as overridden by `Update.esm`): the weather's
    /// `IMSP` day slot names `ISSkyrimClearDAY` (`IMGS` 0x00012F88), whose `CNAM`
    /// holds Saturation 1.5, Brightness 1.135 and Contrast 1.4, and whose `TNAM`
    /// holds tint amount 0.42 toward RGB 0.894, 0.839, 0.773.
    /// These constants preserve the earlier local extraction; this module does
    /// not parse records or verify their mapping into retail shader slots.
    pub const SKYRIM_CLEAR_DAY: Self = Self {
        saturation: 1.5,
        brightness: 1.135,
        contrast: 1.4,
        tint_amount: 0.42,
        tint_rgb: [0.894_118, 0.839_216, 0.772_549],
    };

    /// Rounded Photo Mode UI values supplied at the Sleeping Giant Inn on 2026-10-13.
    /// This is one active base-grade snapshot, not universal Skyrim defaults.
    /// Photo Mode lists RGB controls as Red, Blue, Green; these are stored as RGB.
    pub const SLEEPING_GIANT_SNAPSHOT: Self = Self {
        saturation: 1.66,
        brightness: 1.15,
        contrast: 1.50,
        tint_amount: 0.56,
        tint_rgb: [0.88, 0.81, 0.72],
    };

    /// The post-tonemap pass settings for this grade: brightness, contrast and
    /// tint, applied by [`SkyrimCinematicPost`] after the tone curve.
    pub fn post(&self) -> SkyrimCinematicPost {
        SkyrimCinematicPost {
            brightness: self.brightness,
            contrast: self.contrast,
            tint_amount: self.tint_amount,
            pivot: CINEMATIC_PIVOT,
            tint_rgb: Vec3::from_array(self.tint_rgb),
            enabled: 1.0,
            saturation: 1.0,
            linear_domain: 0,
            tone_operator: 0,
            tone_coefficient: 0.0,
            sampled_luminance: 1.0,
            target_luminance: 1.0,
        }
    }

    /// The Bevy grade for this cinematic grade.
    ///
    /// Bevy applies `post_saturation` after its tone curve, in linear RGB.
    /// Brightness, contrast and tint use the separate sRGB-encoded post pass.
    /// This split preserves the approved experimental appearance; it does not
    /// reproduce a verified Skyrim pipeline. Bevy's luminance weights also
    /// differ slightly from the weights used by the custom tint pass.
    ///
    /// Sectional gamma, lift, contrast and gain remain at identity. Global
    /// exposure, temperature, tint and hue remain at zero. The reported day
    /// slot's `HNAM` fields (Eye Adapt Speed 45.0, Bloom Blur Radius 7.0,
    /// Bloom Threshold 0.8, Bloom Scale 4.0, Receive Bloom Threshold 0.8,
    /// White 1.0, Sunlight Scale 2.8, Sky Scale 0.05, Eye Adapt Strength 5.0)
    /// remain unapplied. No calibrated mapping to Bevy exposure, bloom or
    /// adaptation has been established.
    pub fn color_grading(&self) -> ColorGrading {
        ColorGrading::with_identical_sections(
            ColorGradingGlobal {
                post_saturation: self.saturation,
                ..default()
            },
            ColorGradingSection::default(),
        )
    }
}

/// Post-tonemap cinematic settings for one camera: the part of the image-space
/// grade Bevy's `ColorGrading` cannot hold in the right domain. Carried as a
/// fullscreen-material component on every scene camera. A disabled setting
/// (the water reflection) returns its input untouched in the shader, so HDR
/// values reach the main view unclamped and are graded once there.
#[derive(Component, Clone, Copy, Debug, PartialEq, ShaderType, ExtractComponent)]
#[extract_component_filter(With<Camera>)]
pub struct SkyrimCinematicPost {
    pub brightness: f32,
    pub contrast: f32,
    pub tint_amount: f32,
    /// Display-range pivot for brightness/contrast; see [`CINEMATIC_PIVOT`].
    pub pivot: f32,
    pub tint_rgb: Vec3,
    /// 1.0 applies grading; 0.0 skips grading after any explicitly selected curve.
    pub enabled: f32,
    /// Saturation evaluated in this pass; 1.0 leaves legacy Bevy saturation unchanged.
    pub saturation: f32,
    /// 0 grades sRGB-encoded values; 1 grades linear values, including saturation and tint.
    pub linear_domain: u32,
    /// 0 keeps Bevy's selected mapper; 1/2 apply the explicit scalar curve here.
    pub tone_operator: u32,
    pub tone_coefficient: f32,
    /// Fixed experimental stand-ins for native texture sample P.x/P.y.
    /// They are independent of the artistic grade pivot and have no history.
    pub sampled_luminance: f32,
    pub target_luminance: f32,
}

impl Default for SkyrimCinematicPost {
    /// No grading or curve: pass the input through untouched.
    fn default() -> Self {
        Self {
            brightness: 1.0,
            contrast: 1.0,
            tint_amount: 0.0,
            pivot: CINEMATIC_PIVOT,
            tint_rgb: Vec3::ONE,
            enabled: 0.0,
            saturation: 1.0,
            linear_domain: 0,
            tone_operator: 0,
            tone_coefficient: 0.0,
            sampled_luminance: 1.0,
            target_luminance: 1.0,
        }
    }
}

impl FullscreenMaterial for SkyrimCinematicPost {
    fn fragment_shader() -> ShaderRef {
        "embedded://engine/shaders/cinematic_post.wgsl".into()
    }

    /// Run after Bevy tonemapping, inside `PostProcess` and before upscaling.
    /// The shader expects linear RGB storage and grades its sRGB encoding.
    /// Cameras using this experiment must retain `CompositingSpace::Linear`.
    /// Ordering relative to AA and CAS has not been validated.
    /// The linear snapshot mode grades that storage directly and skips sRGB conversion.
    fn schedule_configs(system: ScheduleConfigs<BoxedSystem>) -> ScheduleConfigs<BoxedSystem> {
        system
            .in_set(Core3dSystems::PostProcess)
            .after(tonemapping_system)
    }
}

/// Registers the post-tonemap cinematic pass and its embedded shader.
pub struct SkyrimCinematicPostPlugin;

impl Plugin for SkyrimCinematicPostPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/cinematic_post.wgsl");
        app.init_resource::<SceneGradeMode>()
            .init_resource::<SceneVisualPreset>()
            .init_resource::<SceneGradeSource>()
            .init_resource::<SceneToneMapSource>()
            .add_console_command(scene_grade_command())
            .add_console_command(scene_preset_command())
            .add_console_command(scene_tone_map_command())
            .add_systems(PostUpdate, apply_scene_grade_mode)
            .add_plugins(FullscreenMaterialPlugin::<SkyrimCinematicPost>::default());
    }
}

/// Explicit camera settings for scene composition. A reflection uses the same exposure but no
/// display transform and no grade: the receiving water surface joins it to the main view
/// before tone mapping, so the main view grades it once.
#[derive(Bundle)]
pub struct SceneColorPipeline {
    pub grade_target: SceneGradeTarget,
    pub hdr: Hdr,
    /// The custom post pass expects linear RGB input.
    pub compositing_space: CompositingSpace,
    pub exposure: Exposure,
    pub tonemapping: Tonemapping,
    pub grading: ColorGrading,
    /// Post-tonemap cinematic pass settings; disabled passes through.
    pub cinematic_post: SkyrimCinematicPost,
}

impl Default for SceneColorPipeline {
    fn default() -> Self {
        Self {
            grade_target: SceneGradeTarget::Scene,
            hdr: Hdr,
            compositing_space: CompositingSpace::Linear,
            exposure: Exposure {
                ev100: DEFAULT_SCENE_EV100,
            },
            tonemapping: Tonemapping::TonyMcMapface,
            grading: SkyrimCinematicGrade::SKYRIM_CLEAR_DAY.color_grading(),
            cinematic_post: SkyrimCinematicGrade::SKYRIM_CLEAR_DAY.post(),
        }
    }
}

impl SceneColorPipeline {
    /// Compose reflected radiance without applying the main view's display transform.
    pub fn reflection() -> Self {
        Self {
            grade_target: SceneGradeTarget::Reflection,
            tonemapping: Tonemapping::None,
            grading: ColorGrading::default(),
            cinematic_post: SkyrimCinematicPost::default(),
            ..default()
        }
    }
}

#[cfg(test)]
fn assert_grading_matches(grading: &ColorGrading, wanted: &SkyrimCinematicGrade) {
    for actual in grading.all_sections() {
        assert_eq!(*actual, ColorGradingSection::default());
    }
    assert_eq!(grading.global.exposure, 0.0);
    assert_eq!(grading.global.temperature, 0.0);
    assert_eq!(grading.global.tint, 0.0);
    assert_eq!(grading.global.hue, 0.0);
    assert_eq!(grading.global.post_saturation, wanted.saturation);
    assert_eq!(grading.global.midtones_range, 0.2..0.7);
}

#[cfg(test)]
pub(crate) fn assert_scene_camera_output(world: &mut World, expected_count: usize) {
    let mut cameras = world.query_filtered::<(
        Option<&Hdr>,
        Option<&Exposure>,
        Option<&Tonemapping>,
        Option<&ColorGrading>,
        Option<&SkyrimCinematicPost>,
    ), With<Camera3d>>();
    let mut count = 0;
    for (hdr, exposure, tonemapping, grading, post) in cameras.iter(world) {
        assert!(hdr.is_some(), "scene camera must compose in HDR");
        assert_eq!(exposure.unwrap().ev100, DEFAULT_SCENE_EV100);
        assert_eq!(*tonemapping.unwrap(), Tonemapping::TonyMcMapface);
        assert_grading_matches(
            grading.expect("scene camera must carry the Skyrim grade"),
            &SkyrimCinematicGrade::SKYRIM_CLEAR_DAY,
        );
        assert_eq!(
            post.expect("scene camera must carry the cinematic post pass"),
            &SkyrimCinematicGrade::SKYRIM_CLEAR_DAY.post()
        );
        count += 1;
    }
    assert_eq!(count, expected_count);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_file_preserves_exact_default_and_runtime_override_is_separate() {
        let source = SceneGradeSource::default();
        assert_eq!(
            source.approved.raw_snapshot,
            SkyrimCinematicGrade::SLEEPING_GIANT_SNAPSHOT
        );
        let resolved = source
            .resolve(SceneGradeMode::SnapshotSrgb, SceneVisualPreset::CoolGray)
            .unwrap();
        assert_eq!(
            resolved.grade,
            SkyrimCinematicGrade {
                brightness: 1.15_f32 * 1.2,
                ..SkyrimCinematicGrade::SLEEPING_GIANT_SNAPSHOT
            }
        );
        assert_eq!(resolved.bevy_saturation, 1.0);
        assert_eq!(resolved.post.saturation, 1.66);
        assert_eq!(resolved.post.linear_domain, 0);
        assert_eq!(
            source.sky_palette(SceneVisualPreset::CoolGray),
            SkyPalette {
                upper: Color::srgb_u8(178, 187, 198),
                lower: Color::srgb_u8(194, 202, 209),
                horizon: Color::srgb_u8(207, 214, 220),
                fog_far: Color::srgb_u8(210, 222, 235),
                fog_density_scale: 0.25,
                ..SkyPalette::SKYRIM_CLEAR_DAY
            }
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom-look.json");
        let mut changed = serde_json::to_value(&source.approved).unwrap();
        changed["grade"]["brightness"] = 1.07.into();
        std::fs::write(&path, changed.to_string()).unwrap();
        let config = crate::config::EngineConfig {
            grade_preset: Some(path),
            ..default()
        };
        let custom = SceneGradeSource::load(&config, false).unwrap();
        assert_eq!(
            custom
                .resolve(SceneGradeMode::SnapshotSrgb, SceneVisualPreset::CoolGray)
                .unwrap()
                .grade
                .brightness,
            1.07
        );
        assert_eq!(
            custom
                .resolve(SceneGradeMode::Full, SceneVisualPreset::Vivid)
                .unwrap()
                .grade,
            SkyrimCinematicGrade::SKYRIM_CLEAR_DAY
        );
        assert_eq!(
            source
                .resolve(SceneGradeMode::SnapshotSrgb, SceneVisualPreset::CoolGray)
                .unwrap()
                .grade
                .brightness,
            1.38
        );
        changed["grade"]["tint_amount"] = 1.5.into();
        assert!(ApprovedLook::parse(&changed.to_string()).is_err());
    }

    #[test]
    fn optional_grade_overrides_layer_on_custom_base_without_mutating_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom-look.json");
        let mut authored = serde_json::to_value(ApprovedLook::builtin()).unwrap();
        authored["grade"]["saturation"] = 1.21.into();
        authored["grade"]["brightness"] = 1.37.into();
        std::fs::write(&path, authored.to_string()).unwrap();
        let config = crate::config::EngineConfig {
            grade_preset: Some(path),
            ..default()
        };
        let source = SceneGradeSource::load(&config, false).unwrap();
        let base = source
            .resolve(SceneGradeMode::SnapshotSrgb, SceneVisualPreset::CoolGray)
            .unwrap();
        let overrides = SceneGradeOverrides {
            saturation: Some(1.8),
            pivot: Some(0.31),
            ..default()
        };
        let tuned = source
            .resolve_with_overrides(
                SceneGradeMode::SnapshotSrgb,
                SceneVisualPreset::CoolGray,
                overrides,
            )
            .unwrap();
        assert_eq!(base.grade.saturation, 1.21);
        assert_eq!(base.grade.brightness, 1.37);
        assert_eq!(tuned.grade.saturation, 1.8);
        assert_eq!(tuned.grade.brightness, 1.37);
        assert_eq!(tuned.post.pivot, 0.31);
        assert_eq!(source.approved.grade.saturation, 1.21);
        assert_eq!(source.approved.grade.brightness, 1.37);
        let evidence = source.evidence_with_overrides(
            SceneGradeMode::SnapshotSrgb,
            SceneVisualPreset::CoolGray,
            overrides,
        );
        assert_eq!(
            evidence["base_grade"]["brightness"],
            serde_json::json!(1.37_f32)
        );
        assert_eq!(
            evidence["grade_overrides"]["saturation"],
            serde_json::json!(1.8_f32)
        );
        assert_eq!(
            evidence["effective_grade"]["saturation"],
            serde_json::json!(1.8_f32)
        );
        assert_eq!(evidence["pivot"], serde_json::json!(0.31_f32));
        assert_eq!(evidence["effective_stages"]["pivot"], evidence["pivot"]);
        assert_eq!(evidence["default_pivot"], serde_json::json!(0.18_f32));
        let untuned = source.evidence(SceneGradeMode::SnapshotSrgb, SceneVisualPreset::CoolGray);
        assert_eq!(untuned["pivot"], serde_json::json!(0.18_f32));
    }

    #[test]
    fn invalid_grade_override_is_rejected_without_changing_source() {
        let source = SceneGradeSource::default();
        let original = source.approved.grade;
        let invalid = SceneGradeOverrides {
            brightness: Some(f32::NAN),
            ..default()
        };
        assert!(
            source
                .resolve_with_overrides(
                    SceneGradeMode::SnapshotSrgb,
                    SceneVisualPreset::CoolGray,
                    invalid,
                )
                .is_err()
        );
        assert_eq!(source.approved.grade, original);
        let evidence = source.evidence_with_overrides(
            SceneGradeMode::SnapshotSrgb,
            SceneVisualPreset::CoolGray,
            invalid,
        );
        assert!(evidence["pivot"].is_null());
        assert!(evidence["effective_stages"].is_null());
        assert!(evidence["resolution_error"].is_string());
        assert_eq!(evidence["default_pivot"], serde_json::json!(0.18_f32));
    }

    #[test]
    fn grade_overrides_reach_scene_cameras_but_never_reflections() {
        let mut app = App::new();
        app.init_resource::<SceneGradeMode>()
            .insert_resource(SceneVisualPreset::CoolGray)
            .insert_resource(SceneGradeSource::default())
            .insert_resource(SceneToneMapSource::default())
            .insert_resource(SceneGradeOverrides {
                brightness: Some(1.73),
                pivot: Some(0.27),
                ..default()
            })
            .add_systems(PostUpdate, apply_scene_grade_mode);
        let scene = app.world_mut().spawn(SceneColorPipeline::default()).id();
        let reflection = app.world_mut().spawn(SceneColorPipeline::reflection()).id();
        app.update();
        let tuned = app.world().get::<SkyrimCinematicPost>(scene).unwrap();
        assert_eq!(tuned.brightness, 1.73);
        assert_eq!(tuned.pivot, 0.27);
        assert_eq!(
            *app.world().get::<SkyrimCinematicPost>(reflection).unwrap(),
            SkyrimCinematicPost::default()
        );
        let later = app.world_mut().spawn(SceneColorPipeline::default()).id();
        app.update();
        assert_eq!(
            app.world()
                .get::<SkyrimCinematicPost>(later)
                .unwrap()
                .brightness,
            1.73
        );
    }

    #[test]
    fn console_mode_and_preset_keep_explicit_tuning_and_sync_settings() {
        let settings = crate::settings::RenderingSettings {
            grade_brightness: 1.75,
            grade_override_mask: 1 << 1,
            ..default()
        };
        let mut app = App::new();
        app.init_resource::<SceneGradeMode>()
            .insert_resource(SceneVisualPreset::CoolGray)
            .insert_resource(SceneGradeSource::default())
            .insert_resource(SceneToneMapSource::default())
            .insert_resource(SceneGradeOverrides {
                brightness: Some(1.75),
                ..default()
            })
            .insert_resource(settings)
            .init_resource::<crate::console::ConsoleState>()
            .add_console_command(scene_grade_command())
            .add_console_command(scene_preset_command())
            .add_systems(PostUpdate, apply_scene_grade_mode);
        let camera = app.world_mut().spawn(SceneColorPipeline::default()).id();
        crate::console::execute_line(app.world_mut(), "SceneGrade full");
        crate::console::execute_line(app.world_mut(), "ScenePreset vivid");
        app.update();
        let world = app.world();
        assert_eq!(*world.resource::<SceneGradeMode>(), SceneGradeMode::Full);
        assert_eq!(
            *world.resource::<SceneVisualPreset>(),
            SceneVisualPreset::Vivid
        );
        assert_eq!(
            world
                .resource::<crate::settings::RenderingSettings>()
                .color_grade,
            SceneGradeMode::Full.index()
        );
        assert_eq!(
            world
                .resource::<crate::settings::RenderingSettings>()
                .visual_preset,
            0
        );
        assert_eq!(
            world.get::<SkyrimCinematicPost>(camera).unwrap().brightness,
            1.75
        );
    }

    #[test]
    fn changed_database_grade_drives_record_mode_and_console_restores_approved() {
        use shared::records::{ArchivedRecordData, ArchivedSubRecord};
        let directory = tempfile::tempdir().unwrap();
        let connection =
            rusqlite::Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        connection.execute_batch("CREATE TABLE schema_info(version INTEGER); INSERT INTO schema_info VALUES(9); CREATE TABLE records(form_id INTEGER PRIMARY KEY,record_type TEXT,load_order INTEGER,data BLOB); CREATE TABLE plugins(id INTEGER PRIMARY KEY,name TEXT,priority INTEGER); INSERT INTO plugins VALUES(1,'Winner.esm',7); CREATE TABLE formid_map(form_id INTEGER PRIMARY KEY,plugin_name TEXT);").unwrap();
        let write_record = |brightness: f32| {
            let bytes = |values: &[f32]| values.iter().flat_map(|v| v.to_le_bytes()).collect();
            let record = ArchivedRecordData {
                subrecords: vec![
                    ArchivedSubRecord {
                        tag: *b"CNAM",
                        data: bytes(&[1.23, brightness, 1.49]),
                    },
                    ArchivedSubRecord {
                        tag: *b"TNAM",
                        data: bytes(&[0.23, 0.31, 0.47, 0.59]),
                    },
                    ArchivedSubRecord {
                        tag: *b"HNAM",
                        data: bytes(&[11., 12., 13., 14., 15., 16., 17., 18., 19.]),
                    },
                ],
            };
            let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&record).unwrap();
            connection
                .execute(
                    "INSERT OR REPLACE INTO records VALUES(100,'IMGS',7,?1)",
                    [encoded.as_slice()],
                )
                .unwrap();
        };
        let config = crate::config::EngineConfig {
            assets_dir: directory.path().into(),
            color_grade: SceneGradeMode::RecordSrgb,
            image_space: Some(100),
            ..default()
        };
        write_record(1.17);
        let first = SceneGradeSource::load(&config, true).unwrap();
        write_record(1.29);
        let changed = SceneGradeSource::load(&config, true).unwrap();
        let resolved = changed
            .resolve(SceneGradeMode::RecordSrgb, SceneVisualPreset::CoolGray)
            .unwrap();
        assert_eq!(
            first
                .resolve(SceneGradeMode::RecordSrgb, SceneVisualPreset::CoolGray)
                .unwrap()
                .grade
                .brightness,
            1.17
        );
        assert_eq!(
            resolved.grade,
            SkyrimCinematicGrade {
                saturation: 1.23,
                brightness: 1.29,
                contrast: 1.49,
                tint_amount: 0.23,
                tint_rgb: [0.31, 0.47, 0.59]
            }
        );
        assert_eq!(resolved.post.pivot, 0.18);
        assert_eq!(resolved.bevy_saturation, 1.0);
        let overrides = SceneGradeOverrides {
            brightness: Some(1.62),
            tint_red: Some(0.91),
            ..default()
        };
        let tuned = changed
            .resolve_with_overrides(
                SceneGradeMode::RecordSrgb,
                SceneVisualPreset::CoolGray,
                overrides,
            )
            .unwrap();
        assert_eq!(tuned.grade.saturation, 1.23);
        assert_eq!(tuned.grade.brightness, 1.62);
        assert_eq!(tuned.grade.contrast, 1.49);
        assert_eq!(tuned.grade.tint_rgb, [0.91, 0.47, 0.59]);
        assert_eq!(changed.record.as_ref().unwrap().cinematic.brightness, 1.29);
        assert_eq!(
            changed.record.as_ref().unwrap().tint.rgb,
            [0.31, 0.47, 0.59]
        );
        let evidence = changed.evidence(SceneGradeMode::RecordSrgb, SceneVisualPreset::CoolGray);
        assert_eq!(evidence["loaded_record"]["winner_plugin"], "Winner.esm");
        assert_eq!(evidence["loaded_record"]["hdr"]["sky_scale"], 18.0);
        assert_eq!(evidence["hnam_applied"], false);
        assert_eq!(evidence["approved_override"]["applied_to_grade"], false);
        let tuned_evidence = changed.evidence_with_overrides(
            SceneGradeMode::RecordSrgb,
            SceneVisualPreset::CoolGray,
            overrides,
        );
        assert_eq!(
            tuned_evidence["grade_overrides"]["brightness"],
            serde_json::json!(1.62_f32)
        );
        assert_eq!(
            tuned_evidence["loaded_record"]["cinematic"]["brightness"],
            serde_json::json!(1.29_f32)
        );
        assert_eq!(
            tuned_evidence["effective_grade"]["brightness"],
            serde_json::json!(1.62_f32)
        );
        let mut app = App::new();
        app.insert_resource(changed)
            .insert_resource(SceneGradeMode::SnapshotSrgb)
            .insert_resource(SceneVisualPreset::CoolGray)
            .init_resource::<crate::console::ConsoleState>()
            .add_console_command(scene_grade_command())
            .add_systems(PostUpdate, apply_scene_grade_mode);
        let camera = app.world_mut().spawn(SceneColorPipeline::default()).id();
        crate::console::execute_line(app.world_mut(), "SceneGrade record-srgb");
        app.update();
        assert_eq!(
            app.world()
                .get::<SkyrimCinematicPost>(camera)
                .unwrap()
                .brightness,
            1.29
        );
        crate::console::execute_line(app.world_mut(), "SceneGrade snapshot-srgb");
        app.update();
        assert_eq!(
            app.world()
                .get::<SkyrimCinematicPost>(camera)
                .unwrap()
                .brightness,
            1.38
        );
        assert_eq!(app.world().get::<Exposure>(camera).unwrap().ev100, 9.7);
        assert_eq!(
            *app.world().get::<Tonemapping>(camera).unwrap(),
            Tonemapping::TonyMcMapface
        );
    }

    #[test]
    fn record_mode_requires_loaded_data_and_preserves_previous_console_mode_on_error() {
        let config = crate::config::EngineConfig {
            color_grade: SceneGradeMode::RecordSrgb,
            ..default()
        };
        assert!(
            SceneGradeSource::load(&config, false)
                .unwrap_err()
                .to_string()
                .contains("explicit")
        );
        let config = crate::config::EngineConfig {
            image_space: Some(100),
            ..config
        };
        assert!(
            SceneGradeSource::load(&config, false)
                .unwrap_err()
                .to_string()
                .contains("fixture")
        );
        let mut app = App::new();
        app.insert_resource(SceneGradeMode::SnapshotSrgb)
            .insert_resource(SceneVisualPreset::CoolGray)
            .init_resource::<crate::console::ConsoleState>()
            .add_console_command(scene_grade_command());
        crate::console::execute_line(app.world_mut(), "SceneGrade record-srgb");
        assert_eq!(
            *app.world().resource::<SceneGradeMode>(),
            SceneGradeMode::SnapshotSrgb
        );
        assert!(
            app.world()
                .resource::<crate::console::ConsoleState>()
                .scrollback
                .last()
                .unwrap()
                .contains("requires")
        );
    }

    #[test]
    fn snapshot_modes_grade_once_restore_presets_and_leave_sky_and_reflections_alone() {
        let mut app = App::new();
        app.init_resource::<SceneGradeMode>()
            .insert_resource(SceneVisualPreset::CoolGray)
            .insert_resource(SceneVisualPreset::CoolGray.sky_palette())
            .insert_resource(SceneGradeSource::default())
            .insert_resource(SceneToneMapSource::default())
            .init_resource::<SceneGradeOverrides>()
            .insert_resource(crate::settings::RenderingSettings::default())
            .init_resource::<crate::console::ConsoleState>()
            .add_console_command(scene_grade_command())
            .add_console_command(scene_preset_command())
            .add_systems(PostUpdate, apply_scene_grade_mode);
        let main = app.world_mut().spawn(SceneColorPipeline::default()).id();
        let reflection = app.world_mut().spawn(SceneColorPipeline::reflection()).id();
        let snapshot = SkyrimCinematicGrade::SLEEPING_GIANT_SNAPSHOT;
        assert_eq!(
            (snapshot.saturation, snapshot.brightness, snapshot.contrast),
            (1.66, 1.15, 1.50)
        );
        assert_eq!(snapshot.tint_amount, 0.56);
        assert_eq!(snapshot.tint_rgb, [0.88, 0.81, 0.72]);
        for (command, domain, brightness) in [
            ("SceneGrade SNAPSHOT-SRGB", 0, snapshot.brightness * 1.2),
            ("SceneGrade snapshot-linear", 1, snapshot.brightness),
        ] {
            crate::console::execute_line(app.world_mut(), command);
            app.update();
            let world = app.world();
            let post = world.get::<SkyrimCinematicPost>(main).unwrap();
            assert_eq!(
                *post,
                SkyrimCinematicPost {
                    saturation: 1.66,
                    brightness,
                    linear_domain: domain,
                    ..snapshot.post()
                }
            );
            if domain == 0 {
                assert!((post.brightness - 1.38).abs() < 1e-6);
            }
            assert_eq!(
                world
                    .get::<ColorGrading>(main)
                    .unwrap()
                    .global
                    .post_saturation,
                1.0
            );
            assert_eq!(
                *world.resource::<SkyPalette>(),
                SceneVisualPreset::CoolGray.sky_palette()
            );
            assert_eq!(
                world
                    .resource::<crate::settings::RenderingSettings>()
                    .color_grade,
                if domain == 0 {
                    SceneGradeMode::SnapshotSrgb.index()
                } else {
                    SceneGradeMode::SnapshotLinear.index()
                }
            );
            assert_eq!(
                *world.get::<SkyrimCinematicPost>(reflection).unwrap(),
                SkyrimCinematicPost::default()
            );
            assert_eq!(
                world
                    .get::<ColorGrading>(reflection)
                    .unwrap()
                    .global
                    .post_saturation,
                1.0
            );
        }
        let later = app.world_mut().spawn(SceneColorPipeline::default()).id();
        app.update();
        assert_eq!(
            app.world()
                .get::<SkyrimCinematicPost>(later)
                .unwrap()
                .linear_domain,
            1
        );
        assert_eq!(
            app.world()
                .get::<SkyrimCinematicPost>(later)
                .unwrap()
                .saturation,
            1.66
        );
        crate::console::execute_line(app.world_mut(), "SceneGrade off");
        app.update();
        assert_eq!(
            app.world()
                .get::<SkyrimCinematicPost>(main)
                .unwrap()
                .enabled,
            0.0
        );
        for (preset, saturation) in [("cool-gray", 1.25), ("vivid", 1.5)] {
            crate::console::execute_line(app.world_mut(), &format!("ScenePreset {preset}"));
            crate::console::execute_line(app.world_mut(), "SceneGrade full");
            app.update();
            let world = app.world();
            assert_eq!(
                world
                    .get::<ColorGrading>(main)
                    .unwrap()
                    .global
                    .post_saturation,
                saturation
            );
            assert_eq!(
                world.get::<SkyrimCinematicPost>(main).unwrap().saturation,
                1.0
            );
            assert_eq!(
                world
                    .get::<SkyrimCinematicPost>(main)
                    .unwrap()
                    .linear_domain,
                0
            );
            assert_eq!(
                *world.get::<SkyrimCinematicPost>(main).unwrap(),
                world.resource::<SceneVisualPreset>().grade().post()
            );
        }
    }

    fn assert_fog_matches(actual: &bevy::pbr::DistanceFog, expected: &bevy::pbr::DistanceFog) {
        assert_eq!(actual.color, expected.color);
        assert_eq!(
            actual.directional_light_color,
            expected.directional_light_color
        );
        assert_eq!(
            actual.directional_light_exponent,
            expected.directional_light_exponent
        );
        let bevy::pbr::FogFalloff::Atmospheric {
            extinction,
            inscattering,
        } = actual.falloff
        else {
            panic!("expected atmospheric fog");
        };
        let bevy::pbr::FogFalloff::Atmospheric {
            extinction: wanted_extinction,
            inscattering: wanted_inscattering,
        } = expected.falloff
        else {
            panic!("expected atmospheric fog");
        };
        assert_eq!(extinction, wanted_extinction);
        assert_eq!(inscattering, wanted_inscattering);
    }

    #[test]
    fn presets_restore_vivid_and_keep_camera_and_lighting_state() {
        let mut app = App::new();
        app.init_resource::<SceneGradeMode>()
            .init_resource::<SceneVisualPreset>()
            .init_resource::<SkyPalette>()
            .init_resource::<crate::console::ConsoleState>()
            .add_console_command(scene_grade_command())
            .add_console_command(scene_preset_command())
            .add_systems(PostUpdate, apply_scene_grade_mode);
        let vivid = SceneColorPipeline::default();
        let expected_post = vivid.cinematic_post;
        let transform = Transform::from_xyz(7.0, 11.0, 13.0);
        let fog = crate::sky::fog_for(&SkyPalette::default());
        let main = app.world_mut().spawn((vivid, transform, fog.clone())).id();
        let reflection = app.world_mut().spawn(SceneColorPipeline::reflection()).id();
        app.update();

        for command in [
            "ScenePreset COOL-GRAY",
            "ScenePreset vivid",
            "ScenePreset toggle",
        ] {
            crate::console::execute_line(app.world_mut(), command);
            app.update();
            let world = app.world();
            let preset = *world.resource::<SceneVisualPreset>();
            assert_eq!(*world.resource::<SkyPalette>(), preset.sky_palette());
            if preset == SceneVisualPreset::Vivid {
                assert_fog_matches(&crate::sky::fog_for(world.resource::<SkyPalette>()), &fog);
            }
            assert_grading_matches(world.get::<ColorGrading>(main).unwrap(), &preset.grade());
            assert_eq!(
                *world.get::<SkyrimCinematicPost>(main).unwrap(),
                expected_post
            );
            assert_eq!(*world.get::<Transform>(main).unwrap(), transform);
            assert_fog_matches(world.get::<bevy::pbr::DistanceFog>(main).unwrap(), &fog);
            assert_eq!(
                world.get::<Exposure>(main).unwrap().ev100,
                DEFAULT_SCENE_EV100
            );
            assert_eq!(
                *world.get::<Tonemapping>(main).unwrap(),
                Tonemapping::TonyMcMapface
            );
            assert_grading_matches(
                world.get::<ColorGrading>(reflection).unwrap(),
                &SkyrimCinematicGrade {
                    saturation: 1.0,
                    ..SkyrimCinematicGrade::SKYRIM_CLEAR_DAY
                },
            );
            assert_eq!(
                *world.get::<SkyrimCinematicPost>(reflection).unwrap(),
                SkyrimCinematicPost::default()
            );
        }
        let later = app.world_mut().spawn(SceneColorPipeline::default()).id();
        app.update();
        assert_eq!(
            app.world()
                .get::<ColorGrading>(later)
                .unwrap()
                .global
                .post_saturation,
            1.25
        );
        for invalid in ["ScenePreset unknown", "ScenePreset vivid extra"] {
            crate::console::execute_line(app.world_mut(), invalid);
            assert_eq!(
                *app.world().resource::<SceneVisualPreset>(),
                SceneVisualPreset::CoolGray
            );
            assert_eq!(
                *app.world().resource::<SkyPalette>(),
                SceneVisualPreset::CoolGray.sky_palette()
            );
        }
        crate::console::execute_line(app.world_mut(), "SceneGrade off");
        crate::console::execute_line(app.world_mut(), "ScenePreset toggle");
        app.update();
        assert_eq!(
            *app.world().resource::<SceneVisualPreset>(),
            SceneVisualPreset::Vivid
        );
        assert_eq!(
            *app.world().resource::<SceneGradeMode>(),
            SceneGradeMode::Off
        );
        assert_eq!(
            app.world()
                .get::<SkyrimCinematicPost>(main)
                .unwrap()
                .enabled,
            0.0
        );
        crate::console::execute_line(app.world_mut(), "SceneGrade full");
        app.update();
        assert_grading_matches(
            app.world().get::<ColorGrading>(main).unwrap(),
            &SkyrimCinematicGrade::SKYRIM_CLEAR_DAY,
        );
        assert_eq!(
            *app.world().get::<SkyrimCinematicPost>(main).unwrap(),
            expected_post
        );
        assert_eq!(
            *app.world().resource::<SkyPalette>(),
            SkyPalette::SKYRIM_CLEAR_DAY
        );
    }

    #[test]
    fn console_modes_preserve_scene_state_and_leave_reflection_ungraded() {
        let mut app = App::new();
        app.init_resource::<SceneGradeMode>()
            .init_resource::<SceneVisualPreset>()
            .init_resource::<crate::console::ConsoleState>()
            .add_console_command(scene_grade_command())
            .add_systems(PostUpdate, apply_scene_grade_mode);
        let transform = Transform::from_xyz(7.0, 11.0, 13.0);
        let fog = bevy::pbr::DistanceFog {
            color: Color::srgb(0.2, 0.3, 0.4),
            ..default()
        };
        let main = app
            .world_mut()
            .spawn((SceneColorPipeline::default(), transform, fog))
            .id();
        let reflection = app.world_mut().spawn(SceneColorPipeline::reflection()).id();
        for (command, saturation, enabled) in [
            ("SceneGrade off", 1.0, 0.0),
            ("scenegrade SATURATION", 1.5, 0.0),
            ("SceneGrade full", 1.5, 1.0),
        ] {
            crate::console::execute_line(app.world_mut(), command);
            app.update();
            let world = app.world();
            assert_eq!(
                world
                    .get::<ColorGrading>(main)
                    .unwrap()
                    .global
                    .post_saturation,
                saturation
            );
            let post = world.get::<SkyrimCinematicPost>(main).unwrap();
            assert_eq!(post.enabled, enabled);
            assert_eq!(post.brightness, 1.135);
            assert_eq!(
                world.get::<Exposure>(main).unwrap().ev100,
                DEFAULT_SCENE_EV100
            );
            assert_eq!(
                *world.get::<Tonemapping>(main).unwrap(),
                Tonemapping::TonyMcMapface
            );
            assert_eq!(*world.get::<Transform>(main).unwrap(), transform);
            assert_eq!(
                world.get::<bevy::pbr::DistanceFog>(main).unwrap().color,
                Color::srgb(0.2, 0.3, 0.4)
            );
            assert!(world.get::<Hdr>(main).is_some());
            assert_eq!(
                world
                    .get::<ColorGrading>(reflection)
                    .unwrap()
                    .global
                    .post_saturation,
                1.0
            );
            assert_eq!(
                world
                    .get::<SkyrimCinematicPost>(reflection)
                    .unwrap()
                    .enabled,
                0.0
            );
        }
        crate::console::execute_line(app.world_mut(), "SceneGrade invalid");
        assert_eq!(
            *app.world().resource::<SceneGradeMode>(),
            SceneGradeMode::Full
        );
        assert!(
            app.world()
                .resource::<crate::console::ConsoleState>()
                .scrollback
                .last()
                .unwrap()
                .contains("usage:")
        );
        crate::console::execute_line(app.world_mut(), "SceneGrade off");
        app.update();
        let later = app.world_mut().spawn(SceneColorPipeline::default()).id();
        app.update();
        assert_eq!(
            app.world()
                .get::<SkyrimCinematicPost>(later)
                .unwrap()
                .enabled,
            0.0
        );
        assert_eq!(
            app.world()
                .get::<ColorGrading>(later)
                .unwrap()
                .global
                .post_saturation,
            1.0
        );
    }

    #[test]
    fn cinematic_shader_validates_and_matches_uniform_layout() {
        // Supply only Bevy's imported vertex interface; validate the actual
        // embedded fragment source rather than a second copy of its formula.
        let source = include_str!("shaders/cinematic_post.wgsl").replace(
            "#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput",
            "struct FullscreenVertexOutput {\n\
                @builtin(position) position: vec4<f32>,\n\
                @location(0) uv: vec2<f32>,\n\
            }",
        );
        let module = wgpu::naga::front::wgsl::parse_str(&source).unwrap();
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        let uniform = module
            .types
            .iter()
            .find_map(|(_, ty)| {
                (ty.name.as_deref() == Some("SkyrimCinematicPost")).then_some(&ty.inner)
            })
            .unwrap();
        let wgpu::naga::TypeInner::Struct { members, span } = uniform else {
            panic!("cinematic uniform must be a struct");
        };
        assert_eq!(u64::from(*span), SkyrimCinematicPost::min_size().get());
        assert_eq!(
            members
                .iter()
                .map(|member| member.offset)
                .collect::<Vec<_>>(),
            [0, 4, 8, 12, 16, 28, 32, 36, 40, 44, 48, 52]
        );
    }

    #[test]
    fn scene_views_compose_in_hdr_with_explicit_output_settings() {
        let mut world = World::new();
        let entity = world.spawn(SceneColorPipeline::default()).id();
        assert!(world.get::<Hdr>(entity).is_some());
        assert_eq!(
            *world.get::<CompositingSpace>(entity).unwrap(),
            CompositingSpace::Linear
        );
        assert_eq!(world.get::<Exposure>(entity).unwrap().ev100, 9.7);
        assert_eq!(
            *world.get::<Tonemapping>(entity).unwrap(),
            Tonemapping::TonyMcMapface
        );
        assert_grading_matches(
            world.get::<ColorGrading>(entity).unwrap(),
            &SkyrimCinematicGrade::SKYRIM_CLEAR_DAY,
        );
        assert_eq!(
            world.get::<SkyrimCinematicPost>(entity).unwrap(),
            &SkyrimCinematicGrade::SKYRIM_CLEAR_DAY.post()
        );
    }

    #[test]
    fn skyrim_clear_day_grade_preserves_reported_record_constants() {
        // ISSkyrimClearDAY (IMGS 0x00012F88, Update.esm winner), read from the
        // local game data: CNAM Saturation 1.5, Brightness 1.135, Contrast 1.4;
        // TNAM tint amount 0.42 toward RGB 0.894, 0.839, 0.773.
        let grade = SkyrimCinematicGrade::SKYRIM_CLEAR_DAY;
        assert_eq!(
            (grade.saturation, grade.brightness, grade.contrast),
            (1.5, 1.135, 1.4)
        );
        assert_eq!(grade.tint_amount, 0.42);
        assert_eq!(grade.tint_rgb, [0.894_118, 0.839_216, 0.772_549]);
        let grading = grade.color_grading();
        for section in grading.all_sections() {
            assert_eq!(*section, ColorGradingSection::default());
        }
        // Bevy saturation lands after its tone curve in linear RGB.
        assert_eq!(grading.global.post_saturation, 1.5);
        assert_eq!(grading.global.exposure, 0.0);
        // Brightness, contrast and tint land in the post-tonemap pass, in
        // the experimental sRGB domain, around the artistic pivot.
        assert_eq!(
            grade.post(),
            SkyrimCinematicPost {
                brightness: 1.135,
                contrast: 1.4,
                tint_amount: 0.42,
                pivot: CINEMATIC_PIVOT,
                tint_rgb: Vec3::new(0.894_118, 0.839_216, 0.772_549),
                enabled: 1.0,
                saturation: 1.0,
                linear_domain: 0,
                ..SkyrimCinematicPost::default()
            }
        );
    }

    #[test]
    fn a_cinematic_grade_maps_saturation_after_the_tone_curve_only() {
        let grade = SkyrimCinematicGrade {
            saturation: 0.9,
            brightness: 1.1,
            contrast: 1.3,
            tint_amount: 0.25,
            tint_rgb: [1.0, 0.5, 0.0],
        };
        let grading = grade.color_grading();
        assert_eq!(grading.global.post_saturation, 0.9);
        for section in grading.all_sections() {
            assert_eq!(*section, ColorGradingSection::default());
        }
        let post = grade.post();
        assert_eq!(post.brightness, 1.1);
        assert_eq!(post.contrast, 1.3);
        assert_eq!(post.tint_amount, 0.25);
        assert_eq!(post.tint_rgb, Vec3::new(1.0, 0.5, 0.0));
        assert_eq!(post.pivot, CINEMATIC_PIVOT);
    }

    #[test]
    fn reflection_keeps_scene_exposure_without_a_display_transform() {
        let scene = SceneColorPipeline::default();
        let reflection = SceneColorPipeline::reflection();
        assert_eq!(reflection.exposure.ev100, scene.exposure.ev100);
        assert_eq!(reflection.tonemapping, Tonemapping::None);
        // The reflection is graded once by the main view, so its own grade is
        // Bevy's identity grade rather than the Skyrim scene grade.
        for section in reflection.grading.all_sections() {
            assert_eq!(*section, ColorGradingSection::default());
        }
        assert_eq!(reflection.grading.global.post_saturation, 1.0);
        assert_eq!(reflection.grading.global.exposure, 0.0);
        // No post pass on the reflection: its HDR values must reach the main
        // view unclamped, to be graded once there.
        assert_eq!(reflection.cinematic_post.enabled, 0.0);
    }
}
