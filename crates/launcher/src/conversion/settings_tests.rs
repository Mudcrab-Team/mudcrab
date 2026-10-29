use super::*;
use crate::conversion::{CurrentConversion, panel, state::ConversionState};

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("OWNER.txt"), "R41 settings fixture\n").unwrap();
    root
}

fn app(path: &Path) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<ConversionSettings>()
        .init_resource::<UserChosenPaths>()
        .init_resource::<GamePathConfig>()
        .init_resource::<SourceSettings>()
        .init_resource::<ConversionStatus>()
        .init_resource::<CurrentConversion>()
        .init_resource::<crate::conversion::OutputHasManifest>()
        .init_resource::<crate::conversion::PendingInputs>()
        .add_message::<FileDragAndDrop>()
        .add_systems(
            Update,
            (panel::accept_dropped_folder, save_settings).chain(),
        );
    load_settings_from(path.to_path_buf(), app.world_mut());
    app
}

fn read(path: &Path) -> SavedSettings {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn choose(app: &mut App, path: &Path) {
    app.world_mut().write_message(FileDragAndDrop::DroppedFile {
        window: Entity::PLACEHOLDER,
        path_buf: path.to_path_buf(),
    });
    app.update();
}

fn toggle(app: &mut App) {
    let mut settings = app.world_mut().resource_mut::<ConversionSettings>();
    settings.clustered_geometry = !settings.clustered_geometry;
}

fn data(root: &Path) -> PathBuf {
    let path = root.join("Data");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("Skyrim.esm"), []).unwrap();
    path
}

#[test]
fn default_runtime_destination_is_never_saved_as_user_intent() {
    let root = fixture();
    let path = root.path().join("settings.json");
    let mut app = app(&path);
    assert_eq!(
        app.world()
            .resource::<GamePathConfig>()
            .converted_assets_path,
        PathBuf::from("modern_assets")
    );
    toggle(&mut app);
    app.update();
    let saved = read(&path);
    assert!(saved.skyrim_data_path.is_none());
    assert!(saved.converted_assets_path.is_none());
    assert!(saved.conversion.clustered_geometry);
}

#[test]
fn actual_folder_drops_save_absolute_data_and_output() {
    let root = fixture();
    let path = root.path().join("settings.json");
    let data = data(root.path());
    let output = root.path().join("output");
    std::fs::create_dir(&output).unwrap();
    let mut app = app(&path);
    choose(&mut app, root.path()); // installation-root drop resolves its Data child
    choose(&mut app, &output);
    let saved = read(&path);
    assert_eq!(saved.skyrim_data_path, Some(data));
    assert_eq!(saved.converted_assets_path, Some(output));
    assert!(saved.skyrim_data_path.unwrap().is_absolute());
    assert!(saved.converted_assets_path.unwrap().is_absolute());
}

#[test]
fn unavailable_intent_survives_checkbox_save_until_real_replacement_drop() {
    let root = fixture();
    let path = root.path().join("settings.json");
    let stale_data = root.path().join("absent-data");
    let stale_output = root.path().join("absent-parent/output");
    SavedSettings {
        skyrim_data_path: Some(stale_data.clone()),
        converted_assets_path: Some(stale_output.clone()),
        ..default()
    }
    .save(&path)
    .unwrap();
    let mut app = app(&path);
    let runtime = app.world().resource::<GamePathConfig>();
    assert!(runtime.skyrim_data_path.is_none());
    assert!(runtime.converted_assets_path.as_os_str().is_empty());
    toggle(&mut app);
    app.update();
    let saved = read(&path);
    assert_eq!(saved.skyrim_data_path, Some(stale_data));
    assert_eq!(saved.converted_assets_path, Some(stale_output));
    let data = data(root.path());
    let output = root.path().join("replacement");
    std::fs::create_dir(&output).unwrap();
    choose(&mut app, &data);
    choose(&mut app, &output);
    let saved = read(&path);
    assert_eq!(saved.skyrim_data_path, Some(data));
    assert_eq!(saved.converted_assets_path, Some(output));
}

