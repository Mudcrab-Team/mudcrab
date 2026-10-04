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
        "[General]\nbase_directory=Storage\n[Settings]\nmod_directory=%BASE_DIR%/mods\nprofiles_directory=%BASE_DIR%\\profiles\noverwrite_directory=%BASE_DIR%/overwrite\n",
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
            "[Settings]\nmod_directory=\"{}\"\nprofiles_directory=profiles\noverwrite_directory=overwrite\n",
            external.display()
        ),
    );
    assert_eq!(
        Instance::open(&root).unwrap().mods_dir,
        fs::canonicalize(external).unwrap()
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
        "[Settings]\nmod_directory=missing\n",
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
