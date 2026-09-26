use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::{AssetPipeline, PipelineConfig, ProgressEvent, ProgressStage};
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
    report: &'a converter::PipelineReport,
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
        let mut summary = String::from("Stage times (first event to last):");
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

/// `h:mm:ss.s`, for log lines that are read by eye and compared across runs.
fn format_elapsed(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u64;
    let (hours, rest) = (tenths / 36_000, tenths % 36_000);
    let (minutes, rest) = (rest / 600, rest % 600);
    format!("{hours}:{minutes:02}:{:02}.{}", rest / 10, rest % 10)
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    suppress_caught_nif_parser_panics();
    let cli = parse_cli(std::env::args_os().skip(1).collect())?;
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
        let mut clock = StageClock::default();
        while let Some(event) = rx.recv().await {
            *printer_progress.lock().expect("progress mutex poisoned") = Some(event.clone());
            let elapsed = started.elapsed();
            clock.record(event.stage, elapsed);
            println!(
                "[{}] {:?} {:.0}% {}",
                format_elapsed(elapsed.as_secs_f64()),
                event.stage,
                event.fraction() * 100.0,
                event.message
            );
        }
        clock
    });
    let pipeline_result = AssetPipeline::run_async(config, tx).await;
    let clock = printer.await?;
    println!("{}", clock.summary());
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
                    stages: clock.stages,
                };
                write_json_atomic(path, &failure)?;
            }
            return Err(error);
        }
    };
    if let Some(path) = &cli.report_json {
        let run = RunReport {
            report: &report,
            stages: clock.stages,
        };
        write_json_atomic(path, &run)?;
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
    "usage: converter <Skyrim Data> [output directory] [--cpu-jobs N] [--io-jobs N] [--fail-fast] [--invalidate-cache] [--no-verify-cache] [--resume-staging DIR] [--report-json FILE]"
}

#[cfg(test)]
mod tests {
    use super::*;

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