#[test]
fn rejected_real_drops_never_commit_path_intent() {
    let root = fixture();
    let path = root.path().join("settings.json");
    let mut app = app(&path);
    toggle(&mut app);
    app.update();
    let before = std::fs::read(&path).unwrap();
    choose(&mut app, &root.path().join("missing"));
    let file = root.path().join("plain.txt");
    std::fs::write(&file, "keep").unwrap();
    choose(&mut app, &file);
    let unsafe_output = root.path().join("unsafe");
    std::fs::create_dir(&unsafe_output).unwrap();
    std::fs::write(unsafe_output.join("unrelated.txt"), "keep").unwrap();
    choose(&mut app, &unsafe_output);
    let valid = root.path().join("locked");
    std::fs::create_dir(&valid).unwrap();
    app.world_mut().resource_mut::<CurrentConversion>().0 = ConversionState::Running;
    choose(&mut app, &valid);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let choices = app.world().resource::<UserChosenPaths>();
    assert!(choices.skyrim_data_path.is_none() && choices.converted_assets_path.is_none());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");
}

#[test]
fn checkbox_save_changes_file_and_leaves_no_temporary_file() {
    let root = fixture();
    let path = root.path().join("settings.json");
    let mut app = app(&path);
    toggle(&mut app);
    app.update();
    assert!(read(&path).conversion.clustered_geometry);
    toggle(&mut app);
    app.update();
    assert!(!read(&path).conversion.clustered_geometry);
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    assert!(!temporary.exists());
}

#[test]
fn failed_write_notices_once_and_retries_only_after_distinct_edit() {
    let root = fixture();
    let parent = root.path().join("blocked");
    let path = parent.join("settings.json");
    std::fs::write(&parent, "parent is a file").unwrap();
    let mut app = app(&root.path().join("initially-absent.json"));
    app.world_mut().resource_mut::<SettingsFile>().path = path.clone();
    toggle(&mut app);
    app.update();
    let notice = app.world().resource::<ConversionStatus>().notice_text();
    assert!(notice.contains("Cannot save launcher settings"), "{notice}");
    std::fs::remove_file(&parent).unwrap();
    std::fs::create_dir(&parent).unwrap();
    std::fs::write(parent.join("OWNER.txt"), "R41 repaired fixture\n").unwrap();
    for _ in 0..3 {
        app.update();
    }
    assert!(
        !path.exists(),
        "unchanged settings must not retry each frame"
    );
    assert_eq!(
        app.world().resource::<ConversionStatus>().notice_text(),
        notice
    );
    toggle(&mut app);
    app.update();
    assert!(!read(&path).conversion.clustered_geometry);
}

#[test]
fn corrupt_settings_are_quarantined_without_overwriting_prior_backups() {
    for bytes in [
        b"".as_slice(),
        b"{",
        br#"{"conversion":{"clustered_geometry":"wrong"}}"#,
    ] {
        let root = fixture();
        let path = root.path().join("settings.json");
        let backup = root.path().join("settings.json.corrupt-1");
        std::fs::write(&backup, b"older evidence").unwrap();
        std::fs::write(&path, bytes).unwrap();
        let mut app = app(&path);
        assert!(app.world().contains_resource::<SettingsFile>());
        assert!(
            !app.world()
                .resource::<ConversionSettings>()
                .clustered_geometry
        );
        assert_eq!(std::fs::read(&backup).unwrap(), b"older evidence");
        let preserved = root.path().join("settings.json.corrupt-2");
        assert_eq!(std::fs::read(&preserved).unwrap(), bytes);
        let notice = app.world().resource::<ConversionStatus>().notice_text();
        assert!(notice.contains(&path.display().to_string()), "{notice}");
        assert!(
            notice.contains(&preserved.display().to_string()),
            "{notice}"
        );
        toggle(&mut app);
        app.update();
        assert!(read(&path).conversion.clustered_geometry);
        assert_eq!(std::fs::read(preserved).unwrap(), bytes);
    }
}

#[test]
fn first_corrupt_backup_preserves_bytes_and_directory_read_error_disables_writer() {
    let root = fixture();
    let path = root.path().join("settings.json");
    std::fs::write(&path, b"bad json").unwrap();
    let app = app(&path);
    assert!(app.world().contains_resource::<SettingsFile>());
    assert_eq!(
        std::fs::read(root.path().join("settings.json.corrupt-1")).unwrap(),
        b"bad json"
    );
    let directory = root.path().join("directory.json");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("OWNER.txt"), "preserve directory\n").unwrap();
    let mut app = self::app(&directory);
    assert!(!app.world().contains_resource::<SettingsFile>());
    toggle(&mut app);
    app.update();
    assert_eq!(
        std::fs::read(directory.join("OWNER.txt")).unwrap(),
        b"preserve directory\n"
    );
    let notice = app.world().resource::<ConversionStatus>().notice_text();
    assert!(notice.contains("Cannot read launcher settings"), "{notice}");
    assert!(
        notice.contains(&directory.display().to_string()),
        "{notice}"
    );
}

