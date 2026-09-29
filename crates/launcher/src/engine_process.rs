//! Watches an engine started by the launcher, so the launcher can say why it stopped.
//!
//! The engine reports a refusal to start (stale data, a bad option) on stderr and exits. The
//! launcher sends the engine's stdout and stderr to one log file rather than a pipe, so the
//! engine keeps running whether or not the launcher stays open, and when the engine exits early
//! it shows the file's tail as-is: it never matches on the engine's wording.

use bevy::prelude::Resource;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// The engine's log (stdout and stderr), truncated at each launch.
/// The engine's log (stdout and stderr) is `<prefix><launcher pid>.log` in the temp directory,
/// truncated at each launch. The launcher's own id keeps two launchers from sharing one log.
pub const STDERR_LOG_PREFIX: &str = "mudcrab-engine-";
/// Bytes read from the end of the log when the engine stops early.
pub const TAIL_MAX_BYTES: u64 = 8 * 1024;
/// Log lines shown when the engine stops early.
pub const SHOWN_LINES: usize = 12;
/// A longer log line is cut, so one line cannot fill the status text.
const MAX_LINE_CHARS: usize = 300;

/// Where the launcher writes the engine's stdout and stderr.
pub fn stderr_log_path() -> PathBuf {
    std::env::temp_dir().join(format!("{STDERR_LOG_PREFIX}{}.log", std::process::id()))
}

/// Strips ANSI escape sequences (the engine's log colours), trims trailing whitespace and cuts
/// the line at [`MAX_LINE_CHARS`] characters.
fn clean_line(line: &str) -> String {
    let mut cleaned = String::with_capacity(line.len().min(MAX_LINE_CHARS * 4));
    let mut chars = line.chars();
    let mut kept = 0;
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            // CSI: ESC '[' parameters... final byte in '@'..='~'. Any other escape: drop ESC and
            // the one character after it.
            if chars.next() == Some('[') {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        if kept == MAX_LINE_CHARS {
            cleaned.push('…');
            break;
        }
        cleaned.push(ch);
        kept += 1;
    }
    cleaned.truncate(cleaned.trim_end().len());
    cleaned
}

/// The lines in the last `max_bytes` of the file at `path`, each through [`clean_line`]. When the
/// read starts inside the file, the first (partial) line is dropped.
pub fn read_log_tail(path: &Path, max_bytes: u64) -> io::Result<Vec<String>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(max_bytes).read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0
        && let Some(newline) = text.find('\n')
    {
        text.drain(..=newline);
    }
    Ok(text.lines().map(clean_line).collect())
}

/// The last `count` lines of `lines`, without blank lines at either end.
fn last_lines<S: AsRef<str>>(lines: &[S], count: usize) -> Vec<&str> {
    let end = lines
        .iter()
        .rposition(|line| !line.as_ref().trim().is_empty())
        .map_or(0, |last| last + 1);
    let start = end.saturating_sub(count);
    let shown = &lines[start..end];
    let first = shown
        .iter()
        .position(|line| !line.as_ref().trim().is_empty())
        .unwrap_or(shown.len());
    shown[first..].iter().map(AsRef::as_ref).collect()
}

/// The launcher's status text for an engine that exited with `code` (`None`: killed) after
/// writing the lines `stderr` (the log's tail), whose full text is in `log` when there is one.
pub fn engine_exit_message<S: AsRef<str>>(
    code: Option<i32>,
    stderr: &[S],
    log: Option<&Path>,
) -> String {
    let reason = match code {
        Some(0) => return "Mudcrab engine closed.".into(),
        // Windows reports crashes as NTSTATUS values, which read better in hex.
        Some(code) if code < 0 => format!("exit code {code} (0x{:08X})", code as u32),
        Some(code) => format!("exit code {code}"),
        None => "no exit code; it was killed".into(),
    };
    let shown = last_lines(stderr, SHOWN_LINES);
    if shown.is_empty() {
        let mut message = format!("The engine stopped ({reason}) without an error message.");
        if let Some(log) = log {
            message.push_str(&format!(
                "
Full log: {}",
                log.display()
            ));
        }
        return message;
    }
    let mut message = format!("The engine stopped ({reason}):\n{}", shown.join("\n"));
    if let Some(log) = log {
        message.push_str(&format!("\nFull log: {}", log.display()));
    }
    message
}

/// An engine process started by the launcher, its stdout and stderr going to a log file.
#[derive(Resource)]
pub struct EngineProcess {
    child: Child,
    /// `None` when the log file could not be created and both streams were inherited instead.
    log: Option<PathBuf>,
}

