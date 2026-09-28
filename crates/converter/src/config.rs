use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    pub data_dir: PathBuf,
    pub output_dir: PathBuf,
    #[serde(skip)]
    pub resume_staging: Option<PathBuf>,
    /// Persistent ingestion-cache root. Defaults to `<output>.assets-cache`
    /// alongside the output directory; the runtime pack no longer contains it.
    #[serde(skip)]
    pub cache_dir: Option<PathBuf>,
    pub plugins_file: Option<PathBuf>,
    pub cpu_jobs: usize,
    pub io_jobs: usize,
    pub enable_ba2: bool,
    pub fail_fast: bool,
    pub invalidate_cache: bool,
    pub verify_cache: bool,
    /// Quality for the UASTC fallback path (uncompressed/legacy sources).
    /// Named `texture_etc1s_quality` in serialized configs for compatibility;
    /// it never selected ETC1S encoding, which the Bevy runtime rejects.
    #[serde(rename = "texture_etc1s_quality", alias = "texture_fallback_quality")]
    pub texture_fallback_quality: u8,
    pub texture_uastc_level: u8,
    pub script_abi_version: u32,
}

impl PipelineConfig {
    pub fn new(data_dir: impl Into<PathBuf>, output_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            output_dir: output_dir.into(),
            resume_staging: None,
            cache_dir: None,
            plugins_file: None,
            cpu_jobs: std::thread::available_parallelism().map_or(1, usize::from),
            io_jobs: 2,
            enable_ba2: true,
            fail_fast: false,
            invalidate_cache: false,
            verify_cache: true,
            texture_fallback_quality: 192,
            texture_uastc_level: 2,
            script_abi_version: 1,
        }
    }

    /// Resolves the persistent ingestion-cache root outside the published pack.
    pub fn ingestion_cache_dir(&self) -> PathBuf {
        if let Some(dir) = &self.cache_dir {
            return dir.clone();
        }
        let file_name = self
            .output_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("modern_assets");
        self.output_dir
            .with_file_name(format!("{file_name}.assets-cache"))
    }

    pub(crate) fn validate(&self) -> color_eyre::Result<()> {
        color_eyre::eyre::ensure!(
            self.data_dir.is_dir(),
            "Skyrim Data directory does not exist: {}",
            self.data_dir.display()
        );
        color_eyre::eyre::ensure!(self.cpu_jobs > 0, "cpu_jobs must be greater than zero");
        color_eyre::eyre::ensure!(self.io_jobs > 0, "io_jobs must be greater than zero");
        color_eyre::eyre::ensure!(
            (1..=255).contains(&self.texture_fallback_quality),
            "texture_fallback_quality must be between 1 and 255"
        );
        color_eyre::eyre::ensure!(
            self.texture_uastc_level <= 4,
            "texture_uastc_level must be between 0 and 4"
        );
        color_eyre::eyre::ensure!(
            self.data_dir != self.output_dir,
            "output directory must not be the Skyrim Data directory"
        );
        if let Some(staging) = &self.resume_staging {
            color_eyre::eyre::ensure!(
                staging.is_dir(),
                "resume staging directory does not exist: {}",
                staging.display()
            );
            let output_name = self
                .output_dir
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| color_eyre::eyre::eyre!("output directory has no valid name"))?;
            let staging_name = staging
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            color_eyre::eyre::ensure!(
                staging_name.starts_with(&format!("{output_name}.staging-")),
                "resume directory is not a staging directory for {}",
                self.output_dir.display()
            );
            let output_parent = parent_or_cwd(&self.output_dir);
            let staging_parent = parent_or_cwd(staging);
            color_eyre::eyre::ensure!(
                std::fs::canonicalize(output_parent)? == std::fs::canonicalize(staging_parent)?,
                "resume directory must share the output directory parent"
            );
        }
        Ok(())
    }
}

// `Path::parent` returns `Some("")` for bare file names, so empty parents
// must also fall back to the current directory.
fn parent_or_cwd(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_relative_names_resolve_to_the_current_directory() {
        assert_eq!(parent_or_cwd(Path::new("modern_assets")), Path::new("."));
        assert_eq!(
            parent_or_cwd(Path::new("modern_assets.staging-1")),
            Path::new(".")
        );
        assert_eq!(parent_or_cwd(Path::new("./modern_assets")), Path::new("."));
        assert_eq!(
            parent_or_cwd(Path::new("/data/modern_assets")),
            Path::new("/data")
        );
        assert_eq!(parent_or_cwd(Path::new("/data")), Path::new("/"));
    }
}
