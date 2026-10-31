use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use converter::archive::IngestionSync;
use converter::config::RecordReader;
use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, ProgressEvent, ProgressStage, TextureEncoder,
    config::TerrainDefaultTexture,
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
    plugins_file: Option<PathBuf>,
    skyrim_ccc_file: Option<PathBuf>,
    skyrim_ini_file: Option<PathBuf>,
    reuse_assets: Option<PathBuf>,
    report_json: Option<PathBuf>,
    cpu_jobs: Option<usize>,
    io_jobs: Option<usize>,
    ingestion_sync: IngestionSync,
    extract_all_archive_files: bool,
    texture_encoder: TextureEncoder,
    lod_texture_encoder: TextureEncoder,
    record_reader: RecordReader,
    schema_addons: Vec<PathBuf>,
    texture_fallback_quality: Option<u8>,
    texture_uastc_level: Option<u8>,
    texture_zstd_level: Option<i32>,
    fail_fast: bool,
    invalidate_cache: bool,
    verify_cache: bool,
    no_lod: bool,
    discard_derived: bool,
    lod_mode: converter::cluster::LodMode,
    cluster_triangle_threshold: u32,
    cluster_terrain: bool,
    merged_terrain: bool,
    terrain_default_texture: TerrainDefaultTexture,
    terrain_ini: Option<PathBuf>,
    terrain_allow_downscale: bool,
    verbose: bool,
    /// Overrides the default string-table language.
    language: Option<String>,
}

