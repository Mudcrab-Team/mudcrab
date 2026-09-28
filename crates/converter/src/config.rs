use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    pub data_dir: PathBuf,
    pub output_dir: PathBuf,
    #[serde(skip)]
    pub resume_staging: Option<PathBuf>,
    pub plugins_file: Option<PathBuf>,
    pub cpu_jobs: usize,
    pub io_jobs: usize,
    pub enable_ba2: bool,
    pub fail_fast: bool,
    pub invalidate_cache: bool,
    pub verify_cache: bool,
    pub texture_etc1s_quality: u8,
    pub texture_uastc_level: u8,
    pub script_abi_version: u32,
}

impl PipelineConfig {
    pub fn new(data_dir: impl Into<PathBuf>, output_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            output_dir: output_dir.into(),
            resume_staging: None,
            plugins_file: None,
            cpu_jobs: std::thread::available_parallelism().map_or(1, usize::from),
            io_jobs: 2,
            enable_ba2: true,
            fail_fast: false,
            invalidate_cache: false,
            verify_cache: true,
            texture_etc1s_quality: 192,
            texture_uastc_level: 2,
            script_abi_version: 1,
        }
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
            (1..=255).contains(&self.texture_etc1s_quality),
            "texture_etc1s_quality must be between 1 and 255"
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
