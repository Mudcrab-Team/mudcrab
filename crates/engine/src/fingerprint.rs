//! What a measured run ran on, so two benchmark reports can be told apart before their numbers are
//! compared.
//!
//! A report's frame times only mean something next to the machine, the renderer, the world data and
//! the settings that produced them. [`RunFingerprint`] gathers all of that once, on the first frame
//! after the saved settings have been applied to the window, from resources Bevy has already set up,
//! and only for a run that records measurements
//! ([`records_measurements`]): an ordinary play session installs nothing from this module.

use crate::config::EngineConfig;
use bevy::{
    prelude::*,
    render::{
        render_resource::WgpuFeatures,
        renderer::{RenderAdapterInfo, RenderDevice},
    },
    window::{PrimaryWindow, Window},
};
use serde::Serialize;

/// The Bevy release this engine tracks, as a run fingerprint records it: the major and minor
/// release, without the patch (`0.19`, not `0.19.0`), because the patch release is what the
/// workspace's `bevy` requirement allows to move on its own and it does not change what a report
/// measures.
///
/// One named constant rather than a literal at the field, because the fingerprint is what two
/// reports are compared on: the Bevy bump has to move it in one place, and
/// `the_fingerprint_bevy_version_matches_the_workspace_manifest` fails until it does.
pub const BEVY_VERSION: &str = "0.19";

/// Whether this run records measurement data: a benchmark report (`--benchmark-frames` or
/// `--benchmark-duration`) or a profile bundle (`--profile-output`).
///
/// The fingerprint, the per-pass GPU timings and the GPU-memory diagnostics are all installed only
/// for these runs, so nothing here is paid for by normal play.
pub fn records_measurements(config: &EngineConfig) -> bool {
    config.is_benchmark_run() || config.profile_output_dir.is_some()
}

/// Reads the run's [`RunFingerprint`] once. Added only when [`records_measurements`] is true.
pub struct RunFingerprintPlugin;

impl Plugin for RunFingerprintPlugin {
    fn build(&self, app: &mut App) {
        // Startup is too early: the settings menu applies saved vsync and window size on the first
        // frame, so a Startup snapshot would describe the window before the run used it.
        app.add_systems(
            Update,
            insert_run_fingerprint
                .after(crate::settings_ui::SettingsUiSet)
                .run_if(not(resource_exists::<RunFingerprint>)),
        );
    }
}

/// Reads the run into a [`RunFingerprint`] resource, after the renderer and the window exist.
///
/// Bevy's `RenderPlugin` inserts [`RenderAdapterInfo`] and [`RenderDevice`] into the main world when
/// it finishes building, before any schedule runs; a run with no renderer has neither and the
/// fingerprint records an absent adapter rather than inventing one. The world database, when the run
/// opened one, is in the world by the time this runs too, so its own schema version is read here
/// rather than guessed from the engine's constant.
fn insert_run_fingerprint(
    mut commands: Commands,
    config: Res<EngineConfig>,
    adapter: Option<Res<RenderAdapterInfo>>,
    device: Option<Res<RenderDevice>>,
    database: Option<Res<crate::world::database::WorldDatabase>>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let window = windows.iter().next();
    commands.insert_resource(RunFingerprint::collect(
        &config,
        adapter.as_deref(),
        device.as_deref(),
        window,
        database.is_some(),
    ));
}

/// One measured run, as it was when it started.
#[derive(Debug, Clone, Serialize, Resource)]
pub struct RunFingerprint {
    /// `--profile-commit`, the hash the run was built from.
    pub commit: String,
    /// `--profile-dirty-worktree`: whether the worktree had uncommitted changes.
    pub dirty_worktree: bool,
    pub engine_version: &'static str,
    /// The Bevy release the run was built against ([`BEVY_VERSION`]), which the unit tests assert
    /// against the workspace's own `bevy` requirement.
    pub bevy_version: &'static str,
    /// `release` or `debug`, from the build itself.
    pub build_profile: &'static str,
    /// The renderer Bevy selected, or `null` when the run had none.
    pub renderer: Option<RendererFingerprint>,
    /// The primary window's physical size, or `null` when the run opens no window (`--headless`).
    pub resolution: Option<[u32; 2]>,
    /// The world-database schema this engine build expects
    /// (`shared::WORLD_DATABASE_SCHEMA_VERSION`).
    pub world_database_schema: u32,
    /// The schema of the database the run actually opened, read from its `schema_info` table. `null`
    /// when the run opened no database (the synthetic benchmark and the single-scene fixtures).
    pub opened_world_database_schema: Option<u32>,
    /// The cell-cache layout the run's `cell_cache.rkyv` must match
    /// (`shared::CELL_CACHE_VERSION`); a cache of any other version fails to open.
    pub cell_cache_version: u32,
    pub graphics: GraphicsFingerprint,
}

