//! Per-user choices, separate from detected folders and runtime defaults.

use super::{ConversionStatus, GamePathConfig};
use crate::{LauncherSet, mo2_settings::SourceSettings};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Resource, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversionSettings {
    pub clustered_geometry: bool,
}

impl ConversionSettings {
    /// Applies the same geometry choice to new and resumed conversions.
    pub fn apply(&self, config: &mut converter::PipelineConfig) {
        config.lod_mode = if self.clustered_geometry {
            converter::cluster::LodMode::Cluster
        } else {
            converter::cluster::LodMode::Baked
        };
        config.cluster_terrain = self.clustered_geometry;
    }
}

/// Explicit folder choices survive unavailable drives and unrelated settings changes.
/// Automatic startup discovery and the relative modern_assets default never enter it.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UserChosenPaths {
    pub skyrim_data_path: Option<PathBuf>,
    pub converted_assets_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SavedMo2 {
    instance_path: PathBuf,
    profile: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct SavedSettings {
    conversion: ConversionSettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    skyrim_data_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    converted_assets_path: Option<PathBuf>,
    use_mo2: bool,
    mo2: Option<SavedMo2>,
}

impl SavedSettings {
    fn capture(
        settings: &ConversionSettings,
        paths: &UserChosenPaths,
        source: &SourceSettings,
    ) -> Self {
        Self {
            conversion: settings.clone(),
            skyrim_data_path: paths.skyrim_data_path.clone(),
            converted_assets_path: paths.converted_assets_path.clone(),
            use_mo2: source.use_mo2,
            mo2: source.selection.as_ref().map(|selection| SavedMo2 {
                instance_path: selection.instance_path.clone(),
                profile: selection.profile.clone(),
            }),
        }
    }

    fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        // A sibling replacement preserves the old settings if writing fails.
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let result =
            std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, path));
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}

#[derive(Resource)]
struct SettingsFile {
    path: PathBuf,
    observed: SavedSettings,
}

fn settings_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let folder = config_folder(std::env::var_os("APPDATA").map(PathBuf::from), None, None);
    #[cfg(target_os = "macos")]
    let folder = config_folder(None, None, std::env::var_os("HOME").map(PathBuf::from));
    #[cfg(not(any(windows, target_os = "macos")))]
    let folder = config_folder(
        None,
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    );
    folder.map(|folder| folder.join("mudcrab-launcher.json"))
}

fn config_folder(
    appdata: Option<PathBuf>,
    xdg: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let absolute = |path: &PathBuf| path.is_absolute();
    // Keep platform policy testable without process-global environment mutation.
    #[cfg(windows)]
    {
        let _ = (xdg, home);
        appdata.filter(absolute)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (appdata, xdg);
        home.filter(absolute)
            .map(|p| p.join("Library/Application Support"))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = appdata;
        xdg.filter(absolute)
            .or_else(|| home.filter(absolute).map(|p| p.join(".config")))
    }
}

/// Installs disk persistence only for the running launcher; headless logic remains reusable.
pub fn install(app: &mut App) {
    app.init_resource::<UserChosenPaths>()
        .add_systems(
            Startup,
            load_settings.before(super::panel::detect_skyrim_at_start),
        )
        .add_systems(Update, save_settings.in_set(LauncherSet::Draw));
}

fn load_settings(world: &mut World) {
    let Some(path) = settings_path() else {
        world.resource_mut::<ConversionStatus>().push_notice(
            "Cannot locate the user configuration folder; conversion settings cannot be saved.",
        );
        return;
    };
    load_settings_from(path, world);
}

/// A new output leaf is allowed when its parent exists; existing files are refused.
fn available_output_path(path: &Path) -> bool {
    if path.is_dir() {
        return true;
    }
    matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
        && path.parent().is_some_and(|p| p.is_dir())
}

/// Preserve corrupt bytes without ever replacing an earlier recovery file.
fn quarantine(path: &Path) -> io::Result<PathBuf> {
    for n in 1_u64.. {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".corrupt-{n}"));
        let backup = PathBuf::from(name);
        // Link + unlink is a no-overwrite rename on the same filesystem. If either
        // operation fails, retain the original and disable saving rather than lose data.
        match std::fs::hard_link(path, &backup) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
            Ok(()) => {
                std::fs::remove_file(path)?;
                return Ok(backup);
            }
        }
    }
    Err(io::Error::other("settings recovery suffix exhausted"))
}