impl EngineProcess {
    /// Spawns `command` with its stdout and stderr written to `log` (created or truncated). If
    /// the file cannot be created, both streams are inherited and no error text will be shown; if
    /// only its second handle fails, stdout alone is inherited.
    pub fn spawn(mut command: Command, log: &Path) -> io::Result<Self> {
        let log = match File::create(log) {
            Ok(file) => {
                let stdout = file
                    .try_clone()
                    .map_or_else(|_| Stdio::inherit(), Stdio::from);
                command.stdout(stdout).stderr(Stdio::from(file));
                Some(log.to_path_buf())
            }
            Err(_) => {
                command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
                None
            }
        };
        let child = command.spawn()?;
        Ok(Self { child, log })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Checks the process without blocking. Returns the status text once the engine has exited.
    pub fn poll(&mut self) -> Option<String> {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return None,
            // A failed poll does not mean the engine exited: keep it and poll again next frame.
            Err(error) => {
                eprintln!("Could not poll the engine process: {error}");
                return None;
            }
        };
        let lines = match &self.log {
            Some(log) if !status.success() => {
                read_log_tail(log, TAIL_MAX_BYTES).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        Some(engine_exit_message(
            status.code(),
            &lines,
            self.log.as_deref(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const NO_LOG: Option<&Path> = None;

    /// A file path under the temp directory unique to this test process and `name`.
    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mudcrab-launcher-test-{}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn exit_zero_says_closed() {
        assert_eq!(
            engine_exit_message(Some(0), &["some log line"], Some(Path::new("x.log"))),
            "Mudcrab engine closed."
        );
    }

    #[test]
    fn exit_two_shows_the_stderr_lines_and_the_log() {
        let stderr = [
            "error: unknown option --fly",
            "Run engine --help for the options.",
        ];
        assert_eq!(
            engine_exit_message(Some(2), &stderr, Some(Path::new("engine.log"))),
            "The engine stopped (exit code 2):\n\
             error: unknown option --fly\n\
             Run engine --help for the options.\n\
             Full log: engine.log"
        );
    }

    #[test]
    fn long_stderr_shows_only_the_tail() {
        let stderr: Vec<String> = (0..100).map(|index| format!("line {index}")).collect();
        let message = engine_exit_message(Some(1), &stderr, NO_LOG);
        let shown: Vec<&str> = message.lines().skip(1).collect();
        assert_eq!(shown.len(), SHOWN_LINES);
        assert_eq!(shown.first(), Some(&"line 88"));
        assert_eq!(shown.last(), Some(&"line 99"));
    }

    #[test]
    fn trailing_blank_lines_do_not_hide_the_error() {
        let mut stderr = vec!["the real error".to_string()];
        stderr.extend(std::iter::repeat_n(String::new(), 20));
        assert_eq!(
            engine_exit_message(Some(1), &stderr, NO_LOG),
            "The engine stopped (exit code 1):\nthe real error"
        );
    }

    #[test]
    fn empty_stderr_says_no_message() {
        assert_eq!(
            engine_exit_message::<&str>(Some(3), &[], Some(Path::new("engine.log"))),
            "The engine stopped (exit code 3) without an error message.
Full log: engine.log"
        );
        assert_eq!(
            engine_exit_message(Some(3), &["", "   "], NO_LOG),
            "The engine stopped (exit code 3) without an error message."
        );
    }

    #[test]
    fn each_launcher_writes_its_own_engine_log() {
        let path = stderr_log_path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            name,
            format!("{STDERR_LOG_PREFIX}{}.log", std::process::id())
        );
    }

    #[test]
    fn a_kill_without_a_code_says_so() {
        assert_eq!(
            engine_exit_message(None, &["partial output"], NO_LOG),
            "The engine stopped (no exit code; it was killed):\npartial output"
        );
    }

    #[test]
    fn a_windows_crash_code_is_shown_in_hex_too() {
        assert_eq!(
            engine_exit_message::<&str>(Some(-1073741819), &[], NO_LOG),
            "The engine stopped (exit code -1073741819 (0xC0000005)) without an error message."
        );
    }

    #[test]
    fn lines_lose_colour_codes_trailing_whitespace_and_excess_length() {
        assert_eq!(
            clean_line("\u{1b}[31mERROR\u{1b}[0m engine: data is stale \r"),
            "ERROR engine: data is stale"
        );
        let cut = clean_line(&"x".repeat(1000));
        assert_eq!(cut.chars().count(), MAX_LINE_CHARS + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_log_tail_keeps_only_whole_lines_from_the_end() {
        let path = test_path("tail.log");
        let text: String = (0..2000)
            .map(|index| format!("\u{1b}[33mline {index}\u{1b}[0m\r\n"))
            .collect();
        std::fs::write(&path, &text).unwrap();
        let lines = read_log_tail(&path, 200).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(!lines.is_empty());
        assert_eq!(lines.last().map(String::as_str), Some("line 1999"));
        assert!(
            lines.iter().all(|line| line.starts_with("line ")),
            "the partial first line is dropped and colours stripped: {lines:?}"
        );
        let total: usize = lines.iter().map(|line| line.len() + 2).sum();
        assert!(total <= 200);
    }

    #[test]
    fn a_short_log_is_read_whole() {
        let path = test_path("short.log");
        std::fs::write(&path, "first\nsecond\nlast without newline").unwrap();
        let lines = read_log_tail(&path, TAIL_MAX_BYTES).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(lines, ["first", "second", "last without newline"]);
    }

    fn failing_command() -> Command {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut command = Command::new("cmd");
            command
                .raw_arg("/C \"echo loading data & echo engine refused to start 1>&2 & exit 2\"");
            command
        }
        #[cfg(not(windows))]
        {
            let mut command = Command::new("sh");
            command.args([
                "-c",
                "echo loading data; echo engine refused to start >&2; exit 2",
            ]);
            command
        }
    }

    fn wait_for_exit(engine: &mut EngineProcess) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(message) = engine.poll() {
                return message;
            }
            assert!(Instant::now() < deadline, "the child never exited");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_real_child_that_fails_reports_its_code_output_and_log() {
        let log = test_path("child.log");
        std::fs::write(&log, "left over from an earlier launch\n").unwrap();
        let mut engine = EngineProcess::spawn(failing_command(), &log).expect("spawn the shell");
        let message = wait_for_exit(&mut engine);
        std::fs::remove_file(&log).unwrap();
        assert_eq!(
            message,
            format!(
                "The engine stopped (exit code 2):\nloading data\nengine refused to start\nFull log: {}",
                log.display()
            )
        );
    }

    #[test]
    fn without_a_log_file_the_child_still_runs_and_reports_its_code() {
        let log = test_path("missing-dir").join("child.log");
        let mut engine = EngineProcess::spawn(failing_command(), &log).expect("spawn the shell");
        assert_eq!(
            wait_for_exit(&mut engine),
            "The engine stopped (exit code 2) without an error message."
        );
    }
}
