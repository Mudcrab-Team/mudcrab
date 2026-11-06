use super::EngineConfig;
use std::{fs, path::PathBuf};

/// Parse synthetic arguments through the engine's public configuration entry point.
fn parse(args: &[&str]) -> EngineConfig {
    EngineConfig::run_from_args(args.iter().map(|value| (*value).to_owned()))
}

/// Write a synthetic INI fixture and return its argument spelling.
fn ini(directory: &tempfile::TempDir, name: &str, text: &str) -> String {
    let path = directory.path().join(name);
    fs::write(&path, text).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
/// Verify no-file defaults enable grass while explicit off retains the terrain baseline.
fn grass_ini_defaults_enable_grass_and_pin_the_shipped_minimum_fade() {
    assert!(parse(&[]).grass);
    let disabled = parse(&["--no-grass"]);
    assert!(!disabled.grass);
    assert_eq!((disabled.stream_radius, disabled.unload_radius), (2, 3));
    let enabled = parse(&["--grass"]);
    assert!(enabled.validate_grass().is_ok());
    assert_eq!(enabled.grass_min_spacing, 20.0);
    assert_eq!(enabled.grass_radius, 2);
    assert_eq!(enabled.grass_max_types_per_texture, 2);
    assert_eq!(enabled.grass_texture_threshold, 0.0);
    assert_eq!((enabled.stream_radius, enabled.unload_radius), (3, 4));
    assert_eq!(enabled.effective_grass_fade(), (7000.0, 8000.0));
    let report = enabled.grass_settings_report.as_ref().unwrap();
    assert_eq!(report.float("fGrassMinStartFadeDistance"), Some(0.0));
    assert_eq!(report.float("fGrassMaxStartFadeDistance"), Some(7000.0));
    assert!(report.sources.is_empty());
    assert!(report.settings.values().all(|setting| {
        setting.effective.source == "default" && setting.effective.line.is_none()
    }));
    let clamped = parse(&["--grass", "--stream-radius", "2"]);
    assert_eq!(clamped.effective_grass_radius(), 1);
    let (start, end) = clamped.effective_grass_fade();
    assert!((start - 3584.0).abs() < 0.001);
    assert_eq!(end, 4096.0);
    assert!(
        clamped
            .grass_settings_diagnostics
            .iter()
            .any(|line| line.contains("clamped"))
    );
}

#[test]
/// Verify file order wins per key in both caller orders, without assigning filename roles.
fn grass_ini_files_apply_in_caller_order_key_by_key_and_preserve_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let prefs = ini(
        &dir,
        "SkyrimPrefs.ini",
        "[Grass]\niMinGrassSize=35\nfGrassFadeRange=750\niMaxGrassTypesPerTexure=7\n",
    );
    let skyrim = ini(
        &dir,
        "Skyrim.ini",
        "[Grass]\niMinGrassSize=65\nfGrassStartFadeDistance=3200\nfTexturePctThreshold=0.25\n",
    );
    let before = [fs::read(&prefs).unwrap(), fs::read(&skyrim).unwrap()];
    for (first, last, spacing) in [(&prefs, &skyrim, 65.0), (&skyrim, &prefs, 35.0)] {
        let config = parse(&["--grass", "--ini", first, "--ini", last]);
        assert!(config.validate_grass().is_ok());
        assert_eq!(config.grass_min_spacing, spacing);
        assert_eq!(config.grass_max_types_per_texture, 7);
        assert_eq!(config.grass_texture_threshold, 0.25);
        assert_eq!(
            (config.grass_fade_start, config.grass_fade_end),
            (3200.0, 3950.0)
        );
        let report = config.grass_settings_report.as_ref().unwrap();
        assert_eq!(report.settings["iMinGrassSize"].effective.source, *last);
        assert_eq!(report.settings["iMinGrassSize"].effective.line, Some(2));
        assert_eq!(report.settings["fGrassFadeRange"].effective.source, prefs);
        assert_eq!(
            report.settings["fGrassStartFadeDistance"].effective.source,
            skyrim
        );
        assert_eq!(
            report
                .sources
                .iter()
                .map(|source| source.path.clone())
                .collect::<Vec<_>>(),
            [PathBuf::from(first), PathBuf::from(last)]
        );
    }
    assert_eq!(fs::read(&prefs).unwrap(), before[0]);
    assert_eq!(fs::read(&skyrim).unwrap(), before[1]);
}

