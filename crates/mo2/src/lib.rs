//! Read-only Mod Organizer 2 instance and profile resolution.
//! Resolves data files, not the MO2 process environment or native SKSE extensions.
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selection {
    pub instance_path: PathBuf,
    pub profile: String,
}

#[derive(Debug, Clone)]
pub struct Instance {
    pub profiles: Vec<String>,
    /// The INI's active profile, matched to its discovered name; absent or stale values are `None`.
    pub selected_profile: Option<String>,
    pub instance_path: PathBuf,
    pub mods_dir: PathBuf,
    pub profiles_dir: PathBuf,
    pub overwrite_dir: PathBuf,
    // Classic Skyrim lists active plugins as bare names; SE/VR require stars.
    classic_skyrim: bool,
}

/// Winning source paths indexed by lowercase Data-relative paths, and active plugin order.
#[derive(Debug)]
pub struct Resolved {
    pub files: BTreeMap<String, PathBuf>,
    pub plugins: Vec<String>,
}

pub const OFFICIAL_MASTERS: &[&str] = &[
    "skyrim.esm",
    "update.esm",
    "dawnguard.esm",
    "hearthfires.esm",
    "dragonborn.esm",
    // Bundled creations and the resource pack are implicitly enabled by Skyrim SE,
    // so MO2 can omit them from plugins.txt while retaining them in loadorder.txt.
    "ccbgssse001-fish.esm",
    "ccqdrsse001-survivalmode.esl",
    "ccbgssse037-curios.esl",
    "ccbgssse025-advdsgs.esm",
    "_resourcepack.esl",
];

