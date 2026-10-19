//! Produce a diagnostic in-house database/cache bundle without asset conversion.
use color_eyre::{Result, eyre::ensure};
use std::{ffi::OsString, path::PathBuf, time::Instant};

/// Explicit options for the records-only export and its selected typed evidence.
struct RecordOptions {
    signatures: Vec<[u8; 4]>,
    work: Option<PathBuf>,
    mode: converter::esm::inhouse::RecordBundleMode,
}

#[cfg(test)]
/// Expose the established option pair to behavior tests.
fn parse_record_options(args: &[OsString]) -> Result<(Vec<[u8; 4]>, Option<PathBuf>)> {
    let options = parse_record_options_with_mode(args)?;
    Ok((options.signatures, options.work))
}

/// Parse explicit diagnostic scopes without changing legacy option defaults.
fn parse_record_options_with_mode(args: &[OsString]) -> Result<RecordOptions> {
    use converter::esm::inhouse::RecordBundleMode;
    let mut signatures = Vec::new();
    let mut typed_seen = false;
    let mut work = None;
    let mut mode = RecordBundleMode::Full;
    let mut index = 0;
    while index < args.len() {
        let option = &args[index];
        if option == "--record-archive-only" {
            ensure!(
                mode == RecordBundleMode::Full,
                "duplicate record-archive-only option"
            );
            mode = RecordBundleMode::RecordArchiveOnly;
            index += 1;
            continue;
        }
        ensure!(
            option == "--typed-signatures" || option == "--database-work-dir",
            "unknown diagnostic option"
        );
        let value = args
            .get(index + 1)
            .ok_or_else(|| color_eyre::eyre::eyre!("diagnostic option requires a value"))?;
        ensure!(
            !value.is_empty()
                && value != "--typed-signatures"
                && value != "--database-work-dir"
                && value != "--record-archive-only",
            "diagnostic option requires a value"
        );
        if option == "--typed-signatures" {
            ensure!(!typed_seen, "duplicate typed-signatures option");
            typed_seen = true;
            let text = value
                .to_str()
                .ok_or_else(|| color_eyre::eyre::eyre!("typed signatures must be ASCII"))?;
            for tag in text.split(',') {
                ensure!(
                    tag.len() == 4 && tag.is_ascii(),
                    "typed signatures must be four ASCII bytes"
                );
                signatures.push(tag.as_bytes().try_into().expect("checked signature"));
            }
        } else {
            ensure!(work.is_none(), "duplicate database-work-dir option");
            work = Some(PathBuf::from(value));
        }
        index += 2;
    }
    Ok(RecordOptions {
        signatures,
        work,
        mode,
    })
}

#[cfg(test)]
#[path = "../inhouse_records_option_tests.rs"]
mod record_option_tests;

/// Export selected plugins to a fresh evidence directory without publishing a runtime pack.
fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "history-source") {
        return source_history(&args[1..], false);
    }
    if args.first().is_some_and(|arg| arg == "history-physical") {
        return source_history(&args[1..], true);
    }
    if args.first().is_some_and(|arg| arg == "history") {
        // Per-version data failures are represented in the report, and a bad pack
        // receives a diagnostic without making this inspection command fail as a whole.
        match history(&args[1..]) {
            Ok(()) => {}
            Err(error) => println!("history unavailable: {error}"),
        }
        return Ok(());
    }
    args.retain(|arg| arg != "--no-lod"); // This diagnostic exporter never compiles LOD.
    ensure!(
        args.len() >= 3,
        "usage: inhouse-records <Skyrim Data> <plugins.txt> <new output directory> --no-lod [--typed-signatures WEAP,NPC_,SPEL] [--database-work-dir <new directory>] [--record-archive-only]; or history <pack> <hex FormID> [--data DIR] [--json]"
    );
    let data = PathBuf::from(&args[0]);
    let plugins = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);
    let options = parse_record_options_with_mode(&args[3..])?;
    let started = Instant::now();
    let cells = converter::esm::inhouse::export_record_bundle_typed_with_options(
        &data,
        &plugins,
        &output,
        &options.signatures,
        options.work.as_deref(),
        options.mode,
    )?;
    println!(
        "inhouse diagnostic records: mode={:?} cells={cells} seconds={:.3} output={}",
        options.mode,
        started.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}

/// Emit each exact raw source version as one JSONL row and then a bounded summary.
/// A partial/unavailable raw result is retained in output and returns an error status.
fn source_history(args: &[std::ffi::OsString], physical: bool) -> Result<()> {
    use converter::records::history_source::{
        SourceQuery, SourceSummary, SourceVersion, visit_source_history,
    };
    use std::io::Write;
    #[derive(serde::Serialize)]
    struct VersionLine<'a> {
        kind: &'static str,
        version: &'a SourceVersion,
    }
    #[derive(serde::Serialize)]
    struct SummaryLine<'a> {
        kind: &'static str,
        summary: &'a SourceSummary,
    }
    ensure!(
        args.len() == 3 || args.len() == 5,
        "history-source <pack> <owner basename> <hex full local> [--data DIR]; history-physical <pack> <decimal plugin id> <decimal offset> [--data DIR]"
    );
    let pack = PathBuf::from(&args[0]);
    let query = if physical {
        SourceQuery::Physical {
            plugin_id: args[1].to_string_lossy().parse()?,
            source_record_offset: args[2].to_string_lossy().parse()?,
        }
    } else {
        let local = args[2].to_string_lossy();
        SourceQuery::Source {
            original_owner: args[1].to_string_lossy().into_owned(),
            full_local: u32::from_str_radix(
                local.trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )?,
        }
    };
    let data = if args.len() == 5 {
        ensure!(args[3] == "--data", "unknown source-history option");
        Some(PathBuf::from(&args[4]))
    } else {
        None
    };
    let stdout = std::io::stdout();
    let mut writer = std::io::BufWriter::new(stdout.lock());
    let summary = visit_source_history(&pack, data.as_deref(), query, |version| {
        serde_json::to_writer(
            &mut writer,
            &VersionLine {
                kind: "source_version",
                version: &version,
            },
        )?;
        writer.write_all(b"\n")?;
        Ok(())
    })?;
    serde_json::to_writer(
        &mut writer,
        &SummaryLine {
            kind: "source_summary",
            summary: &summary,
        },
    )?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    ensure!(
        summary.fully_raw_available,
        "raw source access is partial, unavailable or empty; see retained JSONL summary"
    );
    Ok(())
}

/// Inspect source versions without altering a pack or writing to source plugins.
fn history(args: &[std::ffi::OsString]) -> Result<()> {
    ensure!(
        args.len() >= 2,
        "usage: inhouse-records history <pack> <hex FormID> [--data DIR] [--json]"
    );
    let pack = PathBuf::from(&args[0]);
    let id = args[1].to_string_lossy();
    let id = u32::from_str_radix(id.trim_start_matches("0x").trim_start_matches("0X"), 16)?;
    let mut data = None;
    let mut json = false;
    let mut cursor = 2;
    while cursor < args.len() {
        if args[cursor] == "--json" {
            json = true;
            cursor += 1;
        } else if args[cursor] == "--data" && cursor + 1 < args.len() {
            data = Some(PathBuf::from(&args[cursor + 1]));
            cursor += 2;
        } else {
            color_eyre::eyre::bail!("unknown or incomplete history option");
        }
    }
    let report = converter::records::history::read_history(&pack, data.as_deref(), id)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", converter::records::history::display(&report));
    }
    Ok(())
}
