use bevy::prelude::Resource;
use std::path::PathBuf;

#[derive(Debug, Clone, Resource)]
pub struct EngineConfig {
    pub assets_dir: PathBuf,
    pub worldspace_id: u32,
    pub start_grid: (i32, i32),
    pub stream_radius: i32,
    pub unload_radius: i32,
    pub max_cell_commits_per_frame: usize,
    pub max_commit_micros_per_frame: u64,
    /// Cells outside the unload radius a frame may despawn. `0` despawns every one at once, which
    /// is the unbudgeted behaviour unloading used to have.
    pub max_cell_unloads_per_frame: usize,
    /// Converted models a frame may hand to Bevy's scene spawner. `0` arms every model whose asset
    /// is loaded, which is the unbudgeted behaviour a single spawn batch used to have.
    pub max_model_spawns_per_frame: usize,
    /// MiB of newly loaded render assets (meshes, textures) the renderer may
    /// prepare per frame. `0` prepares every asset the frame extracted.
    pub max_upload_mib_per_frame: usize,
    pub headless: bool,
    pub benchmark_only: bool,
    pub benchmark_frames: Option<u32>,
    pub benchmark_duration_secs: Option<f64>,
    pub benchmark_warmup_frames: u32,
    pub benchmark_output: PathBuf,
    /// Where to write every measured frame time, in order, as CSV (`--benchmark-frame-times`).
    /// Off by default: the report's summary is what acceptance reads; the series is for choosing
    /// run lengths and spotting drift within a run.
    pub benchmark_frame_times: Option<PathBuf>,
    /// `--run-label <text>`: names an automated run in its window title, e.g. a benchmark's
    /// variant and round ([`EngineConfig::window_title`]).
    pub run_label: Option<String>,
    pub accept_min_fps: f64,
    pub accept_p95_ms: f64,
    pub accept_max_memory_growth_gib: f64,
    pub auto_fly_speed: f32,
    pub allow_incomplete_assets: bool,
    pub synthetic_instances: usize,
    pub profile_output_dir: Option<PathBuf>,
    pub profile_scenario: String,
    pub profile_run_id: String,
    pub profile_commit: String,
    pub profile_dirty_worktree: bool,
    pub profile_hardware: String,
    pub acceptance_screenshot: Option<PathBuf>,
    pub screenshot_camera_offset: Option<(f32, f32, f32)>,
    pub diagnostic_asset_fallbacks: bool,
    pub material_fixture: bool,
    pub terrain_water_fixture: bool,
    pub transform_bounds_fixture: bool,
    pub renderer_fixture: bool,
    pub streaming_fixture: bool,
    /// Whether a streamed `LIGH` reference places a point light (`--lights`). Off by default, so
    /// every run that does not ask for lights renders exactly as it did before.
    pub lights: bool,
    pub physics_fixture: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            assets_dir: PathBuf::from("modern_assets"),
            worldspace_id: 0x3c,
            start_grid: (0, 0),
            stream_radius: 2,
            unload_radius: 3,
            max_cell_commits_per_frame: 1,
            max_commit_micros_per_frame: 16_670,
            max_cell_unloads_per_frame: 2,
            // A cell holds on the order of 15 references with a model and cells commit one per
            // frame, so 15 models is the largest batch a frame can be handed at once. Bevy
            // instantiates a batch like that in an estimated 5-8 ms on the stress scenario, which is
            // most of a 60 fps frame; arming 4 per frame keeps a batch near 1.5 ms and a whole
            // cell's models armed within four frames (~67 ms at 60 fps). `0` arms the batch whole,
            // as the engine did before this budget existed.
            max_model_spawns_per_frame: 4,
            // Three 2K BC7/UASTC textures with a full mip chain (~5.3 MiB each):
            // a cell's new textures spread over a few frames instead of landing
            // in one 13 ms upload burst, and at 60 fps the budget still admits
            // far more new assets than the streaming radius can produce.
            max_upload_mib_per_frame: 16,
            headless: false,
            benchmark_only: false,
            benchmark_frames: None,
            benchmark_duration_secs: None,
            benchmark_warmup_frames: 60,
            benchmark_output: PathBuf::from("benchmark-report.json"),
            benchmark_frame_times: None,
            run_label: None,
            accept_min_fps: 60.0,
            accept_p95_ms: 16.67,
            accept_max_memory_growth_gib: 0.5,
            auto_fly_speed: 0.0,
            allow_incomplete_assets: false,
            synthetic_instances: 250_000,
            profile_output_dir: None,
            profile_scenario: "adhoc".into(),
            profile_run_id: "run-1".into(),
            profile_commit: "unknown".into(),
            profile_dirty_worktree: false,
            profile_hardware: "unspecified".into(),
            acceptance_screenshot: None,
            screenshot_camera_offset: None,
            diagnostic_asset_fallbacks: false,
            material_fixture: false,
            terrain_water_fixture: false,
            transform_bounds_fixture: false,
            renderer_fixture: false,
            streaming_fixture: false,
            lights: false,
            physics_fixture: false,
        }
    }
}