impl Instance {
    /// Opens an MO2 instance, resolves configured directories, and discovers sorted profile names.
    ///
    /// Fails on unreadable or invalid configuration, missing directories, unsafe names, or no profiles.
    pub fn open(path: &Path) -> Result<Self> {
        let instance_path = fs::canonicalize(path)
            .wrap_err_with(|| format!("MO2 instance not found: {}", path.display()))?;
        ensure!(
            instance_path.is_dir(),
            "MO2 instance is not a directory: {}",
            path.display()
        );
        let ini = child(&instance_path, "ModOrganizer.ini")?;
        let contents = fs::read_to_string(&ini).wrap_err("reading MO2 ModOrganizer.ini")?;
        let mut settings = BTreeMap::new();
        let mut section = String::new();
        for line in lines(&contents) {
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].to_ascii_lowercase();
            } else if let Some((key, value)) = line.split_once('=') {
                settings.insert(
                    (section.clone(), key.trim().to_ascii_lowercase()),
                    value.trim().trim_matches('"').to_owned(),
                );
            }
        }
        ensure!(
            settings
                .keys()
                .any(|(section, _)| section == "general" || section == "settings"),
            "{} is not an MO2 instance configuration",
            ini.display()
        );
        let value = |key: &str| {
            settings
                .get(&("settings".into(), key.into()))
                .or_else(|| settings.get(&("general".into(), key.into())))
        };
        let game = value("gamename")
            .ok_or_else(|| color_eyre::eyre::eyre!("MO2 configuration is missing gameName"))?;
        ensure!(
            ["Skyrim", "Skyrim Special Edition", "Skyrim VR"]
                .iter()
                .any(|supported| game.eq_ignore_ascii_case(supported)),
            "unsupported MO2 gameName: {game}"
        );
        let base = match value("base_directory").filter(|v| !v.is_empty()) {
            Some(v) => configured_dir(&instance_path, &instance_path, v)?,
            None => instance_path.clone(),
        };
        let dir = |key: &str, default: &str| {
            configured_dir(
                &instance_path,
                &base,
                value(key)
                    .filter(|v| !v.is_empty())
                    .map(String::as_str)
                    .unwrap_or(default),
            )
        };
        let mods_dir = dir("mod_directory", "%BASE_DIR%/mods")?;
        let profiles_dir = dir("profiles_directory", "%BASE_DIR%/profiles")?;
        let overwrite_dir = dir("overwrite_directory", "%BASE_DIR%/overwrite")?;
        let mut profiles = Vec::new();
        for entry in fs::read_dir(&profiles_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| color_eyre::eyre::eyre!("MO2 profile name is not UTF-8"))?;
                safe_name(&name)?;
                profiles.push(name);
            }
        }
        profiles.sort_by_key(|name| (name.to_ascii_lowercase(), name.clone()));
        ensure!(
            !profiles.is_empty(),
            "MO2 instance has no profiles: {}",
            profiles_dir.display()
        );
        let selected_profile = value("selected_profile").and_then(|selected| {
            let selected = decode_selected_profile(selected)?;
            profiles
                .iter()
                .find(|name| name.eq_ignore_ascii_case(&selected))
                .cloned()
        });
        Ok(Self {
            profiles,
            selected_profile,
            instance_path,
            mods_dir,
            profiles_dir,
            overwrite_dir,
            classic_skyrim: game.eq_ignore_ascii_case("Skyrim"),
        })
    }

    /// Finds a profile directory by case-insensitive name, rejecting unsafe or missing selections.
    pub fn profile_dir(&self, profile: &str) -> Result<PathBuf> {
        safe_name(profile)?;
        let name = self
            .profiles
            .iter()
            .find(|name| name.eq_ignore_ascii_case(profile))
            .ok_or_else(|| color_eyre::eyre::eyre!("MO2 profile not found: {profile}"))?;
        let path = child(&self.profiles_dir, name)?;
        ensure!(
            path.is_dir(),
            "MO2 profile is not a directory: {}",
            path.display()
        );
        Ok(path)
    }

    /// Instance/profile files that determine selection, for converter cache provenance.
    pub fn configuration_files(&self, profile: &str) -> Result<Vec<PathBuf>> {
        let profile = self.profile_dir(profile)?;
        let mut paths = vec![
            child(&self.instance_path, "ModOrganizer.ini")?,
            child(&profile, "modlist.txt")?,
            child(&profile, "plugins.txt")?,
        ];
        if let Some(path) = optional_child(&profile, "loadorder.txt")? {
            paths.push(path);
        }
        Ok(paths)
    }

    /// Roots that must never overlap any converter write destination.
    pub fn source_roots(&self) -> [&Path; 4] {
        [
            &self.instance_path,
            &self.mods_dir,
            &self.profiles_dir,
            &self.overwrite_dir,
        ]
    }

    /// Resolves winning files and plugin order for a profile without cancellation.
    ///
    /// Enabled mods overlay the base Data directory in priority order; overwrite files take precedence.
    pub fn resolve(&self, data: &Path, profile: &str) -> Result<Resolved> {
        self.resolve_with_cancel(data, profile, &|| false)
    }

    /// Cancellation is checked during traversal. No files are written by the resolver.
    pub fn resolve_with_cancel(
        &self,
        data: &Path,
        profile: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Resolved> {
        let profile_dir = self.profile_dir(profile)?;
        let modlist = fs::read_to_string(child(&profile_dir, "modlist.txt")?)?;
        let mut mods = Vec::new();
        let mut seen = BTreeSet::new();
        for line in lines(&modlist) {
            let (enabled, name) = match line.as_bytes().first() {
                Some(b'+') => (true, &line[1..]),
                Some(b'-' | b'*') => (false, &line[1..]),
                _ => bail!("MO2 profile {profile}: malformed modlist entry {line}"),
            };
            if !enabled {
                continue;
            }
            safe_name(name)?;
            ensure!(
                seen.insert(name.to_ascii_lowercase()),
                "MO2 profile {profile}: duplicate enabled mod {name}"
            );
            let path = child(&self.mods_dir, name)
                .wrap_err_with(|| format!("MO2 profile {profile}: enabled mod missing: {name}"))?;
            ensure!(
                path.is_dir(),
                "MO2 profile {profile}: enabled mod is not a directory: {name}"
            );
            mods.push(path);
        }
        let mut files = BTreeMap::new();
        overlay(data, &mut files, cancelled)?;
        // MO2 stores highest priority first; apply low priority first so the winner replaces it.
        for root in mods.iter().rev() {
            overlay(root, &mut files, cancelled)?;
        }
        overlay(&self.overwrite_dir, &mut files, cancelled)?;
        let active_text = fs::read_to_string(child(&profile_dir, "plugins.txt")?)?;
        let mut active = BTreeSet::new();
        let mut active_order = Vec::new();
        for line in lines(&active_text) {
            let enabled = self.classic_skyrim || line.starts_with('*');
            let name = line.strip_prefix('*').unwrap_or(line).trim();
            plugin_name(name)?;
            if enabled {
                let name = name.to_ascii_lowercase();
                ensure!(
                    active.insert(name.clone()),
                    "MO2 profile {profile}: duplicate active plugin {name}"
                );
                active_order.push(name);
            }
        }
        let mut plugins = Vec::new();
        for &master in OFFICIAL_MASTERS {
            if files.contains_key(master) {
                plugins.push(master.to_owned());
            }
        }
        let order_path = optional_child(&profile_dir, "loadorder.txt")?;
        let order = match order_path {
            Some(path) => fs::read_to_string(path)?,
            None => active_order.join("\n"),
        };
        let mut ordered = BTreeSet::new();
        for name in lines(&order) {
            plugin_name(name)?;
            let name = name.to_ascii_lowercase();
            ensure!(
                ordered.insert(name.clone()),
                "MO2 profile {profile}: duplicate loadorder plugin {name}"
            );
            if active.contains(&name) && !plugins.contains(&name) {
                plugins.push(name);
            }
        }
        for name in active_order {
            ensure!(
                plugins.contains(&name),
                "MO2 profile {profile}: active plugin absent from loadorder.txt: {name}"
            );
        }
        for name in &plugins {
            ensure!(
                files.contains_key(name),
                "MO2 profile {profile}: active plugin missing from merged Data: {name}"
            );
        }
        Ok(Resolved { files, plugins })
    }
}

