//! Skyrim SE plugin selection shared with converter and mod-manager readers.
use color_eyre::{Result, eyre::WrapErr};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

/// Installed official masters are always active in this conventional order.
pub const OFFICIAL_MASTERS: [&str; 5] = [
    "Skyrim.esm",
    "Update.esm",
    "Dawnguard.esm",
    "HearthFires.esm",
    "Dragonborn.esm",
];

/// Use one Unicode lowercase identity for installed, selected and master plugin names.
pub fn normalize_plugin_name(name: &str) -> String {
    name.to_lowercase()
}

/// Decode Windows-1252 without discarding accented plugin or master names.
pub fn decode_windows_1252(bytes: &[u8]) -> String {
    const HIGH: [char; 32] = [
        '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}',
        '\u{8f}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}',
        '\u{2014}', '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}',
        '\u{178}',
    ];
    bytes
        .iter()
        .map(|&byte| match byte {
            0x80..=0x9f => HIGH[(byte - 0x80) as usize],
            _ => char::from(byte),
        })
        .collect()
}

/// Recognize plugin filenames directly inside Data, excluding path components.
fn plugin_name(name: &str) -> bool {
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
    !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|character| character.is_control() || "\\/:*?\"<>|".contains(character))
        && !["con", "prn", "aux", "nul"].contains(&stem.as_str())
        && !numbered_device
        && matches!(
            Path::new(name)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("esm" | "esp" | "esl")
        )
}

/// Ignore comments and blank lines without interpreting checkbox state.
fn lines(contents: &str) -> impl Iterator<Item = &str> {
    contents
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(['#', ';']))
}

/// Count repeated diagnostics and emit one line for each distinct problem.
#[derive(Default)]
pub struct Warnings(BTreeMap<String, usize>);

impl Warnings {
    /// Accumulate repeated occurrences without producing unbounded log output.
    pub fn add(&mut self, message: String) {
        *self
            .0
            .entry(message.replace(['\r', '\n'], " "))
            .or_default() += 1;
    }

    /// Forward warnings through the caller's notice channel.
    pub fn finish(self, notices: &mut Vec<String>) {
        for (message, count) in self.0 {
            notices.push(if count == 1 {
                message
            } else {
                format!("{message} ({count} occurrences)")
            });
        }
    }
}

/// A valid player-list entry, with its original name and Unicode lowercase plugin key.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub name: String,
    pub key: String,
    pub checked: bool,
}

/// Parse valid names once, retaining each duplicate's last position and activation state.
fn parse_entries(
    text: &str,
    classic: bool,
    source: &str,
    checkbox: bool,
    warnings: &mut Warnings,
) -> Vec<PluginEntry> {
    let mut entries = Vec::new();
    let mut last = HashMap::new();
    for line in lines(text) {
        let checked = classic || (checkbox && line.starts_with('*'));
        let name = if checkbox {
            line.strip_prefix('*').unwrap_or(line).trim()
        } else {
            line
        };
        if !plugin_name(name) {
            warnings.add(format!("invalid {source} plugin entry {name:?}; skipping"));
            continue;
        }
        let key = normalize_plugin_name(name);
        if last.insert(key.clone(), entries.len()).is_some() {
            let retained = if checkbox {
                "position and checkbox"
            } else {
                "position"
            };
            warnings.add(format!(
                "duplicate {source} entry {key}; keeping the last {retained}"
            ));
        }
        entries.push(PluginEntry {
            name: name.to_owned(),
            key,
            checked,
        });
    }
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| (last[&entry.key] == index).then_some(entry))
        .collect()
}

/// Read activation from stars for SE/VR, or bare names for classic Skyrim.
pub fn parse_plugin_entries(
    text: &str,
    classic: bool,
    warnings: &mut Warnings,
) -> Vec<PluginEntry> {
    parse_entries(text, classic, "plugins.txt", true, warnings)
}

/// Read an explicit ordering source, keeping the last position of repeated names.
pub fn parse_plugin_order(text: &str, warnings: &mut Warnings) -> Vec<String> {
    parse_entries(text, true, "loadorder.txt", false, warnings)
        .into_iter()
        .map(|entry| entry.key)
        .collect()
}

