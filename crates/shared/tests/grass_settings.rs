use serde_json::json;
use shared::grass_settings::{Category, GrassSettings, Source};

#[test]
/// Pin built-in values, typed access, and provenance without input files.
fn missing_settings_use_the_defaults_and_report_their_origin() {
    let report = GrassSettings::default().report();
    assert_eq!(report.placement["iMinGrassSize"], 20);
    assert_eq!(report.placement["iMaxGrassTypesPerTexure"], 2);
    assert_eq!(report.placement["fTexturePctThreshold"], 0.0);
    assert_eq!(report.runtime["fGrassMinStartFadeDistance"], 0.0);
    assert_eq!(report.settings.len(), 15);
    assert_eq!(report.default_profile, "default");
    assert!(report.sources.is_empty());
    assert!(report.diagnostics.is_empty());
    for (name, value) in [
        ("iMinGrassSize", 20),
        ("iMaxGrassTypesPerTexure", 2),
        ("iGrassCellRadius", 2),
    ] {
        assert_eq!(report.integer(name), Some(value));
    }
    for (name, value) in [
        ("fTexturePctThreshold", 0.0),
        ("fGrassStartFadeDistance", 7000.0),
        ("fGrassFadeRange", 1000.0),
        ("fGrassMinStartFadeDistance", 0.0),
        ("fGrassMaxStartFadeDistance", 7000.0),
        ("fGrassWindMagnitudeMin", 5.0),
        ("fGrassWindMagnitudeMax", 125.0),
    ] {
        assert_eq!(report.float(name), Some(value), "{name}");
    }
    for (name, value) in [
        ("bDrawShaderGrass", true),
        ("bEnableGrassFade", true),
        ("bAllowCreateGrass", true),
        ("bAllowLoadGrass", false),
        ("bGenerateGrassDataFiles", false),
    ] {
        assert_eq!(report.boolean(name), Some(value), "{name}");
    }
    assert!(report.settings.values().all(|setting| {
        setting.effective.source == "default"
            && setting.effective.line.is_none()
            && setting.history.len() == 1
            && setting.default == setting.effective.value
    }));
}

#[test]
/// Verify case-insensitive typed lookup rejects unknown names and incompatible types.
fn typed_access_is_case_insensitive_and_unknown_or_wrong_types_return_none() {
    let report = GrassSettings::default().report();
    assert_eq!(report.integer("imingrasssize"), Some(20));
    assert_eq!(report.float("FGRASSFADERANGE"), Some(1000.0));
    assert_eq!(report.boolean("BDRAWSHADERGRASS"), Some(true));
    assert_eq!(report.integer("fGrassFadeRange"), None);
    assert_eq!(report.boolean("iGrassCellRadius"), None);
    assert_eq!(report.float("unimplemented"), None);
}

#[test]
/// Verify a typed custom configuration retains built-in defaults in its report.
fn typed_values_are_reported_without_redefining_the_built_in_defaults() {
    let report = GrassSettings {
        min_grass_size: 37,
        start_fade_distance: 3456.0,
        draw_shader_grass: false,
        ..Default::default()
    }
    .report();
    assert_eq!(report.integer("iMinGrassSize"), Some(37));
    assert_eq!(report.float("fGrassStartFadeDistance"), Some(3456.0));
    assert_eq!(report.boolean("bDrawShaderGrass"), Some(false));
    assert_eq!(report.settings["iMinGrassSize"].default, 20);
    assert_eq!(report.settings["fGrassStartFadeDistance"].default, 7000.0);
    assert_eq!(report.settings["bDrawShaderGrass"].default, true);
}

#[test]
/// Verify accepted assignments retain caller order and update the categorized report.
fn assignments_preserve_value_history_and_effective_source() {
    let mut report = GrassSettings::default().report();
    assert!(report.assign("imingrasssize", json!(31), "first.ini".into(), Some(2)));
    assert!(report.assign("iMinGrassSize", json!(64), "last.ini".into(), Some(5)));
    assert_eq!(report.integer("iMinGrassSize"), Some(64));
    assert_eq!(report.placement["iMinGrassSize"], 64);
    let setting = &report.settings["iMinGrassSize"];
    assert_eq!(setting.category, Category::Placement);
    assert_eq!(setting.default, 20);
    assert_eq!(setting.effective.source, "last.ini");
    assert_eq!(setting.effective.line, Some(5));
    assert_eq!(
        setting
            .history
            .iter()
            .map(|entry| entry.value.clone())
            .collect::<Vec<_>>(),
        [json!(20), json!(31), json!(64)]
    );
    let before = serde_json::to_value(&report).unwrap();
    assert!(!report.assign("unknown", json!(99), "ignored.ini".into(), Some(8)));
    assert_eq!(serde_json::to_value(&report).unwrap(), before);
}

#[test]
/// Verify density hashes ignore runtime, cache flags, source names, and assignment history.
fn placement_hashes_follow_effective_placement_values_only() {
    let defaults = GrassSettings::default().report();
    let mut runtime = defaults.clone();
    assert!(runtime.assign("fGrassFadeRange", json!(2000.0), "fade.ini".into(), Some(2)));
    assert!(runtime.assign("bAllowLoadGrass", json!(true), "cache.ini".into(), Some(3)));
    assert_eq!(
        defaults.placement_settings_hash,
        runtime.placement_settings_hash
    );
    assert_ne!(
        defaults.runtime_settings_hash,
        runtime.runtime_settings_hash
    );
    assert_eq!(
        runtime.settings["bAllowLoadGrass"].category,
        Category::LegacyCache
    );
    assert_eq!(runtime.legacy_cache["bAllowLoadGrass"], true);
    let mut placement = defaults.clone();
    assert!(placement.assign("iMinGrassSize", json!(43), "density.ini".into(), Some(4)));
    assert_ne!(
        defaults.placement_settings_hash,
        placement.placement_settings_hash
    );
    assert_eq!(
        defaults.runtime_settings_hash,
        placement.runtime_settings_hash
    );
    assert!(placement.assign("iMinGrassSize", json!(20), "restored.ini".into(), Some(7)));
    assert_eq!(
        defaults.placement_settings_hash,
        placement.placement_settings_hash
    );
}

#[test]
/// Verify source digests track input bytes while effective hashes ignore formatting and paths.
fn source_byte_hashes_are_independent_of_effective_setting_hashes() {
    let a = Source::from_bytes("plain.ini".into(), b"[Grass]\niMinGrassSize=36\n");
    let b = Source::from_bytes(
        "commented.ini".into(),
        b"; comment\n[Grass]\niMinGrassSize = 36\n",
    );
    assert_ne!(a.sha256, b.sha256);
    assert_eq!(a.sha256.len(), 64);
    let mut first = GrassSettings::default().report();
    let mut second = first.clone();
    assert!(first.assign(
        "iMinGrassSize",
        json!(36),
        a.path.display().to_string(),
        Some(2)
    ));
    assert!(second.assign(
        "iMinGrassSize",
        json!(36),
        b.path.display().to_string(),
        Some(3)
    ));
    assert_eq!(
        first.placement_settings_hash,
        second.placement_settings_hash
    );
    assert_eq!(first.runtime_settings_hash, second.runtime_settings_hash);
}