/// Decodes MO2's UTF-8 profile name stored as a Qt INI byte array.
/// Plain names remain supported; malformed byte arrays do not select a profile.
fn decode_selected_profile(value: &str) -> Option<String> {
    let Some(encoded) = value.strip_prefix("@ByteArray(") else {
        return Some(value.to_owned());
    };
    let mut bytes = encoded.strip_suffix(')')?.bytes().peekable();
    let mut decoded = Vec::new();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            decoded.push(byte);
            continue;
        }
        decoded.push(match bytes.next()? {
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 11,
            b'0' => 0,
            byte @ (b'\\' | b'"' | b'\'' | b'?') => byte,
            b'x' => {
                let mut value = 0u8;
                let mut digits = 0;
                // Qt hex escapes consume all consecutive hexadecimal digits.
                while let Some(digit) = bytes.peek().and_then(|b| char::from(*b).to_digit(16)) {
                    value = value.checked_mul(16)?.checked_add(digit as u8)?;
                    bytes.next();
                    digits += 1;
                }
                if digits == 0 {
                    return None;
                }
                value
            }
            _ => return None,
        });
    }
    String::from_utf8(decoded).ok()
}

/// Iterates trimmed configuration lines, ignoring an initial BOM, blank lines, and comments.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with(';'))
}

/// Rejects unsafe path components, including traversal, Windows device names, and reserved characters.
fn safe_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name.ends_with(['.', ' '])
            && !name
                .chars()
                .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c)),
        "unsafe MO2 path component: {name:?}"
    );
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let numbered_device = stem
        .strip_prefix("com")
        .or_else(|| stem.strip_prefix("lpt"))
        .is_some_and(|number| {
            matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        });
    ensure!(
        !["con", "prn", "aux", "nul"].contains(&stem.as_str()) && !numbered_device,
        "unsafe Windows device path component: {name:?}"
    );
    Ok(())
}

/// Validates a safe plugin filename with an ESM, ESP, or ESL extension.
fn plugin_name(name: &str) -> Result<()> {
    safe_name(name)?;
    ensure!(
        Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["esm", "esp", "esl"]
                .iter()
                .any(|v| e.eq_ignore_ascii_case(v))),
        "invalid MO2 plugin name: {name}"
    );
    Ok(())
}

