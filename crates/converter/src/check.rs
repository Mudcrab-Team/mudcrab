//! Checks a converted output folder against its `conversion-manifest.json`
//! without converting anything.
//!
//! The quick mode looks at file metadata only (existence and size); the full
//! mode also re-hashes every artifact. The check only reads: it never writes
//! to the output folder, never creates files, and never reads a manifest path
//! that points outside the folder.

use crate::cache::{CONVERTER_SCHEMA_VERSION, CacheEntry, ConversionManifest, hash_file};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use rayon::prelude::*;
use std::{
    fmt, fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

/// File name of the manifest in an output root.
pub const MANIFEST_FILE_NAME: &str = "conversion-manifest.json";
/// File name of the world database in an output root.
pub const WORLD_DATABASE_FILE_NAME: &str = "skyrim_world.db";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    /// Existence and size of every artifact (metadata only).
    Quick,
    /// Quick, plus the SHA-256 of every artifact against the manifest.
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckProblem {
    /// The manifest was written by another converter schema.
    SchemaVersion {
        found: u32,
        expected: u32,
    },
    /// The conversion that wrote this folder did not finish cleanly.
    Incomplete,
    /// The manifest records inputs that failed to convert.
    RecordedFailures {
        count: usize,
    },
    /// `skyrim_world.db` is not in the output root.
    WorldDatabaseMissing,
    /// A manifest entry names a path that is absolute or leaves the folder;
    /// it is reported and never read.
    UnsafePath {
        output: String,
    },
    Missing {
        output: String,
    },
    WrongSize {
        output: String,
        expected: u64,
        found: u64,
    },
    /// Full mode only.
    WrongHash {
        output: String,
    },
    /// The artifact exists but could not be read.
    Unreadable {
        output: String,
        error: String,
    },
}

impl CheckProblem {
    /// True for problems with a single artifact, which a reconversion repairs
    /// by converting that artifact again.
    pub fn is_artifact_problem(&self) -> bool {
        matches!(
            self,
            Self::Missing { .. }
                | Self::WrongSize { .. }
                | Self::WrongHash { .. }
                | Self::Unreadable { .. }
        )
    }
}

impl fmt::Display for CheckProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaVersion { found, expected } => write!(
                f,
                "schema: the manifest is schema {found}, this converter writes schema {expected}"
            ),
            Self::Incomplete => write!(f, "incomplete: the manifest is not marked complete"),
            Self::RecordedFailures { count } => {
                write!(f, "failures: {count} input(s) failed to convert")
            }
            Self::WorldDatabaseMissing => write!(f, "missing: {WORLD_DATABASE_FILE_NAME}"),
            Self::UnsafePath { output } => {
                write!(f, "unsafe path: {output} (outside the output folder)")
            }
            Self::Missing { output } => write!(f, "missing: {output}"),
            Self::WrongSize {
                output,
                expected,
                found,
            } => write!(
                f,
                "wrong size: {output} ({found} bytes, expected {expected})"
            ),
            Self::WrongHash { output } => write!(f, "wrong hash: {output}"),
            Self::Unreadable { output, error } => write!(f, "unreadable: {output} ({error})"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheckReport {
    pub mode: CheckMode,
    /// Manifest entries looked at.
    pub files_checked: usize,
    /// Total size of the artifacts found (hashed, in full mode).
    pub bytes_checked: u64,
    pub elapsed: Duration,
    /// Folder-level problems first, then artifact problems sorted by path.
    pub problems: Vec<CheckProblem>,
}

impl CheckReport {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }

    /// What to do about the problems found, or `None` when there are none.
    pub fn advice(&self) -> Option<&'static str> {
        if self.is_ok() {
            return None;
        }
        if self
            .problems
            .iter()
            .any(|problem| matches!(problem, CheckProblem::SchemaVersion { .. }))
        {
            return Some(
                "This folder was written by another converter version: the next conversion into it redoes what the version change requires, usually everything.",
            );
        }
        if self
            .problems
            .iter()
            .any(|problem| matches!(problem, CheckProblem::UnsafePath { .. }))
        {
            return Some(
                "The manifest names paths outside this folder, so it was not written by the converter: convert into a fresh folder.",
            );
        }
        if self.problems.iter().any(CheckProblem::is_artifact_problem) {
            return Some(
                "Run the converter again on the same Data folder and this output: it re-converts only the files listed and reuses the rest.",
            );
        }
        Some(
            "Run the converter again on the same Data folder and this output: it retries the failed inputs listed in conversion-manifest.json and rebuilds the world database.",
        )
    }
}