/// The renderer Bevy picked for the run, from its [`RenderAdapterInfo`], plus the device features
/// the render diagnostics depend on.
#[derive(Debug, Clone, Serialize)]
pub struct RendererFingerprint {
    /// The wgpu backend: `Vulkan`, `Dx12`, `Metal`, `Gl`, `BrowserWebGpu` or `Noop`.
    pub backend: String,
    pub adapter: String,
    pub driver: String,
    pub driver_info: String,
    /// `DiscreteGpu`, `IntegratedGpu`, `VirtualGpu`, `Cpu` or `Other`.
    pub device_type: String,
    /// Whether the device can time GPU work. Bevy's render diagnostics record
    /// `render/<pass>/elapsed_gpu` only on a device that has this feature (Vulkan and DX12 here).
    pub timestamp_queries_supported: bool,
    /// Whether the device can count pipeline statistics (per-pass shader invocations), which DX12
    /// does not offer.
    pub pipeline_statistics_supported: bool,
}

/// The settings the run actually rendered with, after `--ini` files and the command line have both
/// been applied.
///
/// This engine keeps its render-affecting settings on [`EngineConfig`] rather than in a separate
/// graphics-settings type, so these are the effective values of the fields that change what is
/// drawn: how far the world streams, how far each terrain LOD tier reaches, and how much render
/// asset data a frame may upload.
#[derive(Debug, Clone, Serialize)]
pub struct GraphicsFingerprint {
    pub stream_radius: i32,
    pub unload_radius: i32,
    /// `fBlockLevel0Distance`, the level-4 terrain tier's distance in Creation units.
    pub block_level0_distance: f32,
    /// `fBlockLevel1Distance`, the level-8 terrain tier's distance.
    pub block_level1_distance: f32,
    /// `fBlockMaximumDistance`, the level-16 terrain tier's distance.
    pub block_maximum_distance: f32,
    /// `fSplitDistanceMult`, the multiplier on all three block distances.
    pub split_distance_mult: f32,
    /// MiB of newly loaded render assets a frame may prepare; `0` is unlimited.
    pub max_upload_mib_per_frame: usize,
    /// Whether streamed `LIGH` references place point lights.
    pub lights: bool,
    /// Whether the run opens no window at all.
    pub headless: bool,
    /// The window's present mode (`AutoNoVsync` for a benchmark, `AutoVsync` otherwise), or `null`
    /// when the run opens no window.
    pub present_mode: Option<String>,
}

impl RunFingerprint {
    /// Reads what this run is: its settings, the renderer Bevy selected, the window it opened, and
    /// the world data it loaded.
    ///
    /// `opens_world_database` says whether the run has a world database at all; when it does, its
    /// own schema version is read from the file. The read is one query against a database the run
    /// has already opened and validated, and only a measured run asks for it.
    pub(crate) fn collect(
        config: &EngineConfig,
        adapter: Option<&RenderAdapterInfo>,
        device: Option<&RenderDevice>,
        window: Option<&Window>,
        opens_world_database: bool,
    ) -> Self {
        let features = device.map(RenderDevice::features);
        let renderer = adapter.map(|info| RendererFingerprint {
            backend: format!("{:?}", info.backend),
            adapter: info.name.clone(),
            driver: info.driver.clone(),
            driver_info: info.driver_info.clone(),
            device_type: format!("{:?}", info.device_type),
            timestamp_queries_supported: features
                .is_some_and(|features| features.contains(WgpuFeatures::TIMESTAMP_QUERY)),
            pipeline_statistics_supported: features
                .is_some_and(|features| features.contains(WgpuFeatures::PIPELINE_STATISTICS_QUERY)),
        });
        Self {
            commit: config.profile_commit.clone(),
            dirty_worktree: config.profile_dirty_worktree,
            engine_version: env!("CARGO_PKG_VERSION"),
            bevy_version: BEVY_VERSION,
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            renderer,
            resolution: window.map(|window| {
                [
                    window.resolution.physical_width(),
                    window.resolution.physical_height(),
                ]
            }),
            world_database_schema: shared::WORLD_DATABASE_SCHEMA_VERSION,
            opened_world_database_schema: opens_world_database
                .then(|| opened_world_database_schema(config))
                .flatten(),
            cell_cache_version: shared::CELL_CACHE_VERSION,
            graphics: GraphicsFingerprint {
                stream_radius: config.stream_radius,
                unload_radius: config.unload_radius,
                block_level0_distance: config.terrain_lod.block_level0_distance,
                block_level1_distance: config.terrain_lod.block_level1_distance,
                block_maximum_distance: config.terrain_lod.block_maximum_distance,
                split_distance_mult: config.terrain_lod.split_distance_mult,
                max_upload_mib_per_frame: config.max_upload_mib_per_frame,
                lights: config.lights,
                headless: config.headless,
                present_mode: window.map(|window| format!("{:?}", window.present_mode)),
            },
        }
    }
}

