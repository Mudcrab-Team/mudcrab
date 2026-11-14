//! Default-on grass preference regressions through the production startup path.
use super::*;

/// Parse a session without consulting the user's environment.
fn config(args: &[&str]) -> EngineConfig {
    EngineConfig::run_from_args(args.iter().map(|arg| (*arg).to_owned()))
}

/// Write the grass preferences as a player settings file, including older readers.
fn preferences(path: &Path, grass: bool, radius: i32) {
    let mut saved = serde_json::to_value(RenderingSettings::default()).unwrap();
    saved["grass"] = grass.into();
    saved["grass_radius"] = radius.into();
    std::fs::write(path, serde_json::to_vec(&saved).unwrap()).unwrap();
}

/// Valid caches enable grass with no launch flag; invalid caches restore terrain radii.
#[test]
fn g6_default_on_and_invalid_cache_fallback() {
    let directory = tempfile::tempdir().unwrap();
    for valid in [true, false] {
        let cache_path = directory.path().join("grass_cache.rkyv");
        if valid {
            let cache = shared::grass::GrassCache {
                version: shared::grass::GRASS_CACHE_VERSION,
                types: vec![],
                cells: vec![],
            };
            std::fs::write(
                &cache_path,
                rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap(),
            )
            .unwrap();
        } else {
            std::fs::write(&cache_path, b"invalid").unwrap();
        }
        let mut launch = config(&[]);
        launch.assets_dir = directory.path().to_owned();
        let mut settings = prepare_settings_at(&mut launch, None);
        let cache = crate::app::load_optional_grass_cache(&mut launch, true, Some(&mut settings));
        assert_eq!(cache.is_some(), valid);
        assert_eq!(launch.grass, valid);
        assert_eq!(
            (launch.stream_radius, launch.unload_radius),
            if valid { (3, 4) } else { (2, 3) }
        );
    }
}

/// The menu exposes a persistent grass switch and radius without rewriting terrain preferences.
#[test]
fn g6_toggle_and_radius_persist_without_terrain_radius_leak() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    preferences(&path, true, 2);
    let mut launch = config(&[]);
    let (mut settings, mut file) = prepare_settings_at(&mut launch, Some(path.clone()));
    assert_eq!(launch.grass_radius, 2);
    assert_eq!((launch.stream_radius, launch.unload_radius), (3, 4));
    let toggle = SETTING_SPECS.iter().find(|spec| spec.label == "Grass");
    assert!(toggle.is_some(), "Grass menu toggle is required");
    (toggle.unwrap().write)(&mut settings, 0.0);
    let radius = SETTING_SPECS
        .iter()
        .find(|spec| spec.label == "Grass radius")
        .unwrap();
    (radius.write)(&mut settings, 1.0);
    settings.apply_config(&mut launch);
    assert!(launch.grass, "the menu applies grass state on next start");
    assert_eq!(
        launch.grass_radius, 2,
        "the active ring stays unchanged until next start"
    );
    file.save(&settings);
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["grass"], false);
    assert_eq!(stored["grass_radius"], 1);
    assert_eq!(stored["stream_radius"], 2);
    assert_eq!(stored["unload_radius"], 3);
    let mut next = config(&[]);
    prepare_settings_at(&mut next, Some(path));
    assert!(!next.grass);
    assert_eq!(next.grass_radius, 1);
    assert_eq!((next.stream_radius, next.unload_radius), (2, 3));
}

/// CLI owns its fields for one session, above saved preferences and default-on behavior.
#[test]
fn g6_cli_saved_default_precedence_and_session_only_save() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    preferences(&path, false, 6);
    let mut saved = config(&[]);
    prepare_settings_at(&mut saved, Some(path.clone()));
    assert!(!saved.grass);
    let mut explicit = config(&["--grass", "--grass-radius", "4"]);
    let (settings, mut file) = prepare_settings_at(&mut explicit, Some(path.clone()));
    assert!(explicit.grass);
    assert_eq!(explicit.grass_radius, 4);
    file.save(&settings);
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["grass"], false);
    assert_eq!(stored["grass_radius"], 2);
}

/// The off flag works by itself and the last explicit boolean flag wins in either order.
#[test]
fn g6_no_grass_and_last_switch_wins() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    preferences(&path, true, 2);
    for (args, enabled) in [
        (vec!["--no-grass"], false),
        (vec!["--grass", "--no-grass"], false),
        (vec!["--no-grass", "--grass"], true),
    ] {
        let parsed = EngineConfig::from_args(args.iter().map(|arg| (*arg).to_owned()));
        assert!(parsed.is_ok(), "grass switches must parse: {args:?}");
        let mut launch = config(&args);
        let (settings, mut file) = prepare_settings_at(&mut launch, Some(path.clone()));
        assert_eq!(launch.grass, enabled);
        assert_eq!(
            (launch.stream_radius, launch.unload_radius),
            if enabled { (3, 4) } else { (2, 3) }
        );
        file.save(&settings);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["grass"], true);
    }
}

