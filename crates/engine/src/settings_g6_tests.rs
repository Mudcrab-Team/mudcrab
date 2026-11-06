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
    preferences(&path, true, 7);
    let mut launch = config(&[]);
    let (mut settings, mut file) = prepare_settings_at(&mut launch, Some(path.clone()));
    assert_eq!(launch.grass_radius, 7);
    assert_eq!((launch.stream_radius, launch.unload_radius), (8, 9));
    let toggle = SETTING_SPECS.iter().find(|spec| spec.label == "Grass");
    assert!(toggle.is_some(), "Grass menu toggle is required");
    (toggle.unwrap().write)(&mut settings, 0.0);
    let radius = SETTING_SPECS
        .iter()
        .find(|spec| spec.label == "Grass radius")
        .unwrap();
    (radius.write)(&mut settings, 5.0);
    settings.apply_config(&mut launch);
    assert!(launch.grass, "the menu applies grass state on next start");
    assert_eq!(
        launch.grass_radius, 7,
        "the active ring stays unchanged until next start"
    );
    file.save(&settings);
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["grass"], false);
    assert_eq!(stored["grass_radius"], 5);
    assert_eq!(stored["stream_radius"], 2);
    assert_eq!(stored["unload_radius"], 3);
    let mut next = config(&[]);
    prepare_settings_at(&mut next, Some(path));
    assert!(!next.grass);
    assert_eq!(next.grass_radius, 5);
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
    assert_eq!(stored["grass_radius"], 6);
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
    let state = crate::grass_tuning::GrassTuningState::new(launch.grass_tuning.clone());
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
        assert_eq!(stored["grass_radius"], 7);
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
    assert_eq!(stored.grass_radius, 6);
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
    assert!(active.validate_grass().is_err());
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
    assert_eq!(stored.grass_radius, 6);
}
