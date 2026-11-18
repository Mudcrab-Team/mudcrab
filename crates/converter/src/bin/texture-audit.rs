//! Audits every texture slot of a converted pack against the game data, read-only.
//!
//! ```text
//! texture-audit --pack <pack> --data <Skyrim Data> --scratch <dir>
//!     [--json report.json] [--csv report.csv] [--baseline baseline.json]
//!     [--write-baseline baseline.json] [--strict] [--mesh-filter text]
//! ```
//!
//! The pack and the game data are never written: every output path and `--scratch` must lie
//! outside both. The pack is held under its shared asset lock from the first read to the last.
//! The lock does not block: a conversion that wants the same output fails at once while the audit
//! runs, and the audit fails at once when a conversion holds the pack. The lock file sits beside
//! the pack (`<pack>.lock`). Source NIFs are extracted to `--scratch`.
//!
//! With `--baseline` the run exits non-zero when any cause has more unresolved slots than the
//! baseline allows or the audit is incomplete (an unreadable mesh or archive). `--strict` also
//! fails when a count is below the baseline, so the ceiling has to follow the count down.
//! `--write-baseline` never raises an existing baseline. A baseline needs the whole pack, so
//! both options refuse `--mesh-filter`.

use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use converter::texture_audit::{
    AuditOptions, Baseline, Report, SourceCatalog, audit_pack, compare_to_baseline, findings_csv,
    improvements, lock_pack, plugin_names,
};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

const USAGE: &str = "usage: texture-audit --pack <pack> --data <data> --scratch <dir> [--json f] [--csv f] [--baseline f] [--write-baseline f] [--strict] [--mesh-filter text]";

#[derive(Default)]
struct Args {
    pack: Option<PathBuf>,
    data: Option<PathBuf>,
    scratch: Option<PathBuf>,
    json: Option<PathBuf>,
    csv: Option<PathBuf>,
    baseline: Option<PathBuf>,
    write_baseline: Option<PathBuf>,
    strict: bool,
    mesh_filter: Option<String>,
}

fn parse() -> Result<Args> {
    let mut args = Args::default();
    let mut iter = env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .ok_or_else(|| color_eyre::eyre::eyre!("{name} needs a value"))
        };
        match flag.as_str() {
            "--pack" => args.pack = Some(value("--pack")?.into()),
            "--data" => args.data = Some(value("--data")?.into()),
            "--scratch" => args.scratch = Some(value("--scratch")?.into()),
            "--json" => args.json = Some(value("--json")?.into()),
            "--csv" => args.csv = Some(value("--csv")?.into()),
            "--baseline" => args.baseline = Some(value("--baseline")?.into()),
            "--write-baseline" => args.write_baseline = Some(value("--write-baseline")?.into()),
            "--strict" => args.strict = true,
            "--mesh-filter" => args.mesh_filter = Some(value("--mesh-filter")?),
            other => bail!("unknown argument {other}; {USAGE}"),
        }
    }
    Ok(args)
}

/// The most symbolic links `expand_symlinks` follows for one path. An engineering guard against a
/// link loop, not a data rule. 40 is the per-lookup limit of the Linux kernel: far more than any
/// real chain of links, and low enough to stop a loop at once.
const MAX_SYMLINK_HOPS: usize = 40;

/// One piece of a path, owned, so a symbolic link's target can be queued in front of the rest.
enum Part {
    Root(std::ffi::OsString),
    Current,
    Parent,
    Name(std::ffi::OsString),
}

fn parts(path: &Path) -> Vec<Part> {
    use std::path::Component;
    path.components()
        .map(|component| match component {
            Component::Prefix(_) | Component::RootDir => {
                Part::Root(component.as_os_str().to_owned())
            }
            Component::CurDir => Part::Current,
            Component::ParentDir => Part::Parent,
            Component::Normal(name) => Part::Name(name.to_owned()),
        })
        .collect()
}