/// Rejects source links, including Windows junctions and other reparse points.
fn reject_source_link(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    let is_link = {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT also covers junctions, not just symbolic links.
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let is_link = metadata.file_type().is_symlink();
    ensure!(
        !is_link,
        "MO2 source symlinks and reparse points are unsupported: {}",
        path.display()
    );
    Ok(())
}

/// Finds an optional child by case-insensitive name, rejecting unsafe names, collisions, and links.
fn optional_child(root: &Path, name: &str) -> Result<Option<PathBuf>> {
    find_child(root, name, false)
}

// Configured directory paths may traverse junctions; profile and asset children may not.
fn find_child(root: &Path, name: &str, allow_links: bool) -> Result<Option<PathBuf>> {
    safe_name(name)?;
    let mut found = None;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
        {
            ensure!(
                found.is_none(),
                "case-insensitive path collision in {}: {name}",
                root.display()
            );
            if !allow_links {
                reject_source_link(&entry.path())?;
            }
            found = Some(entry.path());
        }
    }
    Ok(found)
}

/// Finds a required child by case-insensitive name, failing if it is missing, ambiguous, or a link.
fn child(root: &Path, name: &str) -> Result<PathBuf> {
    optional_child(root, name)?
        .ok_or_else(|| color_eyre::eyre::eyre!("MO2 path not found: {}", root.join(name).display()))
}

/// Expands an MO2 directory setting and resolves existing components case-insensitively.
///
/// Relative paths use the instance directory; only the `%BASE_DIR%` variable is supported.
fn configured_dir(instance: &Path, base: &Path, value: &str) -> Result<PathBuf> {
    let expanded = value
        .replace("%BASE_DIR%", &base.to_string_lossy())
        .replace('\\', "/");
    ensure!(
        !expanded.contains('%'),
        "unsupported MO2 path variable: {value}"
    );
    let path = PathBuf::from(expanded);
    // Joining onto a Windows verbatim path normalizes `..` before junctions can be resolved.
    let prefix = if path.is_absolute() {
        None
    } else {
        Some(instance)
    };
    // Resolve each existing component case-insensitively even on case-sensitive hosts.
    let mut resolved = PathBuf::new();
    for component in prefix
        .into_iter()
        .flat_map(Path::components)
        .chain(path.components())
    {
        match component {
            Component::Normal(name) => {
                let name = name
                    .to_str()
                    .ok_or_else(|| color_eyre::eyre::eyre!("MO2 directory is not UTF-8"))?;
                resolved = find_child(&resolved, name, true)?.ok_or_else(|| {
                    color_eyre::eyre::eyre!("MO2 path not found: {}", resolved.join(name).display())
                })?;
            }
            Component::ParentDir => {
                // Resolve junctions before taking their parent, rather than the lexical parent.
                resolved = fs::canonicalize(&resolved)?;
                resolved.pop();
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    ensure!(
        resolved.is_dir(),
        "MO2 configured directory does not exist: {}",
        path.display()
    );
    Ok(fs::canonicalize(resolved)?)
}

/// Overlays a directory onto winning file paths, skipping `.mohidden` entries and checking cancellation.
///
/// Rejects symlinks/reparse points, unsafe components, and case-insensitive collisions within the layer.
fn overlay(
    root: &Path,
    files: &mut BTreeMap<String, PathBuf>,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    ensure!(
        root.is_dir(),
        "MO2 Data source is not a directory: {}",
        root.display()
    );
    reject_source_link(root)?;
    let mut layer = BTreeMap::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".mohidden")
        })
    {
        ensure!(!cancelled(), "MO2 resolution cancelled");
        let entry = entry.wrap_err_with(|| format!("reading MO2 source {}", root.display()))?;
        reject_source_link(entry.path())?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(root)?;
        for part in relative.components() {
            safe_name(
                part.as_os_str()
                    .to_str()
                    .ok_or_else(|| color_eyre::eyre::eyre!("MO2 asset path is not UTF-8"))?,
            )?;
        }
        let key = relative
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        ensure!(
            layer.insert(key.clone(), entry.into_path()).is_none(),
            "case-insensitive asset collision in {}: {key}",
            root.display()
        );
    }
    files.extend(layer);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::decode_selected_profile;

    #[test]
    fn decodes_qt_backslash_escapes_without_changing_plain_names() {
        assert_eq!(
            decode_selected_profile(r#"@ByteArray(a\\b\"c\t\n)"#).as_deref(),
            Some("a\\b\"c\t\n")
        );
        assert_eq!(decode_selected_profile(r"a\b").as_deref(), Some(r"a\b"));
        assert!(decode_selected_profile(r"@ByteArray(trailing\)").is_none());
    }
}
