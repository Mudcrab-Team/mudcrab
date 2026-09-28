use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::{AssetPipeline, PipelineConfig, ProgressEvent};
use serde::Serialize;
use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

#[derive(Debug)]
struct Cli {
    data: PathBuf,
    output: PathBuf,
    resume_staging: Option<PathBuf>,
    report_json: Option<PathBuf>,
    cpu_jobs: Option<usize>,
    io_jobs: Option<usize>,
    fail_fast: bool,
    invalidate_cache: bool,
    verify_cache: bool,
}

#[derive(Debug)]
struct CheckCli {
    output: PathBuf,
    full: bool,
}

#[derive(Debug)]
enum Command {
    Convert(Cli),
    Check(CheckCli),
}

/// Problem lines printed before "and N more".
const CHECK_PROBLEM_LINES: usize = 20;

#[derive(Debug, Serialize)]
struct FailureReport {
    complete: bool,
    stage: Option<converter::ProgressStage>,
    file: Option<PathBuf>,
    error: String,
    elapsed_ms: u128,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    suppress_caught_nif_parser_panics();
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // Asking for help is not an error: print the usage and exit successfully.
    if args
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{}", usage());
        return Ok(());
    }
    let cli = match parse_command(args)? {
        Command::Convert(cli) => cli,
        Command::Check(check) => std::process::exit(run_check(&check)),
    };
    let mut config = PipelineConfig::new(cli.data, cli.output);
    config.resume_staging = cli.resume_staging;
    config.fail_fast = cli.fail_fast;
    config.invalidate_cache = cli.invalidate_cache;
    config.verify_cache = cli.verify_cache;
    if let Some(cpu_jobs) = cli.cpu_jobs {
        config.cpu_jobs = cpu_jobs;
    }
    if let Some(io_jobs) = cli.io_jobs {
        config.io_jobs = io_jobs;
    }
    let started = Instant::now();
    let last_progress = Arc::new(Mutex::new(None::<ProgressEvent>));
    let printer_progress = Arc::clone(&last_progress);
    let (tx, mut rx) = mpsc::channel::<ProgressEvent>(128);
    let printer = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            *printer_progress.lock().expect("progress mutex poisoned") = Some(event.clone());
            println!(
                "{:?} {:.0}% {}",
                event.stage,
                event.fraction() * 100.0,
                event.message
            );
        }
    });
    let pipeline_result = AssetPipeline::run_async(config, tx).await;
    printer.await?;
    let report = match pipeline_result {
        Ok(report) => report,
        Err(error) => {
            if let Some(path) = &cli.report_json {
                let progress = last_progress
                    .lock()
                    .expect("progress mutex poisoned")
                    .clone();
                let failure = FailureReport {
                    complete: false,
                    stage: progress.as_ref().map(|event| event.stage),
                    file: progress.and_then(|event| event.current_file),
                    error: format!("{error:#}"),
                    elapsed_ms: started.elapsed().as_millis(),
                };
                write_json_atomic(path, &failure)?;
            }
            return Err(error);
        }
    };
    if let Some(path) = &cli.report_json {
        write_json_atomic(path, &report)?;
    }
    println!(
        "Converted {}, reused {}, skipped {} in {} ms (complete: {})",
        report.converted, report.cache_hits, report.skipped, report.elapsed_ms, report.complete
    );
    if !report.complete {
        bail!(
            "conversion produced {} warning(s) and {} skipped input(s); see conversion-manifest.json",
            report.warnings.len(),
            report.skipped
        );
    }
    Ok(())
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("json.{}.partial", std::process::id()));
    let backup = path.with_extension(format!("json.{}.backup", std::process::id()));
    if temporary.exists() || backup.exists() {
        bail!("refusing to overwrite stale report temporary file");
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = fs::File::create(&temporary)
        .wrap_err_with(|| format!("failed to create {}", temporary.display()))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);

    if path.exists() {
        fs::rename(path, &backup)
            .wrap_err_with(|| format!("failed to preserve previous report {}", path.display()))?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(error).wrap_err_with(|| format!("failed to publish {}", path.display()));
    }
    if backup.exists() {
        fs::remove_file(backup)?;
    }
    Ok(())
}

fn suppress_caught_nif_parser_panics() {
    let report_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let is_nif_parser = info
            .location()
            .is_some_and(|location| location.file().contains("project-wormhole-nif-"));
        if !is_nif_parser {
            report_panic(info);
        }
    }));
}

