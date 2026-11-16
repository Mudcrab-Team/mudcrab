//! The launcher answers `--version` before it builds a window.

use std::process::Command;

const LAUNCHER: &str = env!("CARGO_BIN_EXE_launcher");

#[test]
fn version_prints_the_workspace_version_without_a_window() {
    let output = Command::new(LAUNCHER)
        .arg("--version")
        .output()
        .expect("the launcher binary runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "--version exited {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("--version writes UTF-8");
    assert_eq!(stdout.trim_end(), shared::VERSION_LINE);
}

#[test]
fn version_is_recognised_after_other_arguments() {
    let output = Command::new(LAUNCHER)
        .args(["--unknown-option", "--version"])
        .output()
        .expect("the launcher binary runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("--version writes UTF-8");
    assert_eq!(stdout.trim_end(), shared::VERSION_LINE);
}