#[test]
/// Verify explicit numeric options override every file before, between, or after selectors.
fn grass_ini_numeric_options_win_at_every_argument_position() {
    let dir = tempfile::tempdir().unwrap();
    let first = ini(
        &dir,
        "first.ini",
        "[Grass]\niMinGrassSize=35\niGrassCellRadius=3\nfGrassStartFadeDistance=2100\nfGrassFadeRange=600\n",
    );
    let last = ini(
        &dir,
        "last.ini",
        "[Grass]\niMinGrassSize=65\niGrassCellRadius=4\nfGrassStartFadeDistance=3200\nfGrassFadeRange=900\n",
    );
    let numeric = [
        "--grass-min-spacing",
        "48",
        "--grass-radius",
        "5",
        "--grass-fade-start",
        "4100",
        "--grass-fade-end",
        "5900",
        "--grass-density",
        "1.75",
        "--grass-uploads-per-frame",
        "0",
        "--grass-cache-mib",
        "23",
    ];
    for position in [1, 3, 5] {
        let mut args = vec!["--grass", "--ini", &first, "--ini", &last];
        args.splice(position..position, numeric);
        let config = parse(&args);
        assert!(config.validate_grass().is_ok(), "position {position}");
        assert_eq!(config.grass_min_spacing, 48.0);
        assert_eq!(config.grass_radius, 5);
        assert_eq!(
            (config.grass_fade_start, config.grass_fade_end),
            (4100.0, 5900.0)
        );
        assert_eq!(config.grass_density_multiplier, 1.75);
        assert_eq!(config.grass_uploads_per_frame, 0);
        assert_eq!(config.grass_cache_mib, 23);
        let report = config.grass_settings_report.as_ref().unwrap();
        assert_eq!(report.integer("iMinGrassSize"), Some(65));
        assert_eq!(report.integer("iGrassCellRadius"), Some(4));
        assert_eq!(report.settings["iMinGrassSize"].effective.source, last);
        assert!(config.grass_settings_diagnostics.iter().any(|line| {
            line == "explicit CLI --grass-min-spacing=48 overrides the imported settings report"
        }));
    }
}

#[test]
/// Verify the shared reader ignores unknown keys, wrong sections, and keys without a section.
fn grass_ini_unknown_keys_do_not_change_defaults_or_create_grass_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let path = ini(
        &dir,
        "unknown.ini",
        "iMinGrassSize=99\n[Grass]\niMaxGrassTypesPerTexture=8\nfUnimplementedSetting=2\n[Display]\niMinGrassSize=60\n",
    );
    let config = parse(&["--grass", "--ini", &path]);
    assert!(config.validate_grass().is_ok());
    assert_eq!(config.grass_min_spacing, 20.0);
    assert_eq!(config.grass_max_types_per_texture, 2);
    let report = config.grass_settings_report.as_ref().unwrap();
    assert_eq!(report.settings.len(), 15);
    assert!(report.diagnostics.is_empty());
    assert!(!report.has_unhandled_settings);
    assert!(
        report
            .settings
            .values()
            .all(|setting| setting.effective.source == "default")
    );
}

#[test]
/// Verify a missing file is ignored like main while successful files still apply in order.
fn grass_ini_missing_files_follow_main_without_discarding_valid_files() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent.ini").to_string_lossy().into_owned();
    let valid = ini(
        &dir,
        "valid.ini",
        "[Grass]\niMinGrassSize=47\n[General]\nuGridsToLoad=9\n",
    );
    let defaults = parse(&["--grass", "--ini", &missing]);
    assert!(defaults.validate_grass().is_ok());
    assert_eq!(defaults.grass_min_spacing, 20.0);
    assert!(
        defaults
            .grass_settings_report
            .as_ref()
            .unwrap()
            .sources
            .is_empty()
    );
    for args in [
        vec!["--grass", "--ini", &missing, "--ini", &valid],
        vec!["--grass", "--ini", &valid, "--ini", &missing],
    ] {
        let config = parse(&args);
        assert!(config.validate_grass().is_ok());
        assert_eq!(config.grass_min_spacing, 47.0);
        assert_eq!((config.stream_radius, config.unload_radius), (4, 5));
        let report = config.grass_settings_report.as_ref().unwrap();
        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.sources[0].path, PathBuf::from(&valid));
    }
}

