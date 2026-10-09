use mo2::Instance;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Writes a text fixture relative to a root, creating its parent directories.
fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Creates an MO2 instance fixture with profiles, enabled and disabled mods, and ordered plugins.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("MO2");
    for folder in [
        "mods",
        "profiles/Default",
        "profiles/alpha",
        "overwrite",
        "Data",
    ] {
        fs::create_dir_all(root.join(folder)).unwrap();
    }
    write(
        &root,
        "ModOrganizer.ini",
        "[General]\ngameName=Skyrim Special Edition\n[Settings]\nmod_directory=%BASE_DIR%/mods\nprofiles_directory=profiles\noverwrite_directory=overwrite\n",
    );
    write(
        &root,
        "profiles/Default/modlist.txt",
        "# Highest first\n+High\n-LowDisabled\n*DLC: Dawnguard\n*Unmanaged: Bashed Patch, 0\n+Low\n",
    );
    write(
        &root,
        "profiles/Default/plugins.txt",
        "# Active\n*Patch.esp\n*Mod.esm\nInactive.esm\n",
    );
    write(
        &root,
        "profiles/Default/loadorder.txt",
        "Skyrim.esm\nMod.esm\nInactive.esm\nPatch.esp\n",
    );
    write(&root, "Data/Skyrim.esm", "official");
    write(&root, "mods/Low/Mod.esm", "master");
    write(&root, "mods/High/Patch.esp", "patch");
    write(&root, "mods/LowDisabled/Inactive.esm", "inactive");
    (dir, root)
}

/// Verifies mod and overwrite precedence, hidden-file exclusion, implicit masters, and active load order.
#[test]
fn resolves_priority_enabled_mods_hidden_files_and_active_order() {
    let (_dir, root) = fixture();
    // Bundled masters may be absent from MO2's plugins.txt even though they are active.
    write(&root, "Data/ccBGSSSE001-Fish.esm", "bundled creation");
    write(
        &root,
        "profiles/Default/loadorder.txt",
        "Skyrim.esm\nccBGSSSE001-Fish.esm\nMod.esm\nPatch.esp\n",
    );
    write(&root, "Data/Textures/priority.dds", "physical");
    write(&root, "mods/Low/textures/PRIORITY.dds", "low");
    write(&root, "mods/High/TEXTURES/priority.dds", "high");
    write(&root, "mods/Low/Textures/overwrite.dds", "low");
    write(&root, "overwrite/textures/Overwrite.dds", "overwrite");
    write(&root, "mods/High/textures/priority.dds.mohidden", "hidden");
    write(&root, "mods/High/secret.mohidden/ignore.nif", "hidden dir");
    write(&root, "mods/LowDisabled/textures/disabled.dds", "disabled");
    let instance = Instance::open(&root).unwrap();
    assert_eq!(instance.profiles, ["alpha", "Default"]);
    let resolved = instance.resolve(&root.join("Data"), "default").unwrap();
    assert_eq!(
        resolved.plugins,
        ["skyrim.esm", "ccbgssse001-fish.esm", "mod.esm", "patch.esp"]
    );
    assert_eq!(
        fs::read_to_string(&resolved.files["textures/priority.dds"]).unwrap(),
        "high"
    );
    assert_eq!(
        fs::read_to_string(&resolved.files["textures/overwrite.dds"]).unwrap(),
        "overwrite"
    );
    assert!(
        !resolved
            .files
            .keys()
            .any(|key| key.contains("hidden") || key.contains("disabled"))
    );
    // Removing an override exposes the next layer, without any stale resolver state.
    fs::remove_file(root.join("mods/High/TEXTURES/priority.dds")).unwrap();
    let resolved = instance.resolve(&root.join("Data"), "Default").unwrap();
    assert_eq!(
        fs::read_to_string(&resolved.files["textures/priority.dds"]).unwrap(),
        "low"
    );
}

