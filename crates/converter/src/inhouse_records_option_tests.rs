//! The diagnostic CLI accepts unambiguous records-only options.
use super::*;

#[test]
/// Explicit archive scopes accept either option order and reject ambiguous values.
fn explicit_archive_option_orders_and_ambiguity() -> Result<()> {
    use converter::esm::inhouse::RecordBundleMode;
    for words in [
        vec![
            "--record-archive-only",
            "--typed-signatures",
            "LAND,LTEX",
            "--database-work-dir",
            "fresh",
        ],
        vec![
            "--database-work-dir",
            "fresh",
            "--record-archive-only",
            "--typed-signatures",
            "LAND,LTEX",
        ],
    ] {
        let parsed = parse_record_options_with_mode(&args(&words))?;
        assert_eq!(parsed.mode, RecordBundleMode::RecordArchiveOnly);
        assert_eq!(parsed.signatures, vec![*b"LAND", *b"LTEX"]);
        assert_eq!(parsed.work, Some(PathBuf::from("fresh")));
    }
    assert_eq!(
        parse_record_options_with_mode(&[])?.mode,
        RecordBundleMode::Full
    );
    for words in [
        vec!["--record-archive-only", "--record-archive-only"],
        vec!["--typed-signatures", "--record-archive-only"],
        vec!["--database-work-dir", "--record-archive-only"],
        vec!["--record-archive-only", "unexpected-value"],
    ] {
        assert!(parse_record_options_with_mode(&args(&words)).is_err());
    }
    Ok(())
}
/// Build option arguments independently of the CLI parser.
fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(|word| OsString::from(*word)).collect()
}

#[test]
/// Accept defaults and either order without changing the selected records.
fn defaults_single_options_and_both_orders_keep_exact_signatures_and_work_path() -> Result<()> {
    assert_eq!(parse_record_options(&[])?, (vec![], None));
    assert_eq!(
        parse_record_options(&args(&["--typed-signatures", "WEAP,NPC_"]))?,
        (vec![*b"WEAP", *b"NPC_"], None)
    );
    assert_eq!(
        parse_record_options(&args(&["--database-work-dir", "new work"]))?,
        (vec![], Some(PathBuf::from("new work")))
    );
    let a = parse_record_options(&args(&[
        "--typed-signatures",
        "INFO,DIAL",
        "--database-work-dir",
        "new work",
    ]))?;
    let b = parse_record_options(&args(&[
        "--database-work-dir",
        "new work",
        "--typed-signatures",
        "INFO,DIAL",
    ]))?;
    assert_eq!(a, b);
    assert_eq!(
        a,
        (vec![*b"INFO", *b"DIAL"], Some(PathBuf::from("new work")))
    );
    Ok(())
}

#[test]
/// Reject ambiguous options before any conversion starts.
fn unknown_duplicate_missing_empty_and_malformed_options_fail() {
    for words in [
        vec!["--unknown", "value"],
        vec!["--typed-signatures"],
        vec!["--database-work-dir"],
        vec!["--typed-signatures", ""],
        vec!["--database-work-dir", ""],
        vec!["--typed-signatures", "--database-work-dir", "new"],
        vec!["--database-work-dir", "--typed-signatures", "INFO"],
        vec!["--typed-signatures", "INFO", "--typed-signatures", "DIAL"],
        vec!["--database-work-dir", "a", "--database-work-dir", "b"],
        vec!["--typed-signatures", "INF"],
        vec!["--typed-signatures", "INFO,"],
        vec!["--typed-signatures", "I\u{00d1}FO"],
        vec!["trailing"],
    ] {
        assert!(
            parse_record_options(&args(&words)).is_err(),
            "unexpected acceptance: {words:?}"
        );
    }
}