/// Exit code: 0 all good, 1 problems found, 2 the manifest could not be read.
fn run_check(check: &CheckCli) -> i32 {
    let mode = if check.full {
        converter::CheckMode::Full
    } else {
        converter::CheckMode::Quick
    };
    // A progress line only for checks slower than a moment; it is erased
    // before the result is printed.
    let started = Instant::now();
    let last_print = Mutex::new(None::<Instant>);
    let progress = |done: usize, total: usize| {
        let Ok(mut last) = last_print.try_lock() else {
            return;
        };
        let due = match *last {
            None => started.elapsed() >= Duration::from_secs(1),
            Some(printed) => printed.elapsed() >= Duration::from_millis(250),
        };
        if due {
            *last = Some(Instant::now());
            eprint!("\rChecking {done}/{total} files");
            let _ = std::io::stderr().flush();
        }
    };
    let result = converter::check_output(&check.output, mode, progress);
    if last_print.lock().is_ok_and(|last| last.is_some()) {
        eprint!("\r{:48}\r", "");
    }
    let code = match result {
        Ok(report) => {
            print!("{}", format_check_report(&report));
            if report.is_ok() { 0 } else { 1 }
        }
        Err(error) => {
            eprintln!("check failed: {error:#}");
            2
        }
    };
    let _ = std::io::stdout().flush();
    code
}

fn format_check_report(report: &converter::CheckReport) -> String {
    let mode = match report.mode {
        converter::CheckMode::Quick => "quick check: existence and size",
        converter::CheckMode::Full => "full check: size and hash",
    };
    let seconds = report.elapsed.as_secs_f64();
    let bytes = format_bytes(report.bytes_checked);
    if report.is_ok() {
        return format!(
            "All good: {} files, {bytes}, {mode}, {seconds:.1} s\n",
            report.files_checked
        );
    }
    let mut text = format!(
        "{} problem(s) in {} files, {bytes}, {mode}, {seconds:.1} s:\n",
        report.problems.len(),
        report.files_checked
    );
    for problem in report.problems.iter().take(CHECK_PROBLEM_LINES) {
        text.push_str(&format!("  {problem}\n"));
    }
    if report.problems.len() > CHECK_PROBLEM_LINES {
        text.push_str(&format!(
            "  and {} more\n",
            report.problems.len() - CHECK_PROBLEM_LINES
        ));
    }
    if let Some(advice) = report.advice() {
        text.push_str(advice);
        text.push('\n');
    }
    text
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn parse_command(args: Vec<OsString>) -> Result<Command> {
    if args.first().and_then(|argument| argument.to_str()) == Some("check") {
        return parse_check(args.into_iter().skip(1)).map(Command::Check);
    }
    parse_cli(args).map(Command::Convert)
}

fn parse_check(args: impl Iterator<Item = OsString>) -> Result<CheckCli> {
    let mut positional = Vec::new();
    let mut full = false;
    for argument in args {
        match argument.to_str() {
            Some("--full") => full = true,
            Some("--help" | "-h") => bail!(usage()),
            Some(flag) if flag.starts_with('-') => bail!("unknown option {flag}\n{}", usage()),
            _ => positional.push(PathBuf::from(argument)),
        }
    }
    if positional.len() != 1 {
        bail!(usage());
    }
    Ok(CheckCli {
        output: positional.remove(0),
        full,
    })
}

fn parse_cli(args: Vec<OsString>) -> Result<Cli> {
    let mut positional = Vec::new();
    let mut report_json = None;
    let mut resume_staging = None;
    let mut cpu_jobs = None;
    let mut io_jobs = None;
    let mut fail_fast = false;
    let mut invalidate_cache = false;
    let mut verify_cache = true;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--report-json") => {
                report_json = Some(PathBuf::from(next_value(&mut args, "--report-json")?))
            }
            Some("--resume-staging") => {
                resume_staging = Some(PathBuf::from(next_value(&mut args, "--resume-staging")?))
            }
            Some("--cpu-jobs") => {
                cpu_jobs = Some(parse_jobs(
                    next_value(&mut args, "--cpu-jobs")?,
                    "--cpu-jobs",
                )?)
            }
            Some("--io-jobs") => {
                io_jobs = Some(parse_jobs(
                    next_value(&mut args, "--io-jobs")?,
                    "--io-jobs",
                )?)
            }
            Some("--fail-fast") => fail_fast = true,
            Some("--invalidate-cache") => invalidate_cache = true,
            Some("--no-verify-cache") => verify_cache = false,
            Some("--help" | "-h") => bail!(usage()),
            Some(flag) if flag.starts_with('-') => bail!("unknown option {flag}\n{}", usage()),
            _ => positional.push(PathBuf::from(argument)),
        }
    }
    if positional.is_empty() || positional.len() > 2 {
        bail!(usage());
    }
    Ok(Cli {
        data: positional.remove(0),
        output: positional
            .pop()
            .unwrap_or_else(|| PathBuf::from("modern_assets")),
        resume_staging,
        report_json,
        cpu_jobs,
        io_jobs,
        fail_fast,
        invalidate_cache,
        verify_cache,
    })
}