/// Select installed official masters, CCC Creations, and active player entries.
///
/// Installed names and supplied ordering use `normalize_plugin_name` keys. An ordering source controls
/// player entries after the fixed implicit prefix; omitted active entries append in player-list
/// order. Callers may supply explicit INI activation and append unlisted INI entries by file time.
pub fn select_plugin_names(
    entries: &[PluginEntry],
    installed: &HashSet<String>,
    creations: &[String],
    always_active: &[String],
    ordering: Option<&[String]>,
    warnings: &mut Warnings,
) -> Vec<String> {
    let always: HashSet<_> = always_active
        .iter()
        .map(|name| normalize_plugin_name(name))
        .collect();
    let mut active: Vec<_> = entries
        .iter()
        .filter(|entry| entry.checked || always.contains(&entry.key))
        .map(|entry| entry.key.clone())
        .collect();
    let mut active_keys: HashSet<_> = active.iter().cloned().collect();
    for key in always_active.iter().map(|name| normalize_plugin_name(name)) {
        if active_keys.insert(key.clone()) {
            active.push(key);
        }
    }
    let implicit: HashSet<_> = OFFICIAL_MASTERS
        .iter()
        .copied()
        .chain(creations.iter().map(String::as_str))
        .map(normalize_plugin_name)
        .collect();
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |name: &str, warnings: &mut Warnings| {
        let key = normalize_plugin_name(name);
        if !seen.insert(key.clone()) {
            return;
        }
        if installed.contains(&key) {
            selected.push(key);
        } else {
            warnings.add(format!("plugin {key} is not installed; skipping"));
        }
    };
    for name in OFFICIAL_MASTERS
        .iter()
        .copied()
        .chain(creations.iter().map(String::as_str))
    {
        add(name, warnings);
    }
    if let Some(order) = ordering {
        let ordered: HashSet<_> = order.iter().cloned().collect();
        for key in order {
            if active_keys.contains(key) {
                add(key, warnings);
            }
        }
        for key in &active {
            if !ordered.contains(key) && installed.contains(key) && !implicit.contains(key) {
                warnings.add(format!("active plugin {key} is absent from loadorder.txt; appending after ordered plugins"));
            }
            add(key, warnings);
        }
    } else {
        for key in &active {
            add(key, warnings);
        }
    }
    for entry in entries {
        if !entry.checked && !always.contains(&entry.key) && !installed.contains(&entry.key) {
            add(&entry.key, warnings);
        }
    }
    selected
}

/// Read Creation filenames in file order for explicit lists and mod-manager reuse.
/// Callers should warn and continue without Creations if this optional file fails.
pub fn read_skyrim_ccc(path: &Path) -> Result<Vec<String>> {
    let bytes = std::fs::read(path).wrap_err_with(|| format!("reading {}", path.display()))?;
    Ok(lines(&decode_windows_1252(&bytes))
        .filter(|name| plugin_name(name))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        Warnings, decode_windows_1252, parse_plugin_entries, parse_plugin_order,
        select_plugin_names,
    };
    use std::collections::HashSet;

    /// Accented spellings share the last checkbox and last ordering position.
    #[test]
    fn unicode_plugin_entries_share_last_checkbox_and_order() {
        let mut warnings = Warnings::default();
        let entries = parse_plugin_entries(
            &decode_windows_1252(b"*CAF\xc9.esp\n*Other.esp\ncaf\xe9.esp\n"),
            false,
            &mut warnings,
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>(),
            ["other.esp", "café.esp"]
        );
        assert!(!entries[1].checked, "the last unchecked spelling wins");
        let order = parse_plugin_order(
            &decode_windows_1252(b"CAF\xc9.esp\nOther.esp\ncaf\xe9.esp\n"),
            &mut warnings,
        );
        assert_eq!(order, ["other.esp", "café.esp"]);
        let mut notices = Vec::new();
        warnings.finish(&mut notices);
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert!(notices.iter().all(|notice| notice.contains("café.esp")));
    }

    /// Checked Windows-1252 names match the installed Unicode lowercase key.
    #[test]
    fn unicode_player_entries_match_installed_plugin_names() {
        let mut warnings = Warnings::default();
        let entries = parse_plugin_entries(
            &decode_windows_1252(b"*CAF\xc9.esp\n*Plain.esp\n"),
            false,
            &mut warnings,
        );
        let installed = HashSet::from(["café.esp".to_owned(), "plain.esp".to_owned()]);
        assert_eq!(
            select_plugin_names(&entries, &installed, &[], &[], None, &mut warnings),
            ["café.esp", "plain.esp"]
        );
    }

    /// CCC and explicit INI activation share the same accented plugin identity.
    #[test]
    fn unicode_implicit_and_ini_names_match_installed_plugin_names() {
        let mut warnings = Warnings::default();
        let entries = parse_plugin_entries(
            &decode_windows_1252(b"r\xe9sum\xe9.esm\n"),
            false,
            &mut warnings,
        );
        let installed = HashSet::from(["café.esp".to_owned(), "résumé.esm".to_owned()]);
        assert_eq!(
            select_plugin_names(
                &entries,
                &installed,
                &["CAFÉ.esp".to_owned()],
                &["RÉSUMÉ.esm".to_owned()],
                None,
                &mut warnings,
            ),
            ["café.esp", "résumé.esm"]
        );
    }
}
