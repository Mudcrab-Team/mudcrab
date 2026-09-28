//! Running a conversion, or a check of an output folder, off the launcher's thread.
//!
//! The pipeline is async and the launcher is a Bevy app, so a run gets its own thread with its own
//! tokio runtime, and everything the run has to say comes back as a [`RunMessage`] on a crossbeam
//! channel the launcher drains each frame. A check is plain blocking work on its own thread and
//! reports on the same kind of channel; its stop button is a shared flag.

use super::state::{CheckSummary, FailureReport, RunReport};
use converter::{
    AssetPipeline, Cancellation, CheckCancelled, CheckMode, PipelineConfig, ProgressEvent,
};
use crossbeam_channel::Sender;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

/// What a running conversion tells the window.
#[derive(Debug)]
pub enum RunMessage {
    /// The run moved forward, or has something to say about one asset.
    Progress(ProgressEvent),
    /// The run published its output.
    Finished(RunReport),
    /// The run stopped short: cancelled, or it failed.
    Failed(FailureReport),
    /// A check has looked at `done` of the manifest's `total` entries.
    CheckProgress { done: usize, total: usize },
    /// The check finished; the summary says what it found.
    CheckFinished(CheckSummary),
    /// The check could not run (no folder, no manifest, an unreadable manifest).
    CheckFailed(String),
    /// The check was stopped before it finished; there is no result to show.
    CheckCancelled,
}

/// How many steps a check's progress is reported in. `check_output` reports every entry, from
/// several threads, and an install has a quarter of a million of them; the bar needs far fewer.
const CHECK_PROGRESS_STEPS: usize = 200;

/// How many progress events may sit between the pipeline and the window. The pipeline sends
/// thousands a second and the window reads once a frame, so the window never has to keep up, and
/// the pipeline waits rather than dropping anything.
const CHANNEL_DEPTH: usize = 256;

/// Starts `config` on its own thread and returns the run's stop button. Every message the run
/// produces arrives on `tx`; the thread ends when the pipeline does, however it ends.
///
/// A run that cannot even start (no runtime, no thread) reports that as a failure on the same
/// channel, so the window has one path for everything that goes wrong.
pub fn spawn(config: PipelineConfig, tx: Sender<RunMessage>) -> Cancellation {
    let cancellation = Cancellation::new();
    let stopped = cancellation.clone();
    let failure_tx = tx.clone();
    let spawned = thread::Builder::new()
        .name("conversion".to_owned())
        .spawn(move || run(config, tx, stopped));
    if let Err(error) = spawned {
        let _ = failure_tx.send(RunMessage::Failed(FailureReport::before_start(format!(
            "could not start the conversion thread: {error}"
        ))));
    }
    cancellation
}

/// The run itself: a tokio runtime on this thread, a forwarder moving the pipeline's events onto
/// the window's channel, and the outcome at the end.
fn run(config: PipelineConfig, tx: Sender<RunMessage>, cancellation: Cancellation) {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = tx.send(RunMessage::Failed(FailureReport::before_start(format!(
                "could not start the converter's async runtime: {error}"
            ))));
            return;
        }
    };
    runtime.block_on(async move {
        let (progress_tx, mut progress_rx) =
            tokio::sync::mpsc::channel::<ProgressEvent>(CHANNEL_DEPTH);
        let forward = tx.clone();
        let forwarder = tokio::spawn(async move {
            // Keep draining even if nobody is listening: a full channel would stall the run.
            while let Some(event) = progress_rx.recv().await {
                let _ = forward.send(RunMessage::Progress(event));
            }
        });
        let result = AssetPipeline::run_async_with_cancel(config, progress_tx, cancellation).await;
        let _ = forwarder.await;
        let message = match result {
            Ok(report) => RunMessage::Finished(RunReport::from_pipeline(&report)),
            Err(failure) => RunMessage::Failed(FailureReport::from_pipeline(failure)),
        };
        let _ = tx.send(message);
    });
}

/// Checks `output` against its manifest on its own thread. Progress, and then the result, arrive on
/// `tx`. Setting `cancel` stops the check within about one artifact's read, and it then reports
/// [`RunMessage::CheckCancelled`] instead of a result: a stopped check never shows a partial one.
pub fn spawn_check(
    output: PathBuf,
    mode: CheckMode,
    cancel: Arc<AtomicBool>,
    tx: Sender<RunMessage>,
) {
    let failure_tx = tx.clone();
    let spawned = thread::Builder::new()
        .name("output-check".to_owned())
        .spawn(move || check(&output, mode, &cancel, &tx));
    if let Err(error) = spawned {
        let _ = failure_tx.send(RunMessage::CheckFailed(format!(
            "could not start the check thread: {error}"
        )));
    }
}

