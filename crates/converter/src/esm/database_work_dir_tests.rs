//! Working SQLite transfer retains exact bytes and protects existing artifacts.
use super::*;

#[test]
/// Resolve parent aliases while retaining every existing input or output.
fn fresh_leaf_resolves_parent_aliases_and_rejects_any_existing_leaf() -> Result<()> {
    let root = tempfile::tempdir()?;
    let future = root.path().join("future");
    assert_eq!(
        fresh_record_bundle_location(&future)?,
        fresh_record_bundle_location(&root.path().join(".").join("future"))?
    );
    assert!(!future.exists());
    fs::create_dir(&future)?;
    assert!(fresh_record_bundle_location(&future).is_err());
    let file = root.path().join("file");
    fs::write(&file, b"existing")?;
    assert!(fresh_record_bundle_location(&file).is_err());
    assert_eq!(fs::read(&file)?, b"existing");
    assert!(
        fresh_record_bundle_location(&root.path().join("missing-parent").join("child")).is_err()
    );
    Ok(())
}

/// Create an asymmetric transfer larger than one buffer with a short final block.
fn fixture(root: &Path) -> Result<(PathBuf, PathBuf, Vec<u8>)> {
    let work = root.join("work");
    let output = root.join("output");
    fs::create_dir(&work)?;
    fs::create_dir(&output)?;
    // Independent expected bytes span more than one copy buffer and a nonaligned tail.
    let bytes = (0..1_048_613)
        .map(|n| ((n * 31 + 17) % 251) as u8)
        .collect::<Vec<_>>();
    fs::write(work.join("skyrim_world.db"), &bytes)?;
    fs::write(
        work.join("inhouse-reader-diagnostics.json"),
        b"{\"synthetic\":true,\"warnings\":3}",
    )?;
    Ok((work, output, bytes))
}

#[test]
/// Retain exact completed bytes at both locations.
fn completed_copy_preserves_every_byte_diagnostics_and_working_artifacts() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (work, output, expected) = fixture(root.path())?;
    copy_completed_record_database(&work, &output)?;
    assert_eq!(fs::read(output.join("skyrim_world.db"))?, expected);
    assert_eq!(fs::read(work.join("skyrim_world.db"))?, expected);
    assert_eq!(
        fs::read(output.join("inhouse-reader-diagnostics.json"))?,
        b"{\"synthetic\":true,\"warnings\":3}"
    );
    assert!(work.join("inhouse-reader-diagnostics.json").is_file());
    assert!(!output.join("record-reader.json").exists());
    Ok(())
}

#[test]
/// Exclusive publication preserves an existing database destination.
fn existing_database_destination_is_never_overwritten() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (work, output, expected) = fixture(root.path())?;
    fs::write(
        output.join("skyrim_world.db"),
        b"retain-original-destination",
    )?;
    assert!(copy_completed_record_database(&work, &output).is_err());
    assert_eq!(
        fs::read(output.join("skyrim_world.db"))?,
        b"retain-original-destination"
    );
    assert_eq!(fs::read(work.join("skyrim_world.db"))?, expected);
    assert!(!output.join("inhouse-reader-diagnostics.json").exists());
    Ok(())
}

#[test]
/// A later copy failure retains all completed and pre-existing artifacts.
fn later_copy_failure_retains_completed_database_and_both_work_artifacts() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (work, output, expected) = fixture(root.path())?;
    fs::write(
        output.join("inhouse-reader-diagnostics.json"),
        b"retain-existing-diagnostic",
    )?;
    assert!(copy_completed_record_database(&work, &output).is_err());
    assert_eq!(fs::read(output.join("skyrim_world.db"))?, expected);
    assert_eq!(fs::read(work.join("skyrim_world.db"))?, expected);
    assert_eq!(
        fs::read(output.join("inhouse-reader-diagnostics.json"))?,
        b"retain-existing-diagnostic"
    );
    assert!(work.join("inhouse-reader-diagnostics.json").is_file());
    assert!(!output.join("record-reader.json").exists());
    Ok(())
}

#[test]
/// An incomplete working bundle cannot publish a database.
fn incomplete_source_preflight_copies_nothing() -> Result<()> {
    let root = tempfile::tempdir()?;
    let work = root.path().join("work");
    let output = root.path().join("output");
    fs::create_dir(&work)?;
    fs::create_dir(&output)?;
    fs::write(work.join("skyrim_world.db"), b"incomplete")?;
    assert!(copy_completed_record_database(&work, &output).is_err());
    assert!(!output.join("skyrim_world.db").exists());
    assert_eq!(fs::read(work.join("skyrim_world.db"))?, b"incomplete");
    Ok(())
}