impl EngineConfig {
    /// The per-frame render-asset upload budget in bytes, or `None` when
    /// uploads are unlimited (`--max-upload-mib-per-frame 0`).
    pub fn max_upload_bytes_per_frame(&self) -> Option<usize> {
        (self.max_upload_mib_per_frame != 0)
            .then(|| self.max_upload_mib_per_frame.saturating_mul(1024 * 1024))
    }

    pub fn from_env() -> Self {
        Self::from_args(std::env::args().skip(1))
    }

    /// The window's title: what kind of automated run this is and its `--run-label`, so a run on
    /// the taskbar says what it is. An interactive run is plain "OpenSkyrim".
    pub fn window_title(&self) -> String {
        let kind = if self.benchmark_frames.is_some() || self.benchmark_duration_secs.is_some() {
            Some("benchmark")
        } else if self.streaming_fixture {
            Some("streaming fixture")
        } else {
            None
        };
        match (kind, self.run_label.as_deref()) {
            (Some(kind), Some(label)) => format!("OpenSkyrim - {kind}: {label}"),
            (Some(kind), None) => format!("OpenSkyrim - {kind}"),
            (None, Some(label)) => format!("OpenSkyrim - {label}"),
            (None, None) => "OpenSkyrim".to_owned(),
        }
    }

    pub fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        let mut config = Self::default();
        let mut args = args.into_iter().peekable();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--assets" => {
                    if let Some(value) = args.next() {
                        config.assets_dir = value.into();
                    }
                }
                "--worldspace" => {
                    if let Some(value) = args.next().and_then(|value| parse_u32(&value)) {
                        config.worldspace_id = value;
                    }
                }
                "--grid-x" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.start_grid.0 = value;
                    }
                }
                "--grid-y" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.start_grid.1 = value;
                    }
                }
                "--stream-radius" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.stream_radius = value;
                        config.unload_radius = value + 1;
                    }
                }
                "--headless" => config.headless = true,
                "--max-unloads-per-frame" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.max_cell_unloads_per_frame = value;
                    }
                }
                "--max-model-spawns-per-frame" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.max_model_spawns_per_frame = value;
                    }
                }
                "--max-upload-mib-per-frame" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.max_upload_mib_per_frame = value;
                    }
                }
                "--max-commit-ms" => {
                    if let Some(value) = args.next().and_then(|value| value.parse::<f64>().ok())
                        && value.is_finite()
                        && value > 0.0
                    {
                        config.max_commit_micros_per_frame =
                            (value * 1_000.0).round().clamp(1.0, u64::MAX as f64) as u64;
                    }
                }
                "--benchmark-only" => config.benchmark_only = true,
                "--benchmark-frames" => {
                    config.benchmark_frames = args.next().and_then(|value| value.parse().ok());
                }
                "--benchmark-duration" => {
                    config.benchmark_duration_secs =
                        args.next().and_then(|value| value.parse().ok());
                }
                "--benchmark-warmup-frames" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.benchmark_warmup_frames = value;
                    }
                }
                "--benchmark-output" => {
                    if let Some(value) = args.next() {
                        config.benchmark_output = value.into();
                    }
                }
                // A label or path left out must not swallow the next option.
                "--run-label" => {
                    config.run_label = args.next_if(|value| !value.starts_with("--"));
                }
                "--benchmark-frame-times" => {
                    if let Some(value) = args.next_if(|value| !value.starts_with("--")) {
                        config.benchmark_frame_times = Some(value.into());
                    }
                }
                "--accept-min-fps" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.accept_min_fps = value;
                    }
                }
                "--accept-p95-ms" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.accept_p95_ms = value;
                    }
                }
                "--accept-max-memory-growth-gib" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.accept_max_memory_growth_gib = value;
                    }
                }
                "--auto-fly-speed" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.auto_fly_speed = value;
                    }
                }
                "--allow-incomplete-assets" => config.allow_incomplete_assets = true,
                "--synthetic-instances" => {
                    if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                        config.synthetic_instances = value;
                    }
                }
                "--profile-output" => {
                    config.profile_output_dir = args.next().map(PathBuf::from);
                }
                "--profile-scenario" => {
                    if let Some(value) = args.next() {
                        config.profile_scenario = value;
                    }
                }
                "--profile-run-id" => {
                    if let Some(value) = args.next() {
                        config.profile_run_id = value;
                    }
                }
                "--profile-commit" => {
                    if let Some(value) = args.next() {
                        config.profile_commit = value;
                    }
                }
                "--profile-dirty-worktree" => config.profile_dirty_worktree = true,
                "--profile-hardware" => {
                    if let Some(value) = args.next() {
                        config.profile_hardware = value;
                    }
                }
                "--acceptance-screenshot" => {
                    config.acceptance_screenshot = args.next().map(PathBuf::from);
                }
                "--screenshot-camera-offset" => match args.next() {
                    Some(raw) => match parse_offset(&raw) {
                        Some(value) => config.screenshot_camera_offset = Some(value),
                        None => eprintln!(
                            "warning: ignoring malformed --screenshot-camera-offset {raw:?}; expected \"x,y,z\" floats"
                        ),
                    },
                    None => eprintln!(
                        "warning: missing value for --screenshot-camera-offset; expected \"x,y,z\" floats"
                    ),
                },
                "--diagnostic-asset-fallbacks" => config.diagnostic_asset_fallbacks = true,
                "--material-fixture" => config.material_fixture = true,
                "--terrain-water-fixture" => config.terrain_water_fixture = true,
                "--transform-bounds-fixture" => config.transform_bounds_fixture = true,
                "--renderer-fixture" => config.renderer_fixture = true,
                "--streaming-fixture" => config.streaming_fixture = true,
                "--lights" => config.lights = true,
                "--physics-fixture" => config.physics_fixture = true,
                _ => {}
            }
        }
        config
    }
}

