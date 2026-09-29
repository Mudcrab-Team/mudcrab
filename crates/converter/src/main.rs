use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, ProgressEvent, ProgressStage,
    pipeline::{Cancellation, Interrupted, PipelineFailure},
    progress::{ProgressRenderer, format_bytes, format_elapsed},
};
use serde::Serialize;
use std::{
    ffi::OsString,
    fs,
    io::{IsTerminal, Write},
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
    verbose: bool,
}

#[derive(Debug, Serialize)]
struct FailureReport {
    complete: bool,
    stage: Option<converter::ProgressStage>,
    file: Option<PathBuf>,
    error: String,
    elapsed_ms: u128,
    stages: Vec<StageTime>,
}

/// The report written by `--report-json`: the pipeline's report plus when each stage ran.
#[derive(Serialize)]
struct RunReport<'a> {
    #[serde(flatten)]
    report: &'a PipelineReport,
    stages: Vec<StageTime>,
}

/// When a stage reported progress, in seconds since the conversion started. Stages can overlap,
/// so each keeps its first and last event rather than a duration from stage changes.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct StageTime {
    stage: ProgressStage,
    first_seconds: f64,
    last_seconds: f64,
}

#[derive(Default)]
struct StageClock {
    stages: Vec<StageTime>,
}

impl StageClock {
    fn record(&mut self, stage: ProgressStage, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64();
        match self.stages.iter_mut().find(|time| time.stage == stage) {
            Some(time) => time.last_seconds = seconds,
            None => self.stages.push(StageTime {
                stage,
                first_seconds: seconds,
                last_seconds: seconds,
            }),
        }
    }

    fn summary(&self) -> String {
        let mut summary = String::from("  stage times (first event to last):");
        for time in &self.stages {
            summary.push_str(&format!(
                "
    {:<11} {} to {} ({})",
                format!("{:?}", time.stage),
                format_elapsed(time.first_seconds),
                format_elapsed(time.last_seconds),
                format_elapsed(time.last_seconds - time.first_seconds)
            ));
        }
        summary
    }
}

/// What the printer saw while the run went on: when each stage reported, and which assets failed,
/// so a failure can name them after the pipeline has given up.
#[derive(Default)]
struct RunWatch {
    clock: StageClock,
    failed: Vec<PathBuf>,
    failures: u64,
}

impl RunWatch {
    /// How many failed assets to name before pointing at the manifest.
    const NAMED_FAILURES: usize = 3;

