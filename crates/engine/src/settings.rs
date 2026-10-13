//! Interactive rendering preferences. Automated runs keep their explicit configuration.

use crate::{config::EngineConfig, world::components::StreamingCamera};
use bevy::{
    camera::{Exposure, visibility::NoCpuCulling},
    core_pipeline::{prepass::DepthPrepass, tonemapping::Tonemapping},
    light::{
        CascadeShadowConfig, CascadeShadowConfigBuilder, DirectionalLightShadowMap,
        PointLightShadowMap,
    },
    prelude::*,
    render::{
        occlusion_culling::OcclusionCulling, render_asset::RenderAssetBytesPerFrame,
        renderer::RenderAdapter, view::NoIndirectDrawing,
    },
    window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const TONE_MAPS: [Tonemapping; 9] = [
    Tonemapping::None,
    Tonemapping::Reinhard,
    Tonemapping::ReinhardLuminance,
    Tonemapping::AcesFitted,
    Tonemapping::AgX,
    Tonemapping::SomewhatBoringDisplayTransform,
    Tonemapping::TonyMcMapface,
    Tonemapping::BlenderFilmic,
    Tonemapping::KhronosPbrNeutral,
];
pub const MSAA_MODES: [Msaa; 4] = [Msaa::Off, Msaa::Sample2, Msaa::Sample4, Msaa::Sample8];
pub const SHADOW_SIZES: [usize; 4] = [512, 1024, 2048, 4096];
pub const WINDOW_SIZES: [(u32, u32); 4] = [(1280, 720), (1600, 900), (1920, 1080), (2560, 1440)];

#[derive(Resource, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderingSettings {
    pub version: u32,
    pub point_lights: bool,
    pub point_light_budget: usize,
    pub point_light_shadows: bool,
    pub point_shadow_size: usize,
    pub sun: bool,
    pub sun_illuminance: f32,
    pub sun_color: [f32; 3],
    pub ambient_brightness: f32,
    pub ambient_color: [f32; 3],
    pub shadows: bool,
    pub shadow_size: usize,
    pub shadow_cascades: usize,
    pub shadow_distance_scale: f32,
    pub sky: bool,
    pub sky_brightness_scale: f32,
    pub fog: bool,
    pub fog_density_scale: f32,
    pub water_reflections: bool,
    pub water_reflection_strength: f32,
    pub water_waves: bool,
    pub water_flow_normals: bool,
    pub wave_scale: f32,
    pub wave_speed: f32,
    pub wave_strength: f32,
    pub cpu_frustum_culling: bool,
    pub indirect_drawing: bool,
    pub occlusion_culling: bool,
    pub depth_prepass: bool,
    pub msaa: usize,
    pub exposure_ev100: f32,
    pub tone_map: usize,
    pub fov_degrees: f32,
    pub vsync: bool,
    pub window_mode: usize,
    pub window_size: usize,
    pub hud: bool,
    pub stream_radius: i32,
    pub unload_radius: i32,
    pub lod_level0_distance: f32,
    pub lod_level1_distance: f32,
    pub lod_maximum_distance: f32,
    pub lod_multiplier: f32,
    pub cell_commits: usize,
    pub commit_ms: f64,
    pub cell_unloads: usize,
    pub model_spawns: usize,
    pub upload_mib: usize,
    pub io_threads: usize,
    pub diagnostic_asset_fallbacks: bool,
}