/// Follows every symbolic link of `path`, including a dangling one, so the result names the file
/// a write would create. `fs::canonicalize` cannot do this for a link whose target does not exist
/// yet, and `fs::write` follows such a link and creates its target.
fn expand_symlinks(path: &Path) -> Result<PathBuf> {
    use std::collections::VecDeque;
    let mut resolved = PathBuf::new();
    let mut todo: VecDeque<Part> = parts(path).into();
    let mut hops = 0;
    while let Some(part) = todo.pop_front() {
        match part {
            Part::Root(root) => resolved.push(root),
            Part::Current => {}
            Part::Parent => {
                resolved.pop();
            }
            Part::Name(name) => {
                let next = resolved.join(&name);
                let is_link = fs::symlink_metadata(&next)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink());
                if !is_link {
                    resolved = next;
                    continue;
                }
                hops += 1;
                ensure!(
                    hops <= MAX_SYMLINK_HOPS,
                    "too many symbolic links while resolving {}",
                    path.display()
                );
                let target = fs::read_link(&next)
                    .wrap_err_with(|| format!("cannot read the link {}", next.display()))?;
                if target.is_absolute() {
                    resolved = PathBuf::new();
                }
                for part in parts(&target).into_iter().rev() {
                    todo.push_front(part);
                }
            }
        }
    }
    Ok(resolved)
}

/// Resolves `path` to the file a write would create: every symbolic link is followed, even one
/// that dangles, and the part that exists is canonicalized (so `..`, aliases and letter case
/// cannot hide an input tree). A part that does not exist yet is appended as written.
fn resolve_for_write(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let expanded = expand_symlinks(&absolute)?;
    let mut existing = expanded.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(mut resolved) = fs::canonicalize(existing) {
            for part in rest.iter().rev() {
                resolved.push(part);
            }
            return Ok(resolved);
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => bail!("cannot resolve {}", path.display()),
        }
    }
}

/// Refuses a write target inside an input tree, so the audit stays read-only.
fn ensure_outside(label: &str, path: &Path, inputs: &[(&str, &Path)]) -> Result<()> {
    let resolved = resolve_for_write(path)?;
    for (name, input) in inputs {
        ensure!(
            !resolved.starts_with(input),
            "{label} {} is inside the {name} {}; the audit is read-only, choose a path outside it",
            path.display(),
            input.display()
        );
    }
    Ok(())
}

