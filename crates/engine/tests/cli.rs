//! End-to-end checks of the engine's command line: what a user sees before any
//! asset is opened.

use std::process::Command;

const ENGINE: &str = env!("CARGO_BIN_EXE_engine");

#[test]
fn version_prints_the_workspace_version_and_exits_zero() {
    let output = Command::new(ENGINE)
        .arg("--version")
        .output()
        .expect("the engine binary runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("--version writes UTF-8");
    assert_eq!(stdout.trim_end(), shared::VERSION_LINE);
}

#[test]
fn help_prints_the_usage_and_exits_zero() {
    let output = Command::new(ENGINE)
        .arg("--help")
        .output()
        .expect("the engine binary runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "--help exited {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("--help writes UTF-8");
    assert_eq!(stdout, engine::config::HELP_TEXT);
}

#[test]
fn an_unknown_option_exits_two_and_names_the_option() {
    let output = Command::new(ENGINE)
        .args(["--grid-z", "3"])
        .output()
        .expect("the engine binary runs");
    assert_eq!(
        output.status.code(),
        Some(2),
        "an unknown option exits with the usage code 2; stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("the error message is UTF-8");
    assert!(
        stderr.contains("error: unknown option '--grid-z'"),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("did you mean '--grid-x'?"),
        "stderr was: {stderr}"
    );
    assert!(stderr.contains("--help"), "stderr was: {stderr}");
}

#[test]
fn an_option_where_a_value_belongs_exits_two() {
    let output = Command::new(ENGINE)
        .args(["--profile-hardware", "--wroldspace"])
        .output()
        .expect("the engine binary runs");
    assert_eq!(
        output.status.code(),
        Some(2),
        "an option given as a value exits with the usage code 2; stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("the error message is UTF-8");
    assert!(
        stderr.contains("error: option '--profile-hardware' needs a value"),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("'--wroldspace' is another option"),
        "stderr was: {stderr}"
    );
    assert!(stderr.contains("--help"), "stderr was: {stderr}");
}

const WORLD_INSPECT: &str = env!("CARGO_BIN_EXE_world-inspect");

#[test]
fn world_inspect_answers_version_without_assets() {
    let output = Command::new(WORLD_INSPECT)
        .arg("--version")
        .output()
        .expect("the world-inspect binary runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "world-inspect --version exited {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("--version writes UTF-8");
    assert_eq!(stdout.trim_end(), shared::VERSION_LINE);
}

#[test]
fn world_inspect_recognises_version_after_other_arguments() {
    let output = Command::new(WORLD_INSPECT)
        .args(["--radius", "1", "-V"])
        .output()
        .expect("the world-inspect binary runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("--version writes UTF-8");
    assert_eq!(stdout.trim_end(), shared::VERSION_LINE);
}