#[test]
fn restored_new_output_leaf_under_existing_parent_is_preserved() {
    let root = fixture();
    let path = root.path().join("settings.json");
    let output = root.path().join("new-leaf");
    assert!(available_output_path(&output));
    SavedSettings {
        converted_assets_path: Some(output.clone()),
        ..default()
    }
    .save(&path)
    .unwrap();
    let app = app(&path);
    assert_eq!(
        app.world()
            .resource::<GamePathConfig>()
            .converted_assets_path,
        output
    );
    assert!(!output.exists());
    assert!(
        !app.world()
            .resource::<ConversionStatus>()
            .notice_text()
            .contains("Saved output folder")
    );
}

#[test]
fn restored_output_file_is_not_treated_as_a_new_folder() {
    let root = fixture();
    let path = root.path().join("settings.json");
    let output = root.path().join("file");
    std::fs::write(&output, b"keep me").unwrap();
    assert!(!available_output_path(&output));
    SavedSettings {
        converted_assets_path: Some(output.clone()),
        ..default()
    }
    .save(&path)
    .unwrap();
    let app = app(&path);
    assert!(
        app.world()
            .resource::<GamePathConfig>()
            .converted_assets_path
            .as_os_str()
            .is_empty()
    );
    assert_eq!(
        app.world()
            .resource::<UserChosenPaths>()
            .converted_assets_path,
        Some(output.clone())
    );
    assert_eq!(std::fs::read(output).unwrap(), b"keep me");
}

#[test]
fn settings_schema_round_trips_choices_source_and_omitted_fields() {
    let root = fixture();
    let path = root.path().join("settings.json");
    let saved = SavedSettings {
        conversion: ConversionSettings {
            clustered_geometry: true,
        },
        skyrim_data_path: Some(root.path().join("Data")),
        converted_assets_path: Some(root.path().join("output")),
        use_mo2: true,
        mo2: Some(SavedMo2 {
            instance_path: root.path().join("instance"),
            profile: "Exact Profile".into(),
        }),
    };
    saved.save(&path).unwrap();
    assert_eq!(
        serde_json::to_value(read(&path)).unwrap(),
        serde_json::to_value(saved).unwrap()
    );
    let defaults: SavedSettings = serde_json::from_str("{}").unwrap();
    assert!(!defaults.conversion.clustered_geometry && !defaults.use_mo2);
    assert!(
        defaults.skyrim_data_path.is_none()
            && defaults.converted_assets_path.is_none()
            && defaults.mo2.is_none()
    );
}

#[test]
fn conversion_settings_default_to_baked_and_map_both_choices() {
    let mut config = converter::PipelineConfig::new("data", "output");
    ConversionSettings {
        clustered_geometry: true,
    }
    .apply(&mut config);
    assert_eq!(config.lod_mode, converter::cluster::LodMode::Cluster);
    assert!(config.cluster_terrain);
    ConversionSettings::default().apply(&mut config);
    assert_eq!(config.lod_mode, converter::cluster::LodMode::Baked);
    assert!(!config.cluster_terrain);
}

