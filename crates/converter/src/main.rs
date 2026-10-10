use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::config::RecordReader;
use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, ProgressEvent, ProgressStage, TextureEncoder,
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
    mo2: Option<mo2::Selection>,
    resume_staging: Option<PathBuf>,
    reuse_assets: Option<PathBuf>,
    report_json: Option<PathBuf>,
    cpu_jobs: Option<usize>,
    io_jobs: Option<usize>,
    texture_encoder: TextureEncoder,
    texture_fallback_quality: Option<u8>,
    texture_uastc_level: Option<u8>,
    texture_zstd_level: Option<i32>,
    record_reader: RecordReader,
    lod_texture_encoder: TextureEncoder,
    fail_fast: bool,
    invalidate_cache: bool,
    verify_cache: bool,
    no_lod: bool,
    verbose: bool,
    /// Overrides the default string-table language.
    language: Option<String>,
}

impl Cli {
    fn pipeline_config(&self) -> PipelineConfig {
        let mut config = PipelineConfig::new(self.data.clone(), self.output.clone());
        config.resume_staging = self.resume_staging.clone();
        config.mo2 = self.mo2.clone();
        config.fail_fast = self.fail_fast;
        config.invalidate_cache = self.invalidate_cache;
        config.verify_cache = self.verify_cache;
        config.no_lod = self.no_lod;
        config.texture_encoder = self.texture_encoder;
        config.lod_texture_encoder = self.lod_texture_encoder;
        config.record_reader = self.record_reader;
        if let Some(value) = self.texture_fallback_quality {
            config.texture_fallback_quality = value;
        }
        if let Some(value) = self.texture_uastc_level {
            config.texture_uastc_level = value;
        }
        if let Some(value) = self.texture_zstd_level {
            config.texture_zstd_level = value;
        }
        if let Some(value) = self.cpu_jobs {
            config.cpu_jobs = value;
        }
        if let Some(value) = self.io_jobs {
            config.io_jobs = value;
        }
        if let Some(language) = &self.language {
            config.language = language.clone();
        }
        config
    }
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
    Repair(Cli, bool),
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

/// Parses the command line, runs the conversion (or `check`) and reports its progress and result.
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
    let command = parse_command(args)?;
    if let Command::Convert(cli) | Command::Repair(cli, _) = &command
        && let Some(selection) = &cli.mo2
    {
        eprintln!(
            "MO2 profile: {:?} (instance: {})",
            selection.profile,
            selection.instance_path.display()
        );
    }
    let cli = match command {
        Command::Convert(cli) => cli,
        Command::Check(check) => std::process::exit(run_check(&check)),
        Command::Repair(cli, apply) => {
            let config = cli.pipeline_config();
            let report = converter::repair::repair_failed(&config, apply)?;
            println!(
                "Repair {}: {} converted, {} classified/excluded, {} failures. Files/report: {}",
                if report.published {
                    "published"
                } else {
                    "staged (original pack unchanged)"
                },
                report.converted,
                report.excluded,
                report.failures.len(),
                report.directory.display()
            );
            if !report.failures.is_empty() {
                std::process::exit(1);
            }
            return Ok(());
        }
    };
    validate_metadata_report_path(&cli)?;
    let config = cli.pipeline_config();
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
    let metadata_rebuild = cli.reuse_assets.is_some();
    tokio::spawn(async move {
        let mut received = 0;
        while tokio::signal::ctrl_c().await.is_ok() {
            received += 1;
            if received == 1 && metadata_rebuild {
                eprintln!(
                    "\nMetadata rebuild continues: it cannot cooperatively stop or resume. Press Ctrl+C again to force exit; its staging directory will require manual cleanup."
                );
            } else if received == 1 {
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

    let pipeline_result = if let Some(source) = &cli.reuse_assets {
        eprintln!(
            "Reusing manifest-verified package assets from {}. Retained models, textures and scripts are not refreshed from Data; use normal conversion after source asset changes.",
            source.display()
        );
        AssetPipeline::rebuild_metadata_async(config, source, tx)
            .await
            .map_err(converter::PipelineFailure::from)
    } else {
        AssetPipeline::run_async_with_cancel(config, tx, cancellation.clone()).await
    };
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
    if report.pruned_texture_references > 0 {
        println!(
            "Published meshes omit {} texture reference(s) the game data does not contain; conversion-manifest.json records them under pruned_texture_references",
            report.pruned_texture_references
        );
    }
    if !report.complete {
        eprintln!("{}", incomplete_summary(&report, &cli.output));
        for warning in report.warnings.iter().take(RunWatch::NAMED_FAILURES) {
            eprintln!("    {warning}");
        }
        std::process::exit(1);
    }
    Ok(())
}

/// Reports failures and skipped inputs separately: an asset-integration failure is a failure
/// without a skipped input, and its details live in `integration-report.json`, not the manifest.
fn incomplete_summary(report: &PipelineReport, output: &Path) -> String {
    let mut summary = format!(
        "Conversion incomplete: {} failure(s), {} input(s) skipped. The output was published anyway; the manifest lists what is missing: {}",
        report.warnings.len(),
        report.skipped,
        output.join("conversion-manifest.json").display()
    );
    if report
        .integration
        .as_ref()
        .is_some_and(|integration| !integration.passed)
    {
        summary.push_str(&format!(
            "; asset integration details: {}",
            output.join("integration-report.json").display()
        ));
    }
    summary
}

/// The summary a finished run prints: what it produced, how long it took, and where to look.
fn print_summary(cli: &Cli, report: &PipelineReport, clock: &StageClock) {
    println!("{}", summary_headline(report));
    for notice in &report.notices {
        println!("  note: {notice}");
    }
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
        "converted {}, reused {}, failed {}, skipped {}",
        report.converted,
        report.cache_hits,
        report.warnings.len(),
        report.skipped
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
            eprintln!("    {}", resume_command(&program_name(), cli, staging));
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

/// The name the converter was started as (`converter`, `converter.exe`, or whatever a packager
/// renamed it to), for the commands it prints. `converter` when the system cannot say.
fn program_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "converter".to_owned())
}

/// The exact command that resumes a run from a kept staging folder, started as `program`.
fn resume_command(program: &str, cli: &Cli, staging: &Path) -> String {
    let program = if program.contains(char::is_whitespace) {
        format!("\"{program}\"")
    } else {
        program.to_owned()
    };
    let mut command = format!(
        "{program} \"{}\" \"{}\" --resume-staging \"{}\"",
        cli.data.display(),
        cli.output.display(),
        staging.display()
    );
    if let Some(selection) = &cli.mo2 {
        command.push_str(&format!(
            " --mo2-instance \"{}\" --mo2-profile \"{}\"",
            selection.instance_path.display(),
            selection.profile
        ));
    }
    if let Some(value) = cli.texture_fallback_quality {
        command.push_str(&format!(" --texture-fallback-quality {value}"));
    }
    if let Some(value) = cli.texture_uastc_level {
        command.push_str(&format!(" --texture-uastc-level {value}"));
    }
    if let Some(value) = cli.texture_zstd_level {
        command.push_str(&format!(" --texture-zstd-level {value}"));
    }
    // A resumed run must keep the texture encoder: GPU-encoded textures are cached under their
    // own label, so resuming on the CPU would convert them again.
    if let TextureEncoder::Gpu { quality, batch_mb } = cli.texture_encoder {
        command.push_str(&format!(
            " --texture-encoder gpu --gpu-quality {quality} --gpu-batch-mb {batch_mb}"
        ));
    }
    if let TextureEncoder::Gpu { quality, batch_mb } = cli.lod_texture_encoder {
        command.push_str(" --lod-encoder gpu");
        if !matches!(cli.texture_encoder, TextureEncoder::Gpu { .. }) {
            command.push_str(&format!(
                " --gpu-quality {quality} --gpu-batch-mb {batch_mb}"
            ));
        }
    } else {
        command.push_str(" --lod-encoder cpu");
    }
    if cli.no_lod {
        command.push_str(" --no-lod");
    }
    // The database is rebuilt on resume; it must read the same string tables.
    if let Some(language) = &cli.language {
        command.push_str(&format!(" --language {language}"));
    }
    if cli.record_reader == RecordReader::Inhouse {
        command.push_str(" --record-reader inhouse");
    }
    command
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

fn validate_metadata_report_path(cli: &Cli) -> Result<()> {
    let (Some(source), Some(report)) = (&cli.reuse_assets, &cli.report_json) else {
        return Ok(());
    };
    let report = shared::asset_lock::resolve_asset_path(report)
        .wrap_err_with(|| format!("failed to resolve metadata report {}", report.display()))?;
    for root in [source, &cli.data, &cli.output] {
        let root = shared::asset_lock::resolve_asset_path(root)?;
        color_eyre::eyre::ensure!(
            !report.starts_with(root),
            "metadata report must be outside source, Data, and output directories"
        );
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

/// Parses conversion, check, or repair arguments and rejects options unsupported by repair.
fn parse_command(args: Vec<OsString>) -> Result<Command> {
    if args.first().and_then(|argument| argument.to_str()) == Some("check") {
        return parse_check(args.into_iter().skip(1)).map(Command::Check);
    }
    if args.first().and_then(|argument| argument.to_str()) == Some("repair-failed") {
        let apply = args.iter().any(|argument| argument == "--apply");
        let cli = parse_cli(
            args.into_iter()
                .skip(1)
                .filter(|argument| argument != "--apply")
                .collect(),
        )?;
        if cli.resume_staging.is_some()
            || cli.reuse_assets.is_some()
            || cli.invalidate_cache
            || cli.fail_fast
            || cli.report_json.is_some()
            || cli.cpu_jobs.is_some()
            || cli.io_jobs.is_some()
            || !cli.verify_cache
            || cli.verbose
        {
            bail!(
                "repair-failed accepts Data/output, MO2 selection, encoding options, --no-lod and --apply only"
            );
        }
        return Ok(Command::Repair(cli, apply));
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

/// Parses the arguments of a conversion run.
fn parse_cli(args: Vec<OsString>) -> Result<Cli> {
    let mut positional = Vec::new();
    let mut mo2_instance = None;
    let mut mo2_profile = None;
    let mut report_json = None;
    let mut resume_staging = None;
    let mut reuse_assets = None;
    let mut cpu_jobs = None;
    let mut io_jobs = None;
    let mut use_gpu = false;
    let mut record_reader = RecordReader::Legacy;
    let mut use_lod_gpu = true;
    let mut gpu_quality = None;
    let mut gpu_batch_mb = None;
    let mut texture_fallback_quality = None;
    let mut texture_uastc_level = None;
    let mut texture_zstd_level = None;
    let mut fail_fast = false;
    let mut invalidate_cache = false;
    let mut verify_cache = true;
    let mut no_lod = false;
    let mut verbose = false;
    let mut language = None;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--mo2-instance") => {
                mo2_instance = Some(PathBuf::from(next_value(&mut args, "--mo2-instance")?));
            }
            Some("--mo2-profile") => {
                mo2_profile = Some(
                    next_value(&mut args, "--mo2-profile")?
                        .into_string()
                        .map_err(|_| color_eyre::eyre::eyre!("--mo2-profile must be UTF-8"))?,
                );
            }
            Some("--report-json") => {
                report_json = Some(PathBuf::from(next_value(&mut args, "--report-json")?))
            }
            Some("--resume-staging") => {
                resume_staging = Some(PathBuf::from(next_value(&mut args, "--resume-staging")?))
            }
            Some("--reuse-assets") => {
                reuse_assets = Some(PathBuf::from(next_value(&mut args, "--reuse-assets")?))
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
            Some("--texture-encoder") => {
                let value = next_value(&mut args, "--texture-encoder")?;
                use_gpu = match value.to_str() {
                    Some("cpu") => false,
                    Some("gpu") => true,
                    _ => bail!("--texture-encoder must be cpu or gpu"),
                };
            }
            Some("--record-reader") => {
                record_reader = match next_value(&mut args, "--record-reader")?.to_str() {
                    Some("legacy") => RecordReader::Legacy,
                    Some("inhouse") => RecordReader::Inhouse,
                    _ => bail!("--record-reader must be legacy or inhouse"),
                };
            }
            Some("--lod-encoder") => {
                let value = next_value(&mut args, "--lod-encoder")?;
                use_lod_gpu = match value.to_str() {
                    Some("cpu") => false,
                    Some("gpu") => true,
                    _ => bail!("--lod-encoder must be cpu or gpu"),
                };
            }
            Some("--gpu-quality") => {
                gpu_quality = Some(parse_u32(
                    next_value(&mut args, "--gpu-quality")?,
                    "--gpu-quality",
                )?);
            }
            Some("--gpu-batch-mb") => {
                gpu_batch_mb = Some(parse_u64(
                    next_value(&mut args, "--gpu-batch-mb")?,
                    "--gpu-batch-mb",
                )?);
            }
            Some(option @ ("--texture-fallback-quality" | "--texture-etc1s-quality")) => {
                texture_fallback_quality =
                    Some(
                        parse_encoding_value(next_value(&mut args, option)?, option, 1, 255)? as u8,
                    );
            }
            Some(option @ "--texture-uastc-level") => {
                texture_uastc_level =
                    Some(parse_encoding_value(next_value(&mut args, option)?, option, 0, 4)? as u8);
            }
            Some(option @ "--texture-zstd-level") => {
                texture_zstd_level =
                    Some(
                        parse_encoding_value(next_value(&mut args, option)?, option, 0, 22)? as i32,
                    );
            }
            Some("--fail-fast") => fail_fast = true,
            Some("--invalidate-cache") => invalidate_cache = true,
            Some("--no-verify-cache") => verify_cache = false,
            Some("--no-lod") => no_lod = true,
            Some("--language") => {
                let value = next_value(&mut args, "--language")?;
                language =
                    Some(value.into_string().map_err(|_| {
                        color_eyre::eyre::eyre!("--language value is not valid UTF-8")
                    })?);
            }
            Some("--verbose") => verbose = true,
            Some("--help" | "-h") => bail!(usage()),
            Some(flag) if flag.starts_with('-') => bail!("unknown option {flag}\n{}", usage()),
            _ => positional.push(PathBuf::from(argument)),
        }
    }
    if positional.is_empty() || positional.len() > 2 {
        bail!(usage());
    }
    let gpu_encoder = TextureEncoder::Gpu {
        quality: gpu_quality.unwrap_or(converter::texture_gpu::DEFAULT_QUALITY),
        batch_mb: gpu_batch_mb.unwrap_or(converter::texture_gpu::DEFAULT_BATCH_MB),
    };
    if !use_gpu && !use_lod_gpu && (gpu_quality.is_some() || gpu_batch_mb.is_some()) {
        bail!(
            "--gpu-quality and --gpu-batch-mb require --texture-encoder gpu or --lod-encoder gpu"
        );
    }
    let texture_encoder = if use_gpu {
        gpu_encoder
    } else {
        TextureEncoder::Cpu
    };
    let lod_texture_encoder = if use_lod_gpu {
        gpu_encoder
    } else {
        TextureEncoder::Cpu
    };
    let mo2 = match mo2_instance {
        Some(instance_path) => {
            let instance = mo2::Instance::open(&instance_path)?;
            let profile = mo2_profile.unwrap_or_else(|| {
                instance
                    .selected_profile
                    .clone()
                    .unwrap_or_else(|| instance.profiles[0].clone())
            });
            instance.profile_dir(&profile)?;
            Some(mo2::Selection {
                instance_path: instance.instance_path,
                profile,
            })
        }
        None => {
            if mo2_profile.is_some() {
                bail!("--mo2-profile requires --mo2-instance");
            }
            None
        }
    };
    Ok(Cli {
        mo2,
        data: positional.remove(0),
        output: positional
            .pop()
            .unwrap_or_else(|| PathBuf::from("modern_assets")),
        resume_staging,
        reuse_assets,
        report_json,
        cpu_jobs,
        io_jobs,
        texture_encoder,
        texture_fallback_quality,
        texture_uastc_level,
        texture_zstd_level,
        record_reader,
        lod_texture_encoder,
        fail_fast,
        invalidate_cache,
        verify_cache,
        no_lod,
        verbose,
        language,
    })
}

fn parse_encoding_value(value: OsString, option: &str, min: u32, max: u32) -> Result<u32> {
    let value = parse_u32(value, option)?;
    if !(min..=max).contains(&value) {
        bail!("{option} must be between {min} and {max}");
    }
    Ok(value)
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

/// Parses the value of an option that takes a non-negative integer.
fn parse_u32(value: OsString, option: &str) -> Result<u32> {
    value
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("{option} value is not valid UTF-8"))?
        .parse()
        .wrap_err_with(|| format!("{option} requires a nonnegative integer"))
}

/// Parses the value of an option that takes a positive integer.
fn parse_u64(value: OsString, option: &str) -> Result<u64> {
    value
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("{option} value is not valid UTF-8"))?
        .parse()
        .wrap_err_with(|| format!("{option} requires a positive integer"))
}

/// The help text printed for `--help` and after a usage error.
fn usage() -> &'static str {
    "usage: converter <Skyrim Data> [output directory] [--cpu-jobs N] [--io-jobs N] [--fail-fast]
                 [--texture-encoder cpu|gpu] [--lod-encoder cpu|gpu] [--gpu-quality N] [--gpu-batch-mb N]
                 [--texture-fallback-quality 1..255] [--texture-uastc-level 0..4]
                 [--texture-zstd-level 0..22]
                 [--invalidate-cache] [--no-verify-cache] [--resume-staging DIR]
                 [--report-json FILE] [--verbose] [--reuse-assets DIR] [--no-lod]
                 [--mo2-instance DIR] [--mo2-profile NAME]
                 [--language NAME]
                 [--record-reader legacy|inhouse]
       converter check <output directory> [--full]
       converter repair-failed <Skyrim Data> <output directory> [--mo2-instance DIR]
                 [--mo2-profile NAME] [--no-lod] [--apply]
                 [--texture-encoder cpu|gpu] [--gpu-quality N] [--gpu-batch-mb N]
                 [--texture-fallback-quality 1..255] [--texture-uastc-level 0..4]
                 [--texture-zstd-level 0..22]

repair-failed stages only manifest failures and missing dependencies in a sibling repair directory.
It never rebuilds the database or cell cache. --apply validates the pack's existing hashes,
publishes validated repairs with backups, and updates the manifest last.
Use the original conversion's encoding settings. --texture-etc1s-quality is an alias for
--texture-fallback-quality; it does not select ETC1S encoding.

Converts a Skyrim Data directory into runtime assets.
--mo2-instance overlays enabled MO2 mods and overwrite over physical Data, using the profile's
active plugins and loadorder.txt. --mo2-profile defaults to ModOrganizer.ini's selected_profile,
or the first sorted profile if that selection is absent or stale.
MO2 files are read only. Native SKSE DLLs and arbitrary mod compatibility are not supported.

Terrain LOD uses the GPU by default, with CPU fallback if GPU initialization fails.
--lod-encoder cpu selects CPU LOD. Ordinary textures default to the CPU independently.

--no-lod skips terrain LOD compilation in conversion and metadata rebuilds. Full-detail
terrain and ordinary assets remain available. Omit it on a later run to build LOD.

--language selects the string tables that localized plugins' names are read from,
Strings/<plugin>_<language>.STRINGS, as Skyrim's sLanguage does. Default: english.

While it runs, one status line is redrawn on the terminal, four times a second at most:

  Textures     61%  [overall  72%]  412 items/s  61.2 MB/s  00:12:31 elapsed  ~00:04:50 left

With stderr piped to a file or a CI log, one plain line per stage every few seconds is printed
instead. --verbose prints one line per converted asset, as older versions always did.

Ctrl+C stops the run after the asset in flight and keeps the staging folder; the exact command that
resumes where it stopped is printed when the run stops. A second Ctrl+C exits immediately.

converter check compares a converted output with its conversion-manifest.json without converting:
the existence and size of every file, and with --full their hashes too. Exit code 0: all good,
1: problems found, 2: no readable manifest.

--reuse-assets rebuilds metadata while preserving manifest-verified assets from an existing
package. Data supplies matching plugins, LOD settings and terrain diffuse inputs; retained
models, textures and scripts are not refreshed from Data. Use normal conversion after
changing those source assets. This route cannot resume or cooperatively cancel."
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies MO2 profile defaults, invalid selections, and selection retention in resume commands.
    #[test]
    fn mo2_cli_defaults_validates_and_preserves_selection_on_resume() {
        let dir = tempfile::tempdir().unwrap();
        for folder in ["mods", "profiles/Zed", "profiles/Alpha", "overwrite"] {
            fs::create_dir_all(dir.path().join(folder)).unwrap();
        }
        fs::write(
            dir.path().join("ModOrganizer.ini"),
            "[General]\ngameName=Skyrim\n",
        )
        .unwrap();
        let instance = dir.path().to_str().unwrap();
        let cli = parse_cli(args(&["Data", "out", "--mo2-instance", instance])).unwrap();
        assert_eq!(cli.mo2.as_ref().unwrap().profile, "Alpha");
        fs::write(
            dir.path().join("ModOrganizer.ini"),
            "[General]\ngameName=Skyrim\n[Settings]\nselected_profile=zed\n",
        )
        .unwrap();
        let cli = parse_cli(args(&["Data", "out", "--mo2-instance", instance])).unwrap();
        assert_eq!(cli.mo2.as_ref().unwrap().profile, "Zed");
        let Command::Repair(repair, _) = parse_command(args(&[
            "repair-failed",
            "Data",
            "out",
            "--mo2-instance",
            instance,
        ]))
        .unwrap() else {
            panic!("expected repair");
        };
        assert_eq!(repair.pipeline_config().mo2.unwrap().profile, "Zed");
        let explicit = parse_cli(args(&[
            "Data",
            "out",
            "--mo2-instance",
            instance,
            "--mo2-profile",
            "Alpha",
        ]))
        .unwrap();
        assert_eq!(explicit.mo2.unwrap().profile, "Alpha");
        let cli = parse_cli(args(&[
            "Data",
            "out",
            "--mo2-instance",
            instance,
            "--mo2-profile",
            "Zed",
        ]))
        .unwrap();
        let resume = resume_command("converter", &cli, Path::new("out.staging-1-2"));
        assert!(resume.contains("--mo2-instance"));
        assert!(resume.ends_with("--mo2-profile \"Zed\""));
        assert!(parse_cli(args(&["Data", "--mo2-profile", "Zed"])).is_err());
        assert!(
            parse_cli(args(&[
                "Data",
                "--mo2-instance",
                instance,
                "--mo2-profile",
                "Missing"
            ]))
            .is_err()
        );
        assert!(parse_cli(args(&["Data", "--mo2-instance"])).is_err());
        assert!(parse_cli(args(&["Data", "--mo2-profile"])).is_err());
        fs::write(
            dir.path().join("ModOrganizer.ini"),
            "[General]\ngameName=Skyrim\n[Settings]\nselected_profile=Missing\n",
        )
        .unwrap();
        let cli = parse_cli(args(&["Data", "--mo2-instance", instance])).unwrap();
        assert_eq!(cli.mo2.unwrap().profile, "Alpha");
    }

    #[test]
    fn repair_rejects_metadata_rebuild_options() {
        let error = parse_command(args(&[
            "repair-failed",
            "Data",
            "out",
            "--reuse-assets",
            "original",
        ]))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("repair-failed accepts Data/output, MO2 selection, encoding options, --no-lod and --apply only")
        );
        assert!(matches!(
            parse_command(args(&["repair-failed", "Data", "out", "--apply"])).unwrap(),
            Command::Repair(_, true)
        ));
    }

    #[test]
    fn parses_metadata_rebuild_source() {
        let cli = parse_cli(
            ["Data", "derived", "--reuse-assets", "original"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        )
        .unwrap();
        assert_eq!(cli.reuse_assets, Some(PathBuf::from("original")));
    }

    #[test]
    fn metadata_reports_cannot_overwrite_source_data_or_generated_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        for path in [&data, &source, &output] {
            fs::create_dir(path).unwrap();
        }
        for root in [&source, &data, &output] {
            let cli = parse_cli(vec![
                data.clone().into_os_string(),
                output.clone().into_os_string(),
                OsString::from("--reuse-assets"),
                source.clone().into_os_string(),
                OsString::from("--report-json"),
                root.join("new/nested/metadata.json").into_os_string(),
            ])
            .unwrap();
            assert!(validate_metadata_report_path(&cli).is_err());
        }
    }

    #[test]
    fn metadata_reports_accept_missing_disjoint_parent_without_creating_it() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        fs::create_dir(&data).unwrap();
        fs::create_dir(&source).unwrap();
        let reports = directory.path().join("new/nested");
        let cli = parse_cli(vec![
            data.into_os_string(),
            directory.path().join("derived").into_os_string(),
            OsString::from("--reuse-assets"),
            source.into_os_string(),
            OsString::from("--report-json"),
            reports.join("metadata.json").into_os_string(),
        ])
        .unwrap();
        validate_metadata_report_path(&cli).unwrap();
        assert!(!reports.exists());
    }

    #[test]
    fn formats_elapsed_time_for_log_lines() {
        assert_eq!(format_elapsed(0.0), "0:00:00.0");
        assert_eq!(format_elapsed(62.25), "0:01:02.3");
        assert_eq!(
            format_elapsed(5.0 * 3600.0 + 7.0 * 60.0 + 9.94),
            "5:07:09.9"
        );
        assert_eq!(format_elapsed(59.96), "0:01:00.0");
    }

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

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn repair_preserves_conversion_encoding_config_and_resume_flags() {
        for encoder in ["cpu", "gpu"] {
            let mut options = vec![
                "Data",
                "output",
                "--no-lod",
                "--texture-fallback-quality",
                "123",
                "--texture-uastc-level",
                "4",
                "--texture-zstd-level",
                "0",
                "--texture-encoder",
                encoder,
            ];
            if encoder == "gpu" {
                options.extend(["--gpu-quality", "7", "--gpu-batch-mb", "64"]);
            }
            let conversion = parse_cli(args(&options)).unwrap();
            let mut repair_args = vec!["repair-failed"];
            repair_args.extend(options);
            repair_args.push("--apply");
            let Command::Repair(repair, apply) = parse_command(args(&repair_args)).unwrap() else {
                panic!("repair was not parsed as a subcommand");
            };
            assert!(apply);
            let config = repair.pipeline_config();
            assert!(config.no_lod);
            assert!(conversion.pipeline_config().no_lod);
            assert!(config.lod_origins.is_empty());
            assert_eq!(config.texture_fallback_quality, 123);
            assert_eq!(config.texture_uastc_level, 4);
            assert_eq!(config.texture_zstd_level, 0);
            assert_eq!(
                config.texture_encoder,
                if encoder == "gpu" {
                    TextureEncoder::Gpu {
                        quality: 7,
                        batch_mb: 64,
                    }
                } else {
                    TextureEncoder::Cpu
                }
            );
            assert_eq!(
                converter::cache::configuration_hash(&config).unwrap(),
                converter::cache::configuration_hash(&conversion.pipeline_config()).unwrap(),
            );
            let resume = resume_command("converter", &conversion, Path::new("staging"));
            assert!(resume.contains("--texture-fallback-quality 123"));
            assert!(resume.contains("--texture-uastc-level 4"));
            assert!(resume.contains("--texture-zstd-level 0"));
            assert!(resume.contains("--no-lod"));
            let mut lod_config = config.clone();
            lod_config.no_lod = false;
            lod_config
                .lod_origins
                .insert("CustomWorld".into(), [-4, 12]);
            assert_eq!(
                converter::cache::configuration_hash(&config).unwrap(),
                converter::cache::configuration_hash(&lod_config).unwrap(),
            );
        }
    }

    #[test]
    fn encoding_defaults_alias_and_validation_apply_to_repair() {
        let cli = parse_cli(args(&["Data"])).unwrap();
        let expected = PipelineConfig::new("Data", "modern_assets");
        let config = cli.pipeline_config();
        assert_eq!(
            config.texture_fallback_quality,
            expected.texture_fallback_quality
        );
        assert_eq!(config.texture_uastc_level, expected.texture_uastc_level);
        assert_eq!(config.texture_zstd_level, expected.texture_zstd_level);
        let Command::Repair(cli, apply) = parse_command(args(&[
            "repair-failed",
            "Data",
            "out",
            "--texture-etc1s-quality",
            "255",
            "--texture-uastc-level",
            "0",
            "--texture-zstd-level",
            "22",
        ]))
        .unwrap() else {
            panic!("expected repair");
        };
        assert!(!apply);
        assert_eq!(cli.pipeline_config().texture_fallback_quality, 255);
        for (flag, value) in [
            ("--texture-fallback-quality", "0"),
            ("--texture-fallback-quality", "256"),
            ("--texture-uastc-level", "5"),
            ("--texture-zstd-level", "23"),
            ("--texture-zstd-level", "-1"),
            ("--texture-uastc-level", "bad"),
            ("--gpu-quality", "2"),
            ("--resume-staging", "staging"),
            ("--cpu-jobs", "2"),
        ] {
            assert!(parse_command(args(&["repair-failed", "Data", "out", flag, value])).is_err());
        }
        for flag in [
            "--texture-fallback-quality",
            "--texture-uastc-level",
            "--texture-zstd-level",
            "--fail-fast",
            "--invalidate-cache",
        ] {
            assert!(parse_command(args(&["repair-failed", "Data", "out", flag])).is_err());
        }
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
            "23 problem(s) in 100 files, 3.1 MB, quick check: existence and size, 0.2 s:"
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
            "All good: 100 files, 3.1 MB, quick check: existence and size, 0.2 s\n"
        );
    }

    /// The resume command repeats the paths, quotes them, and keeps the GPU encoder flags.
    #[test]
    fn names_the_exact_command_that_resumes_a_kept_staging_folder() {
        let cli = Cli {
            mo2: None,
            data: PathBuf::from("C:/Games/Skyrim/Data"),
            output: PathBuf::from("C:/Modding/SkyrimConverted"),
            resume_staging: None,
            reuse_assets: None,
            report_json: None,
            cpu_jobs: None,
            io_jobs: None,
            texture_encoder: TextureEncoder::Cpu,
            texture_fallback_quality: None,
            texture_uastc_level: None,
            texture_zstd_level: None,
            record_reader: RecordReader::Legacy,
            lod_texture_encoder: TextureEncoder::Cpu,
            fail_fast: false,
            invalidate_cache: false,
            verify_cache: true,
            no_lod: false,
            verbose: false,
            language: None,
        };
        let staging = Path::new("C:/Modding/SkyrimConverted.staging-1-2");
        assert_eq!(
            resume_command("converter.exe", &cli, staging),
            "converter.exe \"C:/Games/Skyrim/Data\" \"C:/Modding/SkyrimConverted\" --resume-staging \"C:/Modding/SkyrimConverted.staging-1-2\" --lod-encoder cpu"
        );
        // A renamed binary is named as it is, quoted when its name has a space.
        assert_eq!(
            resume_command("mudcrab converter", &cli, staging),
            "\"mudcrab converter\" \"C:/Games/Skyrim/Data\" \"C:/Modding/SkyrimConverted\" --resume-staging \"C:/Modding/SkyrimConverted.staging-1-2\" --lod-encoder cpu"
        );
        // The test binary itself stands in for the running converter.
        assert!(!program_name().is_empty());
        let no_lod = parse_cli(vec!["Data".into(), "--no-lod".into()]).unwrap();
        assert!(no_lod.no_lod);
        assert!(resume_command("converter", &no_lod, staging).ends_with(" --no-lod"));
        let french = parse_cli(vec!["Data".into(), "--language".into(), "french".into()]).unwrap();
        assert_eq!(french.language.as_deref(), Some("french"));
        assert!(resume_command("converter", &french, staging).ends_with(" --language french"));
        let inhouse = parse_cli(vec![
            "Data".into(),
            "--record-reader".into(),
            "inhouse".into(),
        ])
        .unwrap();
        assert_eq!(inhouse.record_reader, RecordReader::Inhouse);
        assert!(
            resume_command("converter", &inhouse, staging).ends_with(" --record-reader inhouse")
        );
        assert!(
            parse_cli(vec![
                "Data".into(),
                "--record-reader".into(),
                "other".into()
            ])
            .is_err()
        );
        assert!(parse_cli(vec!["Data".into(), "--record-reader".into()]).is_err());
        // A GPU run resumes on the GPU.
        let gpu = Cli {
            texture_encoder: TextureEncoder::Gpu {
                quality: 2,
                batch_mb: 256,
            },
            ..cli
        };
        assert!(resume_command("converter.exe", &gpu, staging).ends_with(
            " --texture-encoder gpu --gpu-quality 2 --gpu-batch-mb 256 --lod-encoder cpu"
        ));
    }

    /// Integration failures count as failures without skipped inputs and point to the
    /// integration report; advisory notices are never counted.
    #[test]
    fn incomplete_summary_counts_integration_warnings_but_not_notices() {
        let mut report = PipelineReport {
            warnings: vec!["asset integration failed: 1 missing models".into()],
            notices: vec!["nested plugins ignored".into(), "another advisory".into()],
            integration: Some(converter::IntegrationReport {
                passed: false,
                ..Default::default()
            }),
            ..PipelineReport::default()
        };
        assert_eq!(report.skipped, 0);
        let summary = incomplete_summary(&report, Path::new("modern"));
        assert!(
            summary.starts_with("Conversion incomplete: 1 failure(s), 0 input(s) skipped."),
            "{summary}"
        );
        assert!(summary.contains("integration-report.json"), "{summary}");

        // A skipped input with a passing integration does not mention the integration report.
        report.integration = Some(converter::IntegrationReport {
            passed: true,
            ..Default::default()
        });
        report.warnings = vec!["textures/bad.dds: not a DDS".into()];
        report.skipped = 1;
        let summary = incomplete_summary(&report, Path::new("modern"));
        assert!(
            summary.starts_with("Conversion incomplete: 1 failure(s), 1 input(s) skipped."),
            "{summary}"
        );
        assert!(!summary.contains("integration-report.json"), "{summary}");
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
            "Conversion complete in 0:01:02.3: converted 10, reused 4, failed 0, skipped 0"
        );

        report.skipped = 2;
        report.warnings = vec!["bad mesh".into(), "bad texture".into()];
        report.complete = false;
        assert_eq!(
            summary_headline(&report),
            "Conversion finished in 0:01:02.3, incomplete: converted 10, reused 4, failed 2, skipped 2"
        );
    }

