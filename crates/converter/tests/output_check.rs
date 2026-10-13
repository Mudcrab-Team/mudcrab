//! `check_output` against a real converted output: the `dummy-content gen`
//! `Data/` tree through the real pipeline, then damaged one way at a time.

mod common;

use converter::{
    CheckCancelled, CheckMode, CheckProblem, cache::CONVERTER_SCHEMA_VERSION, cache::hash_file,
    check_output, check_output_with_cancel,
};
use dummy_content::layout;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::SystemTime,
};
use walkdir::WalkDir;

struct Converted {
    directory: tempfile::TempDir,
    output: PathBuf,
}

impl Converted {
    fn manifest_path(&self) -> PathBuf {
        self.output.join("conversion-manifest.json")
    }

    fn manifest(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.manifest_path()).unwrap()).unwrap()
    }

    fn write_manifest(&self, manifest: &serde_json::Value) {
        fs::write(
            self.manifest_path(),
            serde_json::to_vec_pretty(manifest).unwrap(),
        )
        .unwrap();
    }

    /// The manifest's `output` for the first artifact with this extension.
    fn artifact(&self, extension: &str) -> String {
        self.manifest()["entries"]
            .as_object()
            .unwrap()
            .values()
            .map(|entry| entry["output"].as_str().unwrap().to_owned())
            .find(|output| output.ends_with(extension))
            .unwrap_or_else(|| panic!("no {extension} artifact in the manifest"))
    }
}

/// Converts the default generated `Data/` tree the way
/// `dummy-content gen` writes it.
fn convert_fixture() -> Converted {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();

    let output = directory.path().join("modern");
    let mut config = common::cpu_lod_config(&data, &output);
    config.cpu_jobs = 2;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let report = runtime.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let report = converter::AssetPipeline::run_async(config, tx)
            .await
            .unwrap();
        drain.await.unwrap();
        report
    });
    assert!(report.complete, "the fixture did not convert cleanly");
    Converted { directory, output }
}

fn check(output: &Path, mode: CheckMode) -> converter::CheckReport {
    check_output(output, mode, |_, _| {}).unwrap()
}

/// Every file and folder under `root`, with each file's size and
/// modification time. Folder times are left out: NTFS updates the copy a
/// folder listing reports lazily, so they can move without any write.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<(u64, SystemTime)>> {
    WalkDir::new(root)
        .into_iter()
        .map(|entry| {
            let path = entry.unwrap().path().to_path_buf();
            let metadata = fs::metadata(&path).unwrap();
            let file = metadata
                .is_file()
                .then(|| (metadata.len(), metadata.modified().unwrap()));
            (path, file)
        })
        .collect()
}

#[test]
fn a_fresh_conversion_is_all_good_in_both_modes_and_nothing_is_written() {
    let converted = convert_fixture();
    let before = snapshot(converted.directory.path());

    let entries = converted.manifest()["entries"].as_object().unwrap().len();
    assert!(entries > 0);
    for mode in [CheckMode::Quick, CheckMode::Full] {
        let calls = AtomicUsize::new(0);
        let last = AtomicUsize::new(0);
        let report = check_output(&converted.output, mode, |done, total| {
            assert_eq!(total, entries);
            calls.fetch_add(1, Ordering::Relaxed);
            last.fetch_max(done, Ordering::Relaxed);
        })
        .unwrap();
        assert!(report.is_ok(), "{mode:?}: {:?}", report.problems);
        assert_eq!(report.files_checked, entries);
        assert!(report.bytes_checked > 0);
        assert_eq!(report.advice(), None);
        assert_eq!(calls.load(Ordering::Relaxed), entries + 1);
        assert_eq!(last.load(Ordering::Relaxed), entries);
    }

    assert_eq!(
        snapshot(converted.directory.path()),
        before,
        "the check changed the tree"
    );
}

#[test]
fn a_deleted_artifact_is_missing() {
    let converted = convert_fixture();
    let artifact = converted.artifact(".glb");
    fs::remove_file(converted.output.join(&artifact)).unwrap();

    for mode in [CheckMode::Quick, CheckMode::Full] {
        let report = check(&converted.output, mode);
        assert_eq!(
            report.problems,
            vec![CheckProblem::Missing {
                output: artifact.clone()
            }],
            "{mode:?}"
        );
        assert!(report.advice().unwrap().contains("only the files listed"));
    }
}