fn next_value(args: &mut impl Iterator<Item = OsString>, option: &str) -> Result<OsString> {
    args.next()
        .ok_or_else(|| color_eyre::eyre::eyre!("{option} requires a value"))
}

fn parse_jobs(value: OsString, option: &str) -> Result<usize> {
    value
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("{option} value is not valid UTF-8"))?
        .parse()
        .wrap_err_with(|| format!("{option} requires a positive integer"))
}

fn usage() -> &'static str {
    "usage: converter <Skyrim Data> [output directory] [--cpu-jobs N] [--io-jobs N] [--fail-fast] [--invalidate-cache] [--no-verify-cache] [--resume-staging DIR] [--report-json FILE]
       converter check <output directory> [--full]
         checks a converted output against its conversion-manifest.json without converting:
         existence and size of every file, and with --full their hashes too.
         Exit code 0: all good, 1: problems found, 2: no readable manifest."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pipeline_options() {
        let cli = parse_cli(
            [
                "Data",
                "output",
                "--cpu-jobs",
                "8",
                "--io-jobs",
                "2",
                "--fail-fast",
                "--invalidate-cache",
                "--no-verify-cache",
                "--report-json",
                "report.json",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
        .unwrap();
        assert_eq!(cli.cpu_jobs, Some(8));
        assert_eq!(cli.io_jobs, Some(2));
        assert!(cli.fail_fast);
        assert!(cli.invalidate_cache);
        assert!(!cli.verify_cache);
        assert_eq!(cli.report_json, Some(PathBuf::from("report.json")));
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_the_check_subcommand() {
        let Command::Check(check) = parse_command(args(&["check", "converted"])).unwrap() else {
            panic!("check was not parsed as a subcommand");
        };
        assert_eq!(check.output, PathBuf::from("converted"));
        assert!(!check.full);

        let Command::Check(check) = parse_command(args(&["check", "--full", "converted"])).unwrap()
        else {
            panic!("check was not parsed as a subcommand");
        };
        assert_eq!(check.output, PathBuf::from("converted"));
        assert!(check.full);
    }

    #[test]
    fn rejects_malformed_check_arguments() {
        for arguments in [
            &["check"][..],
            &["check", "a", "b"],
            &["check", "converted", "--cpu-jobs", "2"],
            &["check", "converted", "--help"],
        ] {
            assert!(
                parse_command(args(arguments)).is_err(),
                "{arguments:?} was accepted"
            );
        }
        let usage = parse_command(args(&["check", "--help"]))
            .unwrap_err()
            .to_string();
        assert!(usage.contains("converter check <output directory> [--full]"));
    }

    #[test]
    fn keeps_the_conversion_form_without_a_subcommand() {
        let Command::Convert(cli) = parse_command(args(&["Data", "check"])).unwrap() else {
            panic!("a conversion was parsed as a check");
        };
        assert_eq!(cli.data, PathBuf::from("Data"));
        assert_eq!(cli.output, PathBuf::from("check"));
    }

    #[test]
    fn formats_a_check_report() {
        let problems = (0..23)
            .map(|index| converter::CheckProblem::Missing {
                output: format!("meshes/{index:02}.glb"),
            })
            .collect();
        let report = converter::CheckReport {
            mode: converter::CheckMode::Quick,
            files_checked: 100,
            bytes_checked: 3 * 1024 * 1024,
            elapsed: Duration::from_millis(200),
            problems,
        };
        let text = format_check_report(&report);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "23 problem(s) in 100 files, 3.0 MiB, quick check: existence and size, 0.2 s:"
        );
        assert_eq!(lines[1], "  missing: meshes/00.glb");
        assert_eq!(lines[20], "  missing: meshes/19.glb");
        assert_eq!(lines[21], "  and 3 more");
        assert!(lines[22].contains("only the files listed"));

        let ok = converter::CheckReport {
            problems: Vec::new(),
            ..report
        };
        assert_eq!(
            format_check_report(&ok),
            "All good: 100 files, 3.0 MiB, quick check: existence and size, 0.2 s\n"
        );
    }

    #[test]
    fn atomically_replaces_json_report() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("conversion-report.json");
        fs::write(&report, b"old report").unwrap();

        let failure = FailureReport {
            complete: false,
            stage: Some(converter::ProgressStage::Extracting),
            file: Some(PathBuf::from("Skyrim - Animations.bsa")),
            error: "unsupported flags".to_owned(),
            elapsed_ms: 42,
        };
        write_json_atomic(&report, &failure).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
        assert_eq!(value["complete"], false);
        assert_eq!(value["stage"], "extracting");
        assert_eq!(value["file"], "Skyrim - Animations.bsa");
        assert_eq!(value["error"], "unsupported flags");
        assert_eq!(value["elapsed_ms"], 42);
        assert_eq!(
            fs::read_dir(directory.path()).unwrap().count(),
            1,
            "temporary report files were not cleaned up"
        );
    }
}