/// User appearance discovery uses the existing parser without replacing a developer selection.
#[test]
fn g6_user_appearance_file_and_explicit_override() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let tuning = directory.path().join("grass.toml");
    std::fs::write(&tuning, "brightness = 1.3\n").unwrap();
    let mut launch = config(&[]);
    prepare_settings_at(&mut launch, Some(path.clone()));
    assert_eq!(launch.grass_tuning, Some(tuning));
    let mut state = crate::grass_tuning::GrassTuningState::new(launch.grass_tuning.clone());
    assert!(state.poll_at(std::time::Duration::from_secs(1)));
    assert_eq!(state.effective.brightness, 1.3);
    let mut explicit = config(&["--grass-tuning", "developer.toml"]);
    prepare_settings_at(&mut explicit, Some(path));
    assert_eq!(explicit.grass_tuning, Some(PathBuf::from("developer.toml")));
    let mut absent = config(&[]);
    prepare_settings_at(&mut absent, None);
    assert!(absent.grass_tuning.is_none());
}

/// Missing and old caches keep the requested preference while reverting temporary coverage.
#[test]
fn g6_invalid_cache_preserves_preference_and_terrain_save() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    for kind in ["missing", "old", "corrupt"] {
        preferences(&path, true, 7);
        let cache_path = directory.path().join("grass_cache.rkyv");
        if kind == "old" {
            let cache = shared::grass::GrassCache {
                version: 5,
                types: vec![],
                cells: vec![],
            };
            std::fs::write(
                &cache_path,
                rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap(),
            )
            .unwrap();
        } else if kind == "corrupt" {
            std::fs::write(&cache_path, b"corrupt").unwrap();
        }
        let mut launch = config(&[]);
        launch.assets_dir = directory.path().to_owned();
        let mut settings = prepare_settings_at(&mut launch, Some(path.clone()));
        assert!(
            crate::app::load_optional_grass_cache(&mut launch, true, Some(&mut settings)).is_none()
        );
        assert!(!launch.grass);
        assert_eq!((launch.stream_radius, launch.unload_radius), (2, 3));
        settings.1.save(&settings.0);
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["grass"], true);
        assert_eq!(stored["grass_radius"], 2);
        assert_eq!(stored["stream_radius"], 2);
        assert_eq!(stored["unload_radius"], 3);
    }
}

/// A numeric CLI value overrides its own field without re-enabling saved-off grass.
#[test]
fn g6_numeric_override_keeps_saved_off_and_stays_session_only() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    preferences(&path, false, 6);
    let mut launch = config(&["--grass-radius", "4", "--grass-density", "1.75"]);
    let (settings, mut file) = prepare_settings_at(&mut launch, Some(path.clone()));
    assert!(!launch.grass);
    assert_eq!(launch.grass_radius, 4);
    assert_eq!(launch.grass_density_multiplier, 1.75);
    assert_eq!((launch.stream_radius, launch.unload_radius), (2, 3));
    file.save(&settings);
    let stored: RenderingSettings = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(!stored.grass);
    assert_eq!(stored.grass_radius, 2);
}

/// Existing version-one files inherit grass defaults without requiring a migration.
#[test]
fn g6_older_preferences_inherit_grass_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"version":1,"stream_radius":2,"unload_radius":5,"exposure_ev100":8.5}"#,
    )
    .unwrap();
    let mut launch = config(&[]);
    let (settings, _) = prepare_settings_at(&mut launch, Some(path));
    assert!(launch.grass);
    assert_eq!(launch.grass_radius, 2);
    assert_eq!((launch.stream_radius, launch.unload_radius), (3, 5));
    assert_eq!(settings.exposure_ev100, 8.5);
}

/// Saved-off grass ignores unsupported INI generator values while retaining CLI validation.
#[test]
fn g6_saved_off_ignores_inactive_ini_generator_limits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let ini = directory.path().join("grass.ini");
    std::fs::write(
        &ini,
        "[Grass]\niMinGrassSize=0\nfGrassFadeRange=0\niMaxGrassTypesPerTexure=-1\n",
    )
    .unwrap();
    preferences(&path, false, 6);
    for extra in [
        vec![],
        vec!["--no-grass"],
        vec!["--grass-min-spacing", "48"],
        vec!["--grass-min-spacing", "0"],
    ] {
        let mut args = vec!["--ini", ini.to_str().unwrap()];
        args.extend(extra.iter().copied());
        let mut launch = config(&args);
        prepare_settings_at(&mut launch, Some(path.clone()));
        assert!(!launch.grass);
        let invalid_cli = extra.last() == Some(&"0");
        assert_eq!(launch.validate_grass().is_err(), invalid_cli, "{args:?}");
        if extra.last() == Some(&"48") {
            assert_eq!(launch.grass_min_spacing, 48.0);
        }
    }
    let mut active = config(&["--grass", "--ini", ini.to_str().unwrap()]);
    prepare_settings_at(&mut active, Some(path));
    assert!(active.validate_grass().is_ok());
}