#[test]
/// Explicit CLI grass switches beat the imported draw state in any argument position.
fn grass_ini_draw_flag_yields_to_explicit_cli_switches() {
    let dir = tempfile::tempdir().unwrap();
    let off = ini(
        &dir,
        "off.ini",
        "[Grass]\nbDrawShaderGrass=0\niGrassCellRadius=6\n",
    );
    let on = ini(&dir, "on.ini", "[Grass]\nbDrawShaderGrass=1\n");
    let disabled = parse(&["--ini", &off]);
    assert!(!disabled.grass);
    assert_eq!((disabled.stream_radius, disabled.unload_radius), (2, 3));
    assert!(parse(&["--ini", &on]).grass);
    assert!(parse(&["--ini", &on, "--grass", "--ini", &off]).grass);
    assert!(parse(&["--ini", &off, "--grass", "--ini", &on]).grass);
    assert!(!parse(&["--ini", &on, "--no-grass"]).grass);
}

#[test]
/// Verify reported slider, wind, and legacy cache values retain provenance without runtime effects.
fn grass_ini_report_only_values_keep_provenance_and_do_not_control_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = ini(
        &dir,
        "reported.ini",
        "[Grass]\nfGrassMinStartFadeDistance=123\nfGrassMaxStartFadeDistance=9876\nfGrassWindMagnitudeMin=17\nfGrassWindMagnitudeMax=83\nbAllowCreateGrass=0\nbAllowLoadGrass=1\nbGenerateGrassDataFiles=1\n",
    );
    let config = parse(&["--grass", "--ini", &path]);
    assert!(config.validate_grass().is_ok());
    assert!(config.grass);
    assert_eq!(config.grass_min_spacing, 20.0);
    assert_eq!(
        (config.grass_fade_start, config.grass_fade_end),
        (7000.0, 8000.0)
    );
    let report = config.grass_settings_report.as_ref().unwrap();
    for (offset, (key, expected)) in [
        ("fGrassMinStartFadeDistance", serde_json::json!(123.0)),
        ("fGrassMaxStartFadeDistance", serde_json::json!(9876.0)),
        ("fGrassWindMagnitudeMin", serde_json::json!(17.0)),
        ("fGrassWindMagnitudeMax", serde_json::json!(83.0)),
        ("bAllowCreateGrass", serde_json::json!(false)),
        ("bAllowLoadGrass", serde_json::json!(true)),
        ("bGenerateGrassDataFiles", serde_json::json!(true)),
    ]
    .into_iter()
    .enumerate()
    {
        let setting = &report.settings[key];
        assert_eq!(setting.effective.value, expected, "{key}");
        assert_eq!(setting.effective.source, path, "{key}");
        assert_eq!(setting.effective.line, Some(offset + 2), "{key}");
    }
    assert!(
        config
            .grass_settings_diagnostics
            .iter()
            .any(|line| line.contains("wind"))
    );
    assert!(
        config
            .grass_settings_diagnostics
            .iter()
            .any(|line| line.contains("legacy"))
    );
    assert!(
        config
            .grass_settings_diagnostics
            .iter()
            .any(|line| line.contains("slider"))
    );
}

#[test]
/// Verify the latest malformed scalar is ignored as main does while unrelated keys survive.
fn grass_ini_malformed_latest_values_use_defaults_and_leave_valid_keys_usable() {
    let dir = tempfile::tempdir().unwrap();
    let first = ini(
        &dir,
        "first.ini",
        "[Grass]\niMinGrassSize=45\nfGrassStartFadeDistance=2100\n",
    );
    let last = ini(
        &dir,
        "malformed.ini",
        "[Grass]\niMinGrassSize=not-a-number\nfGrassStartFadeDistance=NaN\nfTexturePctThreshold=0.4\nbDrawShaderGrass=2\n",
    );
    let config = parse(&["--grass", "--ini", &first, "--ini", &last]);
    assert!(config.validate_grass().is_ok());
    assert!(config.grass);
    assert_eq!(config.grass_min_spacing, 20.0);
    assert_eq!(config.grass_fade_start, 7000.0);
    assert_eq!(config.grass_texture_threshold, 0.4);
    let report = config.grass_settings_report.as_ref().unwrap();
    assert_eq!(report.settings["iMinGrassSize"].effective.source, "default");
    assert_eq!(
        report.settings["fTexturePctThreshold"].effective.source,
        last
    );
}

