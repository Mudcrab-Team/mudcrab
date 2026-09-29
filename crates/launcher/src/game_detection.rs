//! Finding the Skyrim `Data` folder: at start-up, when Detect is pressed, and when a folder is
//! dropped onto the launcher.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const SKYRIM_STEAM_APP_ID: &str = "489830";
const SKYRIM_DEFAULT_INSTALL_DIR: &str = "Skyrim Special Edition";

pub fn find_skyrim_data_dir() -> Option<PathBuf> {
    local_candidates()
        .into_iter()
        .chain(steam_library_paths())
        .find_map(find_skyrim_in_root)
}

fn local_candidates() -> [PathBuf; 2] {
    [PathBuf::from("skyrim_game"), PathBuf::from("game_data")]
}

fn find_skyrim_in_root(root: PathBuf) -> Option<PathBuf> {
    let direct_data = root.join("Data");
    if is_skyrim_data_dir(&direct_data) {
        return Some(direct_data);
    }

    let steamapps = root.join("steamapps");
    let manifest = steamapps.join(format!("appmanifest_{SKYRIM_STEAM_APP_ID}.acf"));
    let install_dir = fs::read_to_string(manifest)
        .ok()
        .and_then(|contents| vdf_value(&contents, "installdir"))
        .unwrap_or_else(|| SKYRIM_DEFAULT_INSTALL_DIR.to_owned());
    let data_dir = steamapps.join("common").join(install_dir).join("Data");

    is_skyrim_data_dir(&data_dir).then_some(data_dir)
}

/// Checks whether a given directory is a valid Skyrim `Data` folder by searching
/// for `Skyrim.esm` case-insensitively on both Windows and POSIX file systems.
pub(crate) fn is_skyrim_data_dir(data_dir: &Path) -> bool {
    data_dir.join("Skyrim.esm").is_file()
        || data_dir.join("skyrim.esm").is_file()
        || fs::read_dir(data_dir)
            .ok()
            .map(|mut entries| {
                entries.any(|entry| {
                    entry.ok().is_some_and(|e| {
                        e.path().is_file()
                            && e.file_name()
                                .to_string_lossy()
                                .eq_ignore_ascii_case("skyrim.esm")
                    })
                })
            })
            .unwrap_or(false)
}

/// The `Data` folder a dropped path means, if it means one at all: the path itself when it is a
/// `Data` folder, or its own `Data` subfolder when a Skyrim installation root was dropped. `None`
/// means the folder is not a Skyrim installation and belongs in the Output row.
pub(crate) fn data_dir_from_drop(path: &Path) -> Option<PathBuf> {
    if is_skyrim_data_dir(path) {
        return Some(path.to_owned());
    }
    let nested = path.join("Data");
    is_skyrim_data_dir(&nested).then_some(nested)
}

fn steam_library_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for steam_root in steam_install_paths() {
        paths.push(steam_root.clone());

        let library_file = steam_root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(contents) = fs::read_to_string(library_file) {
            paths.extend(vdf_values(&contents, "path").into_iter().map(PathBuf::from));
        }
    }

    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

#[cfg(windows)]
fn steam_install_paths() -> Vec<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let candidates = [
        (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Valve\Steam",
            "InstallPath",
        ),
    ];

    candidates
        .into_iter()
        .filter_map(|(hive, key_path, value_name)| {
            RegKey::predef(hive)
                .open_subkey(key_path)
                .ok()?
                .get_value::<String, _>(value_name)
                .ok()
        })
        .map(PathBuf::from)
        .collect()
}

/// Discovers Steam installation directories on non-Windows platforms (macOS, native Linux,
/// Steam Deck, Flatpak, and Snap distributions).
#[cfg(not(windows))]
fn steam_install_paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates = Vec::new();
    if let Some(home) = home {
        // Linux native, Flatpak & Snap Steam locations
        candidates.push(home.join(".local/share/Steam"));
        candidates.push(home.join(".steam/steam"));
        candidates.push(home.join(".steam/root"));
        candidates.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        candidates.push(home.join(".var/app/com.valvesoftware.Steam/.steam/steam"));
        candidates.push(home.join(".var/app/com.valvesoftware.Steam/data/Steam"));
        candidates.push(home.join("snap/steam/common/.local/share/Steam"));
        candidates.push(home.join("snap/steam/common/.steam/steam"));

        // macOS standard Steam location
        candidates.push(home.join("Library/Application Support/Steam"));
    }
    candidates
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

fn vdf_value(contents: &str, key: &str) -> Option<String> {
    vdf_values(contents, key).into_iter().next()
}

fn vdf_values(contents: &str, key: &str) -> Vec<String> {
    let tokens = quoted_vdf_tokens(contents);
    tokens
        .windows(2)
        .filter(|pair| pair[0].eq_ignore_ascii_case(key))
        .map(|pair| pair[1].clone())
        .collect()
}

fn quoted_vdf_tokens(contents: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = contents.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '"' {
            continue;
        }

        let mut token = String::new();
        while let Some(ch) = chars.next() {
            match ch {
                '"' => break,
                '\\' => match chars.peek().copied() {
                    Some('"') | Some('\\') => token.push(chars.next().unwrap()),
                    _ => token.push('\\'),
                },
                _ => token.push(ch),
            }
        }
        tokens.push(token);
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn reads_all_library_paths_from_modern_steam_vdf() {
        let contents = r#"
            "libraryfolders"
            {
                "0" { "path" "C:\\Program Files (x86)\\Steam" }
                "1" { "path" "D:\\SteamLibrary" }
            }
        "#;

        assert_eq!(
            vdf_values(contents, "path"),
            [r"C:\Program Files (x86)\Steam", r"D:\SteamLibrary"]
        );
    }

    #[test]
    fn uses_manifest_install_directory() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("mudcrab-steam-detection-{unique}"));
        let steamapps = root.join("steamapps");
        let data_dir = steamapps.join("common").join("Skyrim SSE").join("Data");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join("Skyrim.esm"), []).unwrap();
        fs::write(
            steamapps.join("appmanifest_489830.acf"),
            r#""AppState" { "appid" "489830" "installdir" "Skyrim SSE" }"#,
        )
        .unwrap();

        assert_eq!(find_skyrim_in_root(root.clone()), Some(data_dir));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_case_insensitive_skyrim_esm() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("mudcrab-case-detection-{unique}"));
        let data_dir = root.join("Data");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join("skyrim.esm"), []).unwrap();
        assert!(is_skyrim_data_dir(&data_dir));
        fs::remove_dir_all(root).unwrap();
    }

    /// A dropped folder means the Data row when it is a Skyrim `Data` folder or an installation
    /// root, and the Output row (which the window reads as `None`) otherwise.
    #[test]
    fn a_dropped_folder_is_a_data_folder_only_when_it_holds_skyrim_esm() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("mudcrab-drop-detection-{unique}"));
        let install = root.join("Skyrim Special Edition");
        let data = install.join("Data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("Skyrim.esm"), []).unwrap();
        let elsewhere = root.join("somewhere else");
        fs::create_dir_all(&elsewhere).unwrap();

        assert_eq!(data_dir_from_drop(&data), Some(data.clone()));
        assert_eq!(data_dir_from_drop(&install), Some(data.clone()));
        assert_eq!(data_dir_from_drop(&elsewhere), None);

        fs::remove_dir_all(root).unwrap();
    }
}