/// The check itself, with its per-entry progress thinned to [`CHECK_PROGRESS_STEPS`] messages.
fn check(output: &std::path::Path, mode: CheckMode, cancel: &AtomicBool, tx: &Sender<RunMessage>) {
    let reported = AtomicUsize::new(0);
    let progress = |done: usize, total: usize| {
        let step = done * CHECK_PROGRESS_STEPS / total.max(1);
        // `fetch_max` lets only the first checking thread to reach a step report it.
        if done == 0 || reported.fetch_max(step, Ordering::Relaxed) < step {
            let _ = tx.send(RunMessage::CheckProgress { done, total });
        }
    };
    let message = match converter::check_output_with_cancel(output, mode, progress, cancel) {
        Ok(report) => RunMessage::CheckFinished(CheckSummary::from_report(&report)),
        Err(error) if error.is::<CheckCancelled>() => RunMessage::CheckCancelled,
        Err(error) => RunMessage::CheckFailed(format!("{error:#}")),
    };
    let _ = tx.send(message);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion::state::ConversionState;
    use crossbeam_channel::unbounded;
    use std::time::Duration;

    /// Everything a check sent, up to and including its result.
    fn check_messages(output: PathBuf, mode: CheckMode) -> Vec<RunMessage> {
        check_messages_with(output, mode, Arc::new(AtomicBool::new(false)))
    }

    fn check_messages_with(
        output: PathBuf,
        mode: CheckMode,
        cancel: Arc<AtomicBool>,
    ) -> Vec<RunMessage> {
        let (tx, rx) = unbounded();
        spawn_check(output, mode, cancel, tx);
        let mut messages = Vec::new();
        loop {
            let message = rx
                .recv_timeout(Duration::from_secs(60))
                .expect("the check reported nothing");
            let done = !matches!(message, RunMessage::CheckProgress { .. });
            messages.push(message);
            if done {
                return messages;
            }
        }
    }

    #[test]
    fn a_check_of_a_sound_output_reports_progress_and_all_good() {
        let output = crate::conversion::tests::tiny_output("check-sound", false);
        for mode in [CheckMode::Quick, CheckMode::Full] {
            let messages = check_messages(output.clone(), mode);
            assert!(
                matches!(
                    messages.first(),
                    Some(RunMessage::CheckProgress { done: 0, total: 1 })
                ),
                "{messages:?}"
            );
            assert!(
                messages.iter().any(|message| matches!(
                    message,
                    RunMessage::CheckProgress { done: 1, total: 1 }
                )),
                "the bar never reached the end: {messages:?}"
            );
            let Some(RunMessage::CheckFinished(summary)) = messages.last() else {
                panic!("{messages:?}");
            };
            assert!(summary.is_ok(), "{summary:?}");
            assert_eq!(summary.mode, mode);
            assert!(
                summary.lines()[0].starts_with("All good: 1 files, 5 B"),
                "{summary:?}"
            );
        }
        std::fs::remove_dir_all(&output).unwrap();
    }

    #[test]
    fn a_check_of_a_damaged_output_lists_the_problem() {
        let output = crate::conversion::tests::tiny_output("check-damaged", true);
        let messages = check_messages(output.clone(), CheckMode::Quick);
        let Some(RunMessage::CheckFinished(summary)) = messages.last() else {
            panic!("{messages:?}");
        };
        assert_eq!(summary.problem_count, 1, "{summary:?}");
        assert_eq!(
            summary.first_problems,
            vec!["missing: meshes/b.glb".to_owned()]
        );
        assert!(summary.advice.is_some());
        std::fs::remove_dir_all(&output).unwrap();
    }

    #[test]
    fn a_check_of_a_folder_without_a_manifest_reports_a_failure() {
        let output = crate::conversion::tests::temp_dir("check-empty");
        std::fs::create_dir_all(&output).unwrap();
        let messages = check_messages(output.clone(), CheckMode::Quick);
        let [RunMessage::CheckFailed(message)] = messages.as_slice() else {
            panic!("{messages:?}");
        };
        assert!(message.contains("conversion-manifest.json"), "{message}");
        std::fs::remove_dir_all(&output).unwrap();
    }

    /// A check whose stop flag is already set reports that it was stopped, and nothing else: no
    /// progress, no result, not even a failure for the manifest it never read.
    #[test]
    fn a_check_stopped_before_it_starts_reports_cancelled() {
        let output = crate::conversion::tests::tiny_output("check-cancelled", true);
        for mode in [CheckMode::Quick, CheckMode::Full] {
            let messages =
                check_messages_with(output.clone(), mode, Arc::new(AtomicBool::new(true)));
            assert!(
                matches!(messages.as_slice(), [RunMessage::CheckCancelled]),
                "{messages:?}"
            );
        }
        std::fs::remove_dir_all(&output).unwrap();
    }

    /// The launcher refuses a Data folder that is not there before it ever starts a run, so the only
    /// way to drive this is straight at the runner: a config the pipeline rejects has to come back
    /// as a failure message rather than a panic or a silent thread.
    #[test]
    fn a_run_that_cannot_start_reports_a_failure() {
        let missing = std::env::temp_dir().join("openskyrim-launcher-no-such-data-folder");
        assert!(!missing.exists(), "{missing:?} exists");
        let (tx, rx) = unbounded();
        let cancellation = spawn(
            PipelineConfig::new(missing, PathBuf::from("openskyrim-launcher-test-output")),
            tx,
        );
        assert!(!cancellation.is_cancelled());
        let message = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the run reported nothing");
        let RunMessage::Failed(failure) = message else {
            panic!("a run that cannot start reported {message:?}");
        };
        assert!(!failure.cancelled, "this was not a stop: {failure:?}");
        assert_eq!(failure.staging, None);
        assert!(
            failure.message.contains("Skyrim Data directory"),
            "the failure does not say what was wrong: {}",
            failure.message
        );
        // And the state machine takes it exactly as any other failure.
        let (state, _) = crate::conversion::state::apply(
            ConversionState::Running,
            crate::conversion::state::Input::Failed(failure),
        );
        assert_eq!(
            state,
            ConversionState::Stopped {
                staging: None,
                cancelled: false,
            }
        );
    }
}