#[test]
fn r41_cross_cwd_driver() {
    const MODE: &str = "MUDCRAB_R41_SETTINGS_CHILD_MODE";
    const FILE: &str = "MUDCRAB_R41_SETTINGS_CHILD_FILE";
    if let Some(mode) = std::env::var_os(MODE) {
        let path = PathBuf::from(std::env::var_os(FILE).unwrap());
        let mut app = app(&path);
        if mode == "choose" {
            choose(&mut app, Path::new("Data"));
            choose(&mut app, Path::new("chosen-output"));
            assert!(read(&path).converted_assets_path.unwrap().is_absolute());
        } else if mode == "restore" {
            let saved = read(&path);
            let runtime = app.world().resource::<GamePathConfig>();
            assert_eq!(
                runtime.converted_assets_path,
                saved.converted_assets_path.unwrap()
            );
            assert_eq!(runtime.skyrim_data_path, saved.skyrim_data_path);
            assert!(runtime.converted_assets_path.is_absolute());
        } else {
            app.add_systems(Startup, panel::detect_skyrim_at_start);
            // The local game_data/Data candidate belongs to this isolated child cwd.
            toggle(&mut app);
            app.update();
            assert!(
                app.world()
                    .resource::<GamePathConfig>()
                    .skyrim_data_path
                    .is_some()
            );
            let saved = read(&path);
            assert!(saved.skyrim_data_path.is_none() && saved.converted_assets_path.is_none());
        }
        return;
    }
    let root = fixture();
    let first = root.path().join("first");
    let second = root.path().join("second");
    for folder in [&first, &second] {
        std::fs::create_dir(folder).unwrap();
        std::fs::write(folder.join("OWNER.txt"), "R41 subprocess cwd\n").unwrap();
    }
    data(&first);
    std::fs::create_dir(first.join("chosen-output")).unwrap();
    let candidate = first.join("game_data");
    std::fs::create_dir(&candidate).unwrap();
    data(&candidate);
    let path = root.path().join("settings.json");
    for (mode, cwd) in [("auto", &first), ("choose", &first), ("restore", &second)] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "conversion::settings::tests::r41_cross_cwd_driver",
                "--nocapture",
            ])
            .env(MODE, mode)
            .env(FILE, &path)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child must execute exact test"
        );
    }
    let saved = read(&path);
    assert_eq!(saved.skyrim_data_path, Some(first.join("Data")));
    assert_eq!(
        saved.converted_assets_path,
        Some(first.join("chosen-output"))
    );
}

#[test]
fn actual_checkbox_observer_writes_choice_and_respects_running_lock() {
    use bevy::ui_widgets::ValueChange;
    let root = fixture();
    let path = root.path().join("settings.json");
    let mut app = app(&path);
    app.add_observer(panel::change_cluster_geometry);
    let checkbox = app.world_mut().spawn(panel::ClusterGeometryCheckbox).id();
    for choice in [true, false] {
        app.world_mut().trigger(ValueChange {
            source: checkbox,
            value: choice,
            is_final: true,
        });
        app.update();
        assert_eq!(read(&path).conversion.clustered_geometry, choice);
        assert!(
            !path
                .with_extension(format!("{}.tmp", std::process::id()))
                .exists()
        );
    }
    let bytes = std::fs::read(&path).unwrap();
    app.world_mut().resource_mut::<CurrentConversion>().0 = ConversionState::Running;
    app.world_mut().trigger(ValueChange {
        source: checkbox,
        value: true,
        is_final: true,
    });
    app.update();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let restored = self::app(&path);
    assert!(
        !restored
            .world()
            .resource::<ConversionSettings>()
            .clustered_geometry
    );
}

#[test]
fn restored_relative_default_output_is_preserved() {
    let root = fixture();
    let path = root.path().join("settings.json");
    // Legacy R40 persisted the cwd-relative default. Preserve the runtime default,
    // but never carry this unchosen location into another persisted edit.
    std::fs::write(&path, br#"{"converted_assets_path":"modern_assets"}"#).unwrap();
    let mut app = app(&path);
    assert_eq!(
        app.world()
            .resource::<GamePathConfig>()
            .converted_assets_path,
        PathBuf::from("modern_assets")
    );
    assert!(
        app.world()
            .resource::<UserChosenPaths>()
            .converted_assets_path
            .is_none()
    );
    toggle(&mut app);
    app.update();
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(json.get("converted_assets_path").is_none());
}

#[test]
fn settings_save_creates_nested_configuration_parent() {
    let root = fixture();
    let path = root.path().join("fresh/config/settings.json");
    let mut app = app(&path);
    toggle(&mut app);
    app.update();
    assert!(read(&path).conversion.clustered_geometry);
    assert!(
        !path
            .with_extension(format!("{}.tmp", std::process::id()))
            .exists()
    );
}

#[test]
fn failed_replacement_preserves_destination_and_removes_temporary_file() {
    let root = fixture();
    let path = root.path().join("directory.json");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("OWNER.txt"), b"preserve destination").unwrap();
    assert!(SavedSettings::default().save(&path).is_err());
    assert_eq!(
        std::fs::read(path.join("OWNER.txt")).unwrap(),
        b"preserve destination"
    );
    assert!(
        !path
            .with_extension(format!("{}.tmp", std::process::id()))
            .exists()
    );
}