#[test]
fn classic_skyrim_bare_plugins_are_active_with_and_without_loadorder() {
    for game in ["Skyrim", "skyrim"] {
        let (_dir, root) = fixture();
        write(
            &root,
            "ModOrganizer.ini",
            &format!("[General]\ngameName={game}\n"),
        );
        write(
            &root,
            "profiles/Default/plugins.txt",
            "# Active classic Skyrim plugins\nSkyrim.esm\nPatch.esp\nMod.esm\n",
        );
        write(&root, "Data/Patch.esp", "base loser");
        write(&root, "mods/Low/Patch.esp", "low loser");
        write(&root, "Data/Inactive.esm", "inactive");
        write(&root, "mods/High/textures/example.dds", "winning asset");
        let instance = Instance::open(&root).unwrap();
        for with_loadorder in [true, false] {
            if !with_loadorder {
                fs::remove_file(root.join("profiles/Default/loadorder.txt")).unwrap();
            }
            let resolved = instance.resolve(&root.join("Data"), "Default").unwrap();
            let expected = if with_loadorder {
                ["skyrim.esm", "mod.esm", "patch.esp"]
            } else {
                ["skyrim.esm", "patch.esp", "mod.esm"]
            };
            assert_eq!(
                resolved.plugins, expected,
                "{game}, loadorder={with_loadorder}"
            );
            assert_eq!(
                resolved.files["patch.esp"],
                fs::canonicalize(root.join("mods/High/Patch.esp")).unwrap()
            );
            assert_eq!(
                fs::read_to_string(&resolved.files["patch.esp"]).unwrap(),
                "patch"
            );
            assert_eq!(
                fs::read_to_string(&resolved.files["textures/example.dds"]).unwrap(),
                "winning asset"
            );
            assert!(resolved.files.contains_key("inactive.esm"));
        }
    }
}

#[test]
fn se_and_vr_bare_plugins_remain_inactive() {
    for game in ["Skyrim Special Edition", "Skyrim VR"] {
        let (_dir, root) = fixture();
        write(
            &root,
            "ModOrganizer.ini",
            &format!("[General]\ngameName={game}\n"),
        );
        write(
            &root,
            "profiles/Default/plugins.txt",
            "Skyrim.esm\nPatch.esp\n*Mod.esm\nInactive.esm\n",
        );
        write(&root, "Data/Inactive.esm", "inactive");
        write(&root, "mods/High/textures/example.dds", "winning asset");
        let instance = Instance::open(&root).unwrap();
        for with_loadorder in [true, false] {
            if !with_loadorder {
                fs::remove_file(root.join("profiles/Default/loadorder.txt")).unwrap();
            }
            let resolved = instance.resolve(&root.join("Data"), "Default").unwrap();
            assert_eq!(resolved.plugins, ["skyrim.esm", "mod.esm"], "{game}");
            assert_eq!(
                fs::read_to_string(&resolved.files["patch.esp"]).unwrap(),
                "patch"
            );
            assert_eq!(
                fs::read_to_string(&resolved.files["textures/example.dds"]).unwrap(),
                "winning asset"
            );
            assert!(resolved.files.contains_key("inactive.esm"));
        }
    }
}

#[test]
fn classic_skyrim_bare_plugins_preserve_validation() {
    let (_dir, root) = fixture();
    write(&root, "ModOrganizer.ini", "[General]\ngameName=Skyrim\n");
    let instance = Instance::open(&root).unwrap();
    for (plugins, order, error) in [
        (
            "Patch.esp\nPATCH.ESP\n",
            "Patch.esp\n",
            "duplicate active plugin",
        ),
        ("../escape.esp\n", "Patch.esp\n", "unsafe"),
        ("Missing.esp\n", "Missing.esp\n", "active plugin missing"),
        ("Patch.esp\n", "Mod.esm\n", "absent from loadorder"),
        (
            "Patch.esp\n",
            "Patch.esp\nPATCH.ESP\n",
            "duplicate loadorder plugin",
        ),
    ] {
        write(&root, "profiles/Default/plugins.txt", plugins);
        write(&root, "profiles/Default/loadorder.txt", order);
        let result = instance.resolve(&root.join("Data"), "Default").unwrap_err();
        assert!(format!("{result:#}").contains(error), "{result:#}");
    }
}

/// Verifies base-directory expansion, relative paths, case-insensitive lookup, and external mod paths.
#[test]
fn resolves_base_relative_and_external_configured_directories() {
    let (_dir, root) = fixture();
    fs::create_dir_all(root.join("Storage/Mods")).unwrap();
    fs::create_dir_all(root.join("Storage/Profiles/Zed")).unwrap();
    fs::create_dir_all(root.join("Storage/Overwrite")).unwrap();
    write(
        &root,
        "ModOrganizer.ini",
        "[General]\ngameName=Skyrim\nbase_directory=Storage\n[Settings]\nmod_directory=%BASE_DIR%/mods\nprofiles_directory=%BASE_DIR%\\profiles\noverwrite_directory=%BASE_DIR%/overwrite\n",
    );
    let instance = Instance::open(&root).unwrap();
    assert_eq!(instance.profiles, ["Zed"]);
    assert_eq!(
        instance.mods_dir,
        fs::canonicalize(root.join("Storage/Mods")).unwrap()
    );
    let external = root.parent().unwrap().join("External Mods");
    fs::create_dir(&external).unwrap();
    write(
        &root,
        "ModOrganizer.ini",
        &format!(
            "[Settings]\ngameName=Skyrim\nmod_directory=\"{}\"\nprofiles_directory=profiles\noverwrite_directory=overwrite\n",
            external.display()
        ),
    );
    assert_eq!(
        Instance::open(&root).unwrap().mods_dir,
        fs::canonicalize(external).unwrap()
    );
}