impl Cli {
    /// Preserve conversion and profile selection when constructing the pipeline.
    fn pipeline_config(&self) -> PipelineConfig {
        let mut config = PipelineConfig::new(self.data.clone(), self.output.clone());
        config.resume_staging = self.resume_staging.clone();
        config.mo2 = self.mo2.clone();
        config.plugins_file = self.plugins_file.clone();
        config.skyrim_ccc_file = self.skyrim_ccc_file.clone();
        config.skyrim_ini_file = self.skyrim_ini_file.clone();
        config.record_reader = self.record_reader;
        config.schema_addons = self.schema_addons.clone();
        config.fail_fast = self.fail_fast;
        config.invalidate_cache = self.invalidate_cache;
        config.verify_cache = self.verify_cache;
        config.no_lod = self.no_lod;
        config.discard_derived = self.discard_derived;
        config.lod_mode = self.lod_mode;
        config.cluster_triangle_threshold = self.cluster_triangle_threshold;
        config.cluster_terrain = self.cluster_terrain;
        config.merged_terrain = self.merged_terrain;
        config.terrain_default_texture = self.terrain_default_texture;
        config.terrain_ini = self.terrain_ini.clone();
        config.terrain_allow_downscale = self.terrain_allow_downscale;
        config.ingestion_sync = self.ingestion_sync;
        config.extract_all_archive_files = self.extract_all_archive_files;
        config.texture_encoder = self.texture_encoder;
        config.lod_texture_encoder = self.lod_texture_encoder;
        if let Some(language) = &self.language {
            config.language = language.clone();
        }
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
    // Boxed: a conversion's options are an order of magnitude larger than a check's.
    Convert(Box<Cli>),
    Check(CheckCli),
    Repair(Box<Cli>, bool),
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
    if args.first().and_then(|argument| argument.to_str()) == Some("refresh-extensions") {
        return refresh_extensions(&args[1..]);
    }
    if args.first().and_then(|argument| argument.to_str()) == Some("derive-clusters") {
        return derive_clusters(&args[1..]);
    }
    let command = parse_command(args)?;
    let mo2 = match &command {
        Command::Convert(cli) => cli.mo2.as_ref(),
        Command::Repair(cli, _) => cli.mo2.as_ref(),
        Command::Check(_) => None,
    };
    if let Some(selection) = mo2 {
        eprintln!(
            "MO2 profile: {:?} (instance: {})",
            selection.profile,
            selection.instance_path.display()
        );
    }
    let cli = match command {
        Command::Convert(cli) => *cli,
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
            if !report.record_data_complete {
                eprintln!(
                    "Record data remains incomplete or unverified. Run normal conversion to recheck plugin data and rebuild world metadata."
                );
            }
            if !report.failures.is_empty() || !report.record_data_complete {
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
    for notice in report.notices.iter().chain(&report.animation_notices) {
        println!("  note: {notice}");
    }
    for warning in &report.selection_warnings {
        println!("  warning: {warning}");
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
    for (flag, path) in [
        ("--plugins-file", &cli.plugins_file),
        ("--skyrim-ccc-file", &cli.skyrim_ccc_file),
        ("--skyrim-ini-file", &cli.skyrim_ini_file),
    ] {
        if let Some(path) = path {
            command.push_str(&format!(" {flag} \"{}\"", path.display()));
        }
    }
    if cli.discard_derived {
        command.push_str(" --discard-derived");
    }
    if cli.extract_all_archive_files {
        command.push_str(" --extract-all-archive-files");
    }
    match cli.ingestion_sync {
        IngestionSync::PerFile => command.push_str(" --ingestion-sync per-file"),
        IngestionSync::Archive => command.push_str(" --ingestion-sync archive"),
        IngestionSync::None => command.push_str(" --ingestion-sync none"),
    }
    if cli.no_lod {
        command.push_str(" --no-lod");
    }
    command.push_str(match cli.record_reader {
        RecordReader::Legacy => " --record-reader legacy",
        RecordReader::Inhouse => " --record-reader inhouse",
    });
    for path in &cli.schema_addons {
        command.push_str(&format!(" --schema-addon \"{}\"", path.display()));
    }
    if cli.lod_mode != converter::cluster::LodMode::default() {
        command.push_str(&format!(" --lod-mode {}", cli.lod_mode));
    }
    if cli.cluster_triangle_threshold != converter::cluster::DEFAULT_TRIANGLE_THRESHOLD {
        command.push_str(&format!(
            " --cluster-triangle-threshold {}",
            cli.cluster_triangle_threshold
        ));
    }
    if cli.cluster_terrain {
        command.push_str(" --cluster-terrain");
    }
    // A resumed run must build the same generated outputs: the terrain stage is rebuilt from
    // the records on every run, so dropping the option would publish without it, and an option
    // the stage reads must not be switched silently.
    if cli.merged_terrain {
        command.push_str(" --merged-terrain");
        command.push_str(match cli.terrain_default_texture {
            TerrainDefaultTexture::White => " --terrain-default-texture white",
            TerrainDefaultTexture::SkyrimIni => " --terrain-default-texture skyrim-ini",
        });
        if let Some(ini) = &cli.terrain_ini {
            command.push_str(&format!(" --terrain-ini \"{}\"", ini.display()));
        }
        if cli.terrain_allow_downscale {
            command.push_str(" --terrain-allow-downscale");
        }
    }
    // The database is rebuilt on resume; it must read the same string tables.
    if let Some(language) = &cli.language {
        command.push_str(&format!(" --language {language}"));
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

/// Derive optional extensions in a new dataset without repairing or relabelling its core.
fn parse_derive_options(args: &[OsString]) -> Result<converter::derive_extensions::DeriveOptions> {
    use converter::derive_extensions::DeriveOptions;
    ensure!(
        !args.is_empty(),
        "usage: converter derive-clusters <base> [--label L] [--replace] [--keep-failed-staging] [--terrain] [--terrain-sources VFS] [--include-trees] [--jobs N] [--triangle-threshold N]"
    );
    let mut options = DeriveOptions {
        source: PathBuf::from(&args[0]),
        label: "default".into(),
        replace: false,
        keep_failed_staging: false,
        terrain: false,
        terrain_sources: None,
        jobs: std::thread::available_parallelism().map_or(1, usize::from),
        triangle_threshold: converter::cluster::DEFAULT_TRIANGLE_THRESHOLD,
        include_trees: false,
    };
    let mut flags = args[1..].iter().cloned();
    while let Some(flag) = flags.next() {
        match flag.to_str() {
            Some("--label") => {
                options.label = next_value(&mut flags, "--label")?
                    .into_string()
                    .map_err(|_| color_eyre::eyre::eyre!("label must be UTF-8"))?
            }
            Some("--replace") => options.replace = true,
            Some("--keep-failed-staging") => options.keep_failed_staging = true,
            Some("--terrain") => options.terrain = true,
            Some("--terrain-sources") => {
                options.terrain_sources =
                    Some(PathBuf::from(next_value(&mut flags, "--terrain-sources")?))
            }
            Some("--include-trees") => options.include_trees = true,
            Some("--jobs") => {
                options.jobs = parse_u32(next_value(&mut flags, "--jobs")?, "--jobs")? as usize
            }
            Some("--triangle-threshold") => {
                options.triangle_threshold = parse_u32(
                    next_value(&mut flags, "--triangle-threshold")?,
                    "--triangle-threshold",
                )?
            }
            _ => bail!("unknown derive-clusters option {flag:?}"),
        }
    }
    Ok(options)
}

fn derive_clusters(args: &[OsString]) -> Result<()> {
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        println!(
            "usage: converter derive-clusters <base> [--label L] [--replace] [--keep-failed-staging] [--terrain] [--terrain-sources VFS] [--include-trees] [--jobs N] [--triangle-threshold N]"
        );
        return Ok(());
    }
    let options = parse_derive_options(args)?;
    let report = converter::derive_extensions::run(&options)?;
    println!(
        "Derived {} cluster extensions in {}",
        report.extensions.len(),
        report.output.display()
    );
    for notice in report.notices {
        eprintln!("notice: {notice}");
    }
    if report.incomplete_core {
        eprintln!(
            "DEVELOPMENT DATASET: core record warnings remain. Cluster certification does not certify global data completeness."
        );
    }
    Ok(())
}

/// `converter refresh-extensions <output directory>`: recomputes the manifest's `extensions` list
/// (its content hash and stats) from the folder as it is now, and saves the manifest. For a data
/// set whose list is stale or was written by an older hash implementation; a normal conversion,
/// metadata rebuild or repair does this by itself.
///
/// This repairs hashes, not payloads: it never re-derives an extension's content and never restamps
/// its provenance. Everything it cannot prove against the folder as it is now refuses the whole
/// operation, before the manifest is written:
///
/// * a listed extension whose `built_from` is not the data set's current core identity,
/// * a listed extension whose content is gone from the folder,
/// * a listed extension at a version or with an ownership this converter does not write,
/// * content for an extension the manifest never listed (unknown provenance),
/// * any other holder of the folder's asset lock.
///
/// An extension this converter does not write keeps its original JSON fields and values.
fn refresh_extensions(args: &[OsString]) -> Result<()> {
    let [folder] = args else {
        color_eyre::eyre::bail!("usage: converter refresh-extensions <output directory>");
    };
    let folder = std::path::Path::new(folder);
    // The lock a conversion takes, held before the manifest is read and until it is replaced, so a
    // concurrent conversion, repair or engine read cannot interleave with this refresh.
    let _lock = shared::asset_lock::AssetLock::acquire_exclusive(folder)
        .wrap_err("cannot refresh an asset directory in use")?;
    let path = folder.join("conversion-manifest.json");
    // Only the `extensions` key is replaced: the manifest is edited as plain JSON so nothing else
    // in it (entries, failures, hashes) goes through a load filter or is dropped.
    let text = std::fs::read(&path)
        .map_err(|error| color_eyre::eyre::eyre!("reading {}: {error}", path.display()))?;
    let mut manifest: serde_json::Value = serde_json::from_slice(&text)?;
    ensure!(manifest.is_object(), "the manifest is not a JSON object");
    let before: Vec<serde_json::Value> = match manifest.get("extensions") {
        Some(listed) => serde_json::from_value(listed.clone())?,
        None => Vec::new(),
    };
    let mut listed_ids = std::collections::BTreeSet::new();
    for raw in &before {
        let id = raw
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| color_eyre::eyre::eyre!("extension entry has no string id"))?;
        ensure!(
            listed_ids.insert(id.to_owned()),
            "extension {id} is listed twice"
        );
    }
    let specs = converter::extensions::specs();
    for spec in &specs {
        if !listed_ids.contains(spec.id) && extension_has_owned_content(folder, spec)? {
            bail!(
                "the data set holds {} content but {} has no extension entry for it; \
                convert the data set again so the conversion records where it came from",
                spec.id,
                path.display()
            );
        }
    }
    let now = converter::extensions::build_entries(folder)?;
    let mut entries = Vec::with_capacity(before.len());
    for raw in &before {
        let id = raw["id"].as_str().expect("entry ids validated above");
        let Some(spec) = specs.iter().find(|spec| spec.id == id) else {
            // Preserve foreign fields, including fields this engine does not deserialize.
            entries.push(raw.clone());
            continue;
        };
        let old: shared::extensions::ExtensionEntry = serde_json::from_value(raw.clone())
            .wrap_err_with(|| {
                format!(
                    "{} lists extension {id} owning content this converter does not write; \
                     convert the data set again instead of refreshing its hashes",
                    path.display()
                )
            })?;
        // A payload of another version or shape is not ours to interpret as the current one.
        ensure!(
            old.version == spec.version,
            "{} lists extension {} version {}, but this converter writes version {}; \
             convert the data set again instead of refreshing its hashes",
            path.display(),
            old.id,
            old.version,
            spec.version
        );
        let expected_owns = serde_json::to_value(&spec.owns)?;
        ensure!(
            old.owns == spec.owns
                && raw["owns"].as_object().is_some_and(|owns| {
                    owns.keys().all(|key| expected_owns.get(key).is_some())
                }),
            "{} lists extension {} owning content this converter does not write; \
             convert the data set again instead of refreshing its hashes",
            path.display(),
            old.id
        );
        let Some(fresh) = now.iter().find(|entry| entry.id == old.id) else {
            bail!(
                "{} lists extension {} but the data set no longer holds its content; refresh \
                 repairs hashes, not payloads — convert the data set again",
                path.display(),
                old.id
            );
        };
        // `fresh.built_from` is the folder's current core identity. An entry derived from another
        // conversion cannot be re-hashed without re-deriving its payload, and re-hashing it here
        // would bless content this converter never wrote.
        ensure!(
            fresh.built_from == old.built_from,
            "{} records extension {} built from {}, but the data set's core is now {}; \
             convert the data set again instead of refreshing its hashes",
            path.display(),
            old.id,
            old.built_from,
            fresh.built_from
        );
        let mut updated = raw.clone();
        updated["sha256"] = serde_json::to_value(&fresh.sha256)?;
        updated["stats"] = serde_json::to_value(&fresh.stats)?;
        entries.push(updated);
    }
    // Content for an extension the manifest never listed carries no provenance this command can
    // vouch for: refresh must not invent an entry for it.
    for fresh in &now {
        ensure!(
            listed_ids.contains(&fresh.id),
            "the data set holds {} content but {} has no extension entry for it; \
             convert the data set again so the conversion records where it came from",
            fresh.id,
            path.display()
        );
    }

    if entries == before {
        println!("{}: extension entries unchanged", folder.display());
        return Ok(());
    }
    for entry in &now {
        println!(
            "{} v{} {}: refreshed",
            entry.id, entry.version, entry.sha256
        );
    }
    let object = manifest
        .as_object_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("the manifest is not a JSON object"))?;
    // Never empty here: a listed extension that lost its content is refused above, and a manifest
    // with no entries at all returns before this point.
    object.insert("extensions".to_owned(), serde_json::to_value(&entries)?);
    converter::durability::replace_file(&path, &serde_json::to_vec_pretty(&manifest)?)?;
    Ok(())
}

/// Detect any owned table/index/file, even when the extension is incomplete. glTF-only payloads
/// are detected by build_entries, whose glTF specs do not require database tables.
fn extension_has_owned_content(
    folder: &Path,
    spec: &converter::extensions::ExtensionSpec,
) -> Result<bool> {
    let database = folder.join("skyrim_world.db");
    if database.is_file() && (!spec.owns.tables.is_empty() || !spec.owns.indexes.is_empty()) {
        let connection = rusqlite::Connection::open_with_flags(
            &database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        for (kind, names) in [("table", &spec.owns.tables), ("index", &spec.owns.indexes)] {
            for name in names {
                let exists: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2)",
                    [kind, name.as_str()],
                    |row| row.get(0),
                )?;
                if exists {
                    return Ok(true);
                }
            }
        }
    }
    for name in &spec.owns.files {
        let path = folder.join(name);
        if name.ends_with('/') {
            if !path.exists() {
                continue;
            }
            for entry in walkdir::WalkDir::new(path) {
                if entry?.file_type().is_file() {
                    return Ok(true);
                }
            }
        } else if path.is_file() {
            return Ok(true);
        }
    }
    Ok(false)
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

/// Parse conversion, check, or repair arguments with boxed pipeline commands.
fn parse_command(args: Vec<OsString>) -> Result<Command> {
    if args.first().and_then(|argument| argument.to_str()) == Some("check") {
        return parse_check(args.into_iter().skip(1)).map(Command::Check);
    }
    if args.first().and_then(|argument| argument.to_str()) == Some("repair-failed") {
        let apply = args.iter().any(|argument| argument == "--apply");
        let gpu_tuning = args
            .iter()
            .any(|argument| argument == "--gpu-quality" || argument == "--gpu-batch-mb");
        let cli = parse_cli(
            args.into_iter()
                .skip(1)
                .filter(|argument| argument != "--apply")
                .collect(),
        )?;
        // Repair encodes textures only; terrain LOD defaults to GPU but never runs here.
        if gpu_tuning && !matches!(cli.texture_encoder, TextureEncoder::Gpu { .. }) {
            bail!(
                "repair-failed applies --gpu-quality and --gpu-batch-mb only with --texture-encoder gpu"
            );
        }
        // Repair skips the merged-terrain stage, so accepting the option would publish without it.
        if cli.merged_terrain {
            bail!(
                "repair-failed does not build merged terrain; drop --merged-terrain and its options"
            );
        }
        if cli.resume_staging.is_some()
            || cli.plugins_file.is_some()
            || cli.skyrim_ccc_file.is_some()
            || cli.skyrim_ini_file.is_some()
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
                "repair-failed accepts Data/output, MO2 selection, reader/schema identity, encoding options, --no-lod and --apply only"
            );
        }
        return Ok(Command::Repair(Box::new(cli), apply));
    }
    parse_cli(args).map(|cli| Command::Convert(Box::new(cli)))
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

/// Parse conversion options, preserving source selection and schema add-on order for resumed runs.
fn parse_cli(args: Vec<OsString>) -> Result<Cli> {
    let mut positional = Vec::new();
    let mut mo2_instance = None;
    let mut mo2_profile = None;
    let mut report_json = None;
    let mut resume_staging = None;
    let mut reuse_assets = None;
    let mut plugins_file = None;
    let mut skyrim_ccc_file = None;
    let mut skyrim_ini_file = None;
    let mut cpu_jobs = None;
    let mut io_jobs = None;
    let mut ingestion_sync = IngestionSync::Archive;
    let mut extract_all_archive_files = false;
    let mut use_gpu = false;
    let mut use_lod_gpu = true;
    let mut record_reader = RecordReader::default();
    let mut schema_addons = Vec::new();
    let mut gpu_quality = None;
    let mut gpu_batch_mb = None;
    let mut texture_fallback_quality = None;
    let mut texture_uastc_level = None;
    let mut texture_zstd_level = None;
    let mut fail_fast = false;
    let mut invalidate_cache = false;
    let mut verify_cache = true;
    let mut no_lod = false;
    let mut discard_derived = false;
    let mut lod_mode = converter::cluster::LodMode::default();
    let mut cluster_triangle_threshold = converter::cluster::DEFAULT_TRIANGLE_THRESHOLD;
    let mut cluster_terrain = false;
    let mut merged_terrain = false;
    let mut terrain_default_texture = TerrainDefaultTexture::default();
    let mut terrain_ini = None;
    let mut terrain_allow_downscale = false;
    // The first merged-terrain option named, which needs --merged-terrain to mean anything.
    let mut terrain_option: Option<&'static str> = None;
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
            Some("--plugins-file") => {
                plugins_file = Some(next_path_value(&mut args, "--plugins-file")?)
            }
            Some("--skyrim-ccc-file") => {
                skyrim_ccc_file = Some(next_path_value(&mut args, "--skyrim-ccc-file")?)
            }
            Some("--skyrim-ini-file") => {
                skyrim_ini_file = Some(next_path_value(&mut args, "--skyrim-ini-file")?)
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
            Some("--lod-encoder") => {
                use_lod_gpu = match next_value(&mut args, "--lod-encoder")?.to_str() {
                    Some("cpu") => false,
                    Some("gpu") => true,
                    _ => bail!("--lod-encoder must be cpu or gpu"),
                };
            }
            Some("--record-reader") => {
                record_reader = match next_value(&mut args, "--record-reader")?.to_str() {
                    Some("legacy") => RecordReader::Legacy,
                    Some("inhouse") => RecordReader::Inhouse,
                    _ => bail!("--record-reader must be legacy or inhouse"),
                };
            }
            Some("--schema-addon") => {
                schema_addons.push(next_path_value(&mut args, "--schema-addon")?);
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
            Some("--discard-derived") => discard_derived = true,
            Some("--lod-mode") => {
                let value = next_value(&mut args, "--lod-mode")?;
                lod_mode = value
                    .to_str()
                    .and_then(|value| value.parse().ok())
                    .ok_or_else(|| {
                        color_eyre::eyre::eyre!("--lod-mode must be auto, cluster or baked")
                    })?;
            }
            Some("--cluster-triangle-threshold") => {
                cluster_triangle_threshold = parse_u32(
                    next_value(&mut args, "--cluster-triangle-threshold")?,
                    "--cluster-triangle-threshold",
                )?
            }
            Some("--cluster-terrain") => cluster_terrain = true,
            Some("--merged-terrain") => merged_terrain = true,
            Some("--terrain-default-texture") => {
                terrain_option.get_or_insert("--terrain-default-texture");
                let value = next_value(&mut args, "--terrain-default-texture")?;
                terrain_default_texture = match value.to_str() {
                    Some("white") => TerrainDefaultTexture::White,
                    Some("skyrim-ini") => TerrainDefaultTexture::SkyrimIni,
                    _ => bail!("--terrain-default-texture must be white or skyrim-ini"),
                };
            }
            Some("--terrain-ini") => {
                terrain_option.get_or_insert("--terrain-ini");
                terrain_ini = Some(next_path_value(&mut args, "--terrain-ini")?)
            }
            Some("--terrain-allow-downscale") => {
                terrain_option.get_or_insert("--terrain-allow-downscale");
                terrain_allow_downscale = true
            }
            Some("--ingestion-sync") => {
                ingestion_sync = match next_value(&mut args, "--ingestion-sync")?.to_str() {
                    Some("per-file") => IngestionSync::PerFile,
                    Some("archive") => IngestionSync::Archive,
                    Some("none") => IngestionSync::None,
                    _ => bail!("--ingestion-sync must be per-file, archive, or none"),
                };
            }
            Some("--extract-all-archive-files") => extract_all_archive_files = true,
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
    if let Some(option) = terrain_option
        && !merged_terrain
    {
        bail!("{option} requires --merged-terrain");
    }
    if terrain_ini.is_some() && terrain_default_texture != TerrainDefaultTexture::SkyrimIni {
        bail!("--terrain-ini requires --terrain-default-texture skyrim-ini");
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
    if ingestion_sync == IngestionSync::None && !verify_cache {
        bail!("--ingestion-sync none requires verified cache reuse; omit --no-verify-cache");
    }
    if reuse_assets.is_some() && mo2_instance.is_some() {
        bail!(
            "--reuse-assets does not support MO2 selection; use a normal conversion with --mo2-instance"
        );
    }
    if (skyrim_ccc_file.is_some() || skyrim_ini_file.is_some()) && plugins_file.is_none() {
        bail!("--skyrim-ccc-file and --skyrim-ini-file require --plugins-file");
    }
    if reuse_assets.is_some() && plugins_file.is_some() {
        bail!("--reuse-assets uses recorded plugin order and cannot use --plugins-file");
    }
    if mo2_instance.is_some() && plugins_file.is_some() {
        bail!("--mo2-instance cannot be combined with --plugins-file");
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
        plugins_file,
        skyrim_ccc_file,
        skyrim_ini_file,
        reuse_assets,
        report_json,
        cpu_jobs,
        io_jobs,
        ingestion_sync,
        extract_all_archive_files,
        texture_encoder,
        lod_texture_encoder,
        record_reader,
        schema_addons,
        texture_fallback_quality,
        texture_uastc_level,
        texture_zstd_level,
        fail_fast,
        invalidate_cache,
        verify_cache,
        no_lod,
        discard_derived,
        lod_mode,
        cluster_triangle_threshold,
        cluster_terrain,
        merged_terrain,
        terrain_default_texture,
        terrain_ini,
        terrain_allow_downscale,
        verbose,
        language,
    })
}

/// Validate an encoding setting against its supported range.
fn parse_encoding_value(value: OsString, option: &str, min: u32, max: u32) -> Result<u32> {
    let value = parse_u32(value, option)?;
    if !(min..=max).contains(&value) {
        bail!("{option} must be between {min} and {max}");
    }
    Ok(value)
}

/// Require an explicit path without swallowing the next CLI flag.
fn next_path_value(args: &mut impl Iterator<Item = OsString>, option: &str) -> Result<PathBuf> {
    let value = next_value(args, option)?;
    if value.to_string_lossy().starts_with('-') {
        bail!("{option} requires a path value before the next option");
    }
    Ok(PathBuf::from(value))
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
                 [--record-reader legacy|inhouse] [--language NAME]
                 [--ingestion-sync per-file|archive|none] [--extract-all-archive-files]
                 [--schema-addon JSON]
                 [--mo2-instance DIR] [--mo2-profile NAME]
                 [--merged-terrain] [--terrain-default-texture white|skyrim-ini]
                 [--terrain-ini FILE] [--terrain-allow-downscale]
                 [--plugins-file FILE] [--skyrim-ccc-file FILE] [--skyrim-ini-file FILE]
                 [--lod-mode auto|cluster|baked] [--cluster-triangle-threshold N]
                 [--cluster-terrain]
       converter check <output directory> [--full]
       converter refresh-extensions <output directory>
       converter derive-clusters <base> [--label L] [--replace] [--keep-failed-staging] [--terrain] [--terrain-sources VFS] [--include-trees] [--jobs N] [--triangle-threshold N]
       converter repair-failed <Skyrim Data> <output directory> [--mo2-instance DIR]
                 [--mo2-profile NAME] [--no-lod] [--apply]
                 [--texture-encoder cpu|gpu] [--gpu-quality N] [--gpu-batch-mb N]
                 [--texture-fallback-quality 1..255] [--texture-uastc-level 0..4]
                 [--texture-zstd-level 0..22]
                 [--record-reader legacy|inhouse] [--schema-addon JSON]

repair-failed stages only manifest failures and missing dependencies in a repair directory inside the pack.
It preserves the record projection and cell cache, and finalizes asset references in the staged database.
--apply validates the pack's existing hashes, publishes validated repairs with backups,
and updates the manifest last.
Asset repair does not rebuild records. Unresolved record loss or missing historical
record-completeness proof keeps the pack incomplete and makes repair exit 1 even
when corrected assets were published. Normal conversion rechecks record data.
Use the original conversion's reader/schema identity and encoding settings. --texture-etc1s-quality is an alias for
--texture-fallback-quality; it does not select ETC1S encoding.

Converts a Skyrim Data directory into runtime assets.
The in-house record reader is the default; --record-reader legacy selects the hand-written reader.
Reader recovery preserves usable neighboring records and reports bounded warnings and notices.
Data-loss warnings mark the report and manifest incomplete and make conversion exit 1 even
when usable output was published. --report-json and the launcher show both severities;
metadata rebuild with --reuse-assets follows the same completeness rule.
Successful compatibility remaps, in-house links cleared to absent or disallowed targets,
and missing localized IDs in readable string tables are notices and preserve completeness.
Missing or unreadable string tables and skipped records or fields remain warnings.
--mo2-instance overlays enabled MO2 mods and overwrite over physical Data, using the profile's
active plugins and loadorder.txt. --mo2-profile defaults to ModOrganizer.ini's selected_profile,
or the first sorted profile if that selection is absent or stale.
MO2 files are read only. Native SKSE DLLs and arbitrary mod compatibility are not supported.

--plugins-file reads Windows-1252 player selection. Installed official masters and
readable Skyrim.ccc Creations are always active. Missing or unreadable CCC activates
installed Fish, Survival Mode, Rare Curios, Saints and Seducers and _ResourcePack,
in that order, with a report warning. Readable CCC, including an empty file, wins.
--skyrim-ccc-file overrides the game-folder CCC path. --skyrim-ini-file explicitly
supplies General sTestFile settings; no ambient profile is read. Without
--plugins-file or MO2 selection, automatic discovery orders plugins by dependency and skips unusable
or ambiguously named plugins with bounded notices.
The JSON report lists every recovery exclusion in skipped_plugins and surviving filenames
in load_order; selection_warnings contains Creation fallback warnings separately from
failed asset conversions. Archive companions match an exact plugin stem or a following
space, dash or underscore. The longest installed owner must survive selection; unowned
archives are admitted outside MO2 and excluded in MO2.

MO2 and --plugins-file selections cannot be combined. Automatic discovery applies
when neither selection is supplied.

--schema-addon <JSON> may be repeated to add in-house record types and explicitly
extend fields. The full batch is checked before conversion starts. Schema add-ons
cannot be combined with --record-reader legacy.

Archive ingestion extracts the formats used by conversion and English string banks.
--extract-all-archive-files retains every archive entry for compatibility and comparisons;
it cannot be combined with forced cluster mode, which omits baked-only sources.
--ingestion-sync defaults to archive: sealed packs provide batch durability. per-file also
flushes each unique derived payload; none skips extraction flushes for regenerable bytes
and requires verified reuse. Runtime package publication keeps its durability contract.

--discard-derived retires existing derived overlays only when the new base publishes successfully.
A failed reconversion preserves the previous base and its overlays.
--no-lod skips terrain LOD compilation in conversion and metadata rebuilds. Full-detail
terrain and ordinary assets remain available. Omit it on a later run to build LOD.

--lod-mode chooses how distant opaque static objects are prepared (ADR-0013). `baked` (the
default) preserves native object/tree LOD and baked terrain. `cluster` omits those baked products,
always includes trees and terrain, and records its exact settings-addressed overlay in the base
manifest before publication. `auto` preserves the historical baked base and optional overlay
semantics; --cluster-terrain adds terrain to its overlay. Both publish the base first, then derive
under <base>/derived/. --no-lod cannot be combined with forced cluster mode.
The adapter is probed once and recorded; offline derivation runs even
when an auto probe reports missing runtime features. A derivation error leaves the base valid
and prints the command to retry just the overlay.
--cluster-terrain adds terrain clusters and their dependency metadata in auto mode; requires cluster/auto LOD.
--cluster-triangle-threshold sets the auto overlay's minimum primitive size (default 1024).
Forced cluster mode always uses 1 so every supported nonempty primitive has a draw path.

--merged-terrain builds the merged terrain of #215 after the world database and the land
textures: the land texture arrays, one merged mesh per exterior cell and the terrain chunk
files. It is off by default because the engine does not read those outputs yet.
--terrain-default-texture picks what a cell quadrant with no BTXT blends: the generated
white texture (default), or the game's own [Landscape] default.
--terrain-ini names the Skyrim.ini that default is read from; without it the built-in
textures/landscape/Dirt02.dds and Dirt02_N.dds stand in, as they do for a key the ini does
not name. A default that cannot be converted leaves the white texture, and the report says so.
--terrain-allow-downscale downscales a land texture larger than the largest array size class
into it instead of stopping the merged-terrain build; the report lists every one downscaled.
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
changing those source assets. This route does not support MO2 selection; use a normal
conversion with --mo2-instance. It cannot resume or cooperatively cancel."
}

#[cfg(test)]
mod tests {
    /// Default conversion routes use in-house and explicit legacy survives configuration and resume.
    #[test]
    fn cli_defaults_to_inhouse_and_preserves_explicit_legacy() {
        let conversion = parse_cli(args(&["Data", "pack"])).unwrap();
        assert_eq!(conversion.record_reader, RecordReader::Inhouse);
        assert_eq!(
            conversion.pipeline_config().record_reader,
            RecordReader::Inhouse
        );

        let metadata = parse_cli(args(&["Data", "pack", "--reuse-assets", "source"])).unwrap();
        assert_eq!(
            metadata.pipeline_config().record_reader,
            RecordReader::Inhouse
        );
        let Command::Repair(repair, _) =
            parse_command(args(&["repair-failed", "Data", "pack"])).unwrap()
        else {
            panic!("expected repair");
        };
        assert_eq!(
            repair.pipeline_config().record_reader,
            RecordReader::Inhouse
        );

        let legacy = parse_cli(args(&["Data", "pack", "--record-reader", "legacy"])).unwrap();
        assert_eq!(legacy.pipeline_config().record_reader, RecordReader::Legacy);
        assert!(
            resume_command("converter", &legacy, Path::new("staging"))
                .contains(" --record-reader legacy")
        );
    }

    /// Repeated schema paths preserve input order through parsing and resume commands.
    #[test]
    fn schema_addon_cli_preserves_order_and_resume_flags() {
        let cli = parse_cli(
            [
                "Data",
                "Pack",
                "--record-reader",
                "inhouse",
                "--schema-addon",
                "first.json",
                "--schema-addon",
                "second.json",
            ]
            .map(OsString::from)
            .to_vec(),
        )
        .unwrap();
        assert_eq!(
            cli.schema_addons,
            vec![PathBuf::from("first.json"), PathBuf::from("second.json")]
        );
        let config = cli.pipeline_config();
        assert_eq!(config.record_reader, RecordReader::Inhouse);
        assert_eq!(config.schema_addons, cli.schema_addons);
        let resume = resume_command("converter", &cli, Path::new("staging"));
        assert!(resume.contains("--schema-addon \"first.json\" --schema-addon \"second.json\""));
        assert!(
            parse_cli(
                ["Data", "--schema-addon", "--no-lod"]
                    .map(OsString::from)
                    .to_vec()
            )
            .is_err()
        );
    }
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
        assert!(resume.contains("--mo2-profile \"Zed\""));
        assert!(resume.ends_with(" --record-reader inhouse"));
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
                .contains("repair-failed accepts Data/output, MO2 selection, reader/schema identity, encoding options, --no-lod and --apply only")
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

    /// Unsupported MO2 metadata selection is rejected before opening an absent instance.
    #[test]
    fn mo2_metadata_rebuild_is_rejected_before_instance_open() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output");
        let instance = directory.path().join("absent-instance");
        let error = parse_cli(vec![
            directory.path().join("Data").into_os_string(),
            output.clone().into_os_string(),
            OsString::from("--reuse-assets"),
            directory.path().join("source").into_os_string(),
            OsString::from("--mo2-instance"),
            instance.clone().into_os_string(),
        ])
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("--reuse-assets does not support MO2 selection"),
            "wrong source boundary: {error}"
        );
        assert!(!output.exists());
        assert!(!instance.exists());
    }

    /// Reader and add-on identities survive the shared conversion/repair configuration handoff.
    #[test]
    fn repair_preserves_inhouse_reader_and_schema_identity() {
        let directory = tempfile::tempdir().unwrap();
        let addon = directory.path().join("identity.json");
        fs::write(
            &addon,
            br#"{"version":1,"name":"cli_identity","records":[]}"#,
        )
        .unwrap();
        let options = vec![
            OsString::from("Data"),
            OsString::from("output"),
            OsString::from("--record-reader"),
            OsString::from("inhouse"),
            OsString::from("--schema-addon"),
            addon.clone().into_os_string(),
        ];
        let conversion = parse_cli(options.clone()).unwrap();
        let mut repair_options = vec![OsString::from("repair-failed")];
        repair_options.extend(options);
        let Command::Repair(repair, _) = parse_command(repair_options).unwrap() else {
            panic!("expected repair");
        };
        let config = repair.pipeline_config();
        assert_eq!(config.record_reader, RecordReader::Inhouse);
        assert_eq!(config.schema_addons, vec![addon]);
        let identity = converter::cache::configuration_hash(&config).unwrap();
        assert_eq!(
            identity,
            converter::cache::configuration_hash(&conversion.pipeline_config()).unwrap()
        );
        assert_ne!(
            identity,
            converter::cache::configuration_hash(&PipelineConfig::new("Data", "output")).unwrap()
        );
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

    #[test]
    fn explicit_cluster_selection_reaches_the_pipeline_and_resume_command() {
        let cli = parse_cli(args(&[
            "Data",
            "--lod-mode",
            "cluster",
            "--cluster-triangle-threshold",
            "2048",
            "--cluster-terrain",
        ]))
        .unwrap();
        let config = cli.pipeline_config();
        assert_eq!(config.lod_mode, converter::cluster::LodMode::Cluster);
        assert_eq!(config.cluster_triangle_threshold, 2048);
        assert!(config.cluster_terrain);
        let resumed = resume_command("converter", &cli, Path::new("staging"));
        assert!(resumed.contains("--lod-mode cluster"), "{resumed}");
        assert!(
            resumed.contains("--cluster-triangle-threshold 2048"),
            "{resumed}"
        );
        assert!(resumed.contains("--cluster-terrain"), "{resumed}");
    }

    #[test]
    fn removed_measurement_switch_is_rejected() {
        assert!(parse_cli(args(&["Data", "--cluster-bench-baked"])).is_err());
        assert!(!usage().contains("--cluster-bench-baked"));
        assert!(usage().contains("--cluster-terrain"));
    }

    /// Selection inputs survive parsing and the command printed after interruption.
    #[test]
    fn explicit_plugin_paths_are_preserved_in_resume_commands() {
        let cli = parse_cli(vec![
            "Data".into(),
            "--plugins-file".into(),
            "player list.txt".into(),
            "--skyrim-ccc-file".into(),
            "game list.ccc".into(),
            "--skyrim-ini-file".into(),
            "test settings.ini".into(),
        ])
        .unwrap();
        assert_eq!(cli.plugins_file, Some(PathBuf::from("player list.txt")));
        assert_eq!(cli.skyrim_ccc_file, Some(PathBuf::from("game list.ccc")));
        assert_eq!(
            cli.skyrim_ini_file,
            Some(PathBuf::from("test settings.ini"))
        );
        let config = cli.pipeline_config();
        assert_eq!(config.plugins_file, cli.plugins_file);
        assert_eq!(config.skyrim_ccc_file, cli.skyrim_ccc_file);
        assert_eq!(config.skyrim_ini_file, cli.skyrim_ini_file);
        let resumed = resume_command("converter", &cli, Path::new("modern_assets.staging-1"));
        for pair in [
            "--plugins-file \"player list.txt\"",
            "--skyrim-ccc-file \"game list.ccc\"",
            "--skyrim-ini-file \"test settings.ini\"",
        ] {
            assert!(resumed.contains(pair), "{resumed}");
        }
        for flag in ["--plugins-file", "--skyrim-ccc-file", "--skyrim-ini-file"] {
            assert!(parse_cli(vec!["Data".into(), flag.into()]).is_err());
            assert!(parse_cli(vec!["Data".into(), flag.into(), "--no-lod".into()]).is_err());
        }
        assert!(
            parse_cli(args(&[
                "Data",
                "--plugins-file",
                "plugins.txt",
                "--mo2-instance",
                "instance"
            ]))
            .is_err()
        );
        assert!(
            parse_command(args(&[
                "repair-failed",
                "Data",
                "out",
                "--plugins-file",
                "plugins.txt"
            ]))
            .is_err()
        );
        assert!(
            parse_cli(vec![
                "Data".into(),
                "--skyrim-ini-file".into(),
                "settings.ini".into()
            ])
            .is_err()
        );
        assert!(
            parse_cli(vec![
                "Data".into(),
                "--plugins-file".into(),
                "plugins.txt".into(),
                "--reuse-assets".into(),
                "old_assets".into()
            ])
            .is_err()
        );
    }

    /// The resume command quotes paths and preserves the reader and GPU encoder options.
    #[test]
    fn names_the_exact_command_that_resumes_a_kept_staging_folder() {
        let cli = Cli {
            mo2: None,
            data: PathBuf::from("C:/Games/Skyrim/Data"),
            output: PathBuf::from("C:/Modding/SkyrimConverted"),
            resume_staging: None,
            plugins_file: None,
            skyrim_ccc_file: None,
            skyrim_ini_file: None,
            reuse_assets: None,
            report_json: None,
            cpu_jobs: None,
            io_jobs: None,
            ingestion_sync: IngestionSync::PerFile,
            extract_all_archive_files: false,
            texture_encoder: TextureEncoder::Cpu,
            lod_texture_encoder: TextureEncoder::Cpu,
            record_reader: RecordReader::Legacy,
            schema_addons: Vec::new(),
            texture_fallback_quality: None,
            texture_uastc_level: None,
            texture_zstd_level: None,
            fail_fast: false,
            invalidate_cache: false,
            verify_cache: true,
            no_lod: false,
            discard_derived: false,
            lod_mode: converter::cluster::LodMode::default(),
            cluster_triangle_threshold: converter::cluster::DEFAULT_TRIANGLE_THRESHOLD,
            cluster_terrain: false,
            merged_terrain: false,
            terrain_default_texture: TerrainDefaultTexture::White,
            terrain_ini: None,
            terrain_allow_downscale: false,
            verbose: false,
            language: None,
        };
        let staging = Path::new("C:/Modding/SkyrimConverted.staging-1-2");
        assert_eq!(
            resume_command("converter.exe", &cli, staging),
            "converter.exe \"C:/Games/Skyrim/Data\" \"C:/Modding/SkyrimConverted\" --resume-staging \"C:/Modding/SkyrimConverted.staging-1-2\" --lod-encoder cpu --ingestion-sync per-file --record-reader legacy"
        );
        // A renamed binary is named as it is, quoted when its name has a space.
        assert_eq!(
            resume_command("mudcrab converter", &cli, staging),
            "\"mudcrab converter\" \"C:/Games/Skyrim/Data\" \"C:/Modding/SkyrimConverted\" --resume-staging \"C:/Modding/SkyrimConverted.staging-1-2\" --lod-encoder cpu --ingestion-sync per-file --record-reader legacy"
        );
        // The test binary itself stands in for the running converter.
        assert!(!program_name().is_empty());
        let no_lod = parse_cli(vec!["Data".into(), "--no-lod".into()]).unwrap();
        assert!(no_lod.no_lod);
        assert!(
            resume_command("converter", &no_lod, staging)
                .ends_with(" --no-lod --record-reader inhouse")
        );
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
        // Merged terrain is off unless asked for, and a resume keeps it on with its default
        // texture selection.
        let terrain = parse_cli(vec!["Data".into(), "--merged-terrain".into()]).unwrap();
        assert!(terrain.merged_terrain);
        assert_eq!(
            terrain.terrain_default_texture,
            TerrainDefaultTexture::White
        );
        let terrain = parse_cli(vec![
            "Data".into(),
            "--merged-terrain".into(),
            "--terrain-default-texture".into(),
            "skyrim-ini".into(),
        ])
        .unwrap();
        assert!(terrain.merged_terrain);
        assert_eq!(
            terrain.terrain_default_texture,
            TerrainDefaultTexture::SkyrimIni
        );
        assert!(
            resume_command("converter", &terrain, staging)
                .ends_with(" --merged-terrain --terrain-default-texture skyrim-ini")
        );
        // The default texture is named even when it is the default, and the ini and the
        // downscale allowance travel with the resume: none of them may switch silently.
        let white = parse_cli(vec!["Data".into(), "--merged-terrain".into()]).unwrap();
        assert!(
            resume_command("converter", &white, staging)
                .ends_with(" --merged-terrain --terrain-default-texture white")
        );
        let terrain = parse_cli(vec![
            "Data".into(),
            "--merged-terrain".into(),
            "--terrain-default-texture".into(),
            "skyrim-ini".into(),
            "--terrain-ini".into(),
            "C:/Games/Skyrim/Skyrim.ini".into(),
            "--terrain-allow-downscale".into(),
        ])
        .unwrap();
        assert_eq!(
            terrain.terrain_ini,
            Some(PathBuf::from("C:/Games/Skyrim/Skyrim.ini"))
        );
        assert!(terrain.terrain_allow_downscale);
        assert!(resume_command("converter", &terrain, staging).ends_with(
            " --merged-terrain --terrain-default-texture skyrim-ini --terrain-ini \
                 \"C:/Games/Skyrim/Skyrim.ini\" --terrain-allow-downscale"
        ));
        assert!(
            parse_cli(vec!["Data".into(), "--terrain-ini".into()]).is_err(),
            "--terrain-ini needs a path"
        );
        assert!(
            parse_cli(vec![
                "Data".into(),
                "--terrain-default-texture".into(),
                "dirt".into()
            ])
            .is_err()
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
        let german = parse_cli(vec!["Data".into(), "--language".into(), "german".into()]).unwrap();
        assert_eq!(german.language.as_deref(), Some("german"));
        assert_eq!(german.pipeline_config().language, "german");
        assert!(resume_command("converter", &german, staging).ends_with(" --language german"));
        // A GPU run resumes on the GPU.
        let gpu = Cli {
            texture_encoder: TextureEncoder::Gpu {
                quality: 2,
                batch_mb: 256,
            },
            ..cli
        };
        assert!(resume_command("converter.exe", &gpu, staging).ends_with(
            " --texture-encoder gpu --gpu-quality 2 --gpu-batch-mb 256 --lod-encoder cpu --ingestion-sync per-file --record-reader legacy"
        ));
    }

    /// Merged-terrain options mean nothing without --merged-terrain, an ini needs the skyrim-ini
    /// default, --terrain-ini takes a path and never another option, and repair refuses the option.
    #[test]
    fn merged_terrain_options_need_the_option_and_their_own_values() {
        for dependent in [
            vec!["--terrain-default-texture", "skyrim-ini"],
            vec!["--terrain-default-texture", "white"],
            vec!["--terrain-allow-downscale"],
            vec!["--terrain-ini", "Skyrim.ini"],
        ] {
            let mut values = vec!["Data"];
            values.extend(dependent.iter().copied());
            let error = parse_cli(args(&values)).unwrap_err().to_string();
            assert!(
                error.contains("requires --merged-terrain"),
                "{values:?}: {error}"
            );
        }
        // Explicit white with an ini is refused: the ini is only read by skyrim-ini.
        let error = parse_cli(args(&[
            "Data",
            "--merged-terrain",
            "--terrain-default-texture",
            "white",
            "--terrain-ini",
            "Skyrim.ini",
        ]))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("requires --terrain-default-texture skyrim-ini"),
            "{error}"
        );
        let ini_without_default = parse_cli(args(&[
            "Data",
            "--merged-terrain",
            "--terrain-ini",
            "S.ini",
        ]));
        assert!(ini_without_default.is_err(), "the default is white");

        // The ini value is a path: the next option is not taken as one.
        let error = parse_cli(args(&[
            "Data",
            "--merged-terrain",
            "--terrain-default-texture",
            "skyrim-ini",
            "--terrain-ini",
            "--merged-terrain",
        ]))
        .unwrap_err()
        .to_string();
        assert!(error.contains("requires a path value"), "{error}");

        let ok = parse_cli(args(&[
            "Data",
            "--merged-terrain",
            "--terrain-default-texture",
            "skyrim-ini",
            "--terrain-ini",
            "C:/Games/Skyrim.ini",
        ]))
        .unwrap();
        assert_eq!(ok.terrain_ini, Some(PathBuf::from("C:/Games/Skyrim.ini")));

        // Repair applies no merged terrain, so it refuses the option with its settings.
        let error = parse_command(args(&["repair-failed", "Data", "out", "--merged-terrain"]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("repair-failed does not build merged terrain"),
            "{error}"
        );
        assert!(
            parse_command(args(&[
                "repair-failed",
                "Data",
                "out",
                "--terrain-allow-downscale"
            ]))
            .is_err()
        );
    }

    #[test]
    fn parses_and_preserves_ingestion_options_on_resume() {
        let staging = Path::new("assets.staging");
        let defaults = parse_cli(vec!["Data".into()]).unwrap();
        assert_eq!(defaults.ingestion_sync, IngestionSync::Archive);
        assert!(!defaults.extract_all_archive_files);
        for (value, policy) in [
            ("per-file", IngestionSync::PerFile),
            ("archive", IngestionSync::Archive),
            ("none", IngestionSync::None),
        ] {
            let cli = parse_cli(vec![
                "Data".into(),
                "--ingestion-sync".into(),
                value.into(),
                "--extract-all-archive-files".into(),
            ])
            .unwrap();
            assert_eq!(cli.ingestion_sync, policy);
            assert!(cli.extract_all_archive_files);
            let command = resume_command("converter", &cli, staging);
            assert!(command.contains(" --extract-all-archive-files"));
            assert!(command.contains(&format!(" --ingestion-sync {value}")));
        }
        assert!(parse_cli(vec!["Data".into(), "--ingestion-sync".into()]).is_err());
        assert!(
            parse_cli(vec![
                "Data".into(),
                "--ingestion-sync".into(),
                "invalid".into()
            ])
            .is_err()
        );
        assert!(
            parse_cli(vec![
                "Data".into(),
                "--ingestion-sync".into(),
                "none".into(),
                "--no-verify-cache".into()
            ])
            .is_err()
        );
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
            resume_command("converter", &cpu, Path::new("staging"))
                .ends_with(" --lod-encoder cpu --ingestion-sync archive --record-reader inhouse")
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
            resume_command("converter", &lod, Path::new("staging")).ends_with(
                " --lod-encoder gpu --gpu-quality 3 --gpu-batch-mb 256 --ingestion-sync archive --record-reader inhouse"
            )
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

    use rusqlite::Connection;

    use shared::extensions::{ExtensionEntry, MATERIAL};

    struct RefreshDataSet {
        _root: tempfile::TempDir,
        assets: PathBuf,
    }

    impl RefreshDataSet {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let assets = root.path().join("assets");
            fs::create_dir(&assets).unwrap();
            Self {
                _root: root,
                assets,
            }
        }

        fn path(&self) -> &Path {
            &self.assets
        }
    }

    /// A synthetic authored-animation payload with its core identity and manifest. No game data.
    fn data_set(entries: &[ExtensionEntry]) -> RefreshDataSet {
        let directory = RefreshDataSet::new();
        let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO lod_build(id, build_identity) VALUES (1, 'core')",
                [],
            )
            .unwrap();
        drop(connection);
        fs::create_dir(directory.path().join("meshes")).unwrap();
        fs::write(directory.path().join("meshes/animated.gltf"),
            br#"{"asset":{"version":"2.0"},"extensionsUsed":["OPEN_SKYRIM_material_animation"],"materials":[{"extensions":{"OPEN_SKYRIM_material_animation":{"channels":[]}}}]}"#).unwrap();
        write_manifest(directory.path(), entries);
        directory
    }

    fn write_manifest(folder: &Path, entries: &[ExtensionEntry]) {
        let manifest = serde_json::json!({
            "schema_version": converter::cache::CONVERTER_SCHEMA_VERSION,
            "complete": true,
            "extensions": serde_json::to_value(entries).unwrap(),
        });
        fs::write(
            folder.join("conversion-manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn manifest_text(folder: &Path) -> Vec<u8> {
        fs::read(folder.join("conversion-manifest.json")).unwrap()
    }

    fn manifest_entries(folder: &Path) -> Vec<ExtensionEntry> {
        let value: serde_json::Value = serde_json::from_slice(&manifest_text(folder)).unwrap();
        serde_json::from_value(value["extensions"].clone()).unwrap()
    }

    /// The entries `build_entries` writes for the folder's content as it is now.
    fn current_entries(folder: &Path) -> Vec<ExtensionEntry> {
        converter::extensions::build_entries(folder).unwrap()
    }

    fn entry_of<'a>(entries: &'a [ExtensionEntry], id: &str) -> &'a ExtensionEntry {
        entries.iter().find(|entry| entry.id == id).unwrap()
    }

    fn entry_mut<'a>(entries: &'a mut [ExtensionEntry], id: &str) -> &'a mut ExtensionEntry {
        entries.iter_mut().find(|entry| entry.id == id).unwrap()
    }

    fn refresh(folder: &Path) -> Result<()> {
        refresh_extensions(&[folder.as_os_str().to_os_string()])
    }

    /// A manifest with no extension entries over a folder that holds none is a no-op.
    #[test]
    fn refresh_without_extensions_leaves_the_manifest_alone() {
        let directory = RefreshDataSet::new();
        let folder = directory.path();
        write_manifest(folder, &[]);
        let before = manifest_text(folder);
        refresh(folder).unwrap();
        assert_eq!(manifest_text(folder), before);
    }

    #[test]
    fn refresh_rejects_a_non_object_manifest_even_without_payloads() {
        let directory = RefreshDataSet::new();
        let folder = directory.path();
        fs::write(folder.join("conversion-manifest.json"), b"[]").unwrap();
        assert!(
            refresh(folder)
                .unwrap_err()
                .to_string()
                .contains("not a JSON object")
        );
        assert_eq!(manifest_text(folder), b"[]");
    }

    /// A data set whose core did not change: the hashes and stats of every listed extension are
    /// recomputed and each entry keeps the provenance it recorded.
    #[test]
    fn refresh_recomputes_hashes_and_keeps_the_recorded_provenance() {
        let directory = data_set(&[]);
        let folder = directory.path();
        let mut entries = current_entries(folder);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            converter::extensions::specs()
                .into_iter()
                .filter(|spec| !spec.owns.gltf_extensions.is_empty())
                .map(|spec| spec.id)
                .collect::<Vec<_>>(),
            "the fixture carries the retained material producer"
        );
        for entry in &mut entries {
            entry.sha256 = "0".repeat(64);
            entry.stats.clear();
        }
        write_manifest(folder, &entries);
        refresh(folder).unwrap();
        let after = manifest_entries(folder);
        assert_eq!(
            after,
            current_entries(folder),
            "the fresh hashes and stats were written"
        );
        assert_ne!(after, entries, "the stub hashes were replaced");
        assert!(after.iter().all(|entry| entry.built_from == "core"));
    }

    /// An extension derived from another core build is refused and the manifest is left exactly as
    /// it was: re-hashing it here would bless a payload this converter did not write.
    #[test]
    fn refresh_refuses_a_stale_extension_and_leaves_the_manifest_alone() {
        let directory = data_set(&[]);
        let folder = directory.path();
        let mut entries = current_entries(folder);
        entry_mut(&mut entries, MATERIAL).built_from = "an older core".to_owned();
        write_manifest(folder, &entries);
        let before = manifest_text(folder);
        let error = refresh(folder).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("an older core")
                && message.contains("the data set's core is now core"),
            "{message}"
        );
        assert!(message.contains("convert the data set again"), "{message}");
        assert_eq!(manifest_text(folder), before);
        assert!(
            !folder.join("conversion-manifest.json.refresh").exists(),
            "a refused refresh left its staged manifest behind"
        );
    }