    /// The headline counts integration failures even when no input was skipped, excluding notices.
    #[test]
    fn summary_headline_counts_integration_failures_without_skipped_inputs() {
        let report = PipelineReport {
            warnings: vec!["asset integration failed: 1 missing models".into()],
            notices: vec!["nested plugins ignored".into()],
            integration: Some(converter::IntegrationReport {
                passed: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(report.skipped, 0);
        assert!(summary_headline(&report).ends_with("failed 1, skipped 0"));
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

    /// LOD defaults to GPU independently of ordinary textures, and explicit CPU survives resume.
    #[test]
    fn defaults_to_gpu_lod_and_preserves_explicit_cpu_on_resume() {
        let cli = parse_cli(vec!["Data".into()]).unwrap();
        assert_eq!(cli.texture_encoder, TextureEncoder::Cpu);
        assert_eq!(
            cli.lod_texture_encoder,
            TextureEncoder::Gpu {
                quality: converter::texture_gpu::DEFAULT_QUALITY,
                batch_mb: converter::texture_gpu::DEFAULT_BATCH_MB,
            }
        );
        let tuned = parse_cli(
            ["Data", "--gpu-quality", "3", "--gpu-batch-mb", "128"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        )
        .unwrap();
        assert_eq!(tuned.texture_encoder, TextureEncoder::Cpu);
        assert_eq!(
            tuned.lod_texture_encoder,
            TextureEncoder::Gpu {
                quality: 3,
                batch_mb: 128
            }
        );
        let cpu = parse_cli(
            ["Data", "output", "--lod-encoder", "cpu"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        )
        .unwrap();
        assert_eq!(cpu.lod_texture_encoder, TextureEncoder::Cpu);
        assert!(
            resume_command("converter", &cpu, Path::new("staging")).ends_with(" --lod-encoder cpu")
        );
        let resumed = parse_cli(
            [
                "Data",
                "output",
                "--resume-staging",
                "staging",
                "--lod-encoder",
                "cpu",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
        .unwrap();
        assert_eq!(resumed.lod_texture_encoder, cpu.lod_texture_encoder);
        for option in ["--gpu-quality", "--gpu-batch-mb"] {
            assert!(
                parse_cli(
                    [
                        "Data",
                        "--texture-encoder",
                        "cpu",
                        "--lod-encoder",
                        "cpu",
                        option,
                        "2"
                    ]
                    .into_iter()
                    .map(OsString::from)
                    .collect(),
                )
                .is_err()
            );
        }
    }

    /// The GPU encoder flags reach the parsed configuration.
    #[test]
    fn parses_gpu_texture_options() {
        let cli = parse_cli(
            [
                "Data",
                "--texture-encoder",
                "gpu",
                "--gpu-quality",
                "3",
                "--gpu-batch-mb",
                "128",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
        .unwrap();
        assert_eq!(
            cli.texture_encoder,
            TextureEncoder::Gpu {
                quality: 3,
                batch_mb: 128
            }
        );
        // GPU settings without the GPU encoder are a mistake, not a no-op.
        let error = parse_cli(
            ["Data", "--lod-encoder", "cpu", "--gpu-quality", "3"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("--texture-encoder gpu"));
        let lod = parse_cli(
            ["Data", "--lod-encoder", "gpu", "--gpu-quality", "3"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        )
        .unwrap();
        assert_eq!(lod.texture_encoder, TextureEncoder::Cpu);
        assert_eq!(
            lod.lod_texture_encoder,
            TextureEncoder::Gpu {
                quality: 3,
                batch_mb: 256
            }
        );
        assert!(
            resume_command("converter", &lod, Path::new("staging"))
                .ends_with(" --lod-encoder gpu --gpu-quality 3 --gpu-batch-mb 256")
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