/// The error a cancelled check returns. Find it with
/// `error.downcast_ref::<CheckCancelled>()` (or `error.is::<CheckCancelled>()`)
/// to tell a stop the user asked for from a manifest that could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckCancelled;

impl fmt::Display for CheckCancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the output check was cancelled")
    }
}

impl std::error::Error for CheckCancelled {}

/// Checks `output` against its manifest. `progress(done, total)` is called as
/// entries are checked, possibly from several threads.
///
/// Returns `Err` only when the manifest is missing or cannot be read; every
/// other finding is a [`CheckProblem`] in the report. It cannot be stopped:
/// front ends that need a stop button use [`check_output_with_cancel`].
pub fn check_output(
    output: &Path,
    mode: CheckMode,
    progress: impl Fn(usize, usize) + Sync,
) -> Result<CheckReport> {
    check_output_with_cancel(output, mode, progress, &AtomicBool::new(false))
}

/// [`check_output`] with a stop button: setting `cancel` (from any thread,
/// including the progress callback) stops the check. No new entries are
/// started once it is set; entries already being read on other threads
/// finish first, so the call returns within about one artifact's hash.
///
/// A check that sees `cancel` set before it finishes returns an `Err` holding
/// [`CheckCancelled`], never a partial report, so a stopped check cannot be
/// mistaken for a passed one. A flag already set on entry returns at once,
/// without reading the manifest.
pub fn check_output_with_cancel(
    output: &Path,
    mode: CheckMode,
    progress: impl Fn(usize, usize) + Sync,
    cancel: &AtomicBool,
) -> Result<CheckReport> {
    let cancelled = || cancel.load(Ordering::Relaxed);
    if cancelled() {
        return Err(CheckCancelled.into());
    }
    let started = Instant::now();
    let manifest = read_manifest(output)?;

    let mut problems = Vec::new();
    if manifest.schema_version != CONVERTER_SCHEMA_VERSION {
        problems.push(CheckProblem::SchemaVersion {
            found: manifest.schema_version,
            expected: CONVERTER_SCHEMA_VERSION,
        });
    }
    if !manifest.complete {
        problems.push(CheckProblem::Incomplete);
    }
    if !manifest.failures.is_empty() {
        problems.push(CheckProblem::RecordedFailures {
            count: manifest.failures.len(),
        });
    }
    if !output.join(WORLD_DATABASE_FILE_NAME).is_file() {
        problems.push(CheckProblem::WorldDatabaseMissing);
    }

    let entries: Vec<&CacheEntry> = manifest.entries.values().collect();
    let total = entries.len();
    // The resolved output folder, so an entry reached through a symbolic link that points outside
    // it is refused like a `..` path.
    let root = fs::canonicalize(output).unwrap_or_else(|_| output.to_path_buf());
    let done = AtomicUsize::new(0);
    progress(0, total);
    // `None` for an entry skipped after a cancel; collecting into `Option`
    // stops handing out entries at the first one.
    let outcomes: Option<Vec<(u64, Option<CheckProblem>)>> = entries
        .par_iter()
        .map(|entry| {
            if cancelled() {
                return None;
            }
            let outcome = check_entry(output, &root, entry, mode);
            progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
            Some(outcome)
        })
        .collect();
    let outcomes = match outcomes {
        Some(outcomes) if !cancelled() => outcomes,
        _ => return Err(CheckCancelled.into()),
    };

    let bytes_checked = outcomes.iter().map(|(bytes, _)| bytes).sum();
    let mut artifact_problems: Vec<CheckProblem> = outcomes
        .into_iter()
        .filter_map(|(_, problem)| problem)
        .collect();
    artifact_problems.sort_by(|left, right| artifact_path(left).cmp(artifact_path(right)));
    problems.extend(artifact_problems);

    Ok(CheckReport {
        mode,
        files_checked: total,
        bytes_checked,
        elapsed: started.elapsed(),
        problems,
    })
}

/// Reads the manifest as written, without the migrations
/// [`ConversionManifest::load`] applies for the next conversion: that reader
/// replaces a manifest of another schema with an empty one and treats a
/// missing file as an empty output, and the check must see both as they are.
fn read_manifest(output: &Path) -> Result<ConversionManifest> {
    if !output.is_dir() {
        bail!("{} is not a folder", output.display());
    }
    let path = output.join(MANIFEST_FILE_NAME);
    if !path.is_file() {
        bail!(
            "{} has no {MANIFEST_FILE_NAME}: it is not a converter output, or its conversion never finished",
            output.display()
        );
    }
    let bytes = fs::read(&path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("{} is not a valid conversion manifest", path.display()))
}