/// Checks, before the audit starts, that an output can be created: the folder it would really be
/// written to exists and a file can be made there. A missing folder would otherwise fail the
/// write after a long run.
///
/// The path is resolved first, with the same resolver as [`ensure_outside`], so a symbolic link
/// is followed to the folder of its target. The probe file goes there and never into the folder
/// of the link, which can be an input folder. A real create attempt is used instead of a
/// permission bit because the bits do not show a read-only mount, an access list or the user
/// running the tool. Call this after `ensure_outside`, which proves the resolved folder lies
/// outside the pack and the game data.
fn ensure_parent_writable(label: &str, path: &Path) -> Result<()> {
    let resolved = resolve_for_write(path)?;
    let parent = match resolved.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    ensure!(
        parent.is_dir(),
        "the folder of {label} {} ({}) does not exist; create it first",
        path.display(),
        parent.display()
    );
    tempfile::NamedTempFile::new_in(parent).wrap_err_with(|| {
        format!(
            "cannot create a file next to {label} {} in {}; is the folder writable?",
            path.display(),
            parent.display()
        )
    })?;
    Ok(())
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let args = parse()?;
    let pack = args
        .pack
        .ok_or_else(|| color_eyre::eyre::eyre!("--pack is required; {USAGE}"))?;
    let data = args
        .data
        .ok_or_else(|| color_eyre::eyre::eyre!("--data is required; {USAGE}"))?;
    let scratch = args
        .scratch
        .ok_or_else(|| color_eyre::eyre::eyre!("--scratch is required; {USAGE}"))?;
    ensure!(
        args.mesh_filter.is_none() || (args.baseline.is_none() && args.write_baseline.is_none()),
        "--mesh-filter audits part of the pack, so a baseline check or write would compare the \
         wrong scope; drop --mesh-filter or the baseline option"
    );

    let pack = fs::canonicalize(&pack)
        .wrap_err_with(|| format!("pack {} does not exist", pack.display()))?;
    let data = fs::canonicalize(&data)
        .wrap_err_with(|| format!("data folder {} does not exist", data.display()))?;
    let inputs = [("pack", pack.as_path()), ("game data", data.as_path())];
    ensure_outside("--scratch", &scratch, &inputs)?;
    for (label, path) in [
        ("--json", &args.json),
        ("--csv", &args.csv),
        ("--write-baseline", &args.write_baseline),
    ] {
        if let Some(path) = path {
            ensure_outside(label, path, &inputs)?;
            ensure_parent_writable(label, path)?;
        }
    }

    ensure_distinct_paths(&[
        ("--json", args.json.as_deref()),
        ("--csv", args.csv.as_deref()),
        ("--baseline", args.baseline.as_deref()),
        ("--write-baseline", args.write_baseline.as_deref()),
    ])?;

    // Hold the pack from the first read (the plugin order) to the last (the database), so the
    // report describes one pack.
    let _lock = lock_pack(&pack)?;
    let database = pack.join("skyrim_world.db");
    let database = database.is_file().then_some(database);
    // Without the database the plugin order is unknown, so the converter's archive selection
    // would leave out the base game archives and most textures would look absent.
    ensure!(
        database.is_some() || (args.baseline.is_none() && args.write_baseline.is_none()),
        "{} has no skyrim_world.db; a baseline check or write needs the plugin order it holds",
        pack.display()
    );
    let plugin_names = match &database {
        Some(path) => plugin_names(path)?,
        None => Vec::new(),
    };
    eprintln!("indexing game data in {}", data.display());
    let sources = SourceCatalog::build(&data, &plugin_names)?;
    eprintln!(
        "game data: {} meshes, {} textures; {} archives skipped, {} left out as inactive",
        sources.mesh_count(),
        sources.texture_count(),
        sources.skipped_archives().len(),
        sources.inactive_archives().len()
    );
    let report = audit_pack(&AuditOptions {
        pack: &pack,
        sources: &sources,
        scratch: &scratch,
        database: database.as_deref(),
        mesh_filter: args.mesh_filter.as_deref(),
    })?;
    if let Some(path) = &args.json {
        fs::write(path, serde_json::to_vec_pretty(&report)?)
            .wrap_err_with(|| format!("failed to write {}", path.display()))?;
    }
    if let Some(path) = &args.csv {
        fs::write(path, findings_csv(&report))
            .wrap_err_with(|| format!("failed to write {}", path.display()))?;
    }
    println!("{}", serde_json::to_string_pretty(&report.summary)?);
    // An incomplete audit is shown even without a baseline, so a missing finding is not mistaken
    // for a clean pack.
    for reason in converter::texture_audit::incomplete(&report) {
        eprintln!("INCOMPLETE {reason}");
    }

    apply_baselines(
        &report,
        args.baseline.as_deref(),
        args.write_baseline.as_deref(),
        args.strict,
    )
}

