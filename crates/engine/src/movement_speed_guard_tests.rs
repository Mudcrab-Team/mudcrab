//! Guard: no unlabelled hard-coded actor movement speed in the engine (#1065, #1066).
//!
//! Actor movement speed comes from `MOVT.SPED` (through `RACE.WKMV` and `RACE.RNMV`) and from
//! `NPC_.ACBS`. A bare number in a walk, run, sprint, sneak, swim or fly speed binding is
//! therefore either a bug or a stand-in. This test scans the non-test engine source and fails on
//! a numeric literal bound to such a name unless the line, or the six lines above it, carry
//! `STAND-IN(#N)` with an issue number. Zero is allowed: it is an initial value, not a speed.
//!
//! The scan is textual. It covers `name: literal`, `name = literal` and `const NAME: f32 =
//! literal`. It does not see a speed built from arithmetic or passed as an argument, so reviewers
//! must still read new movement code.

use regex::Regex;
use std::path::{Path, PathBuf};

/// Names that bind an actor movement speed: a gait word and a speed word, in either order.
const NAME: &str = r"\w*(?:walk|run|sprint|sneak|swim|fly)\w*(?:speed|multiplier|below|above|velocity)\w*|\w*(?:speed|velocity)\w*(?:walk|run|sprint|sneak|swim|fly)\w*";

#[derive(Debug, PartialEq, Eq)]
struct Finding {
    line: usize,
    text: String,
}

/// The lines of `source` that bind a non-zero literal to a movement-speed name without a label.
/// Everything from `#[cfg(test)] mod tests {` on is test code and is skipped.
fn unlabelled_speed_literals(source: &str) -> (Vec<Finding>, usize) {
    let binding = Regex::new(&format!(
        r"(?i)\b(?:{NAME})\b\s*(?::\s*f32\s*)?[:=]\s*-?\d[\d_]*(?:\.\d+)?(?:_?f32)?\b"
    ))
    .unwrap();
    let zero = Regex::new(r"[:=]\s*-?0(?:\.0+)?(?:_?f32)?\s*$").unwrap();
    let label = Regex::new(r"STAND-IN\(\s*#\d+").unwrap();
    let production = source
        .find("#[cfg(test)]\nmod tests {")
        .map_or(source, |end| &source[..end]);
    let lines: Vec<&str> = production.lines().collect();
    let mut findings = Vec::new();
    let mut labelled = 0;
    for (index, line) in lines.iter().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        let Some(found) = binding.find(code) else {
            continue;
        };
        if zero.is_match(found.as_str()) {
            continue;
        }
        let first = index.saturating_sub(6);
        if lines[first..=index].iter().any(|near| label.is_match(near)) {
            labelled += 1;
        } else {
            findings.push(Finding {
                line: index + 1,
                text: line.trim().to_owned(),
            });
        }
    }
    (findings, labelled)
}

fn engine_sources(directory: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            engine_sources(&path, out);
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy();
        if path.extension().is_some_and(|ext| ext == "rs")
            && !name.ends_with("_tests.rs")
            && name != "tests.rs"
        {
            out.push(path);
        }
    }
}

#[test]
fn engine_movement_speeds_have_no_unlabelled_literal() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    engine_sources(&root, &mut files);
    assert!(files.len() > 50, "the scan found {} files", files.len());
    let mut failures = Vec::new();
    let mut labelled = 0;
    for file in &files {
        let source = std::fs::read_to_string(file).unwrap();
        let (findings, count) = unlabelled_speed_literals(&source);
        labelled += count;
        for finding in findings {
            failures.push(format!(
                "{}:{}: {}",
                file.strip_prefix(&root).unwrap().display(),
                finding.line,
                finding.text
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "movement speed literals need a record source or `STAND-IN(#N): reason`:\n{}",
        failures.join("\n")
    );
    // The known stand-ins (player and actor fallbacks, sprint factor) must stay visible to the
    // scan, so a changed pattern cannot turn this guard off unnoticed.
    assert!(
        labelled >= 5,
        "the scan saw {labelled} labelled stand-ins, expected at least 5"
    );
}

#[test]
fn the_scan_flags_a_bare_speed_literal() {
    let (findings, _) =
        unlabelled_speed_literals("let tuning = Tuning {\n    walk_speed: 160.0,\n};\n");
    assert_eq!(
        findings,
        vec![Finding {
            line: 2,
            text: "walk_speed: 160.0,".into()
        }]
    );
    for bare in [
        "run_speed: 300.0,",
        "pub const SPRINT_SPEED: f32 = 555.0;",
        "let sneak_speed = 50.0;",
        "swim_speed_run: 90_f32,",
        "run_at_or_above: 300.0,",
        "walk_below: 160.0,",
        "sprint_multiplier: 1.5,",
        "fly_speed: 250.0,",
    ] {
        let (findings, _) = unlabelled_speed_literals(bare);
        assert_eq!(findings.len(), 1, "{bare}");
    }
}

#[test]
fn the_scan_accepts_a_stand_in_label_and_zero() {
    for ok in [
        "// STAND-IN(#1066): no record speed for a legacy package\nwalk_speed: 160.0,\n",
        "walk_speed: 160.0, // STAND-IN(#1066): legacy package keeps the old constant\n",
        "// STAND-IN(#1066): legacy package keeps the old constants\nwalk_speed: 160.0,\nrun_speed: 300.0,\n",
        "run_speed: 0.0,\n",
        "let mut walk_speed = 0;\n",
        "walk_speed: record.walk,\n",
        "speed_multiplier: SpeedMultiplier::NEUTRAL,\n",
    ] {
        let (findings, _) = unlabelled_speed_literals(ok);
        assert!(findings.is_empty(), "{ok:?}: {findings:?}");
    }
}

#[test]
fn the_scan_rejects_a_marker_without_an_issue_and_a_distant_marker() {
    let (findings, _) =
        unlabelled_speed_literals("// STAND-IN: no issue number\nwalk_speed: 160.0,\n");
    assert_eq!(findings.len(), 1);
    let distant = format!(
        "// STAND-IN(#1066): far away\n{}walk_speed: 160.0,\n",
        "\n".repeat(8)
    );
    assert_eq!(unlabelled_speed_literals(&distant).0.len(), 1);
}

#[test]
fn the_scan_skips_the_unit_test_module() {
    let source = "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let run_speed = 300.0; }\n}\n";
    assert!(unlabelled_speed_literals(source).0.is_empty());
}