#[test]
fn a_truncated_artifact_has_the_wrong_size() {
    let converted = convert_fixture();
    let artifact = converted.artifact(".ktx2");
    let path = converted.output.join(&artifact);
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();

    let report = check(&converted.output, CheckMode::Quick);
    assert_eq!(
        report.problems,
        vec![CheckProblem::WrongSize {
            output: artifact,
            expected: bytes.len() as u64,
            found: (bytes.len() / 2) as u64,
        }]
    );
}

#[test]
fn a_same_size_byte_flip_passes_quick_and_fails_full() {
    let converted = convert_fixture();
    let artifact = converted.artifact(".luau");
    let path = converted.output.join(&artifact);
    let mut bytes = fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xFF;
    fs::write(&path, &bytes).unwrap();

    assert!(check(&converted.output, CheckMode::Quick).is_ok());
    assert_eq!(
        check(&converted.output, CheckMode::Full).problems,
        vec![CheckProblem::WrongHash { output: artifact }]
    );
}

#[test]
fn another_schema_version_is_reported_and_the_check_goes_on() {
    let converted = convert_fixture();
    let mut manifest = converted.manifest();
    manifest["schema_version"] = (CONVERTER_SCHEMA_VERSION + 1).into();
    converted.write_manifest(&manifest);
    let artifact = converted.artifact(".glb");
    fs::remove_file(converted.output.join(&artifact)).unwrap();

    let report = check(&converted.output, CheckMode::Quick);
    assert_eq!(
        report.problems,
        vec![
            CheckProblem::SchemaVersion {
                found: CONVERTER_SCHEMA_VERSION + 1,
                expected: CONVERTER_SCHEMA_VERSION,
            },
            CheckProblem::Missing { output: artifact },
        ]
    );
    assert!(
        report
            .advice()
            .unwrap()
            .contains("another converter version")
    );
}

#[test]
fn an_entry_outside_the_output_is_refused_without_reading_it() {
    let converted = convert_fixture();
    // A real file beside the output whose size and hash match the entry: a
    // check that followed the path would find nothing wrong with it.
    let outside = converted.directory.path().join("outside.bin");
    fs::write(&outside, b"outside the output").unwrap();
    let hash = hash_file(&outside).unwrap();
    let mut manifest = converted.manifest();
    let entries = manifest["entries"].as_object_mut().unwrap();
    for (key, output) in [
        ("escape-relative", "../outside.bin".to_owned()),
        ("escape-nested", "meshes/../../outside.bin".to_owned()),
        (
            "escape-absolute",
            outside.to_string_lossy().replace('\\', "/"),
        ),
    ] {
        entries.insert(
            key.to_owned(),
            serde_json::json!({
                "source_hash": "",
                "output": output,
                "output_size": 18,
                "output_hash": hash,
            }),
        );
    }
    converted.write_manifest(&manifest);

    let report = check(&converted.output, CheckMode::Full);
    let mut unsafe_paths: Vec<_> = report
        .problems
        .iter()
        .map(|problem| match problem {
            CheckProblem::UnsafePath { output } => output.clone(),
            other => panic!("unexpected problem {other}"),
        })
        .collect();
    unsafe_paths.sort();
    let mut expected = vec![
        "../outside.bin".to_owned(),
        "meshes/../../outside.bin".to_owned(),
        outside.to_string_lossy().replace('\\', "/"),
    ];
    expected.sort();
    assert_eq!(unsafe_paths, expected);
}

#[test]
fn an_entry_through_a_link_that_leads_outside_the_output_is_refused() {
    let converted = convert_fixture();
    let outside = converted.directory.path().join("elsewhere");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("artifact.bin"), b"outside the output").unwrap();
    let hash = hash_file(&outside.join("artifact.bin")).unwrap();
    let link = converted.output.join("link");
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&outside, &link);
    // A symbolic link needs a privilege on Windows; a directory junction does not, and both lead out.
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(&outside, &link).or_else(|_| {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .map_err(|error| error.to_string())
            .and_then(|output| {
                if output.status.success() {
                    Ok(())
                } else {
                    Err(String::from_utf8_lossy(&output.stderr).into_owned())
                }
            })
            .map_err(std::io::Error::other)
    });
    if linked.is_err() {
        eprintln!("skipped: this system does not allow creating a link here");
        return;
    }
    let mut manifest = converted.manifest();
    manifest["entries"].as_object_mut().unwrap().insert(
        "escape-link".to_owned(),
        serde_json::json!({
            "source_hash": "",
            "output": "link/artifact.bin",
            "output_size": 18,
            "output_hash": hash,
        }),
    );
    converted.write_manifest(&manifest);

    for mode in [CheckMode::Quick, CheckMode::Full] {
        let report = check(&converted.output, mode);
        assert!(
            report.problems.iter().any(|problem| matches!(
                problem,
                CheckProblem::UnsafePath { output } if output == "link/artifact.bin"
            )),
            "{mode:?} followed a link out of the output: {:?}",
            report.problems
        );
    }
}