/// Checks the report against `--baseline`, then writes `--write-baseline`.
///
/// The comparison comes first and a failed one ends the run before anything is written, so a
/// stale ceiling that `--strict` rejects is not silently replaced and a regression never reaches
/// the file. `--write-baseline` onto an existing file refuses to raise it.
fn apply_baselines(
    report: &Report,
    baseline: Option<&Path>,
    write: Option<&Path>,
    strict: bool,
) -> Result<()> {
    if let Some(path) = baseline {
        let baseline = read_baseline(path)?;
        let regressions = compare_to_baseline(report, &baseline);
        for line in &regressions {
            eprintln!("REGRESSION {line}");
        }
        let lower = improvements(report, &baseline);
        for line in &lower {
            eprintln!("{} {line}", if strict { "STALE BASELINE" } else { "note:" });
        }
        if !regressions.is_empty() || (strict && !lower.is_empty()) {
            bail!("texture audit failed its baseline check");
        }
        eprintln!("texture audit is within its baseline");
    }
    if let Some(path) = write {
        let mut next = Baseline::from_report(
            report,
            "Written by texture-audit --write-baseline; lower a value when a cause is fixed",
        );
        if path.is_file() {
            // An existing baseline only goes down: refuse a write that would raise it or that
            // records an incomplete audit, and keep its note.
            let prior = read_baseline(path)?;
            let regressions = compare_to_baseline(report, &prior);
            if !regressions.is_empty() {
                for line in &regressions {
                    eprintln!("REGRESSION {line}");
                }
                bail!(
                    "refusing to write {}: it would raise the baseline",
                    path.display()
                );
            }
            next.note = prior.note;
        } else {
            let reasons = converter::texture_audit::incomplete(report);
            ensure!(
                reasons.is_empty(),
                "refusing to write {}: the audit is incomplete: {}",
                path.display(),
                reasons.join("; ")
            );
        }
        fs::write(path, serde_json::to_vec_pretty(&next)?)?;
        eprintln!("baseline written to {}", path.display());
    }
    Ok(())
}

/// Refuses two outputs that name one file, and a report that names the baseline. `--baseline` and
/// `--write-baseline` may name the same file (compare, then lower it). Paths resolve through
/// symbolic links, so an alias cannot hide the overlap.
fn ensure_distinct_paths(paths: &[(&str, Option<&Path>)]) -> Result<()> {
    let mut seen: Vec<(&str, PathBuf)> = Vec::new();
    for (label, path) in paths {
        let Some(path) = path else { continue };
        let resolved = resolve_for_write(path)?;
        for (other, other_path) in &seen {
            let baseline_pair = matches!(
                (*other, *label),
                ("--baseline", "--write-baseline") | ("--write-baseline", "--baseline")
            );
            ensure!(
                *other_path != resolved || baseline_pair,
                "{label} and {other} name the same file {}; give each output its own path",
                path.display()
            );
        }
        seen.push((label, resolved));
    }
    Ok(())
}

