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
    /// Zstandard level for per-mip KTX2 supercompression (0 = off).
    #[serde(default = "default_texture_zstd_level")]
    pub texture_zstd_level: i32,
    pub script_abi_version: u32,
}

fn default_texture_zstd_level() -> i32 {
    6
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
            texture_zstd_level: default_texture_zstd_level(),
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
            (0..=22).contains(&self.texture_zstd_level),
            "texture_zstd_level must be between 0 and 22"
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
            // A successful run removes its staging folder, so a resume folder that is or holds
            // the Skyrim Data folder would take the game data with it.
            let data = std::fs::canonicalize(&self.data_dir)?;
            color_eyre::eyre::ensure!(
                !data.starts_with(std::fs::canonicalize(staging)?),
                "resume staging directory {} must not be or contain the Skyrim Data directory {}",
                staging.display(),
                self.data_dir.display()
            );
        }
        Ok(())
    }
}

/// The staging folder an unfinished conversion into `output` left behind, for
/// `--resume-staging` (`PipelineConfig::resume_staging`): the newest
/// `<output>.staging-<pid>-<stamp>` directory beside `output`, and how many
/// older staging folders were passed over, which a front end can offer to
/// delete. It checks only what a resume checks before it starts (the name and
/// the parent folder); the resume itself re-verifies the files inside. Other
/// folders the converter keeps beside the output are never returned.
pub fn find_resumable_staging(output: &Path) -> Option<(PathBuf, usize)> {
    let prefix = format!("{}.staging-", output.file_name()?.to_str()?);
    let mut candidates: Vec<(u128, PathBuf)> = std::fs::read_dir(parent_or_cwd(output))
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name();
            let suffix = name.to_str()?.strip_prefix(&prefix)?;
            // `<pid>-<stamp>`, the stamp in nanoseconds since the epoch; a
            // folder named some other way sorts oldest.
            let stamp = suffix.rsplit('-').next()?.parse().unwrap_or(0);
            Some((stamp, entry.path()))
        })
        .collect();
    candidates.sort();
    let (_, newest) = candidates.pop()?;
    Some((newest, candidates.len()))
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
    fn finds_the_newest_staging_folder_beside_the_output() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("modern_assets");
        assert_eq!(find_resumable_staging(&output), None);

        for name in [
            "modern_assets.staging-7-100",
            "modern_assets.staging-9-300",
            "modern_assets.staging-8-200",
            // Not staging for this output: another output's, the publish pack,
            // the persistent cache, and a file with a staging name.
            "other.staging-1-999",
            "modern_assets.pack-1-999",
            "modern_assets.assets-cache",
        ] {
            std::fs::create_dir(directory.path().join(name)).unwrap();
        }
        std::fs::write(directory.path().join("modern_assets.staging-1-999"), b"").unwrap();

        assert_eq!(
            find_resumable_staging(&output),
            Some((directory.path().join("modern_assets.staging-9-300"), 2))
        );
    }

    #[test]
    fn a_resume_staging_folder_holding_the_data_folder_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("modern");
        let staging = directory.path().join("modern.staging-1-1");
        let data = staging.join("Data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("Skyrim.esm"), b"game data").unwrap();

        let mut config = PipelineConfig::new(&data, &output);
        config.resume_staging = Some(staging.clone());
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("must not be or contain"), "{error}");

        // The same folder is refused when it is the data folder itself.
        let mut config = PipelineConfig::new(&staging, &output);
        config.resume_staging = Some(staging.clone());
        assert!(config.validate().is_err());

        assert_eq!(
            std::fs::read(data.join("Skyrim.esm")).unwrap(),
            b"game data"
        );
        assert!(!output.exists());

        // A staging folder beside the data folder still passes.
        let other_data = directory.path().join("Data");
        std::fs::create_dir_all(&other_data).unwrap();
        let mut config = PipelineConfig::new(&other_data, &output);
        config.resume_staging = Some(staging);
        config.validate().unwrap();
    }

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