#[test]
fn platform_configuration_folder_uses_native_convention() {
    let root = fixture();
    let folder = root.path().to_path_buf();
    #[cfg(windows)]
    assert_eq!(
        config_folder(Some(folder.clone()), None, None),
        Some(folder.clone())
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        config_folder(None, None, Some(folder.clone())),
        Some(folder.join("Library/Application Support"))
    );
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        assert_eq!(
            config_folder(None, Some(folder.clone()), None),
            Some(folder.clone())
        );
        assert_eq!(
            config_folder(None, Some(PathBuf::from("relative")), Some(folder.clone())),
            Some(folder.join(".config"))
        );
    }
    assert_eq!(config_folder(None, None, None), None);
    assert_eq!(
        config_folder(
            Some("relative".into()),
            Some("relative".into()),
            Some("relative".into())
        ),
        None
    );
}

#[test]
fn detect_replaces_the_dropped_data_choice_after_restart() {
    const PHASE: &str = "MUDCRAB_R42_DETECT_PHASE";
    const FILE: &str = "MUDCRAB_R42_DETECT_FILE";
    if let Some(phase) = std::env::var_os(PHASE) {
        let path = PathBuf::from(std::env::var_os(FILE).unwrap());
        let expected = path.parent().unwrap().join("first/game_data/Data");
        let mut app = app(&path);
        if phase == "detect" {
            app.add_systems(Update, panel::click_controls.before(save_settings));
            choose(&mut app, Path::new("Data"));
            let original = std::path::absolute("Data").unwrap();
            assert_eq!(read(&path).skyrim_data_path, Some(original));
            app.world_mut()
                .spawn((panel::ControlButton::Detect, Interaction::Pressed));
            app.update();
            // Confirm Detect actually found B before checking its durable choice.
            assert_eq!(
                std::path::absolute(
                    app.world()
                        .resource::<GamePathConfig>()
                        .skyrim_data_path
                        .as_ref()
                        .unwrap()
                )
                .unwrap(),
                expected
            );
            assert_eq!(
                read(&path).skyrim_data_path,
                Some(expected.clone()),
                "explicit Detect must replace the previously dropped Data choice"
            );
            assert_eq!(
                app.world().resource::<GamePathConfig>().skyrim_data_path,
                Some(expected)
            );
            assert!(
                app.world()
                    .get_resource::<crate::conversion::RunChannel>()
                    .is_none()
            );
        } else {
            assert_eq!(phase, "restore");
            assert_eq!(
                app.world().resource::<GamePathConfig>().skyrim_data_path,
                Some(expected.clone())
            );
            assert_eq!(
                app.world().resource::<UserChosenPaths>().skyrim_data_path,
                Some(expected.clone())
            );
            assert_eq!(read(&path).skyrim_data_path, Some(expected));
        }
        return;
    }
    let root = fixture();
    let first = root.path().join("first");
    let second = root.path().join("second");
    for folder in [&first, &second] {
        std::fs::create_dir(folder).unwrap();
        std::fs::write(folder.join("OWNER.txt"), "R42 isolated launch directory\n").unwrap();
    }
    data(&first); // A: explicitly dropped folder.
    let candidate = first.join("game_data");
    std::fs::create_dir(&candidate).unwrap();
    std::fs::write(candidate.join("OWNER.txt"), "R42 discovery fixture\n").unwrap();
    data(&candidate); // B: first valid local discovery candidate.
    let path = root.path().join("settings.json");
    for (phase, cwd) in [("detect", &first), ("restore", &second)] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "conversion::settings::tests::detect_replaces_the_dropped_data_choice_after_restart", "--nocapture"])
            .env(PHASE, phase).env(FILE, &path).current_dir(cwd).output().unwrap();
        assert!(
            output.status.success(),
            "{phase}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child must execute exact test"
        );
    }
    assert_eq!(
        read(&path).skyrim_data_path,
        Some(first.join("game_data/Data"))
    );
}

#[test]
fn geometry_choice_defaults_and_serde_round_trip() {
    assert!(
        !serde_json::from_str::<ConversionSettings>("{}")
            .unwrap()
            .clustered_geometry
    );
    let choice = ConversionSettings {
        clustered_geometry: true,
    };
    assert_eq!(
        serde_json::from_slice::<ConversionSettings>(&serde_json::to_vec(&choice).unwrap())
            .unwrap(),
        choice
    );
}