/// Returns the artifact bytes found and the problem, if any.
fn check_entry(
    output: &Path,
    root: &Path,
    entry: &CacheEntry,
    mode: CheckMode,
) -> (u64, Option<CheckProblem>) {
    let Some(relative) = contained_relative_path(&entry.output) else {
        return (
            0,
            Some(CheckProblem::UnsafePath {
                output: entry.output.clone(),
            }),
        );
    };
    let path = output.join(relative);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            return (
                0,
                Some(CheckProblem::Missing {
                    output: entry.output.clone(),
                }),
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (
                0,
                Some(CheckProblem::Missing {
                    output: entry.output.clone(),
                }),
            );
        }
        // The file is there but can't be read (permissions, a locked file): reconverting
        // would not fix that, so it is not reported as missing.
        Err(error) => {
            return (
                0,
                Some(CheckProblem::Unreadable {
                    output: entry.output.clone(),
                    error: error.to_string(),
                }),
            );
        }
    };
    // The path text stays inside the output, but a symbolic link on the way can still lead out.
    if fs::canonicalize(&path).is_ok_and(|resolved| !resolved.starts_with(root)) {
        return (
            0,
            Some(CheckProblem::UnsafePath {
                output: entry.output.clone(),
            }),
        );
    }
    let found = metadata.len();
    if found != entry.output_size {
        return (
            found,
            Some(CheckProblem::WrongSize {
                output: entry.output.clone(),
                expected: entry.output_size,
                found,
            }),
        );
    }
    if mode == CheckMode::Full {
        match hash_file(&path) {
            Ok(hash) if hash == entry.output_hash => {}
            Ok(_) => {
                return (
                    found,
                    Some(CheckProblem::WrongHash {
                        output: entry.output.clone(),
                    }),
                );
            }
            Err(error) => {
                return (
                    found,
                    Some(CheckProblem::Unreadable {
                        output: entry.output.clone(),
                        error: format!("{error:#}"),
                    }),
                );
            }
        }
    }
    (found, None)
}

/// The manifest path as a relative path inside the output, or `None` when it
/// is empty, absolute, names a drive or stream (`:`), or has a `..` segment.
/// Both separators are checked, so a manifest written on one system cannot
/// escape on another.
fn contained_relative_path(output: &str) -> Option<PathBuf> {
    if output.is_empty()
        || output.starts_with(['/', '\\'])
        || output.contains(':')
        || Path::new(output).is_absolute()
    {
        return None;
    }
    let mut relative = PathBuf::new();
    for segment in output.split(['/', '\\']) {
        match segment {
            ".." => return None,
            "" | "." => {}
            segment => relative.push(segment),
        }
    }
    (!relative.as_os_str().is_empty()).then_some(relative)
}

fn artifact_path(problem: &CheckProblem) -> &str {
    match problem {
        CheckProblem::UnsafePath { output }
        | CheckProblem::Missing { output }
        | CheckProblem::WrongSize { output, .. }
        | CheckProblem::WrongHash { output }
        | CheckProblem::Unreadable { output, .. } => output,
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_paths_inside_the_output() {
        assert_eq!(
            contained_relative_path("meshes/a/b.glb"),
            Some(PathBuf::from("meshes").join("a").join("b.glb"))
        );
        assert_eq!(
            contained_relative_path("textures\\x.ktx2"),
            Some(PathBuf::from("textures").join("x.ktx2"))
        );
        for unsafe_path in [
            "",
            "/etc/passwd",
            "\\\\server\\share\\x",
            "C:/Windows/x",
            "C:x",
            "../outside.glb",
            "meshes/../../outside.glb",
            "meshes\\..\\..\\outside.glb",
            "meshes/..",
            "./",
        ] {
            assert_eq!(
                contained_relative_path(unsafe_path),
                None,
                "{unsafe_path:?} was accepted"
            );
        }
    }

    #[test]
    fn advice_depends_on_what_was_found() {
        let report = |problems| CheckReport {
            mode: CheckMode::Quick,
            files_checked: 1,
            bytes_checked: 0,
            elapsed: Duration::ZERO,
            problems,
        };
        assert_eq!(report(Vec::new()).advice(), None);
        assert!(
            report(vec![CheckProblem::Missing {
                output: "meshes/x.glb".to_owned()
            }])
            .advice()
            .unwrap()
            .contains("only the files listed")
        );
        assert!(
            report(vec![
                CheckProblem::SchemaVersion {
                    found: 1,
                    expected: 2
                },
                CheckProblem::Missing {
                    output: "meshes/x.glb".to_owned()
                }
            ])
            .advice()
            .unwrap()
            .contains("another converter version")
        );
    }
}