impl Default for RenderingSettings {
    fn default() -> Self {
        let config = EngineConfig::default();
        Self {
            version: 1,
            point_lights: false,
            point_light_budget: crate::lights::ENABLED_LIGHT_BUDGET,
            point_light_shadows: false,
            point_shadow_size: 1,
            sun: true,
            sun_illuminance: 12_000.0,
            sun_color: [1.0; 3],
            ambient_brightness: crate::lights::AMBIENT_ILLUMINANCE,
            ambient_color: [0.48, 0.55, 0.7],
            shadows: true,
            shadow_size: 2,
            shadow_cascades: 4,
            shadow_distance_scale: 1.0,
            sky: true,
            sky_brightness_scale: 1.0,
            fog: true,
            fog_density_scale: 1.0,
            water_reflections: true,
            water_reflection_strength: 1.0,
            water_waves: true,
            water_flow_normals: true,
            wave_scale: 0.006,
            wave_speed: 0.15,
            wave_strength: crate::render::WAVE_STRENGTH,
            cpu_frustum_culling: true,
            indirect_drawing: true,
            occlusion_culling: true,
            depth_prepass: true,
            msaa: 0,
            exposure_ev100: crate::color_pipeline::DEFAULT_SCENE_EV100,
            tone_map: 6,
            fov_degrees: PerspectiveProjection::default().fov.to_degrees(),
            vsync: true,
            window_mode: 0,
            window_size: 1,
            hud: true,
            stream_radius: config.stream_radius,
            unload_radius: config.unload_radius,
            lod_level0_distance: config.terrain_lod.block_level0_distance,
            lod_level1_distance: config.terrain_lod.block_level1_distance,
            lod_maximum_distance: config.terrain_lod.block_maximum_distance,
            lod_multiplier: config.terrain_lod.split_distance_mult,
            cell_commits: config.max_cell_commits_per_frame,
            commit_ms: config.max_commit_micros_per_frame as f64 / 1000.0,
            cell_unloads: config.max_cell_unloads_per_frame,
            model_spawns: config.max_model_spawns_per_frame,
            upload_mib: config.max_upload_mib_per_frame,
            io_threads: config.io_threads,
            diagnostic_asset_fallbacks: config.diagnostic_asset_fallbacks,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsPage {
    Graphics,
    Streaming,
    Controls,
}

#[derive(Clone, Copy)]
pub enum SettingKind {
    Toggle,
    Number { min: f64, max: f64, step: f64 },
    Choice(&'static [&'static str]),
}

pub struct SettingSpec {
    pub label: &'static str,
    pub page: SettingsPage,
    /// Heading the menu lists this setting under; entries of a group stay contiguous.
    pub group: &'static str,
    pub kind: SettingKind,
    /// When present and false, the setting has no visible effect and the menu dims it.
    pub requires: Option<fn(&RenderingSettings) -> bool>,
    pub read: fn(&RenderingSettings) -> f64,
    pub write: fn(&mut RenderingSettings, f64),
}

macro_rules! toggle {
    ($group:literal, $label:literal, $field:ident) => {
        SettingSpec {
            label: $label,
            page: SettingsPage::Graphics,
            group: $group,
            kind: SettingKind::Toggle,
            requires: None,
            read: |s| u8::from(s.$field) as f64,
            write: |s, value| s.$field = value != 0.0,
        }
    };
}
macro_rules! number {
    ($page:ident, $group:literal, $label:literal, $field:ident $( [$index:literal] )?; $min:expr, $max:expr, $step:expr) => { SettingSpec {
        label: $label, page: SettingsPage::$page, group: $group,
        kind: SettingKind::Number { min: $min, max: $max, step: $step },
        requires: None,
        read: |s| s.$field $( [$index] )? as f64, write: |s, value| s.$field $( [$index] )? = value as _,
    } };
}
macro_rules! choice {
    ($group:literal, $label:literal, $field:ident, $choices:expr) => {
        SettingSpec {
            label: $label,
            page: SettingsPage::Graphics,
            group: $group,
            kind: SettingKind::Choice($choices),
            requires: None,
            read: |s| s.$field as f64,
            write: |s, value| s.$field = value as usize,
        }
    };
}

/// One inventory supplies UI controls and saved-file validation.
pub static SETTING_SPECS: &[SettingSpec] = &[
    toggle!("Point lights", "Point lights", point_lights),
    number!(Graphics, "Point lights", "Enabled point-light budget", point_light_budget; 0.0, 256.0, 8.0)
        .when(|s| s.point_lights),
    toggle!("Point lights", "Point-light shadows", point_light_shadows).when(|s| s.point_lights),
    choice!(
        "Point lights",
        "Point-light shadow-map resolution",
        point_shadow_size,
        &["512", "1024", "2048", "4096"]
    )
    .when(|s| s.point_lights && s.point_light_shadows),
    toggle!("Sun and ambient light", "Sun lighting", sun),
    number!(Graphics, "Sun and ambient light", "Sun illuminance (lux)", sun_illuminance; 0.0, 100_000.0, 1000.0)
        .when(|s| s.sun),
    number!(Graphics, "Sun and ambient light", "Sun red", sun_color[0]; 0.0, 1.0, 0.05).when(|s| s.sun),
    number!(Graphics, "Sun and ambient light", "Sun green", sun_color[1]; 0.0, 1.0, 0.05).when(|s| s.sun),
    number!(Graphics, "Sun and ambient light", "Sun blue", sun_color[2]; 0.0, 1.0, 0.05).when(|s| s.sun),
    number!(Graphics, "Sun and ambient light", "Ambient brightness", ambient_brightness; 0.0, 2000.0, 20.0),
    number!(Graphics, "Sun and ambient light", "Ambient red", ambient_color[0]; 0.0, 1.0, 0.05),
    number!(Graphics, "Sun and ambient light", "Ambient green", ambient_color[1]; 0.0, 1.0, 0.05),
    number!(Graphics, "Sun and ambient light", "Ambient blue", ambient_color[2]; 0.0, 1.0, 0.05),
    toggle!("Sun shadows", "Sun shadows", shadows).when(|s| s.sun),
    choice!(
        "Sun shadows",
        "Sun shadow-map resolution",
        shadow_size,
        &["512", "1024", "2048", "4096"]
    )
    .when(|s| s.sun && s.shadows),
    number!(Graphics, "Sun shadows", "Sun shadow cascades", shadow_cascades; 1.0, 4.0, 1.0)
        .when(|s| s.sun && s.shadows),
    number!(Graphics, "Sun shadows", "Sun shadow-distance multiplier", shadow_distance_scale; 0.25, 4.0, 0.25)
        .when(|s| s.sun && s.shadows),
    toggle!("Sky and fog", "Sky dome", sky),
    number!(Graphics, "Sky and fog", "Sky-brightness multiplier", sky_brightness_scale; 0.0, 4.0, 0.1)
        .when(|s| s.sky),
    toggle!("Sky and fog", "Exterior distance fog", fog),
    number!(Graphics, "Sky and fog", "Fog-density multiplier", fog_density_scale; 0.0, 4.0, 0.1)
        .when(|s| s.fog),
    toggle!("Water", "Water reflections", water_reflections),
    number!(Graphics, "Water", "Water reflection multiplier", water_reflection_strength; 0.0, 1.0, 0.1)
        .when(|s| s.water_reflections),
    toggle!("Water", "Water waves", water_waves),
    toggle!("Water", "Water flow normals", water_flow_normals),
    number!(Graphics, "Water", "Water wave scale", wave_scale; 0.001, 0.05, 0.001).when(|s| s.water_waves),
    number!(Graphics, "Water", "Water wave speed", wave_speed; 0.0, 2.0, 0.05).when(|s| s.water_waves),
    number!(Graphics, "Water", "Water wave strength", wave_strength; 0.0, 2.0, 0.05).when(|s| s.water_waves),
    toggle!("Culling and drawing", "CPU frustum culling", cpu_frustum_culling),
    toggle!("Culling and drawing", "GPU indirect drawing", indirect_drawing),
    toggle!("Culling and drawing", "GPU occlusion culling", occlusion_culling),
    toggle!("Culling and drawing", "Depth prepass", depth_prepass),
    choice!("Image", "MSAA", msaa, &["Off", "2x", "4x", "8x"]),
    number!(Graphics, "Image", "Exposure (EV100)", exposure_ev100; -5.0, 20.0, 0.25),
    choice!(
        "Image",
        "Tone mapping",
        tone_map,
        &[
            "None",
            "Reinhard",
            "Reinhard luminance",
            "ACES fitted",
            "AgX",
            "Somewhat boring",
            "Tony McMapface",
            "Blender filmic",
            "Khronos PBR neutral",
        ]
    ),
    number!(Graphics, "Image", "Vertical field of view (degrees)", fov_degrees; 30.0, 110.0, 5.0),
    toggle!("Window", "VSync", vsync),
    choice!(
        "Window",
        "Window mode",
        window_mode,
        &["Windowed", "Borderless fullscreen"]
    ),
    choice!(
        "Window",
        "Windowed resolution",
        window_size,
        &["1280 x 720", "1600 x 900", "1920 x 1080", "2560 x 1440"]
    ),
    toggle!("Interface and diagnostics", "Status HUD", hud),
    toggle!(
        "Interface and diagnostics",
        "Diagnostic fallbacks (new loads)",
        diagnostic_asset_fallbacks
    ),
    number!(Streaming, "Range", "Full-detail radius (cells)", stream_radius; 0.0, 16.0, 1.0),
    number!(Streaming, "Range", "Unload radius (cells)", unload_radius; 0.0, 32.0, 1.0),
    number!(Streaming, "Terrain level of detail", "Level-4 terrain LOD distance (units)", lod_level0_distance; 0.0, 1_000_000.0, 5000.0),
    number!(Streaming, "Terrain level of detail", "Level-8 terrain LOD distance (units)", lod_level1_distance; 0.0, 1_000_000.0, 5000.0),
    number!(Streaming, "Terrain level of detail", "Level-16 terrain LOD distance (units)", lod_maximum_distance; 0.0, 1_000_000.0, 10_000.0),
    number!(Streaming, "Terrain level of detail", "Terrain LOD distance multiplier", lod_multiplier; 0.0, 4.0, 0.25),
    number!(Streaming, "Per-frame budgets", "Cell commits per frame", cell_commits; 1.0, 64.0, 1.0),
    number!(Streaming, "Per-frame budgets", "Cell commit budget (ms)", commit_ms; 0.5, 100.0, 0.5),
    number!(Streaming, "Per-frame budgets", "Cell unloads per frame (0 = unlimited)", cell_unloads; 0.0, 128.0, 1.0),
    number!(Streaming, "Per-frame budgets", "Model spawns per frame (0 = unlimited)", model_spawns; 0.0, 512.0, 8.0),
    number!(Streaming, "Per-frame budgets", "Asset upload MiB per frame (0 = unlimited)", upload_mib; 0.0, 256.0, 4.0),
    number!(Streaming, "Threads", "IO threads (restart; 0 = automatic)", io_threads; 0.0, 64.0, 1.0),
];

impl SettingSpec {
    const fn when(self, requires: fn(&RenderingSettings) -> bool) -> Self {
        Self {
            requires: Some(requires),
            ..self
        }
    }

    /// False when a controlling setting makes this one have no visible effect.
    pub fn relevant(&self, settings: &RenderingSettings) -> bool {
        self.requires.is_none_or(|requires| requires(settings))
    }

    /// True when the session value differs from what Reset defaults restores.
    pub fn is_modified(&self, settings: &RenderingSettings) -> bool {
        (self.read)(settings) != (self.read)(&RenderingSettings::default())
    }

    pub fn value_label(&self, settings: &RenderingSettings) -> String {
        let value = (self.read)(settings);
        match self.kind {
            SettingKind::Toggle => if value == 0.0 { "Off" } else { "On" }.to_owned(),
            SettingKind::Choice(labels) => {
                labels.get(value as usize).unwrap_or(&"Unknown").to_string()
            }
            SettingKind::Number { step, .. } => {
                let precision = if step < 0.01 {
                    3
                } else if step < 1.0 {
                    2
                } else {
                    0
                };
                format!("{value:.precision$}")
            }
        }
    }

    pub fn adjust(&self, settings: &mut RenderingSettings, direction: i32) {
        let old_occlusion = settings.occlusion_culling;
        let old_radius = settings.stream_radius;
        let old = (self.read)(settings);
        let value = match self.kind {
            SettingKind::Toggle => u8::from(old == 0.0) as f64,
            SettingKind::Choice(labels) => {
                (old as i32 + direction).rem_euclid(labels.len() as i32) as f64
            }
            SettingKind::Number { min, max, step } => {
                (old + f64::from(direction) * step).clamp(min, max)
            }
        };
        (self.write)(settings, value);
        if !old_occlusion && settings.occlusion_culling {
            settings.indirect_drawing = true;
            settings.depth_prepass = true;
        }
        if !settings.indirect_drawing || !settings.depth_prepass {
            settings.occlusion_culling = false;
        }
        if old_radius != settings.stream_radius {
            settings.unload_radius = settings.unload_radius.max(settings.stream_radius + 1);
        } else {
            settings.unload_radius = settings.unload_radius.max(settings.stream_radius);
        }
    }
}

impl RenderingSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported settings version".into());
        }
        for spec in SETTING_SPECS {
            let value = (spec.read)(self);
            let (min, max) = match spec.kind {
                SettingKind::Toggle => (0.0, 1.0),
                SettingKind::Choice(labels) => (0.0, (labels.len() - 1) as f64),
                SettingKind::Number { min, max, .. } => (min, max),
            };
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(format!("{} must be between {min} and {max}", spec.label));
            }
        }
        if self.unload_radius < self.stream_radius {
            return Err("unload radius must cover the full-detail radius".into());
        }
        if self.occlusion_culling && (!self.depth_prepass || !self.indirect_drawing) {
            return Err("occlusion culling requires depth prepass and indirect drawing".into());
        }
        Ok(())
    }

    fn take_launch_values(&mut self, config: &EngineConfig) {
        self.point_lights = config.lights;
        self.stream_radius = config.stream_radius;
        self.unload_radius = config.unload_radius;
        self.lod_level0_distance = config.terrain_lod.block_level0_distance;
        self.lod_level1_distance = config.terrain_lod.block_level1_distance;
        self.lod_maximum_distance = config.terrain_lod.block_maximum_distance;
        self.lod_multiplier = config.terrain_lod.split_distance_mult;
        self.cell_commits = config.max_cell_commits_per_frame;
        self.commit_ms = config.max_commit_micros_per_frame as f64 / 1000.0;
        self.cell_unloads = config.max_cell_unloads_per_frame;
        self.model_spawns = config.max_model_spawns_per_frame;
        self.upload_mib = config.max_upload_mib_per_frame;
        self.io_threads = config.io_threads;
        self.diagnostic_asset_fallbacks = config.diagnostic_asset_fallbacks;
    }

    fn retain_explicit_launch_values(&mut self, config: &EngineConfig) {
        for flag in &config.settings_overrides {
            match flag.as_str() {
                "--lights" => self.point_lights = config.lights,
                "--stream-radius" => {
                    self.stream_radius = config.stream_radius;
                    self.unload_radius = config.unload_radius;
                }
                "ini:stream-radius" => {
                    if !config
                        .settings_overrides
                        .iter()
                        .any(|s| s == "--stream-radius")
                    {
                        self.stream_radius = config.stream_radius;
                        self.unload_radius = config.unload_radius;
                    }
                }
                "ini:fBlockLevel0Distance" => {
                    self.lod_level0_distance = config.terrain_lod.block_level0_distance
                }
                "ini:fBlockLevel1Distance" => {
                    self.lod_level1_distance = config.terrain_lod.block_level1_distance
                }
                "ini:fBlockMaximumDistance" => {
                    self.lod_maximum_distance = config.terrain_lod.block_maximum_distance
                }
                "ini:fSplitDistanceMult" => {
                    self.lod_multiplier = config.terrain_lod.split_distance_mult
                }
                "--max-commit-ms" => {
                    self.commit_ms = config.max_commit_micros_per_frame as f64 / 1000.0
                }
                "--max-unloads-per-frame" => self.cell_unloads = config.max_cell_unloads_per_frame,
                "--max-model-spawns-per-frame" => {
                    self.model_spawns = config.max_model_spawns_per_frame
                }
                "--max-upload-mib-per-frame" => self.upload_mib = config.max_upload_mib_per_frame,
                "--io-threads" => self.io_threads = config.io_threads,
                "--diagnostic-asset-fallbacks" => {
                    self.diagnostic_asset_fallbacks = config.diagnostic_asset_fallbacks
                }
                _ => {}
            }
        }
    }

    pub(crate) fn apply_config(&self, config: &mut EngineConfig) {
        config.lights = self.point_lights;
        config.stream_radius = self.stream_radius;
        config.unload_radius = self.unload_radius;
        config.terrain_lod.block_level0_distance = self.lod_level0_distance;
        config.terrain_lod.block_level1_distance = self.lod_level1_distance;
        config.terrain_lod.block_maximum_distance = self.lod_maximum_distance;
        config.terrain_lod.split_distance_mult = self.lod_multiplier;
        config.max_cell_commits_per_frame = self.cell_commits;
        config.max_commit_micros_per_frame = (self.commit_ms * 1000.0) as u64;
        config.max_cell_unloads_per_frame = self.cell_unloads;
        config.max_model_spawns_per_frame = self.model_spawns;
        config.max_upload_mib_per_frame = self.upload_mib;
        config.diagnostic_asset_fallbacks = self.diagnostic_asset_fallbacks;
    }

    fn config_differs(&self, config: &EngineConfig) -> bool {
        self.point_lights != config.lights
            || self.stream_radius != config.stream_radius
            || self.unload_radius != config.unload_radius
            || self.lod_level0_distance != config.terrain_lod.block_level0_distance
            || self.lod_level1_distance != config.terrain_lod.block_level1_distance
            || self.lod_maximum_distance != config.terrain_lod.block_maximum_distance
            || self.lod_multiplier != config.terrain_lod.split_distance_mult
            || self.cell_commits != config.max_cell_commits_per_frame
            || (self.commit_ms * 1000.0) as u64 != config.max_commit_micros_per_frame
            || self.cell_unloads != config.max_cell_unloads_per_frame
            || self.model_spawns != config.max_model_spawns_per_frame
            || self.upload_mib != config.max_upload_mib_per_frame
            || self.diagnostic_asset_fallbacks != config.diagnostic_asset_fallbacks
    }
}

