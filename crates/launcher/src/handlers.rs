//! The launcher's own systems around the conversion panel: the Play button and the engine it
//! starts, the mod drop zone, and the [`LauncherState`] that follows what the launcher is doing.

use crate::{
    LauncherState,
    components::{PlayButton, PlayHintText},
    conversion::{
        ConversionStatus, CurrentConversion, GamePathConfig, OutputReady,
        panel::{BUTTON_OFF, BUTTON_ON, BUTTON_ON_HOVER},
        state::ConversionState,
    },
};
use bevy::prelude::*;
use std::path::Path;

/// The file extensions the mod manager takes: archives and plugins.
const MOD_EXTENSIONS: [&str; 5] = ["zip", "7z", "esp", "esm", "esl"];

/// Whether Play can start the engine: the Output folder holds a complete conversion, and no run is
/// going (a run publishes by renaming its staging folder over the output, which fails while the
/// engine has files in it open).
pub fn play_available(ready: OutputReady, state: &ConversionState) -> bool {
    ready.0 && !matches!(state, ConversionState::Running | ConversionState::Stopping)
}

/// What the launcher is doing, from the conversion's state: a run going, a conversion ready to
/// play, no Skyrim `Data` folder yet, or a conversion still to make.
pub fn launcher_state_for(
    state: &ConversionState,
    ready: OutputReady,
    has_data: bool,
) -> LauncherState {
    match state {
        ConversionState::Running | ConversionState::Stopping => LauncherState::ConvertingAssets,
        _ if ready.0 => LauncherState::ModManager,
        _ if !has_data => LauncherState::FirstRunSetup,
        _ => LauncherState::ConvertingAssets,
    }
}

/// Keeps [`LauncherState`] in step with the conversion. A transition already asked for this frame
/// (Play's `LaunchingEngine`) is left alone; the next frame brings the state back to what the
/// conversion says.
pub fn sync_launcher_state(
    conversion: Res<CurrentConversion>,
    ready: Res<OutputReady>,
    paths: Res<GamePathConfig>,
    current: Res<State<LauncherState>>,
    mut next: ResMut<NextState<LauncherState>>,
) {
    if !matches!(*next, NextState::Unchanged) {
        return;
    }
    let wanted = launcher_state_for(&conversion.0, *ready, paths.has_data());
    if *current.get() != wanted {
        next.set(wanted);
    }
}

pub fn handle_play_button_click(
    interaction_query: Query<&Interaction, (Changed<Interaction>, With<PlayButton>)>,
    ready: Res<OutputReady>,
    conversion: Res<CurrentConversion>,
    mut status: ResMut<ConversionStatus>,
    mut next_state: ResMut<NextState<LauncherState>>,
) {
    for interaction in interaction_query.iter() {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if play_available(*ready, &conversion.0) {
            next_state.set(LauncherState::LaunchingEngine);
        } else if ready.0 {
            status.push_notice("Play is off while a conversion runs.");
        } else {
            status.push_notice(
                "Play needs a complete conversion in the Output folder: press Start first.",
            );
        }
    }
}

/// Draws the Play button as available or not, and the line beside it.
pub fn draw_play(
    ready: Res<OutputReady>,
    conversion: Res<CurrentConversion>,
    paths: Res<GamePathConfig>,
    mut buttons: Query<(&Interaction, &mut BackgroundColor), With<PlayButton>>,
    mut hints: Query<&mut Text, With<PlayHintText>>,
) {
    let available = play_available(*ready, &conversion.0);
    for (interaction, mut background) in &mut buttons {
        background.0 = match (available, *interaction) {
            (false, _) => BUTTON_OFF,
            (true, Interaction::Hovered) => BUTTON_ON_HOVER,
            (true, _) => BUTTON_ON,
        };
    }
    let hint = if available {
        format!("Ready to play: {}", paths.converted_assets_path.display())
    } else if ready.0 {
        "Play is off while a conversion runs.".to_owned()
    } else {
        "Play needs a complete conversion in the Output folder.".to_owned()
    };
    for mut text in &mut hints {
        if text.0 != hint {
            text.0 = hint.clone();
        }
    }
}

/// Whether a dropped file is one the mod manager takes (by its extension, in any case).
pub fn is_mod_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            MOD_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// The mod manager's drop zone: mod archives and plugins dropped onto the launcher. Installing
/// them is not built yet, so this only logs what arrived. Dropped folders are the conversion
/// panel's ([`crate::conversion::panel::accept_dropped_folder`]).
pub fn handle_mod_drag_and_drop(mut dnd_events: MessageReader<FileDragAndDrop>) {
    for event in dnd_events.read() {
        if let FileDragAndDrop::DroppedFile { path_buf, .. } = event
            && !path_buf.is_dir()
            && is_mod_file(path_buf)
        {
            println!("Mod dropped into launcher: {path_buf:?}");
        }
    }
}

pub fn launch_engine(config: Res<GamePathConfig>, mut status: ResMut<ConversionStatus>) {
    let executable_name = if cfg!(windows) {
        "engine.exe"
    } else {
        "engine"
    };
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(executable_name)))
        .unwrap_or_else(|| executable_name.into());
    let assets = config
        .converted_assets_path
        .canonicalize()
        .unwrap_or_else(|_| config.converted_assets_path.clone());
    match std::process::Command::new(&executable)
        .arg("--assets")
        .arg(&assets)
        .spawn()
    {
        Ok(child) => {
            status.push_notice(&format!("Mudcrab engine started (process {}).", child.id()));
        }
        Err(error) => {
            status.push_notice(&format!(
                "Failed to start the engine at {}: {error}",
                executable.display()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion::state::RunReport;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn mod_files_are_known_by_their_extension() {
        for name in ["a.zip", "b.7z", "Some Mod.esp", "Update.ESM", "light.Esl"] {
            assert!(is_mod_file(&PathBuf::from(name)), "{name}");
        }
        for name in [
            "readme.txt",
            "Skyrim - Textures0.bsa",
            "Data",
            "archive.rar",
        ] {
            assert!(!is_mod_file(&PathBuf::from(name)), "{name}");
        }
    }

    #[test]
    fn play_needs_a_complete_output_and_no_run_going() {
        let finished = ConversionState::Finished(RunReport {
            complete: true,
            converted: 1,
            cache_hits: 0,
            skipped: 0,
            warnings: Vec::new(),
            artifacts: 1,
            elapsed: Duration::from_secs(1),
        });
        assert!(play_available(OutputReady(true), &ConversionState::Idle));
        assert!(play_available(OutputReady(true), &finished));
        assert!(!play_available(OutputReady(false), &finished));
        assert!(!play_available(
            OutputReady(true),
            &ConversionState::Running
        ));
        assert!(!play_available(
            OutputReady(true),
            &ConversionState::Stopping
        ));
    }

    #[test]
    fn the_launcher_state_follows_the_conversion() {
        use ConversionState::{Idle, Running};
        assert_eq!(
            launcher_state_for(&Running, OutputReady(true), true),
            LauncherState::ConvertingAssets
        );
        assert_eq!(
            launcher_state_for(&Idle, OutputReady(true), false),
            LauncherState::ModManager
        );
        assert_eq!(
            launcher_state_for(&Idle, OutputReady(false), false),
            LauncherState::FirstRunSetup
        );
        assert_eq!(
            launcher_state_for(&Idle, OutputReady(false), true),
            LauncherState::ConvertingAssets
        );
    }
}