fn load_settings_from(path: PathBuf, world: &mut World) {
    let mut saved = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<SavedSettings>(&bytes) {
            Ok(saved) => saved,
            Err(error) => match quarantine(&path) {
                Ok(backup) => {
                    world.resource_mut::<ConversionStatus>().push_notice(&format!("Invalid launcher settings at {}: {error}. Preserved at {}; starting with defaults.", path.display(), backup.display()));
                    SavedSettings::default()
                }
                Err(backup_error) => {
                    world.resource_mut::<ConversionStatus>().push_notice(&format!("Cannot preserve invalid launcher settings at {}: {backup_error}. Saving is disabled; original file retained.", path.display()));
                    return;
                }
            },
        },
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && matches!(std::fs::symlink_metadata(&path), Err(missing) if missing.kind() == io::ErrorKind::NotFound) =>
        {
            SavedSettings::default()
        }
        Err(error) => {
            world.resource_mut::<ConversionStatus>().push_notice(&format!("Cannot read launcher settings at {}: {error}. Saving is disabled until the file is repaired and the launcher restarted.", path.display()));
            return;
        }
    };
    // Old relative settings have no reliable cwd provenance. Never bind them to
    // this launch directory; keep the unsaved default until a new folder is chosen.
    for choice in [
        &mut saved.skyrim_data_path,
        &mut saved.converted_assets_path,
    ] {
        if choice.as_ref().is_some_and(|p| !p.is_absolute()) {
            world.resource_mut::<ConversionStatus>().push_notice("A saved relative folder has no fixed location; choose it again to save an absolute path.");
            *choice = None;
        }
    }
    let choices = UserChosenPaths {
        skyrim_data_path: saved.skyrim_data_path.clone(),
        converted_assets_path: saved.converted_assets_path.clone(),
    };
    if let Some(data) = saved.skyrim_data_path {
        if crate::game_detection::is_skyrim_data_dir(&data) {
            world.resource_mut::<GamePathConfig>().skyrim_data_path = Some(data);
        } else {
            world.resource_mut::<GamePathConfig>().skyrim_data_path = None;
            world.resource_mut::<ConversionStatus>().push_notice(&format!("Saved Skyrim Data folder {} is unavailable; detecting the game again. Saved choice is retained.", data.display()));
        }
    }
    if let Some(output) = saved.converted_assets_path {
        if available_output_path(&output) {
            world.resource_mut::<GamePathConfig>().converted_assets_path = output;
        } else {
            world.resource_mut::<GamePathConfig>().converted_assets_path = PathBuf::new();
            world.resource_mut::<ConversionStatus>().push_notice(&format!("Saved output folder {} is unavailable; choose an output folder. Saved choice is retained.", output.display()));
        }
    }
    *world.resource_mut::<ConversionSettings>() = saved.conversion;
    world.insert_resource(choices);
    let restored = {
        let mut source = world.resource_mut::<SourceSettings>();
        source.use_mo2 = saved.use_mo2;
        source.restore_selection(saved.mo2.map(|s| mo2::Selection {
            instance_path: s.instance_path,
            profile: s.profile,
        }))
    };
    if let Err(error) = restored {
        world.resource_mut::<ConversionStatus>().push_notice(&error);
    }
    let observed = SavedSettings::capture(
        world.resource::<ConversionSettings>(),
        world.resource::<UserChosenPaths>(),
        world.resource::<SourceSettings>(),
    );
    world.insert_resource(SettingsFile { path, observed });
}

fn save_settings(
    file: Option<ResMut<SettingsFile>>,
    settings: Res<ConversionSettings>,
    paths: Res<UserChosenPaths>,
    source: Res<SourceSettings>,
    mut status: ResMut<ConversionStatus>,
) {
    let Some(mut file) = file else { return };
    if !settings.is_changed() && !paths.is_changed() && !source.is_changed() {
        return;
    }
    let saved = SavedSettings::capture(&settings, &paths, &source);
    if saved == file.observed {
        return;
    }
    // A failed distinct edit notifies once, not every frame. Another edit retries.
    file.observed = saved.clone();
    if let Err(error) = saved.save(&file.path) {
        status.push_notice(&format!(
            "Cannot save launcher settings at {}: {error}",
            file.path.display()
        ));
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