#[test]
/// Verify malformed supported scalars warn and preserve defaults without aborting startup.
fn grass_ini_invalid_scalars_follow_the_shared_reader_fallback_policy() {
    let dir = tempfile::tempdir().unwrap();
    for setting in [
        "iMinGrassSize=-1",
        "iMinGrassSize=3.5",
        "iMinGrassSize=2147483648",
        "iMinGrassSize=",
        "fGrassFadeRange=NaN",
        "fGrassFadeRange=inf",
        "fGrassFadeRange=-1",
        "fGrassFadeRange=1e100",
        "fTexturePctThreshold=1.1",
        "bDrawShaderGrass=2",
    ] {
        let path = ini(&dir, "invalid.ini", &format!("[Grass]\n{setting}\n"));
        let config = parse(&["--grass", "--ini", &path]);
        assert!(config.validate_grass().is_ok(), "{setting}");
        assert!(config.grass, "{setting}");
        let report = config.grass_settings_report.as_ref().unwrap();
        assert!(
            report
                .settings
                .values()
                .all(|value| value.effective.source == "default"),
            "{setting}"
        );
    }
}

#[test]
/// Verify main's case, BOM, and duplicate semantics apply to grass alongside terrain keys.
fn grass_ini_uses_the_same_reader_as_terrain() {
    let dir = tempfile::tempdir().unwrap();
    let path = ini(
        &dir,
        "mixed.ini",
        "\u{feff}[gRaSs]\n iMinGrassSize = 40\nIMINGRASSSIZE=60\n[TerrainManager]\nfSplitDistanceMult=2\n[General]\nuGridsToLoad=9\n",
    );
    let config = parse(&["--grass", "--ini", &path]);
    assert!(config.validate_grass().is_ok());
    assert_eq!(config.grass_min_spacing, 60.0);
    assert_eq!(config.terrain_lod.split_distance_mult, 2.0);
    assert_eq!((config.stream_radius, config.unload_radius), (4, 5));
    assert_eq!(
        config.grass_settings_report.as_ref().unwrap().settings["iMinGrassSize"]
            .effective
            .line,
        Some(3)
    );
}

#[test]
/// Verify valid Skyrim spacing still respects the generator limit and explicit repair options.
fn grass_ini_zero_spacing_reports_the_generator_limit_and_cli_can_override_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = ini(&dir, "spacing.ini", "[Grass]\niMinGrassSize=0\n");
    let imported = parse(&["--grass", "--ini", &path]);
    assert!(
        imported
            .validate_grass()
            .unwrap_err()
            .contains("safety minimum 8")
    );
    let overridden = parse(&["--grass-min-spacing", "20", "--grass", "--ini", &path]);
    assert!(overridden.validate_grass().is_ok());
}

#[test]
/// Verify imported fading controls whether a zero fade range is valid.
fn grass_ini_zero_fade_range_is_allowed_only_when_fading_is_disabled() {
    let dir = tempfile::tempdir().unwrap();
    for enabled in [false, true] {
        let path = ini(
            &dir,
            "fade.ini",
            &format!(
                "[Grass]\nbEnableGrassFade={}\nfGrassStartFadeDistance=0\nfGrassFadeRange=0\n",
                u8::from(enabled)
            ),
        );
        let config = parse(&["--grass", "--ini", &path]);
        assert_eq!(config.validate_grass().is_ok(), !enabled);
        if !enabled {
            assert_eq!(config.effective_grass_fade(), (0.0, 0.0));
        }
    }
}

#[test]
/// Verify terrain bounds and deferred grass radius errors cannot overflow unload coordinates.
fn grass_ini_preserves_streaming_and_grass_radius_boundaries() {
    for value in ["--headless", "oops", "2147483648", "2147483647"] {
        assert!(
            EngineConfig::from_args(["--grass", "--stream-radius", value].map(str::to_owned))
                .unwrap_err()
                .to_string()
                .contains("--stream-radius")
        );
    }
    let maximum = super::MAX_STREAM_RADIUS.to_string();
    let config = parse(&["--grass", "--stream-radius", &maximum]);
    assert_eq!(config.unload_radius, super::MAX_STREAM_RADIUS + 1);
    assert!(
        parse(&["--grass", "--grass-radius", "2147483647"])
            .validate_grass()
            .unwrap_err()
            .contains("terrain streaming coordinate range")
    );
    assert!(
        parse(&["--grass", "--grass-radius", "1073741823"])
            .validate_grass()
            .is_err()
    );
    assert!(parse(&["--grass-radius", "-1"]).validate_grass().is_err());
    assert!(parse(&["--grass-density", "NaN"]).validate_grass().is_err());
}

