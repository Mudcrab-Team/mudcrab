//! Winning plugin layers retain original recovery candidates and installed-owner inventory.
use mo2::Instance;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Independently encode a complete TES4 header with a distinguishable full/light flag.
fn plugin_bytes(flags: u32) -> Vec<u8> {
    let hedr = [
        1.7f32.to_le_bytes().as_slice(),
        &0u32.to_le_bytes(),
        &0x800u32.to_le_bytes(),
    ]
    .concat();
    let payload = [
        b"HEDR".as_slice(),
        &(hedr.len() as u16).to_le_bytes(),
        &hedr,
    ]
    .concat();
    [
        b"TES4".as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &0u32.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}

/// Create a fixture file once, preserving actual distinct K and Kelvin-sign filenames.
fn write(root: &Path, name: &str, bytes: &[u8]) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

/// Compare actual source paths independently of Windows verbatim path prefixes.
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

/// A higher unique plugin replaces a lower ambiguity; disabling it restores both excluded originals.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "requires distinct K and Kelvin-sign filenames"
)]
fn winning_plugin_layer_controls_candidates_without_losing_inactive_owner_inventory() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let data = root.join("Data");
    let instance = root.join("MO2");
    let low = instance.join("mods/Low");
    let high = instance.join("mods/High");
    let profile = instance.join("profiles/Default");
    fs::create_dir_all(instance.join("overwrite")).unwrap();
    write(&data, "Skyrim.esm", &plugin_bytes(1));
    write(root, "Skyrim.ccc", b"");
    write(
        &instance,
        "ModOrganizer.ini",
        b"[General]\ngameName=Skyrim Special Edition\n",
    );
    write(&profile, "plugins.txt", b"*K.esp\n");
    write(&profile, "loadorder.txt", b"Skyrim.esm\nK.esp\n");
    write(&profile, "modlist.txt", b"+High\n+Low\n");
    let low_bytes = plugin_bytes(0);
    let kelvin_bytes = plugin_bytes(0x200);
    let high_bytes = plugin_bytes(1);
    write(&low, "K.esp", &low_bytes);
    write(&low, "\u{212A}.esp", &kelvin_bytes);
    write(&high, "K.esp", &high_bytes);
    write(&low, "notes/neighbor.bin", b"lower asset bytes");
    write(&high, "notes/neighbor.bin", b"higher asset bytes");
    assert_ne!(low_bytes, kelvin_bytes);
    assert_eq!(fs::read(low.join("K.esp")).unwrap(), low_bytes);
    assert_eq!(fs::read(low.join("\u{212A}.esp")).unwrap(), kelvin_bytes);
    let reader = Instance::open(&instance).unwrap();
    let resolved = reader.resolve(&data, "Default").unwrap();
    assert_eq!(resolved.plugins, ["skyrim.esm", "k.esp"]);
    assert_eq!(
        canonical(&resolved.files["k.esp"]),
        canonical(&high.join("K.esp"))
    );
    assert_eq!(fs::read(&resolved.files["k.esp"]).unwrap(), high_bytes);
    assert_eq!(
        fs::read(&resolved.files["notes/neighbor.bin"]).unwrap(),
        b"higher asset bytes"
    );
    assert_eq!(
        resolved
            .plugin_candidates
            .iter()
            .map(|path| canonical(path))
            .collect::<Vec<_>>(),
        [
            canonical(&data.join("Skyrim.esm")),
            canonical(&high.join("K.esp"))
        ]
    );
    assert!(
        !resolved
            .notices
            .iter()
            .any(|notice| notice.contains("installed plugin name collision for k.esp;"))
    );
    fs::write(profile.join("modlist.txt"), "-High\n+Low\n").unwrap();
    let recovered = reader.resolve(&data, "Default").unwrap();
    assert_eq!(recovered.plugins, ["skyrim.esm"]);
    assert_eq!(
        recovered
            .plugin_candidates
            .iter()
            .map(|path| canonical(path))
            .collect::<Vec<_>>(),
        [
            canonical(&data.join("Skyrim.esm")),
            canonical(&low.join("K.esp")),
            canonical(&low.join("\u{212A}.esp"))
        ]
    );
    assert_eq!(
        canonical(&recovered.files["k.esp"]),
        canonical(&low.join("K.esp"))
    );
    assert_eq!(fs::read(&recovered.files["k.esp"]).unwrap(), low_bytes);
    assert_eq!(
        fs::read(&recovered.files["notes/neighbor.bin"]).unwrap(),
        b"lower asset bytes"
    );
    assert_eq!(
        recovered
            .notices
            .iter()
            .filter(|notice| notice.contains("installed plugin name collision for k.esp;"))
            .count(),
        1
    );
    assert!(resolved.selection_warnings.is_empty() && recovered.selection_warnings.is_empty());
}