/// The `schema_info` version of the database a run streamed from.
///
/// The run has already opened and validated this database, so a failure here is not fatal: the
/// reason is logged and the fingerprint records `null`, which still says "no version read" rather
/// than a made-up one.
fn opened_world_database_schema(config: &EngineConfig) -> Option<u32> {
    let path = config.assets_dir.join("skyrim_world.db");
    match shared::world_assets::validate_world_database(&path) {
        Ok(version) => Some(version),
        Err(error) => {
            warn!(%error, "could not read the world database schema for the run fingerprint");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint_without_renderer() -> RunFingerprint {
        RunFingerprint::collect(&EngineConfig::default(), None, None, None, false)
    }

    /// Every field of the fingerprint reaches the JSON, and the two the run could not read are
    /// `null` rather than left out or invented.
    #[test]
    fn fingerprint_serialises_every_field() {
        let config = EngineConfig {
            profile_commit: "abc123".into(),
            profile_dirty_worktree: true,
            stream_radius: 3,
            unload_radius: 4,
            terrain_lod: crate::config::TerrainLodDistances {
                block_level0_distance: 1000.0,
                block_level1_distance: 2000.0,
                block_maximum_distance: 3000.0,
                split_distance_mult: 1.25,
            },
            lights: true,
            ..EngineConfig::default()
        };
        let fingerprint = RunFingerprint::collect(&config, None, None, None, false);
        let value = serde_json::to_value(&fingerprint).expect("the fingerprint must serialise");
        assert_eq!(value["commit"], serde_json::json!("abc123"));
        assert_eq!(value["dirty_worktree"], serde_json::json!(true));
        assert_eq!(
            value["engine_version"],
            serde_json::json!(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(value["bevy_version"], serde_json::json!(BEVY_VERSION));
        assert_eq!(
            value["build_profile"],
            serde_json::json!(if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            })
        );
        assert_eq!(value["renderer"], serde_json::Value::Null);
        assert_eq!(value["resolution"], serde_json::Value::Null);
        assert_eq!(
            value["world_database_schema"],
            serde_json::json!(shared::WORLD_DATABASE_SCHEMA_VERSION)
        );
        assert_eq!(
            value["opened_world_database_schema"],
            serde_json::Value::Null
        );
        assert_eq!(
            value["cell_cache_version"],
            serde_json::json!(shared::CELL_CACHE_VERSION)
        );
        let graphics = &value["graphics"];
        assert_eq!(graphics["stream_radius"], serde_json::json!(3));
        assert_eq!(graphics["unload_radius"], serde_json::json!(4));
        assert_eq!(graphics["block_level0_distance"], serde_json::json!(1000.0));
        assert_eq!(graphics["block_level1_distance"], serde_json::json!(2000.0));
        assert_eq!(
            graphics["block_maximum_distance"],
            serde_json::json!(3000.0)
        );
        assert_eq!(graphics["split_distance_mult"], serde_json::json!(1.25));
        assert_eq!(graphics["lights"], serde_json::json!(true));
        assert_eq!(graphics["present_mode"], serde_json::Value::Null);
    }

    /// The renderer half carries the adapter, driver and capability names a report is compared on.
    #[test]
    fn renderer_fingerprint_serialises_its_fields() {
        let renderer = RendererFingerprint {
            backend: "Vulkan".into(),
            adapter: "Test Adapter".into(),
            driver: "test-driver".into(),
            driver_info: "1.2.3".into(),
            device_type: "DiscreteGpu".into(),
            timestamp_queries_supported: true,
            pipeline_statistics_supported: false,
        };
        assert_eq!(
            serde_json::to_value(&renderer).expect("the renderer must serialise"),
            serde_json::json!({
                "backend": "Vulkan",
                "adapter": "Test Adapter",
                "driver": "test-driver",
                "driver_info": "1.2.3",
                "device_type": "DiscreteGpu",
                "timestamp_queries_supported": true,
                "pipeline_statistics_supported": false,
            })
        );
    }

    /// Only a benchmark or a profile run records measurements: the fingerprint, the GPU pass timings
    /// and the GPU memory diagnostics are all gated on this.
    #[test]
    fn only_a_benchmark_or_profile_run_records_measurements() {
        assert!(!records_measurements(&EngineConfig::default()));
        assert!(records_measurements(&EngineConfig {
            benchmark_frames: Some(60),
            ..EngineConfig::default()
        }));
        assert!(records_measurements(&EngineConfig {
            benchmark_duration_secs: Some(30.0),
            ..EngineConfig::default()
        }));
        assert!(records_measurements(&EngineConfig {
            profile_output_dir: Some("profiles/run-1".into()),
            ..EngineConfig::default()
        }));
    }

    /// The plugin reads the config the app was built with; without a renderer it still records a
    /// fingerprint, with the adapter left absent.
    #[test]
    fn the_plugin_inserts_a_fingerprint_on_the_first_frame() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(EngineConfig {
                profile_commit: "deadbeef".into(),
                ..EngineConfig::default()
            })
            .add_plugins(RunFingerprintPlugin);
        app.update();
        let fingerprint = app.world().resource::<RunFingerprint>();
        assert_eq!(fingerprint.commit, "deadbeef");
        assert!(fingerprint.renderer.is_none());
        assert_eq!(
            fingerprint.world_database_schema,
            shared::WORLD_DATABASE_SCHEMA_VERSION
        );
        assert_eq!(
            fingerprint_without_renderer().cell_cache_version,
            shared::CELL_CACHE_VERSION
        );
    }

    /// The workspace manifest's `bevy` version requirement, as its `Cargo.toml` declares it.
    ///
    /// `crates/engine` is two directories below the workspace root, so the manifest is
    /// `<CARGO_MANIFEST_DIR>/../../Cargo.toml`. It is read at test time rather than embedded, so the
    /// test fails on the manifest as it is now, not as it was when this file last compiled.
    fn workspace_bevy_requirement() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        let manifest = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
        let declaration = manifest
            .lines()
            .map(str::trim)
            .find_map(|line| {
                // The key must be exactly `bevy`: the name is followed by `=`, so `bevy_ecs = ...`
                // and the like are left alone.
                let rest = line.strip_prefix("bevy")?.trim_start();
                Some(rest.strip_prefix('=')?.trim_start())
            })
            .expect("the workspace manifest declares a `bevy` dependency");
        // `bevy = { version = "0.19.0", features = [...] }` names the requirement after `version`;
        // the bare `bevy = "0.19"` form is the requirement itself.
        let after_key = declaration
            .split_once("version")
            .map_or(declaration, |(_, rest)| rest)
            .trim_start();
        let after_key = after_key
            .strip_prefix('=')
            .unwrap_or(after_key)
            .trim_start();
        after_key
            .strip_prefix('"')
            .and_then(|quoted| quoted.split('"').next())
            .expect("the workspace's bevy declaration carries a quoted version requirement")
            .to_owned()
    }

    /// The fingerprint's `bevy_version` is the workspace's own Bevy requirement, so a Bevy bump
    /// cannot leave the fingerprint claiming the release that was there before.
    ///
    /// The requirement only has to *begin* with the recorded version: the workspace tracks the patch
    /// release (`0.19.0`) while a report compares on the release (`0.19`). A digit after the constant
    /// is not a match — `0.190` is a different release, not a patch of this one.
    #[test]
    fn the_fingerprint_bevy_version_matches_the_workspace_manifest() {
        let requirement = workspace_bevy_requirement();
        assert!(
            requirement == BEVY_VERSION || requirement.starts_with(&format!("{BEVY_VERSION}.")),
            "the workspace requires bevy `{requirement}` but the fingerprint records \
             `{BEVY_VERSION}`: move BEVY_VERSION when Bevy is bumped"
        );
    }
}