fn read_baseline(path: &Path) -> Result<Baseline> {
    let bytes =
        fs::read(path).wrap_err_with(|| format!("failed to read baseline {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("failed to parse baseline {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use converter::texture_audit::Cause;

    /// A complete report with `count` unresolved slots of one cause.
    fn report_with(count: u64) -> Report {
        let mut report = Report::default();
        report.summary.glb_files = 1;
        report
            .summary
            .counts_by_cause
            .insert(Cause::SourceTextureAbsent, count);
        report
    }

    /// Writes a baseline whose only ceiling is `ceiling` slots of one cause.
    fn write_ceiling(path: &Path, ceiling: u64) {
        let baseline = Baseline::from_report(&report_with(ceiling), "kept note");
        fs::write(path, serde_json::to_vec_pretty(&baseline).unwrap()).unwrap();
    }

    fn ceiling(path: &Path) -> u64 {
        read_baseline(path).unwrap().max_by_cause[&Cause::SourceTextureAbsent]
    }

    #[test]
    fn a_failed_comparison_returns_before_any_baseline_write() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = dir.path().join("baseline.json");
        write_ceiling(&baseline, 5);

        // `--strict` rejects a stale ceiling (3 < 5). Writing the same file must not lower it
        // first and then fail.
        let error =
            apply_baselines(&report_with(3), Some(&baseline), Some(&baseline), true).unwrap_err();
        assert!(format!("{error:#}").contains("failed its baseline check"));
        assert_eq!(ceiling(&baseline), 5);

        // A regression (7 > 5) writes nothing, not even a new file.
        let fresh = dir.path().join("fresh.json");
        assert!(apply_baselines(&report_with(7), Some(&baseline), Some(&fresh), false).is_err());
        assert!(!fresh.exists());
        assert!(apply_baselines(&report_with(7), Some(&baseline), Some(&baseline), false).is_err());
        assert_eq!(ceiling(&baseline), 5);

        // Without `--strict` a stale ceiling passes the check and the write lowers it, keeping
        // the note.
        apply_baselines(&report_with(3), Some(&baseline), Some(&baseline), false).unwrap();
        assert_eq!(ceiling(&baseline), 3);
        assert_eq!(read_baseline(&baseline).unwrap().note, "kept note");
    }

    #[test]
    fn outputs_may_not_name_the_baseline_or_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let (json, csv, baseline) = (
            base.join("report.json"),
            base.join("report.csv"),
            base.join("baseline.json"),
        );
        let paths = |json: &Path, csv: &Path, check: &Path, write: &Path| {
            ensure_distinct_paths(&[
                ("--json", Some(json)),
                ("--csv", Some(csv)),
                ("--baseline", Some(check)),
                ("--write-baseline", Some(write)),
            ])
        };
        // Distinct files, and `--baseline` with `--write-baseline` on one file, are fine.
        paths(&json, &csv, &baseline, &baseline).unwrap();
        // A report that names the baseline would overwrite it before it is read.
        let error = paths(&baseline, &csv, &baseline, &baseline).unwrap_err();
        assert!(
            format!("{error:#}").contains("--baseline and --json"),
            "{error:#}"
        );
        let error = paths(&json, &baseline, &baseline, &base.join("next.json")).unwrap_err();
        assert!(
            format!("{error:#}").contains("--baseline and --csv"),
            "{error:#}"
        );
        // Two reports on one file, and `--write-baseline` on a report.
        assert!(paths(&json, &json, &baseline, &baseline).is_err());
        assert!(paths(&json, &csv, &baseline, &csv).is_err());
        // A different spelling of the same file counts as the same file.
        let spelled = base.join("sub/../baseline.json");
        assert!(paths(&spelled, &csv, &baseline, &baseline).is_err());
    }

    #[test]
    fn an_output_folder_that_is_missing_is_refused_before_the_audit() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("report.json");
        ensure_parent_writable("--json", &existing).unwrap();
        // The check leaves no file behind.
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
        let missing = dir.path().join("new/dir/report.json");
        let error = ensure_parent_writable("--json", &missing).unwrap_err();
        assert!(format!("{error:#}").contains("does not exist"), "{error:#}");
    }

    /// An output link inside a read-only pack folder that points to a writable folder outside it
    /// is a valid output. The probe goes beside the target, and nothing is created in the pack.
    #[cfg(unix)]
    #[test]
    fn an_output_link_is_probed_beside_its_target_not_in_the_link_folder() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let pack = base.join("pack");
        let outside = base.join("outside");
        fs::create_dir_all(&pack).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let link = pack.join("report.json");
        std::os::unix::fs::symlink(outside.join("real.json"), &link).unwrap();
        let listing = |folder: &Path| -> Vec<_> {
            let mut names: Vec<_> = fs::read_dir(folder)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let before = listing(&pack);
        fs::set_permissions(&pack, fs::Permissions::from_mode(0o555)).unwrap();
        // The containment check accepts the resolved target, and so must the folder check.
        let outside_inputs = ensure_outside("--json", &link, &[("pack", pack.as_path())]);
        let result = ensure_parent_writable("--json", &link);
        // Restore access first so the temporary folder can be removed even if an assert fails.
        fs::set_permissions(&pack, fs::Permissions::from_mode(0o755)).unwrap();
        // The link lies in the pack folder, but the file a write would create does not.
        outside_inputs.unwrap();
        result.unwrap();
        assert_eq!(
            listing(&pack),
            before,
            "the probe must not touch the pack folder"
        );
        assert!(
            listing(&outside).is_empty(),
            "the probe file is removed again"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_folder_that_does_not_exist_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let link = base.join("report.json");
        std::os::unix::fs::symlink(base.join("missing/real.json"), &link).unwrap();
        let error = ensure_parent_writable("--json", &link).unwrap_err();
        assert!(format!("{error:#}").contains("does not exist"), "{error:#}");
    }

    #[test]
    fn a_missing_baseline_names_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.json");
        let error = read_baseline(&missing).unwrap_err();
        assert!(
            format!("{error:#}").contains(&missing.display().to_string()),
            "{error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_the_baseline_cannot_be_used_as_a_report_path() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let baseline = base.join("baseline.json");
        let alias = base.join("report.json");
        std::os::unix::fs::symlink(&baseline, &alias).unwrap();
        let error = ensure_distinct_paths(&[
            ("--json", Some(alias.as_path())),
            ("--baseline", Some(baseline.as_path())),
        ])
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("name the same file"),
            "{error:#}"
        );
    }

    #[cfg(unix)]
    mod symlinks {
        use super::*;
        use std::os::unix::fs::symlink;

        /// A pack folder and an outside folder, both canonical.
        fn folders() -> (tempfile::TempDir, PathBuf, PathBuf) {
            let root = tempfile::tempdir().unwrap();
            let base = fs::canonicalize(root.path()).unwrap();
            let pack = base.join("pack");
            let outside = base.join("outside");
            fs::create_dir_all(&pack).unwrap();
            fs::create_dir_all(&outside).unwrap();
            (root, pack, outside)
        }

        fn outside_pack(path: &Path, pack: &Path) -> Result<()> {
            ensure_outside("--json", path, &[("pack", pack)])
        }

        #[test]
        fn a_plain_path_outside_the_inputs_is_accepted() {
            let (_root, pack, outside) = folders();
            outside_pack(&outside.join("report.json"), &pack).unwrap();
            outside_pack(&outside.join("new/dir/report.json"), &pack).unwrap();
            // `..` cannot walk back into the pack.
            let error = outside_pack(&outside.join("../pack/report.json"), &pack).unwrap_err();
            assert!(
                format!("{error:#}").contains("inside the pack"),
                "{error:#}"
            );
        }

        #[test]
        fn a_dangling_output_link_into_the_pack_is_refused() {
            let (_root, pack, outside) = folders();
            // `pack/new.json` does not exist, so the link dangles. `fs::write` through it would
            // create the file inside the pack.
            symlink(pack.join("new.json"), outside.join("report.json")).unwrap();
            assert!(!pack.join("new.json").exists());
            let error = outside_pack(&outside.join("report.json"), &pack).unwrap_err();
            assert!(
                format!("{error:#}").contains("inside the pack"),
                "{error:#}"
            );
            // The same with a relative target and with a chain of links.
            symlink("../pack/other.json", outside.join("relative.json")).unwrap();
            assert!(outside_pack(&outside.join("relative.json"), &pack).is_err());
            symlink(outside.join("relative.json"), outside.join("chain.json")).unwrap();
            assert!(outside_pack(&outside.join("chain.json"), &pack).is_err());
        }

        #[test]
        fn a_dangling_folder_link_into_the_pack_is_refused() {
            let (_root, pack, outside) = folders();
            // The folder the link names does not exist yet, so the file below it is new too.
            symlink(pack.join("missing-folder"), outside.join("dir")).unwrap();
            assert!(outside_pack(&outside.join("dir/report.json"), &pack).is_err());
        }

        #[test]
        fn a_link_that_stays_outside_the_inputs_is_accepted_and_a_loop_is_refused() {
            let (_root, pack, outside) = folders();
            symlink(outside.join("elsewhere.json"), outside.join("report.json")).unwrap();
            outside_pack(&outside.join("report.json"), &pack).unwrap();
            symlink(outside.join("b"), outside.join("a")).unwrap();
            symlink(outside.join("a"), outside.join("b")).unwrap();
            let error = outside_pack(&outside.join("a"), &pack).unwrap_err();
            assert!(
                format!("{error:#}").contains("too many symbolic links"),
                "{error:#}"
            );
        }
    }
}