    /// A recognised extension at a version or with an ownership this converter does not write is
    /// refused rather than read as the current one.
    #[test]
    fn refresh_refuses_a_version_or_ownership_it_does_not_write() {
        let directory = data_set(&[]);
        let folder = directory.path();
        let mut entries = current_entries(folder);
        entry_mut(&mut entries, MATERIAL).version = 2;
        write_manifest(folder, &entries);
        let before = manifest_text(folder);
        let error = refresh(folder).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("version 2") && message.contains("convert the data set again"),
            "{message}"
        );
        assert_eq!(manifest_text(folder), before);

        let other = data_set(&[]);
        let mut entries = current_entries(other.path());
        entry_mut(&mut entries, MATERIAL).owns.gltf_extensions = vec!["ACME_other".to_owned()];
        write_manifest(other.path(), &entries);
        let before = manifest_text(other.path());
        let error = refresh(other.path()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("owning content"), "{message}");
        assert_eq!(manifest_text(other.path()), before);
    }

    /// An extension this converter does not write is preserved while the entries
    /// around it are refreshed.
    #[test]
    fn refresh_keeps_an_unknown_extension_unchanged() {
        let directory = data_set(&[]);
        let folder = directory.path();
        let mut entries = current_entries(folder);
        entry_mut(&mut entries, MATERIAL).sha256 = "f".repeat(64);
        let foreign = ExtensionEntry {
            id: "acme.other_tool".to_owned(),
            version: 9,
            owns: Default::default(),
            sha256: "ab".repeat(32),
            built_from: "another tool".to_owned(),
            stats: Default::default(),
        };
        entries.push(foreign.clone());
        write_manifest(folder, &entries);
        refresh(folder).unwrap();
        let after = manifest_entries(folder);
        assert_eq!(
            after.last(),
            Some(&foreign),
            "the unknown entry is untouched"
        );
        assert_ne!(
            entry_of(&after, MATERIAL).sha256,
            "f".repeat(64),
            "the known entry next to it was refreshed"
        );
        assert_eq!(
            entry_of(&after, MATERIAL).sha256,
            entry_of(&current_entries(folder), MATERIAL).sha256
        );
    }

    #[test]
    fn refresh_preserves_foreign_json_fields_and_omitted_defaults() {
        let directory = data_set(&[]);
        let folder = directory.path();
        let mut entries = current_entries(folder);
        entry_mut(&mut entries, MATERIAL).sha256 = "0".repeat(64);
        write_manifest(folder, &entries);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_text(folder)).unwrap();
        let foreign = serde_json::json!({"id":"acme.foreign", "version":9,
            "owns":{"future_payloads":["custom.bin"]}, "future_field":{"mode":"special"}});
        manifest["extensions"]
            .as_array_mut()
            .unwrap()
            .push(foreign.clone());
        fs::write(
            folder.join("conversion-manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        refresh(folder).unwrap();
        let after: serde_json::Value = serde_json::from_slice(&manifest_text(folder)).unwrap();
        assert_eq!(
            after["extensions"].as_array().unwrap().last(),
            Some(&foreign)
        );
    }

    #[test]
    fn refresh_refuses_duplicate_extension_ids_without_writing() {
        let directory = data_set(&[]);
        let folder = directory.path();
        let mut entries = current_entries(folder);
        entries.push(entry_of(&entries, MATERIAL).clone());
        write_manifest(folder, &entries);
        let before = manifest_text(folder);
        assert!(
            refresh(folder)
                .unwrap_err()
                .to_string()
                .contains("listed twice")
        );
        assert_eq!(manifest_text(folder), before);
    }

    #[test]
    fn refresh_refuses_unknown_ownership_fields_on_a_known_extension() {
        let directory = data_set(&[]);
        let folder = directory.path();
        write_manifest(folder, &current_entries(folder));
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_text(folder)).unwrap();
        let entry = manifest["extensions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| entry["id"] == MATERIAL)
            .unwrap();
        entry["owns"]["future_payloads"] = serde_json::json!(["custom.bin"]);
        fs::write(
            folder.join("conversion-manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let before = manifest_text(folder);
        let error = refresh(folder).unwrap_err();
        assert!(error.to_string().contains("owning content"), "{error:#}");
        assert!(
            error.chain().any(|cause| cause
                .to_string()
                .contains("unknown field `future_payloads`")),
            "{error:#}"
        );
        assert_eq!(manifest_text(folder), before);
        assert!(!folder.join("conversion-manifest.json.refresh").exists());
    }

    #[test]
    fn refresh_refuses_unlisted_material_content() {
        let directory = data_set(&[]);
        let folder = directory.path();
        let before = manifest_text(folder);
        let error = refresh(folder).unwrap_err();
        assert!(
            error.to_string().contains("no extension entry"),
            "{error:#}"
        );
        assert!(error.to_string().contains(MATERIAL), "{error:#}");
        assert_eq!(manifest_text(folder), before);
        assert!(!folder.join("conversion-manifest.json.refresh").exists());
    }

    /// A listed extension whose content is gone is refused, not silently dropped from the manifest.
    #[test]
    fn refresh_refuses_a_listed_extension_whose_content_is_missing() {
        let directory = data_set(&[]);
        let folder = directory.path();
        write_manifest(folder, &current_entries(folder));
        fs::remove_file(folder.join("meshes/animated.gltf")).unwrap();
        let before = manifest_text(folder);
        let error = refresh(folder).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains(MATERIAL) && message.contains("convert the data set again"),
            "{message}"
        );
        assert_eq!(manifest_text(folder), before);
    }

    /// Content for an extension the manifest never listed carries no provenance to record: it is
    /// refused rather than added.
    #[test]
    fn refresh_refuses_content_without_a_listed_entry() {
        let directory = data_set(&[]);
        let folder = directory.path();
        let mut entries = current_entries(folder);
        entries.retain(|entry| entry.id != MATERIAL);
        write_manifest(folder, &entries);
        let before = manifest_text(folder);
        let error = refresh(folder).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("no extension entry for it") && message.contains(MATERIAL),
            "{message}"
        );
        assert_eq!(manifest_text(folder), before);
    }

    /// The command takes the same exclusive asset lock a conversion does, before it reads anything:
    /// another holder of the folder refuses it, and the manifest is left alone.
    #[test]
    fn refresh_refuses_a_data_set_another_holder_locks() {
        let directory = data_set(&[]);
        let folder = directory.path();
        write_manifest(folder, &current_entries(folder));
        let before = manifest_text(folder);
        let reader = shared::asset_lock::AssetLock::acquire_shared(folder).unwrap();
        let error = refresh(folder).unwrap_err();
        // The report's chain names both this command's context and the held lock.
        let message = format!("{error:?}");
        assert!(
            message.contains("cannot refresh an asset directory in use")
                && message.contains("incompatible lock"),
            "{message}"
        );
        drop(reader);
        let writer = shared::asset_lock::AssetLock::acquire_exclusive(folder).unwrap();
        let error = refresh(folder).unwrap_err();
        assert!(
            format!("{error:?}").contains("incompatible lock"),
            "{error:?}"
        );
        drop(writer);
        assert_eq!(manifest_text(folder), before);
        // With the holder gone the same data set refreshes.
        refresh(folder).unwrap();
    }
}