#[derive(Resource)]
pub struct SettingsFile {
    pub path: Option<PathBuf>,
    pub status: String,
}

impl SettingsFile {
    pub fn session_only() -> Self {
        Self {
            path: None,
            status: "Changes apply now. This preview does not save preferences.".into(),
        }
    }

    pub fn save(&mut self, settings: &RenderingSettings) {
        self.status = match self.path.as_ref() {
            Some(path) => match save_settings(path, settings) {
                Ok(()) => "Settings saved. IO-thread changes apply after restart.".into(),
                Err(error) => format!("Could not save settings: {error}"),
            },
            None => "Saving is unavailable for this session.".into(),
        };
    }
}

fn preferences_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let root = std::env::var_os("HOME")
        .map(|root| PathBuf::from(root).join("Library/Application Support"));
    #[cfg(target_os = "windows")]
    let root = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|root| Path::new(root).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".config")));
    root.map(|root| root.join("Mudcrab/settings.json"))
}

pub(crate) fn prepare_settings(config: &mut EngineConfig) -> (RenderingSettings, SettingsFile) {
    prepare_settings_at(config, preferences_path())
}

fn prepare_settings_at(
    config: &mut EngineConfig,
    path: Option<PathBuf>,
) -> (RenderingSettings, SettingsFile) {
    let mut settings = RenderingSettings::default();
    settings.take_launch_values(config);
    let mut file = SettingsFile {
        path,
        status: "Changes apply now. Choose Save to keep them across runs.".into(),
    };
    if let Some(path) = &file.path {
        match std::fs::read(path) {
            Ok(bytes) => {
                let saved = serde_json::from_slice::<RenderingSettings>(&bytes)
                    .map_err(|e| e.to_string())
                    .and_then(|s| s.validate().map(|()| s));
                match saved {
                    Ok(saved) => {
                        settings = saved;
                        settings.retain_explicit_launch_values(config);
                    }
                    Err(error) => {
                        file.status = format!("Saved settings ignored: {error}");
                        warn!(%error, "saved rendering settings ignored");
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => file.status = format!("Could not read settings: {error}"),
        }
    }
    settings.apply_config(config);
    config.io_threads = settings.io_threads;
    (settings, file)
}

fn save_settings(path: &Path, settings: &RenderingSettings) -> Result<(), String> {
    settings.validate()?;
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    let parent = path.parent().ok_or("settings path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temporary = parent.join(format!("settings-{}-{stamp}.tmp", std::process::id()));
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e: std::io::Error| e.to_string())
}

/// Apply only on preference changes. Weather, light budgets and water read the same resource.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn apply_render_settings(
    mut settings: ResMut<RenderingSettings>,
    mut config: ResMut<EngineConfig>,
    mut commands: Commands,
    mut cameras: Query<
        (
            Entity,
            &mut Exposure,
            &mut Tonemapping,
            &mut Msaa,
            &mut Projection,
        ),
        With<StreamingCamera>,
    >,
    reflection: Query<Entity, With<crate::render::WaterReflectionCamera>>,
    mut sun: Query<(&mut DirectionalLight, &mut CascadeShadowConfig)>,
    mut ambient: Option<ResMut<GlobalAmbientLight>>,
    mut shadow_map: Option<ResMut<DirectionalLightShadowMap>>,
    mut point_shadow_map: Option<ResMut<PointLightShadowMap>>,
    mut upload: Option<ResMut<RenderAssetBytesPerFrame>>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut hud: Query<&mut Node, With<crate::physics::NoclipOverlay>>,
    mut previous: Local<Option<RenderingSettings>>,
    adapter: Option<Res<RenderAdapter>>,
    mut file: Option<ResMut<SettingsFile>>,
) {
    if !settings.is_changed() {
        return;
    }
    let samples = [1, 2, 4, 8][settings.msaa];
    if let Some(adapter) = adapter
        && [
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureFormat::Depth32Float,
        ]
        .iter()
        .any(|format| {
            !adapter
                .get_texture_format_features(*format)
                .flags
                .sample_count_supported(samples)
        })
    {
        settings.msaa = 0;
        if let Some(file) = file.as_deref_mut() {
            file.status = format!("{samples}x MSAA is unavailable on this renderer; using Off.");
        }
    }
    if settings.config_differs(&config) {
        settings.apply_config(&mut config);
    }
    for (entity, mut exposure, mut tone_map, mut msaa, mut projection) in &mut cameras {
        exposure.ev100 = settings.exposure_ev100;
        *tone_map = TONE_MAPS[settings.tone_map];
        *msaa = MSAA_MODES[settings.msaa];
        if let Projection::Perspective(p) = &mut *projection {
            p.fov = settings.fov_degrees.to_radians();
        }
        apply_camera_features(&mut commands, entity, &settings);
    }
    for entity in &reflection {
        commands.entity(entity).insert(MSAA_MODES[settings.msaa]);
        apply_camera_features(&mut commands, entity, &settings);
    }
    for (mut light, mut cascades) in &mut sun {
        light.illuminance = if settings.sun {
            settings.sun_illuminance
        } else {
            0.0
        };
        light.color = Color::srgb(
            settings.sun_color[0],
            settings.sun_color[1],
            settings.sun_color[2],
        );
        light.shadow_maps_enabled = settings.sun && settings.shadows;
        let base = crate::app::sun_shadow_cascades(&config);
        let maximum =
            base.bounds.last().copied().unwrap_or(1000.0) * settings.shadow_distance_scale;
        *cascades = CascadeShadowConfigBuilder {
            minimum_distance: 7.0,
            maximum_distance: maximum,
            first_cascade_far_bound: 700.0_f32.min(maximum * 0.5),
            num_cascades: settings.shadow_cascades,
            ..default()
        }
        .into();
    }
    if let Some(ambient) = ambient.as_deref_mut() {
        ambient.brightness = settings.ambient_brightness;
        ambient.color = Color::srgb(
            settings.ambient_color[0],
            settings.ambient_color[1],
            settings.ambient_color[2],
        );
    }
    if let Some(shadow_map) = shadow_map.as_deref_mut() {
        shadow_map.size = SHADOW_SIZES[settings.shadow_size];
    }
    if let Some(shadow_map) = point_shadow_map.as_deref_mut() {
        shadow_map.size = SHADOW_SIZES[settings.point_shadow_size];
    }
    if let Some(upload) = upload.as_deref_mut() {
        *upload = RenderAssetBytesPerFrame {
            max_bytes: config.max_upload_bytes_per_frame(),
        };
    }
    for mut window in &mut windows {
        window.present_mode = if settings.vsync {
            PresentMode::AutoVsync
        } else {
            PresentMode::AutoNoVsync
        };
        if previous
            .as_ref()
            .is_none_or(|previous| previous.window_mode != settings.window_mode)
        {
            window.mode = if settings.window_mode == 0 {
                WindowMode::Windowed
            } else {
                WindowMode::BorderlessFullscreen(MonitorSelection::Current)
            };
        }
        if previous
            .as_ref()
            .is_none_or(|previous| previous.window_size != settings.window_size)
        {
            let (width, height) = WINDOW_SIZES[settings.window_size];
            window.resolution.set(width as f32, height as f32);
        }
    }
    for mut node in &mut hud {
        node.display = if settings.hud {
            Display::Flex
        } else {
            Display::None
        };
    }
    *previous = Some(settings.clone());
}

fn apply_camera_features(commands: &mut Commands, camera: Entity, settings: &RenderingSettings) {
    let mut camera = commands.entity(camera);
    if settings.cpu_frustum_culling {
        camera.remove::<NoCpuCulling>();
    } else {
        camera.insert(NoCpuCulling);
    }
    if settings.indirect_drawing {
        camera.remove::<NoIndirectDrawing>();
    } else {
        camera.insert(NoIndirectDrawing);
    }
    if settings.depth_prepass {
        camera.insert(DepthPrepass);
    } else {
        camera.remove::<DepthPrepass>();
    }
    if settings.occlusion_culling {
        camera.insert(OcclusionCulling);
    } else {
        camera.remove::<OcclusionCulling>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigAction;

    fn launch(args: &[String]) -> EngineConfig {
        match EngineConfig::from_args(args.iter().cloned()).unwrap() {
            ConfigAction::Run(config) => *config,
            ConfigAction::Help => panic!("expected a run"),
        }
    }

    #[test]
    fn setting_groups_are_contiguous_per_page_so_the_menu_headings_do_not_repeat() {
        for page in [SettingsPage::Graphics, SettingsPage::Streaming] {
            let mut seen: Vec<&str> = Vec::new();
            for spec in SETTING_SPECS.iter().filter(|spec| spec.page == page) {
                if seen.last() != Some(&spec.group) {
                    assert!(!seen.contains(&spec.group), "{} is split", spec.group);
                    seen.push(spec.group);
                }
            }
        }
    }

    #[test]
    fn dependent_settings_report_relevance_and_modification() {
        let budget = SETTING_SPECS
            .iter()
            .find(|spec| spec.label == "Enabled point-light budget")
            .unwrap();
        let lights = SETTING_SPECS
            .iter()
            .find(|spec| spec.label == "Point lights")
            .unwrap();
        let mut settings = RenderingSettings::default();
        assert!(!lights.is_modified(&settings));
        assert!(!budget.relevant(&settings));
        lights.adjust(&mut settings, 1);
        assert!(lights.is_modified(&settings));
        assert!(budget.relevant(&settings));
        assert!(!budget.is_modified(&settings));
    }

    #[test]
    fn saved_preferences_round_trip_and_explicit_flags_win() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let saved = RenderingSettings {
            point_lights: true,
            stream_radius: 4,
            unload_radius: 5,
            upload_mib: 8,
            io_threads: 2,
            exposure_ev100: 8.0,
            ..default()
        };
        save_settings(&path, &saved).unwrap();
        let (loaded, _) = prepare_settings_at(&mut EngineConfig::default(), Some(path.clone()));
        assert_eq!(loaded, saved);
        let mut config = launch(&[
            "--stream-radius".into(),
            "1".into(),
            "--max-upload-mib-per-frame".into(),
            "0".into(),
            "--io-threads".into(),
            "0".into(),
        ]);
        let (loaded, _) = prepare_settings_at(&mut config, Some(path));
        assert_eq!((loaded.stream_radius, loaded.unload_radius), (1, 2));
        assert_eq!((loaded.upload_mib, loaded.io_threads), (0, 0));
        assert!(config.lights);
        assert_eq!(loaded.exposure_ev100, 8.0);
        assert_eq!(config.max_upload_bytes_per_frame(), None);
    }

    #[test]
    fn only_valid_explicit_ini_keys_override_saved_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let ini = directory.path().join("SkyrimPrefs.ini");
        let saved = RenderingSettings {
            stream_radius: 4,
            unload_radius: 5,
            lod_multiplier: 2.0,
            lod_level0_distance: 45_000.0,
            ..default()
        };
        save_settings(&path, &saved).unwrap();
        std::fs::write(
            &ini,
            "[TerrainManager]\nfSplitDistanceMult=3\nfBlockLevel0Distance=bad\n",
        )
        .unwrap();
        let mut config = launch(&["--ini".into(), ini.display().to_string()]);
        let (loaded, _) = prepare_settings_at(&mut config, Some(path));
        assert_eq!(loaded.lod_multiplier, 3.0);
        assert_eq!(loaded.lod_level0_distance, 45_000.0);
        assert_eq!((loaded.stream_radius, loaded.unload_radius), (4, 5));
    }

    #[test]
    fn invalid_saved_choices_fall_back_without_losing_launch_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let invalid = RenderingSettings {
            tone_map: 999,
            ..default()
        };
        std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        let mut config = EngineConfig {
            lights: true,
            stream_radius: 1,
            unload_radius: 2,
            ..default()
        };
        let (loaded, file) = prepare_settings_at(&mut config, Some(path));
        assert!(loaded.point_lights);
        assert_eq!(loaded.stream_radius, 1);
        assert_eq!(loaded.tone_map, 6);
        assert!(file.status.contains("ignored"));
    }

    #[test]
    fn rejected_save_keeps_previous_file_and_reports_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        save_settings(&path, &RenderingSettings::default()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let invalid = RenderingSettings {
            exposure_ev100: f32::NAN,
            ..default()
        };
        let mut file = SettingsFile {
            path: Some(path.clone()),
            status: String::new(),
        };
        file.save(&invalid);
        assert!(file.status.contains("Could not save"));
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn renderer_changes_reach_camera_and_upload_budget_but_io_waits_for_restart() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(EngineConfig::default())
            .insert_resource(RenderingSettings::default())
            .insert_resource(RenderAssetBytesPerFrame::default())
            .add_systems(Update, apply_render_settings);
        let camera = app
            .world_mut()
            .spawn((
                Camera3d::default(),
                crate::color_pipeline::SceneColorPipeline::default(),
                StreamingCamera,
                Msaa::Off,
                DepthPrepass,
                OcclusionCulling,
            ))
            .id();
        app.update();
        {
            let mut settings = app.world_mut().resource_mut::<RenderingSettings>();
            settings.depth_prepass = false;
            settings.occlusion_culling = false;
            settings.indirect_drawing = false;
            settings.cpu_frustum_culling = false;
            settings.exposure_ev100 = 8.5;
            settings.fov_degrees = 70.0;
            settings.upload_mib = 0;
            settings.io_threads = 4;
        }
        app.update();
        assert!(app.world().get::<DepthPrepass>(camera).is_none());
        assert!(app.world().get::<OcclusionCulling>(camera).is_none());
        assert!(app.world().get::<NoCpuCulling>(camera).is_some());
        assert!(app.world().get::<NoIndirectDrawing>(camera).is_some());
        assert_eq!(app.world().get::<Exposure>(camera).unwrap().ev100, 8.5);
        let Projection::Perspective(projection) = app.world().get::<Projection>(camera).unwrap()
        else {
            panic!("expected perspective");
        };
        assert!((projection.fov.to_degrees() - 70.0).abs() < 0.001);
        assert_eq!(
            app.world().resource::<RenderAssetBytesPerFrame>().max_bytes,
            None
        );
        assert_eq!(app.world().resource::<EngineConfig>().io_threads, 0);
    }

    #[test]
    fn menu_changes_enforce_culling_dependencies_and_unload_coverage() {
        let mut settings = RenderingSettings::default();
        let depth = SETTING_SPECS
            .iter()
            .find(|spec| spec.label == "Depth prepass")
            .unwrap();
        let occlusion = SETTING_SPECS
            .iter()
            .find(|spec| spec.label == "GPU occlusion culling")
            .unwrap();
        depth.adjust(&mut settings, 1);
        assert!(!settings.depth_prepass && !settings.occlusion_culling);
        occlusion.adjust(&mut settings, 1);
        assert!(settings.depth_prepass && settings.occlusion_culling && settings.indirect_drawing);
        settings.unload_radius = settings.stream_radius;
        SETTING_SPECS
            .iter()
            .find(|spec| spec.label == "Full-detail radius (cells)")
            .unwrap()
            .adjust(&mut settings, 1);
        assert!(settings.unload_radius > settings.stream_radius);
        settings.validate().unwrap();
    }
}
