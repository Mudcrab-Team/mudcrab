//! Explicit Skyrim SE selection. Optional game lists never consult ambient profiles.
use super::{binary::parse_plugin_metadata, load_order};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use shared::plugin_selection::{
    CreationFallbackWarning, PluginEntry, Warnings, decode_windows_1252, normalize_plugin_name,
    parse_plugin_entries, select_creations, select_plugin_names,
};
pub use shared::plugin_selection::{OFFICIAL_MASTERS, read_skyrim_ccc};

/// Recognize plugin filenames directly inside Data, excluding path components.
fn plugin_name(name: &str) -> bool {
    !name.contains(['/', '\\', ':'])
        && matches!(
            Path::new(name)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("esm" | "esp" | "esl")
        )
}

/// Reject every file with an ambiguous normalized filename, with one notice per identity.
fn installed_plugin_paths(
    paths: impl IntoIterator<Item = PathBuf>,
    warnings: &mut Warnings,
) -> HashMap<String, PathBuf> {
    let mut installed = HashMap::new();
    for path in paths {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !plugin_name(&name) {
            continue;
        }
        let key = normalize_plugin_name(&name);
        match installed.entry(key.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Some(path));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if entry.get().is_some() {
                    entry.insert(None);
                    warnings.add(format!(
                        "installed plugin name collision for {key}; skipping all matching files"
                    ));
                }
            }
        }
    }
    installed
        .into_iter()
        .filter_map(|(key, path)| path.map(|path| (key, path)))
        .collect()
}

/// Keep every original installed name, ordering ambiguous originals within their identity.
fn installed_plugin_candidates(paths: &[PathBuf]) -> BTreeMap<String, Vec<PathBuf>> {
    let mut candidates: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in paths {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if plugin_name(&name) {
            candidates
                .entry(normalize_plugin_name(&name))
                .or_default()
                .push(path.clone());
        }
    }
    for paths in candidates.values_mut() {
        paths.sort();
    }
    candidates
}

/// Retain unique installed identities in candidate order before header/dependency recovery.
fn unique_plugin_candidates(paths: Vec<PathBuf>, warnings: &mut Warnings) -> Vec<PathBuf> {
    let installed = installed_plugin_paths(paths.iter().cloned(), warnings);
    paths
        .into_iter()
        .filter(|path| {
            let name =
                normalize_plugin_name(&path.file_name().unwrap_or_default().to_string_lossy());
            installed.contains_key(&name)
        })
        .collect()
}

/// Replace exclusions with present candidates removed by any recovery stage, once per original filename.
/// Candidate order is retained, including distinct originals with one normalized identity.
pub(crate) fn record_recovery_skips(
    candidates: &[PathBuf],
    survivors: &[PathBuf],
    skipped_plugins: &mut Vec<String>,
) {
    let survivors: HashSet<_> = survivors.iter().collect();
    let mut seen = HashSet::new();
    *skipped_plugins = candidates
        .iter()
        .filter(|path| !survivors.contains(path))
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| seen.insert(name.clone()))
        .collect();
}

/// Ignore comments and blank lines without interpreting checkbox state.
fn lines(contents: &str) -> impl Iterator<Item = &str> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(['#', ';']))
}

/// Read only explicit General sTestFile1 through sTestFile10 settings.
fn ini_plugins(path: &Path, warnings: &mut Warnings) -> Vec<String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            warnings.add(format!(
                "could not read optional Skyrim.ini {}: {error}",
                path.display()
            ));
            return Vec::new();
        }
    };
    let text = decode_windows_1252(&bytes);
    let mut general = false;
    let mut settings = BTreeMap::new();
    for line in lines(&text) {
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            general = section.trim().eq_ignore_ascii_case("General");
            continue;
        }
        if !general {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let Some(index) = key
            .strip_prefix("stestfile")
            .and_then(|index| index.parse::<u8>().ok())
            .filter(|index| (1..=10).contains(index))
        else {
            continue;
        };
        let value = value.trim();
        settings.insert(index, value.to_owned());
    }
    settings
        .into_values()
        .filter(|name| plugin_name(name))
        .collect()
}