/// Old oversized grass preferences are clamped independently of other saved settings.
#[test]
fn g6_saved_grass_maximum_stays_within_menu_and_terrain_caps() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    for saved_radius in [-4, 32, i32::MAX] {
        preferences(&path, true, saved_radius);
        let mut saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        saved["exposure_ev100"] = 8.5.into();
        std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
        let mut launch = config(&[]);
        let (settings, mut file) = prepare_settings_at(&mut launch, Some(path.clone()));
        let expected = if saved_radius < 1 { 1 } else { 2 };
        assert_eq!(settings.grass_radius, expected);
        assert_eq!(settings.exposure_ev100, 8.5);
        assert_eq!(launch.grass_radius, expected);
        assert_eq!(launch.stream_radius, expected + 1);
        assert!(launch.stream_radius <= 16);
        assert!(file.status.contains("clamped"), "{}", file.status);
        file.save(&settings);
        let stored: RenderingSettings =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored.grass_radius, expected);
        assert_eq!(stored.stream_radius, 2);
    }
    let radius = SETTING_SPECS
        .iter()
        .find(|spec| spec.label == "Grass radius")
        .unwrap();
    assert!(matches!(radius.kind, SettingKind::Number { max: 2.0, .. }));
    let terrain = SETTING_SPECS
        .iter()
        .find(|spec| spec.label == "Full-detail radius (cells)")
        .unwrap();
    let (
        SettingKind::Number { max: grass_max, .. },
        SettingKind::Number {
            max: terrain_max, ..
        },
    ) = (radius.kind, terrain.kind)
    else {
        panic!("radii must be numeric controls")
    };
    assert!(grass_max <= terrain_max - 1.0);
    let mut cli = config(&["--grass-radius", "7"]);
    prepare_settings_at(&mut cli, Some(path));
    assert_eq!(cli.grass_radius, 7, "larger radii remain launch-only");
}

/// A deliberate terrain limit wins over default, saved, or explicitly requested grass.
#[test]
fn g6_small_explicit_terrain_disables_grass_for_the_session() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    preferences(&path, true, 2);
    for radius in ["0", "1"] {
        for explicit_grass in [false, true] {
            let mut args = vec!["--stream-radius", radius];
            if explicit_grass {
                args.push("--grass");
            }
            let mut automatic = config(&args);
            assert!(!automatic.grass);
            assert!(automatic.validate_grass().is_ok());
            let (settings, mut file) = prepare_settings_at(&mut automatic, Some(path.clone()));
            assert!(!automatic.grass);
            assert!(settings.grass, "the next-start request must survive");
            assert_eq!(automatic.stream_radius, radius.parse::<i32>().unwrap());
            assert!(automatic.validate_grass().is_ok());
            assert!(
                automatic
                    .grass_settings_diagnostics
                    .iter()
                    .any(|message| message.contains("disabled for this session"))
            );
            file.save(&settings);
            let stored: RenderingSettings =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert!(stored.grass);
        }
    }
}

/// Unsupported imported generator values are skipped with warnings; CLI stays strict.
#[test]
fn g6_unsupported_ini_defaults_warn_without_refusing_startup() {
    let directory = tempfile::tempdir().unwrap();
    let ini = directory.path().join("grass.ini");
    std::fs::write(
        &ini,
        "[Grass]\niMinGrassSize=5\nfGrassFadeRange=0\niMaxGrassTypesPerTexure=-1\n",
    )
    .unwrap();
    let launch = config(&["--ini", ini.to_str().unwrap()]);
    assert!(
        launch.validate_grass().is_ok(),
        "{:?}",
        launch.validate_grass()
    );
    assert_eq!(launch.grass_min_spacing, 20.0);
    assert_eq!(
        (launch.grass_fade_start, launch.grass_fade_end),
        (7000.0, 8000.0)
    );
    assert_eq!(launch.grass_max_types_per_texture, 2);
    for key in [
        "iMinGrassSize",
        "fGrassFadeRange",
        "iMaxGrassTypesPerTexure",
    ] {
        assert!(
            launch
                .grass_settings_diagnostics
                .iter()
                .any(|message| message.contains(key) && message.contains("ignored")),
            "{key}"
        );
    }
    let explicit = config(&["--ini", ini.to_str().unwrap(), "--grass-min-spacing", "0"]);
    assert!(explicit.validate_grass().is_err());
}

