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
    AuditOptions, Baseline, SourceCatalog, audit_pack, compare_to_baseline, findings_csv,
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

/// Resolves `path` as far as it exists, then appends the rest, so a path that does not exist yet
/// still resolves symlinks in its existing part (`..` and aliases cannot hide an input tree).
fn resolve_for_write(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(resolved) = fs::canonicalize(existing) {
            let mut resolved = resolved;
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
        }
    }

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

    // Compare with the baseline before anything is written, so `--write-baseline` on the same
    // file cannot replace the ceiling it is about to be checked against.
    let mut failed = false;
    if let Some(path) = &args.baseline {
        let baseline = read_baseline(path)?;
        let regressions = compare_to_baseline(&report, &baseline);
        for line in &regressions {
            eprintln!("REGRESSION {line}");
        }
        failed |= !regressions.is_empty();
        let lower = improvements(&report, &baseline);
        for line in &lower {
            eprintln!(
                "{} {line}",
                if args.strict {
                    "STALE BASELINE"
                } else {
                    "note:"
                }
            );
        }
        failed |= args.strict && !lower.is_empty();
        if !failed {
            eprintln!("texture audit is within its baseline");
        }
    }
    if let Some(path) = &args.write_baseline {
        let mut next = Baseline::from_report(
            &report,
            "Written by texture-audit --write-baseline; lower a value when a cause is fixed",
        );
        if path.is_file() {
            // An existing baseline only goes down: refuse a write that would raise it or that
            // records an incomplete audit, and keep its note.
            let prior = read_baseline(path)?;
            let regressions = compare_to_baseline(&report, &prior);
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
            let reasons = converter::texture_audit::incomplete(&report);
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
    if failed {
        bail!("texture audit failed its baseline check");
    }
    Ok(())
}

fn read_baseline(path: &Path) -> Result<Baseline> {
    serde_json::from_slice(&fs::read(path)?)
        .wrap_err_with(|| format!("failed to read baseline {}", path.display()))
}