#[test]
fn validates_game_name_and_selected_profile() {
    let (_dir, root) = fixture();
    for game in ["Skyrim", "Skyrim Special Edition", "Skyrim VR", "skyrim"] {
        write(
            &root,
            "ModOrganizer.ini",
            &format!("[General]\ngameName={game}\nselected_profile=default\n"),
        );
        let instance = Instance::open(&root).unwrap();
        assert_eq!(instance.profiles[0], "alpha");
        assert_eq!(instance.selected_profile.as_deref(), Some("Default"));
    }
    for game in ["Fallout 4", "Oblivion", "Not Skyrim", ""] {
        write(
            &root,
            "ModOrganizer.ini",
            &format!("[General]\ngameName={game}\n"),
        );
        assert!(
            format!("{:#}", Instance::open(&root).unwrap_err())
                .contains("unsupported MO2 gameName")
        );
    }
    for ini in [
        "[General]\nselected_profile=Default\n",
        "[Settings]\nmod_directory=mods\n",
    ] {
        write(&root, "ModOrganizer.ini", ini);
        assert!(format!("{:#}", Instance::open(&root).unwrap_err()).contains("missing gameName"));
    }
    for settings in [
        "selected_profile=missing\n",
        "selected_profile=../Default\n",
        "base_directory=.\n",
    ] {
        write(
            &root,
            "ModOrganizer.ini",
            &format!("[Settings]\ngameName=Skyrim\n{settings}"),
        );
        assert!(Instance::open(&root).unwrap().selected_profile.is_none());
    }
}

/// Qt byte arrays select the discovered spelling after decoding UTF-8 bytes and escapes.
#[test]
fn selects_qt_byte_array_profile() {
    let (_dir, root) = fixture();
    fs::create_dir(root.join("profiles/Café's profile")).unwrap();
    for (encoded, expected) in [
        ("@ByteArray(default)", "Default"),
        (r"@ByteArray(\x64\x65\x66\x61ult)", "Default"),
        (r"@ByteArray(CAF\xc3\xa9\'s profile)", "Café's profile"),
    ] {
        write(
            &root,
            "ModOrganizer.ini",
            &format!("[General]\ngameName=Skyrim\nselected_profile=\"{encoded}\"\n"),
        );
        assert_eq!(
            Instance::open(&root).unwrap().selected_profile.as_deref(),
            Some(expected),
            "{encoded}"
        );
    }
    for encoded in [r"@ByteArray(\xff)", r"@ByteArray(\x)", "@ByteArray(Default"] {
        write(
            &root,
            "ModOrganizer.ini",
            &format!("[General]\ngameName=Skyrim\nselected_profile={encoded}\n"),
        );
        assert!(Instance::open(&root).unwrap().selected_profile.is_none());
    }
}

#[test]
fn resolves_parent_directory_components() {
    let (_dir, root) = fixture();
    write(
        &root,
        "ModOrganizer.ini",
        "[Settings]\ngameName=Skyrim\nmod_directory=profiles/../mods\n",
    );
    assert_eq!(
        Instance::open(&root).unwrap().mods_dir,
        fs::canonicalize(root.join("mods")).unwrap()
    );
}