#[cfg(test)]
mod overlay_cli_tests {
    use super::*;

    #[test]
    fn derive_cluster_cli_accepts_one_base_and_explicit_overlay_controls() {
        let args: Vec<_> = [
            "base",
            "--label",
            "fixture",
            "--replace",
            "--keep-failed-staging",
            "--jobs",
            "2",
            "--triangle-threshold",
            "1",
        ]
        .map(OsString::from)
        .into();
        let options = parse_derive_options(&args).unwrap();
        assert_eq!(options.source, PathBuf::from("base"));
        assert_eq!(options.label, "fixture");
        assert!(options.replace && options.keep_failed_staging);
        assert_eq!(options.jobs, 2);
        assert_eq!(options.triangle_threshold, 1);
        assert!(parse_derive_options(&["base".into(), "old-full-copy-output".into()]).is_err());
        assert!(parse_derive_options(&[]).is_err());
    }

    #[test]
    fn discard_derived_is_explicit_and_survives_resume_commands() {
        let cli = parse_cli(vec![
            "Data".into(),
            "Output".into(),
            "--discard-derived".into(),
        ])
        .unwrap();
        assert!(cli.pipeline_config().discard_derived);
        assert!(
            resume_command("converter", &cli, Path::new("staging")).contains("--discard-derived")
        );
        assert!(
            !parse_cli(vec!["Data".into(), "Output".into()])
                .unwrap()
                .pipeline_config()
                .discard_derived
        );
    }
}