    fn observe(&mut self, event: &ProgressEvent) {
        if event.is_asset_failure() {
            self.failures += 1;
            if self.failed.len() < Self::NAMED_FAILURES {
                self.failed
                    .extend(event.current_file.clone().map(|file| file.to_path_buf()));
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    suppress_caught_nif_parser_panics();
    let cli = parse_cli(std::env::args_os().skip(1).collect())?;
    let mut config = PipelineConfig::new(cli.data.clone(), cli.output.clone());
    config.resume_staging = cli.resume_staging.clone();
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
    let verbose = cli.verbose;
    let printer = tokio::spawn(async move {
        let mut watch = RunWatch::default();
        // The status line is redrawn on the terminal the user is watching; a run whose stderr is
        // piped to a file or a CI log gets plain lines instead.
        let mut renderer = ProgressRenderer::new(std::io::stderr().is_terminal(), verbose);
        // One asset can take minutes (a large texture), so the line is redrawn on a timer as well
        // as on events, or the elapsed time and the estimate would sit still while it works.
        let mut ticker = tokio::time::interval(ProgressRenderer::TERMINAL_REFRESH);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let event = tokio::select! {
                event = rx.recv() => event,
                _ = ticker.tick() => {
                    if let Some(text) = renderer.tick(started.elapsed()) {
                        write_status(&text);
                    }
                    continue;
                }
            };
            let Some(event) = event else { break };
            *printer_progress.lock().expect("progress mutex poisoned") = Some(event.clone());
            let elapsed = started.elapsed();
            watch.clock.record(event.stage, elapsed);
            watch.observe(&event);
            if let Some(text) = renderer.update(&event, elapsed) {
                write_status(&text);
            }
        }
        if let Some(text) = renderer.finish() {
            write_status(&text);
        }
        watch
    });

    // Ctrl+C stops the run at the next safe point and keeps the staging folder; a second one ends
    // the process where it stands.
    let cancellation = Cancellation::new();
    let interrupt = cancellation.clone();
    tokio::spawn(async move {
        let mut received = 0;
        while tokio::signal::ctrl_c().await.is_ok() {
            received += 1;
            if received == 1 {
                eprintln!(
                    "\nInterrupted: finishing the work in flight, then stopping. The staging folder is kept, so the run can be resumed."
                );
                interrupt.cancel();
            } else {
                eprintln!("Interrupted again: exiting now.");
                std::process::exit(130);
            }
        }
    });

    let pipeline_result =
        AssetPipeline::run_async_with_cancel(config, tx, cancellation.clone()).await;
    let watch = printer.await?;
    let report = match pipeline_result {
        Ok(report) => report,
        Err(failure) => {
            if let Some(path) = &cli.report_json {
                write_failure_report(path, &failure, &last_progress, &watch, started.elapsed())?;
            }
            print_failure(&cli, &failure, &watch, started.elapsed());
            std::process::exit(if failure.cancelled { 130 } else { 1 });
        }
    };
    if let Some(path) = &cli.report_json {
        let run = RunReport {
            report: &report,
            stages: watch.clock.stages.clone(),
        };
        write_json_atomic(path, &run)?;
    }
    print_summary(&cli, &report, &watch.clock);
    if !report.complete {
        let skipped = report.warnings.len();
        eprintln!(
            "Conversion incomplete: {skipped} input(s) were skipped. The output was published anyway; the manifest lists what is missing: {}",
            cli.output.join("conversion-manifest.json").display()
        );
        for warning in report.warnings.iter().take(RunWatch::NAMED_FAILURES) {
            eprintln!("    {warning}");
        }
        std::process::exit(1);
    }
    Ok(())
}

/// The summary a finished run prints: what it produced, how long it took, and where to look.
fn print_summary(cli: &Cli, report: &PipelineReport, clock: &StageClock) {
    println!("{}", summary_headline(report));
    let (bytes, files) = artifact_size(&cli.output, &report.artifacts);
    println!(
        "  output: {} in {} artifacts ({})",
        format_bytes(bytes),
        files,
        cli.output.display()
    );
    println!(
        "  manifest: {}",
        cli.output.join("conversion-manifest.json").display()
    );
    if let Some(path) = &cli.report_json {
        println!("  report: {}", path.display());
    }
    println!("{}", clock.summary());
}

/// The summary's first line. A run that skipped inputs published an output without them, so it
/// says it finished incomplete rather than that it is complete.
fn summary_headline(report: &PipelineReport) -> String {
    let elapsed = format_elapsed(report.elapsed_ms as f64 / 1000.0);
    let counts = format!(
        "converted {}, reused {}, failed {}",
        report.converted, report.cache_hits, report.skipped
    );
    if report.complete {
        format!("Conversion complete in {elapsed}: {counts}")
    } else {
        format!("Conversion finished in {elapsed}, incomplete: {counts}")
    }
}

/// The size of the converted artifacts. The published tree also holds the extracted `vfs` and the
/// ingestion cache, whose files share their bytes with each other, so the artifacts are what the
/// run produced and what a fresh run has to write.
fn artifact_size(output: &Path, artifacts: &[PathBuf]) -> (u64, u64) {
    let mut bytes = 0;
    let mut files = 0;
    for artifact in artifacts {
        if let Ok(metadata) = fs::metadata(output.join(artifact)) {
            bytes += metadata.len();
            files += 1;
        }
    }
    (bytes, files)
}

/// What to say when the run stopped early: what went wrong, which assets failed, and the exact
/// command that picks the run up where it stopped.
fn print_failure(cli: &Cli, failure: &PipelineFailure, watch: &RunWatch, elapsed: Duration) {
    let stage = watch
        .clock
        .stages
        .last()
        .map(|time| format!(" during {:?}", time.stage))
        .unwrap_or_default();
    if failure.cancelled {
        eprintln!(
            "Conversion interrupted after {}{stage}.",
            format_elapsed(elapsed.as_secs_f64())
        );
        if let Some(cause) = stop_cause(failure) {
            eprintln!("  Cause: {cause}");
        }
    } else {
        eprintln!(
            "Conversion failed after {}{stage}: {:#}",
            format_elapsed(elapsed.as_secs_f64()),
            failure.error
        );
    }
    if !watch.failed.is_empty() {
        eprintln!("  assets that failed (first {}):", watch.failed.len());
        for file in &watch.failed {
            eprintln!("    - {}", file.display());
        }
        let remaining = watch.failures.saturating_sub(watch.failed.len() as u64);
        if remaining > 0 {
            eprintln!("    ... and {remaining} more (see conversion-manifest.json)");
        }
    }
    match &failure.staging {
        Some(staging) => {
            eprintln!("  The staging folder was kept: {}", staging.display());
            eprintln!("  Resume where it stopped with:");
            eprintln!("    {}", resume_command(cli, staging));
            eprintln!(
                "  Delete that folder to free the space if you would rather start over: {}",
                staging.display()
            );
        }
        None => eprintln!(
            "  The run stopped before it created a staging folder; fix the error above and run again."
        ),
    }
}

/// What to show under "Conversion interrupted": nothing for a plain stop, otherwise the error that
/// raced it, or the error that ended the run while the stop was pending.
fn stop_cause(failure: &PipelineFailure) -> Option<String> {
    match failure.error.downcast_ref::<Interrupted>() {
        Some(stop) => stop.cause().map(|cause| format!("{cause:#}")),
        None => Some(format!("{:#}", failure.error)),
    }
}

/// The exact command that resumes a run from a kept staging folder.
fn resume_command(cli: &Cli, staging: &Path) -> String {
    format!(
        "converter \"{}\" \"{}\" --resume-staging \"{}\"",
        cli.data.display(),
        cli.output.display(),
        staging.display()
    )
}

fn write_status(text: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(text.as_bytes());
    let _ = stderr.flush();
}

fn write_failure_report(
    path: &Path,
    failure: &PipelineFailure,
    last_progress: &Mutex<Option<ProgressEvent>>,
    watch: &RunWatch,
    elapsed: Duration,
) -> Result<()> {
    let progress = last_progress
        .lock()
        .expect("progress mutex poisoned")
        .clone();
    let report = FailureReport {
        complete: false,
        stage: progress.as_ref().map(|event| event.stage),
        file: progress.and_then(|event| event.current_file),
        error: format!("{:#}", failure.error),
        elapsed_ms: elapsed.as_millis(),
        stages: watch.clock.stages.clone(),
    };
    write_json_atomic(path, &report)
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

fn parse_cli(args: Vec<OsString>) -> Result<Cli> {
    let mut positional = Vec::new();
    let mut report_json = None;
    let mut resume_staging = None;
    let mut cpu_jobs = None;
    let mut io_jobs = None;
    let mut fail_fast = false;
    let mut invalidate_cache = false;
    let mut verify_cache = true;
    let mut verbose = false;
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
            Some("--verbose") => verbose = true,
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
        verbose,
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
    "usage: converter <Skyrim Data> [output directory] [--cpu-jobs N] [--io-jobs N] [--fail-fast]
                 [--invalidate-cache] [--no-verify-cache] [--resume-staging DIR]
                 [--report-json FILE] [--verbose]

Converts a Skyrim Data directory into runtime assets.

While it runs, one status line is redrawn on the terminal, four times a second at most:

  Textures     61%  [overall  72%]  412 items/s  61.2 MB/s  00:12:31 elapsed  ~00:04:50 left

With stderr piped to a file or a CI log, one plain line per stage every few seconds is printed
instead. --verbose prints one line per converted asset, as older versions always did.

Ctrl+C stops the run after the asset in flight and keeps the staging folder; the exact command that
resumes where it stopped is printed when the run stops. A second Ctrl+C exits immediately."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_clock_keeps_first_and_last_event_of_overlapping_stages() {
        let mut clock = StageClock::default();
        clock.record(ProgressStage::Textures, Duration::from_secs(10));
        clock.record(ProgressStage::Meshes, Duration::from_secs(12));
        clock.record(ProgressStage::Textures, Duration::from_secs(30));
        clock.record(ProgressStage::Meshes, Duration::from_secs(20));
        let stage = |stage, first_seconds, last_seconds| StageTime {
            stage,
            first_seconds,
            last_seconds,
        };
        assert_eq!(
            clock.stages,
            vec![
                stage(ProgressStage::Textures, 10.0, 30.0),
                stage(ProgressStage::Meshes, 12.0, 20.0),
            ]
        );
        assert!(
            clock
                .summary()
                .contains("Textures    0:00:10.0 to 0:00:30.0 (0:00:20.0)")
        );
    }

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
                "--verbose",
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
        assert!(cli.verbose);
        assert_eq!(cli.report_json, Some(PathBuf::from("report.json")));
    }

    #[test]
    fn names_the_exact_command_that_resumes_a_kept_staging_folder() {
        let cli = Cli {
            data: PathBuf::from("C:/Games/Skyrim/Data"),
            output: PathBuf::from("C:/Modding/SkyrimConverted"),
            resume_staging: None,
            report_json: None,
            cpu_jobs: None,
            io_jobs: None,
            fail_fast: false,
            invalidate_cache: false,
            verify_cache: true,
            verbose: false,
        };
        assert_eq!(
            resume_command(&cli, Path::new("C:/Modding/SkyrimConverted.staging-1-2")),
            "converter \"C:/Games/Skyrim/Data\" \"C:/Modding/SkyrimConverted\" --resume-staging \"C:/Modding/SkyrimConverted.staging-1-2\""
        );
    }

    #[test]
    fn the_summary_calls_a_run_complete_only_when_it_is() {
        let mut report = PipelineReport {
            converted: 10,
            cache_hits: 4,
            skipped: 0,
            elapsed_ms: 62_300,
            complete: true,
            ..PipelineReport::default()
        };
        assert_eq!(
            summary_headline(&report),
            "Conversion complete in 0:01:02.3: converted 10, reused 4, failed 0"
        );

        report.skipped = 2;
        report.complete = false;
        assert_eq!(
            summary_headline(&report),
            "Conversion finished in 0:01:02.3, incomplete: converted 10, reused 4, failed 2"
        );
    }

    #[test]
    fn watches_the_first_few_failed_assets() {
        let mut watch = RunWatch::default();
        for index in 0..5 {
            let event = ProgressEvent::new(
                ProgressStage::Textures,
                index,
                5,
                Some(PathBuf::from(format!("textures/bad{index}.dds"))),
                "Asset skipped",
            )
            .with_outcome(converter::AssetOutcome::Skipped);
            watch.observe(&event);
        }
        // A failure is recognised by its outcome, not by the wording of its message.
        let reworded = ProgressEvent::new(
            ProgressStage::Meshes,
            0,
            1,
            Some(PathBuf::from("meshes/bad.nif")),
            "Some other wording",
        )
        .with_outcome(converter::AssetOutcome::Failed);
        watch.observe(&reworded);
        let converted = ProgressEvent::new(
            ProgressStage::Meshes,
            1,
            1,
            Some(PathBuf::from("meshes/good.nif")),
            "Asset skipped",
        );
        watch.observe(&converted);
        assert_eq!(watch.failures, 6);
        assert_eq!(watch.failed.len(), RunWatch::NAMED_FAILURES);
        assert_eq!(watch.failed[0], PathBuf::from("textures/bad0.dds"));
    }

    #[test]
    fn a_stop_shows_a_cause_only_when_there_is_one() {
        let failure = |error: color_eyre::Report| PipelineFailure {
            error,
            staging: None,
            cancelled: true,
        };
        assert_eq!(stop_cause(&failure(Interrupted::new().into())), None);
        // The extractor noticed the stop first: its error is the same plain stop.
        assert_eq!(
            stop_cause(&failure(
                Interrupted::after(Interrupted::new().into()).into()
            )),
            None
        );
        assert_eq!(
            stop_cause(&failure(
                Interrupted::after(color_eyre::eyre::eyre!("bad archive header")).into()
            ))
            .as_deref(),
            Some("bad archive header")
        );
        assert_eq!(
            stop_cause(&failure(color_eyre::eyre::eyre!("disk full"))).as_deref(),
            Some("disk full")
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
            stages: vec![StageTime {
                stage: converter::ProgressStage::Extracting,
                first_seconds: 0.5,
                last_seconds: 1.5,
            }],
        };
        write_json_atomic(&report, &failure).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
        assert_eq!(value["complete"], false);
        assert_eq!(value["stage"], "extracting");
        assert_eq!(value["file"], "Skyrim - Animations.bsa");
        assert_eq!(value["error"], "unsupported flags");
        assert_eq!(value["elapsed_ms"], 42);
        assert_eq!(value["stages"][0]["stage"], "extracting");
        assert_eq!(value["stages"][0]["last_seconds"], 1.5);
        assert_eq!(
            fs::read_dir(directory.path()).unwrap().count(),
            1,
            "temporary report files were not cleaned up"
        );
    }
}
