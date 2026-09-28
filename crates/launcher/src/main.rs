mod components;
mod engine_process;
mod game_detection;
mod handlers;
mod ui;

use bevy::{prelude::*, window::WindowResolution};
use crossbeam_channel::Receiver;
use std::path::PathBuf;

use handlers::*;
use ui::*;

#[derive(Event, Clone)]
pub struct ConversionProgressEvent {
    pub percentage: f32,
    pub current_file: String,
    pub finished: bool,
    pub failed: bool,
}

#[derive(Resource)]
pub struct ConversionChannel {
    pub receiver: Receiver<ConversionProgressEvent>,
}

#[derive(Resource)]
pub struct ConversionStatus {
    pub progress: f32,
    pub current_step: String,
    pub is_complete: bool,
    pub has_failed: bool,
}

impl Default for ConversionStatus {
    fn default() -> Self {
        Self {
            progress: 0.0,
            current_step: "Initializing launcher...".into(),
            is_complete: false,
            has_failed: false,
        }
    }
}

#[derive(States, Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum LauncherState {
    #[default]
    DetectingGameFiles,
    FirstRunSetup,
    ModManager,
    ConvertingAssets,
    LaunchingEngine,
}

#[derive(Resource)]
pub struct GamePathConfig {
    pub skyrim_data_path: Option<PathBuf>,
    pub converted_assets_path: PathBuf,
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "OpenSkyrim Launcher & Mod Manager".into(),
                resolution: WindowResolution::new(900, 600),
                resizable: false,
                ..default()
            }),
            ..default()
        }))
        .init_state::<LauncherState>()
        .init_resource::<ConversionStatus>()
        .insert_resource(GamePathConfig {
            skyrim_data_path: None,
            converted_assets_path: "modern_assets".into(),
        })
        .add_systems(Startup, (setup_ui, detect_skyrim_and_start_conversion))
        .add_systems(
            Update,
            (
                update_conversion_progress,
                sync_status_text.after(update_conversion_progress),
                handle_play_button_click,
                handle_mod_drag_and_drop,
                watch_engine_process,
            ),
        )
        .add_systems(OnEnter(LauncherState::LaunchingEngine), launch_engine)
        .run();
}