/// Keep usable selections and recursively omit dependents of missing/disabled masters.
fn usable_plugins(paths: Vec<PathBuf>, warnings: &mut Warnings) -> Vec<PathBuf> {
    let indices: HashMap<_, _> = paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            (
                normalize_plugin_name(&path.file_name().unwrap_or_default().to_string_lossy()),
                i,
            )
        })
        .collect();
    let mut metadata = Vec::with_capacity(paths.len());
    let mut missing = vec![None; paths.len()];
    for (index, path) in paths.iter().enumerate() {
        match parse_plugin_metadata(path) {
            Ok(header) => metadata.push(Some(header)),
            Err(error) => {
                warnings.add(format!(
                    "skipping {}: unusable plugin header: {error:?}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
                missing[index] = Some(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                );
                metadata.push(None);
            }
        }
    }
    loop {
        let mut changed = false;
        for (index, header) in metadata.iter().enumerate() {
            if missing[index].is_some() {
                continue;
            }
            let Some(header) = header else {
                continue;
            };
            for master in &header.masters {
                let cause = match indices.get(&normalize_plugin_name(master)) {
                    Some(&dependency) => missing[dependency].clone(),
                    None => Some(master.clone()),
                };
                if let Some(cause) = cause {
                    missing[index] = Some(cause);
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }
    paths
        .into_iter()
        .enumerate()
        .filter_map(|(index, path)| {
            if let Some(master) = &missing[index] {
                if metadata[index].is_some() {
                    warnings.add(format!(
                        "skipping {}: required master {master} is missing, disabled or unusable",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
                None
            } else {
                Some(path)
            }
        })
        .collect()
}

/// Recover automatically discovered plugins: omit ambiguous normalized names before checking
/// headers, skip unusable headers and missing-master closures with bounded notices, then order
/// the rest by dependency with cycle/slot recovery.
pub(crate) fn recover_discovered(
    paths: Vec<PathBuf>,
    notices: &mut Vec<String>,
    skipped_plugins: &mut Vec<String>,
) -> Result<Vec<PathBuf>> {
    let candidates = paths.clone();
    let mut warnings = Warnings::default();
    let paths = unique_plugin_candidates(paths, &mut warnings);
    let paths = usable_plugins(paths, &mut warnings);
    warnings.finish(notices);
    let paths = load_order::order_discovered_plugins_with_notices(paths, notices, &mut Vec::new())?;
    record_recovery_skips(&candidates, &paths, skipped_plugins);
    Ok(paths)
}

/// Apply ambiguity/header/master and cycle/slot recovery while retaining a mod-manager's order.
pub(crate) fn recover_with_diagnostics(
    paths: Vec<PathBuf>,
    notices: &mut Vec<String>,
    skipped_plugins: &mut Vec<String>,
) -> Result<Vec<PathBuf>> {
    let candidates = paths.clone();
    let mut warnings = Warnings::default();
    let paths = unique_plugin_candidates(paths, &mut warnings);
    let paths = usable_plugins(paths, &mut warnings);
    warnings.finish(notices);
    let paths = load_order::recover_ordered_plugins_with_notices(paths, notices, &mut Vec::new())?;
    record_recovery_skips(&candidates, &paths, skipped_plugins);
    Ok(paths)
}

/// Select plugins using explicit CCC/INI inputs, collecting bounded diagnostics.
pub(crate) fn read_with_notices(
    path: &Path,
    data_dir: &Path,
    ccc: Option<&Path>,
    ini: Option<&Path>,
    notices: &mut Vec<String>,
) -> Result<Vec<PathBuf>> {
    read_with_diagnostics(path, data_dir, ccc, ini, notices, &mut Vec::new())
}

/// Select usable player plugins, forwarding fallback warnings through the legacy notice channel.
pub(crate) fn read_with_diagnostics(
    path: &Path,
    data_dir: &Path,
    ccc: Option<&Path>,
    ini: Option<&Path>,
    notices: &mut Vec<String>,
    skipped_plugins: &mut Vec<String>,
) -> Result<Vec<PathBuf>> {
    let mut selection_warnings = Vec::new();
    let selected = read_with_selection_diagnostics(
        path,
        data_dir,
        ccc,
        ini,
        notices,
        skipped_plugins,
        &mut selection_warnings,
    );
    notices.extend(
        selection_warnings
            .into_iter()
            .map(|warning| warning.to_string()),
    );
    selected
}

/// Select usable player plugins, reporting all recovery skips and typed Creation fallback warnings.
pub(crate) fn read_with_selection_diagnostics(
    path: &Path,
    data_dir: &Path,
    ccc: Option<&Path>,
    ini: Option<&Path>,
    notices: &mut Vec<String>,
    skipped_plugins: &mut Vec<String>,
    selection_warnings: &mut Vec<CreationFallbackWarning>,
) -> Result<Vec<PathBuf>> {
    let bytes =
        std::fs::read(path).wrap_err_with(|| format!("reading plugins.txt {}", path.display()))?;
    let text = decode_windows_1252(&bytes);
    let mut warnings = Warnings::default();
    let mut installed_paths = Vec::new();
    for entry in std::fs::read_dir(data_dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.add(format!("could not read Data directory entry: {error}"));
                continue;
            }
        };
        if entry.path().is_file() {
            installed_paths.push(entry.path());
        }
    }
    let installed_candidates = installed_plugin_candidates(&installed_paths);
    let installed_names = installed_candidates.keys().cloned().collect();
    let installed = installed_plugin_paths(installed_paths, &mut warnings);
    let default_ccc = data_dir.parent().unwrap_or(data_dir).join("Skyrim.ccc");
    let ccc_path = ccc.unwrap_or(&default_ccc);
    let creations = select_creations(read_skyrim_ccc(ccc_path), &installed_names, ccc_path);
    if let Some(warning) = creations.fallback_warning {
        selection_warnings.push(warning);
    }
    let test_files = ini
        .map(|path| ini_plugins(path, &mut warnings))
        .unwrap_or_default();
    let mut listed = parse_plugin_entries(&text, false, &mut warnings);
    let listed_names: HashSet<_> = listed.iter().map(|entry| entry.key.clone()).collect();
    let mut unlisted: Vec<_> = test_files
        .iter()
        .filter(|name| !listed_names.contains(&normalize_plugin_name(name)))
        .collect();
    unlisted.sort_by_key(|name| {
        let key = normalize_plugin_name(name);
        let modified = installed
            .get(&key)
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|meta| meta.modified().ok())
            .unwrap_or(UNIX_EPOCH);
        (modified, key)
    });
    for name in unlisted {
        listed.push(PluginEntry {
            name: name.clone(),
            key: normalize_plugin_name(name),
            checked: true,
        });
    }
    let names = select_plugin_names(
        &listed,
        &installed_names,
        &creations.names,
        &test_files,
        None,
        &mut warnings,
    );
    let candidates: Vec<_> = names
        .iter()
        .flat_map(|name| installed_candidates[name].iter().cloned())
        .collect();
    let selected = names
        .into_iter()
        .filter_map(|name| installed.get(&name).cloned())
        .collect();
    let selected = usable_plugins(selected, &mut warnings);
    warnings.finish(notices);
    let selected =
        load_order::order_explicit_plugins_with_notices(selected, notices, &mut Vec::new())?;
    record_recovery_skips(&candidates, &selected, skipped_plugins);
    ensure!(
        !selected.is_empty(),
        "no usable active plugins remain after plugins.txt selection"
    );
    Ok(selected)
}

/// Read an explicit player list with optional CCC and INI paths; omitted INI stays unused.
pub fn read_plugins_txt_with_options(
    path: &Path,
    data_dir: &Path,
    ccc: Option<&Path>,
    ini: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let mut notices = Vec::new();
    let result = read_with_notices(path, data_dir, ccc, ini, &mut notices);
    for notice in notices {
        eprintln!("warning: {notice}");
    }
    result
}

/// Read a player list with the game-folder CCC default and no ambient Skyrim.ini.
pub fn read_plugins_txt(path: &Path, data_dir: &Path) -> Result<Vec<PathBuf>> {
    read_plugins_txt_with_options(path, data_dir, None, None)
}

#[cfg(test)]
mod tests {
    use super::{Warnings, installed_plugin_paths, parse_plugin_metadata};
    use std::{fs, path::PathBuf};

    /// Give each inventory candidate a real, independently distinguishable header and payload.
    fn inventory_plugin(root: &std::path::Path, folder: &str, name: &str, flags: u32) -> PathBuf {
        let directory = root.join(folder);
        fs::create_dir(&directory).unwrap();
        let path = directory.join(name);
        let payload = [
            b"EDID".as_slice(),
            &4u16.to_le_bytes(),
            &flags.to_le_bytes(),
        ]
        .concat();
        let bytes = [
            b"TES4".as_slice(),
            &(payload.len() as u32).to_le_bytes(),
            &flags.to_le_bytes(),
            &[0; 12],
            &payload,
        ]
        .concat();
        fs::write(&path, bytes).unwrap();
        assert_eq!(parse_plugin_metadata(&path).unwrap().flags, flags);
        path
    }

    /// Reject either enumeration of differently cased real ASCII files, preserving unrelated keys.
    #[test]
    fn ambiguous_installed_ascii_names_are_rejected_in_both_orders() {
        assert_both_orders("Mod.esp", "mod.esp", "mod.esp");
    }

    /// Apply the shared Unicode lowercase identity before deciding which real files are ambiguous.
    #[test]
    fn ambiguous_installed_unicode_names_are_rejected_in_both_orders() {
        assert_both_orders("CAFÉ.esp", "café.esp", "café.esp");
    }

    /// Feed production indexing both permutations without needing a case-sensitive test volume.
    fn assert_both_orders(first: &str, second: &str, key: &str) {
        let dir = tempfile::tempdir().unwrap();
        let first = inventory_plugin(dir.path(), "first", first, 0);
        let second = inventory_plugin(dir.path(), "second", second, 0x200);
        let good = inventory_plugin(dir.path(), "good", "Good.esp", 0);
        assert_ne!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
        for duplicates in [[first.clone(), second.clone()], [second, first]] {
            let mut warnings = Warnings::default();
            let installed = installed_plugin_paths(
                [duplicates[0].clone(), good.clone(), duplicates[1].clone()],
                &mut warnings,
            );
            assert!(!installed.contains_key(key), "{installed:?}");
            assert_eq!(installed.len(), 1);
            assert_eq!(installed["good.esp"], good);
            let mut notices = Vec::new();
            warnings.finish(&mut notices);
            assert_eq!(
                notices,
                [format!(
                    "installed plugin name collision for {key}; skipping all matching files"
                )]
            );
        }
    }

    /// A third spelling cannot revive an ambiguous key or produce repeated collision notices.
    #[test]
    fn ambiguous_installed_names_remain_rejected_after_third_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [
            inventory_plugin(dir.path(), "first", "Mod.esp", 0),
            inventory_plugin(dir.path(), "second", "mod.esp", 0x200),
            inventory_plugin(dir.path(), "third", "MOD.esp", 1),
        ];
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let mut warnings = Warnings::default();
            let installed =
                installed_plugin_paths(order.map(|index| paths[index].clone()), &mut warnings);
            assert!(installed.is_empty(), "{installed:?}");
            let mut notices = Vec::new();
            warnings.finish(&mut notices);
            assert_eq!(
                notices,
                ["installed plugin name collision for mod.esp; skipping all matching files"]
            );
        }
    }

    /// A corrupt copy cannot turn collision handling into an enumeration-dependent header failure.
    #[test]
    fn ambiguous_installed_unicode_name_with_corrupt_copy_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let usable = inventory_plugin(dir.path(), "usable", "CAFÉ.esp", 0);
        let corrupt = inventory_plugin(dir.path(), "corrupt", "café.esp", 0x200);
        fs::write(&corrupt, b"TES4 truncated").unwrap();
        assert!(parse_plugin_metadata(&corrupt).is_err());
        let good = inventory_plugin(dir.path(), "good", "Good.esp", 0);
        for duplicates in [[usable.clone(), corrupt.clone()], [corrupt, usable]] {
            let mut warnings = Warnings::default();
            let installed = installed_plugin_paths(
                [duplicates[0].clone(), good.clone(), duplicates[1].clone()],
                &mut warnings,
            );
            assert!(!installed.contains_key("café.esp"), "{installed:?}");
            assert_eq!(installed.len(), 1);
            assert_eq!(installed["good.esp"], good);
            let mut notices = Vec::new();
            warnings.finish(&mut notices);
            assert_eq!(
                notices,
                ["installed plugin name collision for café.esp; skipping all matching files"]
            );
        }
    }
}
