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
    engine_process::{EngineProcess, stderr_log_path},
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
    engine: Option<Res<EngineProcess>>,
    mut status: ResMut<ConversionStatus>,
    mut next_state: ResMut<NextState<LauncherState>>,
) {
    for interaction in interaction_query.iter() {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if let Some(engine) = &engine {
            status.push_notice(&format!(
                "The Mudcrab engine is already running (process {}). Close it before starting another.",
                engine.id()
            ));
        } else if play_available(*ready, &conversion.0) {
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

pub fn launch_engine(
    mut commands: Commands,
    config: Res<GamePathConfig>,
    engine: Option<Res<EngineProcess>>,
    mut status: ResMut<ConversionStatus>,
) {
    if engine.is_some() {
        return;
    }
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
    let command = match engine_command(&executable, &assets) {
        Ok(command) => command,
        Err(error) => {
            status.push_notice(&format!("Cannot play this conversion: {error}"));
            return;
        }
    };
    match EngineProcess::spawn(command, &stderr_log_path()) {
        Ok(engine) => {
            status.push_notice(&format!(
                "Mudcrab engine running (process {}). Start and Resume are off until it closes.",
                engine.id()
            ));
            commands.insert_resource(engine);
        }
        Err(error) => {
            status.push_notice(&format!(
                "Failed to start engine at {}: {error}",
                executable.display()
            ));
        }
    }
}

/// The output manifest selects runtime geometry, independently of the next conversion's checkbox.
fn engine_command(executable: &Path, assets: &Path) -> Result<std::process::Command, String> {
    let mut command = std::process::Command::new(executable);
    command.arg("--assets").arg(assets);
    // Preflight the recorded package; the engine resolves its manifest independently.
    let geometry =
        shared::geometry::GeometryManifest::load(assets).map_err(|error| format!("{error:#}"))?;
    crate::conversion::validate_output_geometry(assets, &geometry)?;
    Ok(command)
}

/// Watches the engine started by Play and, once it exits, says how: closed normally, or stopped
/// early with the tail of its stderr log.
pub fn watch_engine_process(
    mut commands: Commands,
    engine: Option<ResMut<EngineProcess>>,
    mut status: ResMut<ConversionStatus>,
) {
    let Some(mut engine) = engine else {
        return;
    };
    if let Some(message) = engine.poll() {
        commands.remove_resource::<EngineProcess>();
        status.push_notice(&message);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion::state::RunReport;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn play_uses_the_recorded_package_and_never_the_next_conversion_choice() {
        use shared::cluster_overlay::{OverlayMetadata, ResolvedBuildSettings};
        let folder = tempfile::tempdir().unwrap();
        let base = folder.path();
        std::fs::write(
            base.join("OWNER.txt"),
            "Owner: launcher launcher cluster fixture; lifetime: test.\n",
        )
        .unwrap();
        let database = rusqlite::Connection::open(base.join("skyrim_world.db")).unwrap();
        database
            .execute_batch(&format!(
                "CREATE TABLE schema_info(version INTEGER); INSERT INTO schema_info VALUES({});",
                shared::WORLD_DATABASE_SCHEMA_VERSION
            ))
            .unwrap();
        drop(database);
        let settings = ResolvedBuildSettings::new("fixture", "ab".repeat(32), 1024, true, true);
        let package = format!("derived/clusters-{}", settings.settings_id());
        let overlay = base.join(&package);
        std::fs::create_dir_all(&overlay).unwrap();
        std::fs::write(
            base.join("derived/OWNER.txt"),
            "Owner: launcher launcher fixture; lifetime: test.\n",
        )
        .unwrap();
        std::fs::write(
            overlay.join("OWNER.txt"),
            "Owner: launcher launcher package fixture; lifetime: test.\n",
        )
        .unwrap();
        let metadata = OverlayMetadata {
            schema_version: shared::cluster_overlay::OVERLAY_SCHEMA_VERSION,
            kind: shared::cluster_overlay::OVERLAY_KIND.into(),
            label: "fixture".into(),
            settings_id: settings.settings_id(),
            builder_identity: settings.builder_identity.clone(),
            build_settings: settings,
            terrain_sources: None,
            base_core_identity: "cd".repeat(32),
            base_fingerprint: "ef".repeat(32),
            base_failure_count: 0,
            created: "fixture".into(),
            owner: "launcher fixture".into(),
            extensions: [
                shared::extensions::CLUSTERS,
                shared::extensions::CLUSTER_TERRAIN,
            ]
            .into_iter()
            .map(|id| shared::extensions::ExtensionEntry {
                id: id.into(),
                version: 1,
                owns: Default::default(),
                sha256: "ab".repeat(32),
                built_from: "cd".repeat(32),
                stats: Default::default(),
            })
            .collect(),
        };
        std::fs::write(
            overlay.join("overlay.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        // A label matching a recorded relative selector must never redirect Play.
        let mut distracting = metadata.clone();
        distracting.build_settings.triangle_threshold += 1;
        distracting.settings_id = distracting.build_settings.settings_id();
        distracting.label = package.clone();
        let other = shared::cluster_overlay::overlay_dir(base, &distracting.settings_id);
        std::fs::create_dir(&other).unwrap();
        std::fs::write(
            other.join("OWNER.txt"),
            "Owner: launcher distracting overlay fixture; lifetime: test.\n",
        )
        .unwrap();
        std::fs::write(
            other.join("overlay.json"),
            serde_json::to_vec(&distracting).unwrap(),
        )
        .unwrap();
        std::fs::write(
            base.join("conversion-manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": converter::cache::CONVERTER_SCHEMA_VERSION,
                "complete": true, "entries": {},
                "geometry_mode": "cluster", "cluster_package": package
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(base.join("cell_cache.rkyv"), b"fixture").unwrap();
        std::fs::write(
            base.join("integration-report.json"),
            serde_json::to_vec(&serde_json::json!({
                "passed": true, "schema_version": shared::WORLD_DATABASE_SCHEMA_VERSION
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(crate::conversion::output_is_complete(base));
        assert!(!base.join("lod-manifest.json").exists());
        let command = engine_command(Path::new("engine"), base).unwrap();
        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_os_string())
            .collect();
        assert_eq!(
            arguments,
            vec!["--assets".into(), base.as_os_str().to_owned()]
        );
        assert!(!crate::conversion::settings::ConversionSettings::default().clustered_geometry);
    }

    #[test]
    fn play_preserves_legacy_baked_flags_and_rejects_missing_cluster_packages() {
        let folder = tempfile::tempdir().unwrap();
        let base = folder.path();
        std::fs::write(
            base.join("OWNER.txt"),
            "Owner: launcher launcher baked fixture; lifetime: test.\n",
        )
        .unwrap();
        let manifest = base.join("conversion-manifest.json");
        std::fs::write(&manifest, b"{}").unwrap();
        let command = engine_command(Path::new("engine"), base).unwrap();
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![std::ffi::OsStr::new("--assets"), base.as_os_str()]
        );
        std::fs::write(&manifest, br#"{"geometry_mode":"cluster"}"#).unwrap();
        assert!(
            engine_command(Path::new("engine"), base)
                .unwrap_err()
                .contains("requires a cluster package")
        );
        std::fs::write(
            &manifest,
            br#"{"geometry_mode":"cluster","cluster_package":"derived/clusters-abc"}"#,
        )
        .unwrap();
        assert!(engine_command(Path::new("engine"), base).is_err());
    }

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

    /// Play requires a ready output and no active conversion.
    #[test]
    fn play_needs_a_complete_output_and_no_run_going() {
        let finished = ConversionState::Finished(RunReport {
            complete: true,
            converted: 1,
            cache_hits: 0,
            skipped: 0,
            warnings: Vec::new(),
            notices: Vec::new(),
            lod_chunks: 0,
            lod_warnings: Vec::new(),
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
