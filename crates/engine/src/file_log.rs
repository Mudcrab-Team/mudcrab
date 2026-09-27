//! Warning-and-above file logging for field bug reports.
//!
//! `--log-file <path>` (or the default `openskyrim-YYYYMMDD-HHMMSS.log` next
//! to the working directory) captures WARN/ERROR output via a tracing layer
//! while stderr keeps Bevy's normal console output.

use bevy::log::tracing_subscriber::{Layer, fmt, prelude::*, registry::Registry};
use bevy::log::{BoxedLayer, Level, tracing_subscriber};
use std::{fs::File, path::PathBuf, sync::Mutex};

/// Resolve the log file path: explicit flag, else timestamped default.
pub fn resolve_log_path(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // YYYYMMDD-HHMMSS in UTC via coarse arithmetic (no chrono dep).
    let (days, secs) = (now / 86400, now % 86400);
    let (mut y, mut rem) = (1970u64, days);
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if rem < len {
            break;
        }
        rem -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let (mut m, mut d) = (1u64, rem + 1);
    for len in months {
        if d <= len {
            break;
        }
        d -= len;
        m += 1;
    }
    PathBuf::from(format!(
        "openskyrim-{y:04}{m:02}{d:02}-{:02}{:02}{:02}.log",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    ))
}

/// Build the WARN+ file layer. Returns None if the file cannot be created
/// (startup continues with console logging only, plus a stderr note).
pub fn warning_file_layer(path: &std::path::Path) -> Option<BoxedLayer> {
    match File::create(path) {
        Ok(file) => {
            let layer = fmt::layer()
                .with_ansi(false)
                .with_writer(Mutex::new(file))
                .with_filter(tracing_subscriber::filter::LevelFilter::WARN);
            Some(Box::new(layer))
        }
        Err(error) => {
            eprintln!(
                "warning: cannot open log file {}: {error}; console logging only",
                path.display()
            );
            None
        }
    }
}

/// App access for `LogPlugin::custom_layer`: pulls the path from EngineConfig.
pub fn custom_file_layer(app: &mut bevy::prelude::App) -> Option<BoxedLayer> {
    let path = app
        .world()
        .get_resource::<crate::config::EngineConfig>()
        .map(|config| resolve_log_path(config.log_file.clone()))
        .unwrap_or_else(|| resolve_log_path(None));
    eprintln!("logging warnings+ to {}", path.display());
    warning_file_layer(&path)
}

#[allow(dead_code)]
fn _assert_level_import() {
    let _ = Level::WARN;
    let _: Option<BoxedLayer> = None;
    let _ = Registry::default().with(fmt::layer());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_path_wins_over_default() {
        let explicit = PathBuf::from("/tmp/custom-test.log");
        assert_eq!(resolve_log_path(Some(explicit.clone())), explicit);
    }

    #[test]
    fn default_path_is_timestamped_log() {
        let path = resolve_log_path(None);
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("openskyrim-"), "{name}");
        assert!(name.ends_with(".log"), "{name}");
        assert_eq!(name.len(), "openskyrim-YYYYMMDD-HHMMSS.log".len(), "{name}");
    }

    #[test]
    fn uncreatable_path_returns_none_without_panic() {
        let bad = PathBuf::from("/nonexistent-dir-xyz/openskyrim.log");
        assert!(warning_file_layer(&bad).is_none());
    }
}