#[test]
fn folder_level_problems_are_reported() {
    let converted = convert_fixture();
    let mut manifest = converted.manifest();
    manifest["complete"] = false.into();
    manifest["failures"] = serde_json::json!({
        "textures/bad.dds": "failed to convert",
        "meshes/bad.nif": "failed to convert",
    });
    converted.write_manifest(&manifest);
    fs::remove_file(converted.output.join("skyrim_world.db")).unwrap();

    let report = check(&converted.output, CheckMode::Quick);
    assert_eq!(
        report.problems,
        vec![
            CheckProblem::Incomplete,
            CheckProblem::RecordedFailures { count: 2 },
            CheckProblem::WorldDatabaseMissing,
        ]
    );
    assert!(
        report
            .advice()
            .unwrap()
            .contains("retries the failed inputs")
    );
}

#[test]
fn a_missing_or_unreadable_manifest_is_an_error() {
    let converted = convert_fixture();
    fs::write(converted.manifest_path(), b"{ not json").unwrap();
    assert!(check_output(&converted.output, CheckMode::Quick, |_, _| {}).is_err());

    fs::remove_file(converted.manifest_path()).unwrap();
    let error = check_output(&converted.output, CheckMode::Quick, |_, _| {})
        .unwrap_err()
        .to_string();
    assert!(error.contains("conversion-manifest.json"), "{error}");

    let absent = converted.directory.path().join("never-converted");
    assert!(check_output(&absent, CheckMode::Quick, |_, _| {}).is_err());
    assert!(
        !absent.exists(),
        "the check created the folder it was given"
    );
}

#[test]
fn a_check_cancelled_before_it_starts_reads_nothing() {
    let converted = convert_fixture();
    let calls = AtomicUsize::new(0);
    let error = check_output_with_cancel(
        &converted.output,
        CheckMode::Full,
        |_, _| {
            calls.fetch_add(1, Ordering::Relaxed);
        },
        &AtomicBool::new(true),
    )
    .unwrap_err();
    assert!(error.is::<CheckCancelled>(), "{error:#}");
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn a_check_cancelled_from_its_progress_callback_stops_early() {
    let converted = convert_fixture();
    let entries = converted.manifest()["entries"].as_object().unwrap().len();
    assert!(entries > 1, "the fixture needs several entries");

    // One worker thread, so nothing else is in flight when the flag is set
    // and the count of entries checked is exact.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let cancel = AtomicBool::new(false);
    let checked = AtomicUsize::new(0);
    let result = pool.install(|| {
        check_output_with_cancel(
            &converted.output,
            CheckMode::Full,
            |done, _| {
                checked.fetch_max(done, Ordering::Relaxed);
                if done == 1 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            &cancel,
        )
    });
    let error = result.unwrap_err();
    assert!(error.is::<CheckCancelled>(), "{error:#}");
    assert_eq!(checked.load(Ordering::Relaxed), 1);
    assert!(checked.load(Ordering::Relaxed) < entries);
}

#[test]
fn an_uncancelled_check_with_a_stop_button_matches_check_output() {
    let converted = convert_fixture();
    let artifact = converted.artifact(".glb");
    fs::remove_file(converted.output.join(&artifact)).unwrap();

    for mode in [CheckMode::Quick, CheckMode::Full] {
        let plain = check(&converted.output, mode);
        let stoppable =
            check_output_with_cancel(&converted.output, mode, |_, _| {}, &AtomicBool::new(false))
                .unwrap();
        assert_eq!(stoppable.files_checked, plain.files_checked);
        assert_eq!(stoppable.bytes_checked, plain.bytes_checked);
        assert_eq!(stoppable.problems, plain.problems);
        assert_eq!(
            stoppable.problems,
            vec![CheckProblem::Missing {
                output: artifact.clone()
            }]
        );
    }
}
