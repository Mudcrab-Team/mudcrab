//! File publication and rollback for LOD changes backed by a SQLite transaction.

use crate::pack_clone::require_private_output;
use color_eyre::{Result, eyre::WrapErr};
use rusqlite::Transaction;
use std::{
    collections::BTreeSet,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

/// Keeps the new files and any backups on the destination filesystem until the database commits.
pub(crate) struct StagedGeneration {
    files: tempfile::TempDir,
    pub(crate) paths: BTreeSet<PathBuf>,
    pub(crate) stale: BTreeSet<PathBuf>,
}

impl StagedGeneration {
    pub(crate) fn new(target: &Path) -> Result<Self> {
        let files = tempfile::Builder::new()
            .prefix(".lod-")
            .tempdir_in(target)?;
        Ok(Self {
            files,
            paths: BTreeSet::new(),
            stale: BTreeSet::new(),
        })
    }

    pub(crate) fn staging_root(&self) -> PathBuf {
        self.files.path().join("new")
    }

    /// Moves the old files aside under the pack lock. A failed install or SQLite commit restores
    /// them. Stale payloads are backed up too, so retirement is part of the same operation.
    pub(crate) fn publish(
        self,
        target: &Path,
        transaction: Transaction<'_>,
        before_commit: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let affected = || self.paths.iter().chain(self.stale.difference(&self.paths));
        for path in affected() {
            require_private_output(target, path)?;
        }
        let new = self.staging_root();
        let backup = self.files.path().join("previous");
        let mut moved = Vec::new();
        let mut new_directories = Vec::new();
        let result = (|| -> Result<()> {
            for path in affected() {
                let destination = target.join(path);
                let saved = backup.join(path);
                if let Some(parent) = saved.parent() {
                    fs::create_dir_all(parent)?;
                }
                let had_previous = match fs::rename(&destination, &saved) {
                    Ok(()) => true,
                    Err(error) if error.kind() == ErrorKind::NotFound => false,
                    Err(error) => {
                        return Err(error)
                            .wrap_err_with(|| format!("cannot save {}", destination.display()));
                    }
                };
                moved.push((path, had_previous, false));
                if self.paths.contains(path) {
                    if let Some(parent) = destination.parent() {
                        create_output_dirs(parent, &mut new_directories)?;
                    }
                    fs::rename(new.join(path), &destination)
                        .wrap_err_with(|| format!("cannot install {}", destination.display()))?;
                    if let Some((_, _, installed)) = moved.last_mut() {
                        *installed = true;
                    }
                }
            }
            before_commit()?;
            transaction.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            let mut failures = Vec::new();
            for (path, had_previous, installed) in moved.into_iter().rev() {
                let destination = target.join(path);
                let restore = if had_previous {
                    fs::rename(backup.join(path), &destination)
                } else if installed {
                    fs::remove_file(&destination)
                } else {
                    continue;
                };
                if let Err(error) = restore {
                    failures.push(format!("{}: {error}", destination.display()));
                }
            }
            for path in new_directories.into_iter().rev() {
                if let Err(error) = fs::remove_dir(&path) {
                    failures.push(format!("{}: {error}", path.display()));
                }
            }
            if !failures.is_empty() {
                let retained = self.files.keep();
                return Err(error.wrap_err(format!(
                    "LOD rollback failed ({}); backups remain in {}",
                    failures.join(", "),
                    retained.display(),
                )));
            }
            return Err(
                error.wrap_err("publishing LOD failed; the previous generation was restored")
            );
        }
        if let Err(error) = self.files.close() {
            eprintln!("Warning: could not remove LOD staging files: {error}");
        }
        Ok(())
    }
}

fn create_output_dirs(parent: &Path, created: &mut Vec<PathBuf>) -> Result<()> {
    let mut missing = Vec::new();
    for dir in parent.ancestors() {
        if dir.try_exists()? {
            break;
        }
        missing.push(dir);
    }
    for dir in missing.into_iter().rev() {
        fs::create_dir(dir)?;
        created.push(dir.to_path_buf());
    }
    Ok(())
}
