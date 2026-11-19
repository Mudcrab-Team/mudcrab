//! The converter's `--version` answer, before any folder is opened.

use std::process::Command;

const CONVERTER: &str = env!("CARGO_BIN_EXE_converter");

#[test]
fn version_prints_the_workspace_version_and_exits_zero() {
    let output = Command::new(CONVERTER)
        .arg("--version")
        .output()
        .expect("the converter binary runs");
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
fn short_spelling_answers_the_same_way() {
    let output = Command::new(CONVERTER)
        .arg("-V")
        .output()
        .expect("the converter binary runs");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("--version writes UTF-8")
            .trim_end(),
        shared::VERSION_LINE
    );
}

#[test]
fn version_is_recognised_after_other_arguments_without_opening_them() {
    let output = Command::new(CONVERTER)
        .args(["check", "no-such-output-directory", "--version"])
        .output()
        .expect("the converter binary runs");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("--version writes UTF-8")
            .trim_end(),
        shared::VERSION_LINE
    );
}
