//! Audits every texture slot of a converted pack against the game data, read-only.
//!
//! ```text
//! texture-audit --pack <pack> --data <Skyrim Data> --scratch <dir>
//!     [--json report.json] [--csv report.csv] [--baseline baseline.json]
//!     [--write-baseline baseline.json] [--mesh-filter text]
//! ```
//!
//! The pack and the game data are never written. Source NIFs are extracted to `--scratch`.
//! With `--baseline` the run exits non-zero when any cause has more unresolved slots than the
//! baseline allows.

use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::texture_audit::{
    AuditOptions, Baseline, SourceCatalog, audit_pack, compare_to_baseline, findings_csv,
};
use rusqlite::{Connection, OpenFlags};
use std::{env, fs, path::PathBuf};

#[derive(Default)]
struct Args {
    pack: Option<PathBuf>,
    data: Option<PathBuf>,
    scratch: Option<PathBuf>,
    json: Option<PathBuf>,
    csv: Option<PathBuf>,
    baseline: Option<PathBuf>,
    write_baseline: Option<PathBuf>,
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
            "--mesh-filter" => args.mesh_filter = Some(value("--mesh-filter")?),
            other => bail!(
                "unknown argument {other}; usage: texture-audit --pack <pack> --data <data> --scratch <dir> [--json f] [--csv f] [--baseline f] [--write-baseline f] [--mesh-filter text]"
            ),
        }
    }
    Ok(args)
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let args = parse()?;
    let pack = args.pack.ok_or_else(|| color_eyre::eyre::eyre!("--pack is required"))?;
    let data = args.data.ok_or_else(|| color_eyre::eyre::eyre!("--data is required"))?;
    let scratch = args
        .scratch
        .ok_or_else(|| color_eyre::eyre::eyre!("--scratch is required"))?;
    let database = pack.join("skyrim_world.db");
    let database = database.is_file().then_some(database);
    let plugin_names = match &database {
        Some(path) => {
            let connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let mut statement =
                connection.prepare("SELECT name FROM plugins ORDER BY priority")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        }
        None => Vec::new(),
    };
    eprintln!("indexing game data in {}", data.display());
    let sources = SourceCatalog::build(&data, &plugin_names)?;
    eprintln!(
        "game data: {} meshes, {} textures",
        sources.mesh_count(),
        sources.texture_count()
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
    if let Some(path) = &args.write_baseline {
        let baseline = Baseline::from_report(&report, "regenerate with --write-baseline");
        fs::write(path, serde_json::to_vec_pretty(&baseline)?)?;
        eprintln!("baseline written to {}", path.display());
    }
    if let Some(path) = &args.baseline {
        let baseline: Baseline = serde_json::from_slice(&fs::read(path)?)
            .wrap_err_with(|| format!("failed to read baseline {}", path.display()))?;
        let regressions = compare_to_baseline(&report, &baseline);
        if !regressions.is_empty() {
            for line in &regressions {
                eprintln!("REGRESSION {line}");
            }
            bail!("texture audit exceeds its baseline");
        }
        eprintln!("texture audit is within its baseline");
    }
    Ok(())
}