#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link.to_string_lossy().replace('/', "\\"))
        .arg(target.to_string_lossy().replace('/', "\\"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
#[test]
fn rejects_enabled_mod_profile_and_asset_junctions() {
    let (dir, root) = fixture();
    let target = dir.path().join("external");
    fs::create_dir(&target).unwrap();
    write(&target, "escape.dds", "outside source roots");
    let instance = Instance::open(&root).unwrap();
    // A valid enabled mod must not silently traverse (or skip) its junction target.
    let link = root.join("mods/Escape");
    junction(&link, &target);
    write(&root, "profiles/Default/modlist.txt", "+Escape\n");
    let result = instance.resolve(&root.join("Data"), "Default");
    fs::remove_dir(&link).unwrap();
    assert!(format!("{:#}", result.unwrap_err()).contains("reparse points"));
    write(&root, "profiles/Default/modlist.txt", "+High\n+Low\n");

    for parent in ["mods/High", "overwrite", "Data"] {
        let link = root.join(parent).join("textures");
        junction(&link, &target);
        let result = instance.resolve(&root.join("Data"), "Default");
        fs::remove_dir(&link).unwrap();
        assert!(
            format!("{:#}", result.unwrap_err()).contains("reparse points"),
            "{parent}"
        );
    }
    // Replace a previously discovered profile so selection cannot follow a junction.
    let profile = root.join("profiles/Default");
    fs::remove_dir_all(&profile).unwrap();
    junction(&profile, &target);
    let result = instance.profile_dir("Default");
    fs::remove_dir(&profile).unwrap();
    assert!(format!("{:#}", result.unwrap_err()).contains("reparse points"));
}

#[cfg(windows)]
#[test]
fn resolves_parent_of_junction_target_not_lexical_parent() {
    let (dir, root) = fixture();
    let target = dir.path().join("external/target");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(dir.path().join("external/mods")).unwrap();
    let junction = root.join("junction");
    self::junction(&junction, &target);
    write(
        &root,
        "ModOrganizer.ini",
        "[Settings]\ngameName=Skyrim\nmod_directory=junction/../mods\n",
    );
    let result = Instance::open(&root);
    fs::remove_dir(&junction).unwrap();
    assert_eq!(
        result.unwrap().mods_dir,
        fs::canonicalize(dir.path().join("external/mods")).unwrap()
    );
}

/// Verifies opening an instance fails when its configuration or a configured directory is missing.
#[test]
fn validates_instance_and_configured_directories() {
    let (_dir, root) = fixture();
    fs::remove_file(root.join("ModOrganizer.ini")).unwrap();
    assert!(Instance::open(&root).is_err());
    write(
        &root,
        "ModOrganizer.ini",
        "[Settings]\ngameName=Skyrim\nmod_directory=missing\n",
    );
    assert!(Instance::open(&root).is_err());
}

/// Verifies resolution rejects missing sources, unsafe names, and incomplete plugin load order.
#[test]
fn rejects_missing_mods_plugins_unsafe_paths_and_incomplete_order() {
    let (_dir, root) = fixture();
    let instance = Instance::open(&root).unwrap();
    write(&root, "profiles/Default/modlist.txt", "+Missing\n");
    let error = instance.resolve(&root.join("Data"), "Default").unwrap_err();
    assert!(format!("{error:#}").contains("enabled mod missing: Missing"));
    write(&root, "profiles/Default/modlist.txt", "+../High\n");
    assert!(instance.resolve(&root.join("Data"), "Default").is_err());
    write(&root, "profiles/Default/modlist.txt", "+High\n+Low\n");
    fs::remove_file(root.join("mods/High/Patch.esp")).unwrap();
    assert!(
        format!(
            "{:#}",
            instance.resolve(&root.join("Data"), "Default").unwrap_err()
        )
        .contains("active plugin missing")
    );
    write(&root, "mods/High/Patch.esp", "patch");
    write(&root, "profiles/Default/loadorder.txt", "Mod.esm\n");
    assert!(
        format!(
            "{:#}",
            instance.resolve(&root.join("Data"), "Default").unwrap_err()
        )
        .contains("absent from loadorder")
    );
    write(&root, "profiles/Default/plugins.txt", "*../escape.esp\n");
    assert!(instance.resolve(&root.join("Data"), "Default").is_err());
    assert!(instance.profile_dir("../Default").is_err());
}

/// Verifies cancellation aborts resolution and absent loadorder.txt falls back to active plugin order.
#[test]
fn cancellation_and_optional_loadorder() {
    let (_dir, root) = fixture();
    let instance = Instance::open(&root).unwrap();
    assert!(
        instance
            .resolve_with_cancel(&root.join("Data"), "Default", &|| true)
            .is_err()
    );
    fs::remove_file(root.join("profiles/Default/loadorder.txt")).unwrap();
    assert_eq!(
        instance
            .resolve(&root.join("Data"), "Default")
            .unwrap()
            .plugins,
        ["skyrim.esm", "patch.esp", "mod.esm"]
    );
}