fn parse_offset(value: &str) -> Option<(f32, f32, f32)> {
    let mut parts = value.split(',');
    let x: f32 = parts.next()?.trim().parse().ok()?;
    let y: f32 = parts.next()?.trim().parse().ok()?;
    let z: f32 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() || !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return None;
    }
    Some((x, y, z))
}

fn parse_u32(value: &str) -> Option<u32> {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or_else(
            || value.parse().ok(),
            |hex| u32::from_str_radix(hex, 16).ok(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_one_cell_commit_within_a_sixty_fps_frame() {
        let config = EngineConfig::default();
        assert_eq!(config.max_cell_commits_per_frame, 1);
        assert_eq!(config.max_commit_micros_per_frame, 16_670);
        assert_eq!(config.max_cell_unloads_per_frame, 2);
        assert_eq!(config.max_model_spawns_per_frame, 4);
    }

    #[test]
    fn parses_the_model_spawn_budget_and_lets_zero_mean_unlimited() {
        let config =
            EngineConfig::from_args(["--max-model-spawns-per-frame", "12"].map(str::to_owned));
        assert_eq!(config.max_model_spawns_per_frame, 12);

        let unlimited =
            EngineConfig::from_args(["--max-model-spawns-per-frame", "0"].map(str::to_owned));
        assert_eq!(unlimited.max_model_spawns_per_frame, 0);
    }

    #[test]
    fn defaults_to_a_sixteen_mib_upload_budget_per_frame() {
        assert_eq!(
            EngineConfig::default().max_upload_bytes_per_frame(),
            Some(16 * 1024 * 1024)
        );
    }

    #[test]
    fn parses_the_upload_budget_and_lets_zero_mean_unlimited() {
        let config =
            EngineConfig::from_args(["--max-upload-mib-per-frame", "4"].map(str::to_owned));
        assert_eq!(config.max_upload_mib_per_frame, 4);
        assert_eq!(config.max_upload_bytes_per_frame(), Some(4 * 1024 * 1024));

        let unlimited =
            EngineConfig::from_args(["--max-upload-mib-per-frame", "0"].map(str::to_owned));
        assert_eq!(unlimited.max_upload_mib_per_frame, 0);
        assert_eq!(unlimited.max_upload_bytes_per_frame(), None);
    }

    #[test]
    fn parses_screenshot_camera_offset() {
        let config = EngineConfig::from_args(
            ["--screenshot-camera-offset", "0,6000,12000"].map(str::to_owned),
        );
        assert_eq!(
            config.screenshot_camera_offset,
            Some((0.0, 6000.0, 12000.0))
        );
        let config =
            EngineConfig::from_args(["--screenshot-camera-offset", "0,6000"].map(str::to_owned));
        assert_eq!(config.screenshot_camera_offset, None);
        assert_eq!(EngineConfig::default().screenshot_camera_offset, None);
    }

    #[test]
    fn rejects_non_finite_screenshot_camera_offset_components() {
        for invalid in [
            "NaN,0,0",
            "0,NaN,0",
            "0,0,NaN",
            "inf,0,0",
            "0,-inf,0",
            "0,0,Infinity",
        ] {
            assert_eq!(parse_offset(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn an_automated_run_says_what_it_is_in_its_title() {
        let args =
            |list: &[&str]| EngineConfig::from_args(list.iter().map(|value| (*value).to_owned()));
        assert_eq!(
            args(&["--benchmark-duration", "20", "--run-label", "main rural r1"]).window_title(),
            "OpenSkyrim - benchmark: main rural r1"
        );
        assert_eq!(
            args(&["--benchmark-frames", "600"]).window_title(),
            "OpenSkyrim - benchmark"
        );
        // A label left out does not swallow the next option.
        let config = args(&["--run-label", "--benchmark-frames", "600"]);
        assert_eq!(config.run_label, None);
        assert_eq!(config.benchmark_frames, Some(600));
        let config = args(&["--benchmark-frame-times", "--benchmark-frames", "600"]);
        assert_eq!(config.benchmark_frame_times, None);
        assert_eq!(config.benchmark_frames, Some(600));
        assert_eq!(
            args(&["--streaming-fixture"]).window_title(),
            "OpenSkyrim - streaming fixture"
        );
        assert_eq!(args(&[]).window_title(), "OpenSkyrim");
    }

    #[test]
    fn parses_runtime_options() {
        let config = EngineConfig::from_args(
            [
                "--assets",
                "converted",
                "--worldspace",
                "0x3c",
                "--grid-x",
                "4",
                "--stream-radius",
                "5",
                "--headless",
                "--max-commit-ms",
                "8.5",
                "--max-unloads-per-frame",
                "3",
                "--max-model-spawns-per-frame",
                "6",
                "--profile-output",
                "profiles/run-1",
                "--profile-scenario",
                "stress",
                "--profile-run-id",
                "run-3",
                "--profile-commit",
                "abc123",
                "--profile-dirty-worktree",
                "--profile-hardware",
                "test-machine",
                "--acceptance-screenshot",
                "evidence/rural.png",
                "--diagnostic-asset-fallbacks",
                "--material-fixture",
                "--terrain-water-fixture",
                "--transform-bounds-fixture",
                "--renderer-fixture",
                "--streaming-fixture",
                "--lights",
                "--physics-fixture",
            ]
            .map(str::to_owned),
        );
        assert_eq!(config.assets_dir, PathBuf::from("converted"));
        assert_eq!(config.worldspace_id, 0x3c);
        assert_eq!(config.start_grid, (4, 0));
        assert_eq!((config.stream_radius, config.unload_radius), (5, 6));
        assert!(config.headless);
        assert_eq!(config.max_commit_micros_per_frame, 8_500);
        assert_eq!(config.max_cell_unloads_per_frame, 3);
        assert_eq!(config.max_model_spawns_per_frame, 6);
        assert_eq!(
            config.profile_output_dir,
            Some(PathBuf::from("profiles/run-1"))
        );
        assert_eq!(config.profile_scenario, "stress");
        assert_eq!(config.profile_run_id, "run-3");
        assert_eq!(config.profile_commit, "abc123");
        assert!(config.profile_dirty_worktree);
        assert_eq!(config.profile_hardware, "test-machine");
        assert_eq!(
            config.acceptance_screenshot,
            Some(PathBuf::from("evidence/rural.png"))
        );
        assert!(config.diagnostic_asset_fallbacks);
        assert!(config.material_fixture);
        assert!(config.terrain_water_fixture);
        assert!(config.transform_bounds_fixture);
        assert!(config.renderer_fixture);
        assert!(config.streaming_fixture);
        assert!(config.lights);
        assert!(config.physics_fixture);
    }

    /// Lights are opt-in: the flag is off unless it is given, so the default run - and every
    /// acceptance or benchmark baseline taken from one - is unchanged.
    #[test]
    fn lights_are_off_until_the_flag_is_given() {
        assert!(!EngineConfig::default().lights);
        assert!(
            !EngineConfig::from_args(["--headless"].map(str::to_owned)).lights,
            "another flag does not turn lights on"
        );
        assert!(EngineConfig::from_args(["--lights"].map(str::to_owned)).lights);
    }
}