#[test]
/// Verify inactive grass imports cannot reject startup while terrain INI values still apply.
fn grass_ini_inactive_imports_do_not_enforce_generator_runtime_limits() {
    let dir = tempfile::tempdir().unwrap();
    for explicit_off in [false, true] {
        let path = ini(
            &dir,
            "inactive.ini",
            &format!(
                "[Grass]\niMinGrassSize=0\nfGrassFadeRange=0\nbDrawShaderGrass={}\n[General]\nuGridsToLoad=9\n",
                u8::from(explicit_off)
            ),
        );
        let mut args = vec!["--ini", path.as_str()];
        if explicit_off {
            args.push("--no-grass");
        }
        let config = parse(&args);
        assert!(!config.grass, "explicit off: {explicit_off}");
        assert!(
            config.validate_grass().is_ok(),
            "inactive INI grass must not reject startup, explicit off: {explicit_off}: {:?}",
            config.validate_grass()
        );
        assert_eq!((config.stream_radius, config.unload_radius), (4, 5));
    }
}

#[test]
/// Verify applying a partial INI preserves existing settings when values are absent or malformed.
fn grass_ini_public_apply_preserves_existing_fields_and_updates_valid_values() {
    let mut config = EngineConfig {
        grass: true,
        grass_min_spacing: 48.0,
        grass_fade_start: 4300.0,
        grass_fade_end: 5100.0,
        grass_max_types_per_texture: 5,
        grass_radius: 3,
        ..Default::default()
    };
    let mut input = crate::skyrim_ini::SkyrimIni::default();
    input.merge_str("[Grass]\niMinGrassSize=oops\nfTexturePctThreshold=0.75\n");
    input.apply(&mut config);
    assert_eq!(config.grass_min_spacing, 48.0);
    assert_eq!(
        (config.grass_fade_start, config.grass_fade_end),
        (4300.0, 5100.0)
    );
    assert_eq!(config.grass_max_types_per_texture, 5);
    assert_eq!(config.grass_radius, 3);
    assert_eq!(config.grass_texture_threshold, 0.75);
    assert!(config.grass);
    assert!(config.validate_grass().is_ok());

    for (text, expected) in [
        ("[Grass]\nfGrassStartFadeDistance=6100\n", (6100.0, 6900.0)),
        ("[Grass]\nfGrassFadeRange=950\n", (4300.0, 5250.0)),
    ] {
        let mut partial = config.clone();
        let mut input = crate::skyrim_ini::SkyrimIni::default();
        input.merge_str(text);
        input.apply(&mut partial);
        assert_eq!((partial.grass_fade_start, partial.grass_fade_end), expected);
        assert_eq!(partial.grass_min_spacing, 48.0);
        assert_eq!(partial.grass_max_types_per_texture, 5);
        assert_eq!(partial.grass_radius, 3);
        assert_eq!(partial.grass_texture_threshold, 0.75);
        assert!(partial.validate_grass().is_ok());
    }

    let dir = tempfile::tempdir().unwrap();
    let path = ini(&dir, "default", "[Grass]\niMinGrassSize=57\n");
    let mut input = crate::skyrim_ini::SkyrimIni::default();
    input.merge_file(std::path::Path::new(&path)).unwrap();
    input.apply(&mut config);
    assert_eq!(config.grass_min_spacing, 57.0);
    let mut report = config
        .grass_settings_report
        .as_ref()
        .unwrap()
        .as_ref()
        .clone();
    // A relative selector `--ini default` records this exact source spelling.
    assert!(report.assign(
        "iMinGrassSize",
        serde_json::json!(57),
        "default".into(),
        Some(2)
    ));
    config.grass_min_spacing = 48.0;
    config.apply_grass_settings(report);
    assert_eq!(config.grass_min_spacing, 57.0);
}
