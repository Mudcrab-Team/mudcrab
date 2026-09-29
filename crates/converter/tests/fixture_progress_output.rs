//! What a conversion prints when progress does not go to a terminal: one plain line per stage and
//! every few seconds, rather than one line per event.

use converter::progress::ProgressRenderer;
use std::{fs, process::Command, time::Instant};
use tokio::sync::mpsc;

/// Generates the synthetic `Data` directory the converter tests convert.
fn fixture_data(directory: &std::path::Path) -> std::path::PathBuf {
    let data = directory.join("Data");
    dummy_content::layout::prepare_directory(&data, false).unwrap();
    dummy_content::layout::generate(
        &data,
        dummy_content::layout::DEFAULT_SEED,
        dummy_content::layout::Formats::all(),
    )
    .unwrap();
    data
}

#[tokio::test]
async fn a_piped_run_prints_far_fewer_lines_than_events() {
    let directory = tempfile::tempdir().unwrap();
    let data = fixture_data(directory.path());
    let output = directory.path().join("modern");

    let started = Instant::now();
    let (tx, mut rx) = mpsc::channel(64);
    let collector = tokio::spawn(async move {
        let mut renderer = ProgressRenderer::new(false, false);
        let mut events = 0;
        let mut printed = String::new();
        while let Some(event) = rx.recv().await {
            events += 1;
            if let Some(text) = renderer.update(&event, started.elapsed()) {
                printed.push_str(&text);
            }
        }
        if let Some(text) = renderer.finish() {
            printed.push_str(&text);
        }
        (events, printed)
    });

    let report =
        converter::AssetPipeline::run_async(converter::PipelineConfig::new(&data, &output), tx)
            .await
            .unwrap();
    let (events, printed) = collector.await.unwrap();
    assert!(report.complete);

    let lines: Vec<&str> = printed.lines().collect();
    assert!(
        lines.len() < events,
        "printed {} lines for {events} events:\n{printed}",
        lines.len()
    );
    assert!(
        lines.iter().all(|line| line.starts_with('[')),
        "log lines carry the run's time stamp:\n{printed}"
    );
    assert!(printed.contains("elapsed"), "{printed}");
    assert!(printed.contains("[overall"), "{printed}");
    assert!(printed.contains("Complete"), "{printed}");

    // The sample in the task's report: what a piped fixture conversion shows, start to end.
    println!("{events} events became {} lines:\n{printed}", lines.len());
}

/// The command line the converter ships, run the way a contributor runs it: a real process, its
/// stderr a pipe rather than a terminal.
#[test]
fn the_converter_binary_prints_a_few_status_lines_and_a_summary() {
    let directory = tempfile::tempdir().unwrap();
    let data = fixture_data(directory.path());
    let output = directory.path().join("modern");
    let run = Command::new(env!("CARGO_BIN_EXE_converter"))
        .arg(&data)
        .arg(&output)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success(),
        "converter exited with {run:?}\n{stderr}"
    );

    let lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with('['))
        .collect();
    assert!(
        !lines.is_empty() && lines.len() < 20,
        "a piped fixture run printed {} status lines:\n{stderr}",
        lines.len()
    );
    assert!(
        lines.iter().all(|line| line.contains(" elapsed")),
        "{stderr}"
    );
    assert!(stdout.contains("Conversion complete in"), "{stdout}");
    assert!(stdout.contains("converted"), "{stdout}");
    assert!(stdout.contains("stage times"), "{stdout}");
    assert!(stdout.contains("manifest:"), "{stdout}");

    println!(
        "stderr status lines:\n{}\nstdout summary:\n{stdout}",
        lines.join("\n")
    );
}

#[test]
fn a_failed_run_prints_what_went_wrong_and_the_command_that_resumes_it() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    fs::create_dir_all(data.join("textures")).unwrap();
    fs::write(data.join("textures/bad.dds"), b"not a DDS").unwrap();
    let output = directory.path().join("modern");

    let run = Command::new(env!("CARGO_BIN_EXE_converter"))
        .arg(&data)
        .arg(&output)
        .arg("--fail-fast")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success());
    assert!(stderr.contains("Conversion failed after"), "{stderr}");
    // The path is printed in the platform's own separators.
    assert!(stderr.contains("bad.dds"), "{stderr}");
    assert!(stderr.contains("The staging folder was kept"), "{stderr}");
    assert!(stderr.contains("Resume where it stopped with:"), "{stderr}");
    assert!(stderr.contains("--resume-staging"), "{stderr}");
    assert!(
        stderr.contains("Delete that folder to free the space"),
        "{stderr}"
    );
    assert!(
        !output.exists(),
        "a failed run must not publish over the output"
    );

    // The sample in the task's report: what a failed run tells the person who ran it.
    println!("{stderr}");
}

/// A run that skipped an input still publishes, but its summary must not call it complete before
/// stderr says it is not.
#[test]
fn an_incomplete_run_does_not_call_itself_complete() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    fs::create_dir_all(data.join("textures")).unwrap();
    fs::write(data.join("textures/bad.dds"), b"not a DDS").unwrap();
    let output = directory.path().join("modern");

    let run = Command::new(env!("CARGO_BIN_EXE_converter"))
        .arg(&data)
        .arg(&output)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success(), "{stderr}");
    assert!(
        stdout.contains("Conversion finished in") && stdout.contains(", incomplete: "),
        "{stdout}"
    );
    assert!(stdout.contains("failed 1"), "{stdout}");
    assert!(!stdout.contains("Conversion complete"), "{stdout}");
    assert!(stderr.contains("Conversion incomplete"), "{stderr}");
}