/// Cache absence disables optional grass before enforcing enabled-grass coverage.
#[test]
fn g6_pack_without_cache_does_not_require_grass_coverage() {
    let directory = tempfile::tempdir().unwrap();
    let mut launch = EngineConfig {
        assets_dir: directory.path().to_owned(),
        grass_radius: 0,
        stream_radius: 1,
        ..EngineConfig::default()
    };
    assert!(
        crate::app::prepare_optional_grass_cache(&mut launch, true, None)
            .unwrap()
            .is_none()
    );
    assert!(!launch.grass);
    assert!(launch.validate_grass().is_ok());
}

/// An explicit INI radius remains visible while grass is off and never replaces the saved radius.
#[test]
fn g6_inactive_ini_radius_is_launch_owned() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let ini = directory.path().join("grass.ini");
    std::fs::write(&ini, "[Grass]\niGrassCellRadius=7\n").unwrap();
    preferences(&path, false, 6);
    let mut launch = config(&["--no-grass", "--ini", ini.to_str().unwrap()]);
    let (settings, mut file) = prepare_settings_at(&mut launch, Some(path.clone()));
    assert!(!launch.grass);
    assert_eq!(launch.grass_radius, 7);
    assert_eq!(settings.grass_radius, 7);
    file.save(&settings);
    let stored: RenderingSettings = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(stored.grass_radius, 2);
}

/// Unsupported imported coverage never refuses a default-on start; CLI remains strict.
#[test]
fn g6_unsupported_ini_radius_is_ignored_with_a_notice() {
    let directory = tempfile::tempdir().unwrap();
    let ini = directory.path().join("grass.ini");
    for radius in [-1, 0, 33, i32::MAX] {
        std::fs::write(&ini, format!("[Grass]\niGrassCellRadius={radius}\n")).unwrap();
        let launch = config(&["--ini", ini.to_str().unwrap()]);
        assert!(launch.validate_grass().is_ok());
        assert_eq!(launch.grass_radius, 2);
        assert!(
            launch
                .grass_settings_diagnostics
                .iter()
                .any(|message| message.contains("iGrassCellRadius") && message.contains("ignored"))
        );
        let path = directory.path().join("settings.json");
        preferences(&path, true, 1);
        let mut saved = config(&["--ini", ini.to_str().unwrap()]);
        prepare_settings_at(&mut saved, Some(path));
        assert_eq!(
            saved.grass_radius, 1,
            "ignored import must not own the saved field"
        );
    }
    assert!(config(&["--grass-radius", "0"]).validate_grass().is_err());
}

/// Imported fade intervals that collapse or overflow in f32 are warned and skipped.
#[test]
fn g6_unsupported_effective_ini_fade_interval_is_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let ini = directory.path().join("grass.ini");
    for values in [
        "fGrassStartFadeDistance=10000000000\nfGrassFadeRange=1",
        "fGrassStartFadeDistance=3e38\nfGrassFadeRange=3e38",
    ] {
        std::fs::write(&ini, format!("[Grass]\n{values}\n")).unwrap();
        let launch = config(&["--ini", ini.to_str().unwrap()]);
        assert!(launch.validate_grass().is_ok());
        assert_eq!(
            (launch.grass_fade_start, launch.grass_fade_end),
            (7000.0, 8000.0)
        );
        assert!(
            launch
                .grass_settings_diagnostics
                .iter()
                .any(|message| message.contains("fGrassFadeRange") && message.contains("ignored"))
        );
    }
}

/// An unsupported imported range must not discard a supported neighboring start or CLI end.
#[test]
fn g6_bad_ini_fade_range_keeps_valid_neighbor_and_cli_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let ini = directory.path().join("grass.ini");
    std::fs::write(
        &ini,
        "[Grass]\nfGrassStartFadeDistance=0\nfGrassFadeRange=0\n",
    )
    .unwrap();
    for (extra, end) in [
        (vec![], 1000.0),
        (vec!["--grass-fade-end", "10000"], 10000.0),
    ] {
        let mut args = vec!["--ini", ini.to_str().unwrap()];
        args.extend(extra);
        let launch = config(&args);
        assert!(launch.validate_grass().is_ok());
        assert_eq!((launch.grass_fade_start, launch.grass_fade_end), (0.0, end));
        assert!(
            launch
                .grass_settings_diagnostics
                .iter()
                .any(|message| message.contains("fGrassFadeRange") && message.contains("ignored"))
        );
    }
}
