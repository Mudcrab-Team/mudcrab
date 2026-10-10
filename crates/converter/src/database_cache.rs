//! Verified clean database bundles, before mesh integration or other database mutations.
use crate::{
    cache::{CONVERTER_SCHEMA_VERSION, hash_bytes, hash_file},
    config::{PipelineConfig, RecordReader},
    esm::{cell_cache::validate_cell_cache, exporter::validate_database, inhouse},
    pipeline::{Cancellation, Interrupted},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use shared::asset_lock::AssetLock;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

const FORMAT: u32 = 1;
const KIND: &str = "mudcrab_base_database";
const RECEIPT: &str = "receipt.json";

#[derive(Debug, Clone)]
pub(super) struct Identity {
    pub key: String,
    pub inputs: serde_json::Value,
    reader: RecordReader,
}

#[derive(Debug, Serialize, Deserialize)]
struct Payload {
    size: u64,
    sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Receipt {
    kind: String,
    format_version: u32,
    complete: bool,
    key: String,
    identity: serde_json::Value,
    files: BTreeMap<String, Payload>,
}

#[derive(Debug)]
pub(super) struct Restored {
    pub files: u64,
    pub validation_elapsed_ms: u128,
}

fn outputs(reader: RecordReader) -> Vec<&'static str> {
    let mut files = vec!["skyrim_world.db", "cell_cache.rkyv", "record-reader.json"];
    if reader == RecordReader::Inhouse {
        files.extend([
            "cell_cache_preserved.rkyv",
            "inhouse-reader-diagnostics.json",
            inhouse::DATABASE_PROFILE_FILE,
        ]);
    }
    files
}

fn cancelled(cancellation: &Cancellation) -> Result<()> {
    if cancellation.is_cancelled() {
        return Err(Interrupted::new().into());
    }
    Ok(())
}

/// Resolve the same loose-before-VFS lookup as the reader. Ambiguous or unreadable banks
/// prevent caching rather than pretending that a bank was absent.
fn child(parent: &Path, name: &str) -> Result<Option<PathBuf>> {
    let direct = parent.join(name);
    if direct.exists() {
        return Ok(Some(direct));
    }
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
        {
            matches.push(entry.path());
        }
    }
    ensure!(matches.len() <= 1, "ambiguous localized string path");
    Ok(matches.pop())
}

fn source_fingerprint() -> String {
    // Include the native producers and dependency contracts; the compiled program digest in
    // the input identity also binds future modules, build flags and dependency code.
    let sources: &[&[u8]] = &[
        include_bytes!("asset_path.rs"),
        include_bytes!("esm/mod.rs"),
        include_bytes!("esm/binary.rs"),
        include_bytes!("esm/mmap_reader.rs"),
        include_bytes!("esm/load_order.rs"),
        include_bytes!("esm/extractors.rs"),
        include_bytes!("esm/exporter.rs"),
        include_bytes!("esm/inhouse.rs"),
        include_bytes!("esm/cell_cache.rs"),
        include_bytes!("esm/types.rs"),
        include_bytes!("esm/records/mod.rs"),
        include_bytes!("esm/records/record_type/mod.rs"),
        include_bytes!("esm/records/record_type/vmad.rs"),
        include_bytes!("records/mod.rs"),
        include_bytes!("records/scanner.rs"),
        include_bytes!("records/decoder.rs"),
        include_bytes!("records/schema_format.rs"),
        include_bytes!("records/vmad.rs"),
        include_bytes!("records/deciders/mod.rs"),
        include_bytes!("records/deciders/actors.rs"),
        include_bytes!("records/deciders/conditions.rs"),
        include_bytes!("records/deciders/visual_extras.rs"),
        include_bytes!("records/deciders/world_extras.rs"),
        include_bytes!("records/deciders/items.rs"),
        include_bytes!("records/deciders/magic.rs"),
        include_bytes!("records/deciders/audio_extras.rs"),
        include_bytes!("records/deciders/ai.rs"),
        include_bytes!("records/deciders/perks.rs"),
        include_bytes!("../../shared/src/lib.rs"),
        include_bytes!("../../../Cargo.lock"),
        include_bytes!("../../../Cargo.toml"),
        include_bytes!("../Cargo.toml"),
        include_bytes!("../../shared/Cargo.toml"),
        crate::records::SCHEMA_BYTES,
    ];
    hash_bytes(
        &serde_json::to_vec(
            &sources
                .iter()
                .map(|source| hash_bytes(source))
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    )
}

pub(super) fn identity(
    config: &PipelineConfig,
    plugins: &[PathBuf],
    vfs: &Path,
    cancellation: &Cancellation,
) -> Result<Option<Identity>> {
    let mut plugin_inputs = Vec::new();
    let mut localization = Vec::new();
    for (index, plugin) in plugins.iter().enumerate() {
        cancelled(cancellation)?;
        plugin_inputs.push(serde_json::json!({
            "name": plugin.file_name().unwrap_or_default().to_string_lossy(),
            "size": fs::metadata(plugin)?.len(),
            "sha256": hash_file(plugin)?,
        }));
        if config.record_reader != RecordReader::Inhouse {
            continue;
        }
        for bank in ["strings", "dlstrings", "ilstrings"] {
            let name = format!(
                "{}_english.{bank}",
                plugin.file_stem().unwrap_or_default().to_string_lossy()
            );
            let lookup = (|| -> Result<serde_json::Value> {
                for (origin, root) in [
                    ("loose", plugin.parent().unwrap_or(Path::new("."))),
                    ("vfs", vfs),
                ] {
                    if let Some(folder) = child(root, "strings")?
                        && let Some(path) = child(&folder, &name)?
                    {
                        let metadata = fs::metadata(&path)?;
                        ensure!(metadata.is_file(), "localized bank is not a file");
                        return Ok(serde_json::json!({
                            "plugin": index, "bank": bank, "state": "present", "origin": origin,
                            "name": path.file_name().unwrap_or_default().to_string_lossy(),
                            "size": metadata.len(), "sha256": hash_file(&path)?,
                        }));
                    }
                }
                Ok(serde_json::json!({"plugin": index, "bank": bank, "state": "absent"}))
            })();
            let Ok(input) = lookup else { return Ok(None) };
            localization.push(input);
        }
    }
    let inputs = serde_json::json!({
        "cache_format": FORMAT,
        "converter_schema": CONVERTER_SCHEMA_VERSION,
        "database_schema": shared::WORLD_DATABASE_SCHEMA_VERSION,
        "reader": inhouse::reader_identity(config.record_reader),
        "native_sources_sha256": source_fingerprint(),
        "compiled_program_sha256": hash_file(&std::env::current_exe()?)?,
        "target": {"arch": std::env::consts::ARCH, "os": std::env::consts::OS,
            "endian": if cfg!(target_endian = "little") {"little"} else {"big"},
            "pointer_width": usize::BITS},
        "plugins": plugin_inputs,
        "localization": localization,
    });
    Ok(Some(Identity {
        key: hash_bytes(&serde_json::to_vec(&inputs)?),
        inputs,
        reader: config.record_reader,
    }))
}

pub(super) fn inputs_unchanged(
    expected: &Identity,
    config: &PipelineConfig,
    plugins: &[PathBuf],
    vfs: &Path,
    cancellation: &Cancellation,
) -> Result<bool> {
    Ok(identity(config, plugins, vfs, cancellation)?
        .as_ref()
        .is_some_and(|current| current.key == expected.key))
}

fn is_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn owned_layout(directory: &Path, reader: RecordReader) -> bool {
    if !fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.is_dir()) {
        return false;
    }
    let mut expected = outputs(reader);
    expected.push(RECEIPT);
    expected.sort_unstable();
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    let mut actual = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { return false };
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            return false;
        }
        actual.push(entry.file_name().to_string_lossy().into_owned());
    }
    actual.sort_unstable();
    actual == expected
}

fn receipt_matches(receipt: &Receipt, identity: &Identity) -> bool {
    receipt.kind == KIND
        && receipt.format_version == FORMAT
        && receipt.complete
        && receipt.key == identity.key
        && receipt.identity == identity.inputs
        && receipt.files.keys().map(String::as_str).collect::<Vec<_>>() == {
            let mut expected = outputs(identity.reader);
            expected.sort_unstable();
            expected
        }
}

pub(super) fn clear_outputs(staging: &Path) -> Result<()> {
    let mut files = outputs(RecordReader::Inhouse);
    files.extend([
        "skyrim_world.db-wal",
        "skyrim_world.db-shm",
        "skyrim_world.db-journal",
    ]);
    for name in files {
        let path = staging.join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).wrap_err_with(|| format!("failed to unlink {}", path.display()));
            }
        }
    }
    Ok(())
}

fn validate_bundle(directory: &Path, identity: &Identity) -> Result<()> {
    let connection = Connection::open_with_flags(
        directory.join("skyrim_world.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    validate_database(&connection)?;
    if identity.reader == RecordReader::Inhouse {
        for table in [
            "inhouse_source_records",
            "inhouse_localized_fields",
            "inhouse_terrain_layers",
            "inhouse_terrain_source_order",
        ] {
            let present: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )?;
            ensure!(present, "missing in-house provenance table {table}");
        }
        let diagnostics: serde_json::Value = serde_json::from_slice(&fs::read(
            directory.join("inhouse-reader-diagnostics.json"),
        )?)?;
        ensure!(
            diagnostics["decoder"].is_object() && diagnostics["adapter"].is_object(),
            "incomplete reader diagnostics"
        );
        let profile: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join(inhouse::DATABASE_PROFILE_FILE))?)?;
        ensure!(
            profile["counts"].is_object() && profile["elapsed_seconds"].is_object(),
            "incomplete database profile"
        );
        validate_cell_cache(&directory.join("cell_cache_preserved.rkyv"))?;
    }
    validate_cell_cache(&directory.join("cell_cache.rkyv"))?;
    let reader: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("record-reader.json"))?)?;
    ensure!(
        reader == inhouse::reader_identity(identity.reader),
        "reader stamp mismatch"
    );
    Ok(())
}

pub(super) fn restore(
    root: &Path,
    identity: &Identity,
    staging: &Path,
    cancellation: &Cancellation,
) -> Result<Option<Restored>> {
    cancelled(cancellation)?;
    if !fs::symlink_metadata(root).is_ok_and(|metadata| metadata.is_dir()) {
        return Ok(None);
    }
    let Ok(_lock) = AssetLock::acquire_exclusive(root.join("bundle")) else {
        return Ok(None);
    };
    let directory = root.join(&identity.key);
    if !fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir())
        || !regular(&directory.join(RECEIPT))
    {
        return Ok(None);
    }
    let Ok(receipt) = fs::read(directory.join(RECEIPT))
        .and_then(|bytes| serde_json::from_slice::<Receipt>(&bytes).map_err(std::io::Error::other))
    else {
        return Ok(None);
    };
    if !receipt_matches(&receipt, identity) || !owned_layout(&directory, identity.reader) {
        return Ok(None);
    }
    // Check the entire source bundle before touching published staging names.
    for (name, payload) in &receipt.files {
        cancelled(cancellation)?;
        let path = directory.join(name);
        if !regular(&path)
            || fs::metadata(&path).map_or(true, |metadata| metadata.len() != payload.size)
            || hash_file(&path).map_or(true, |hash| hash != payload.sha256)
        {
            return Ok(None);
        }
    }
    let temporary = tempfile::Builder::new()
        .prefix(".database-restore-")
        .tempdir_in(staging)?;
    for (name, payload) in &receipt.files {
        cancelled(cancellation)?;
        let path = temporary.path().join(name);
        if fs::copy(directory.join(name), &path).is_err()
            || hash_file(&path).map_or(true, |hash| hash != payload.sha256)
        {
            return Ok(None);
        }
    }
    let started = Instant::now();
    if validate_bundle(temporary.path(), identity).is_err() {
        return Ok(None);
    }
    let validation_elapsed_ms = started.elapsed().as_millis();
    cancelled(cancellation)?;
    clear_outputs(staging)?;
    for name in receipt.files.keys() {
        fs::rename(temporary.path().join(name), staging.join(name))?;
    }
    Ok(Some(Restored {
        files: receipt.files.len() as u64,
        validation_elapsed_ms,
    }))
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<()> {
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) -> Result<()> {
    Ok(())
}

fn owned_bundle(directory: &Path, key: &str) -> bool {
    if !is_key(key)
        || !fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.is_dir())
        || !regular(&directory.join(RECEIPT))
    {
        return false;
    }
    let Ok(bytes) = fs::read(directory.join(RECEIPT)) else {
        return false;
    };
    let Ok(receipt) = serde_json::from_slice::<Receipt>(&bytes) else {
        return false;
    };
    let reader = if receipt.identity["reader"]["mode"] == "inhouse" {
        RecordReader::Inhouse
    } else {
        RecordReader::Legacy
    };
    let mut names = outputs(reader);
    names.push(RECEIPT);
    names.sort_unstable();
    if receipt.kind != KIND
        || receipt.key != key
        || hash_bytes(&serde_json::to_vec(&receipt.identity).unwrap_or_default()) != key
        || receipt.files.keys().map(String::as_str).collect::<Vec<_>>() != {
            let mut expected = outputs(reader);
            expected.sort_unstable();
            expected
        }
    {
        return false;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    let Ok(mut actual) = entries
        .map(|entry| {
            entry.and_then(|entry| {
                if !entry.file_type()?.is_file() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "unexpected cache entry",
                    ));
                }
                Ok(entry.file_name().to_string_lossy().into_owned())
            })
        })
        .collect::<std::io::Result<Vec<_>>>()
    else {
        return false;
    };
    actual.sort_unstable();
    // A damaged payload can be missing, but unrelated entries never prove cache ownership.
    actual
        .iter()
        .all(|name| names.iter().any(|expected| name == expected))
}

pub(super) fn store(
    root: &Path,
    identity: &Identity,
    staging: &Path,
    cancellation: &Cancellation,
) -> Result<()> {
    cancelled(cancellation)?;
    fs::create_dir_all(root)?;
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "database cache root is not a directory"
    );
    let _lock = AssetLock::acquire_exclusive(root.join("bundle"))?;
    let temporary = tempfile::Builder::new()
        .prefix(".database-pending-")
        .tempdir_in(root)?;
    let mut files = BTreeMap::new();
    for name in outputs(identity.reader) {
        cancelled(cancellation)?;
        let source = staging.join(name);
        ensure!(
            regular(&source),
            "base database output is not a regular file: {name}"
        );
        let destination = temporary.path().join(name);
        fs::copy(source, &destination)?;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&destination)?
            .sync_all()?;
        files.insert(
            name.to_owned(),
            Payload {
                size: fs::metadata(&destination)?.len(),
                sha256: hash_file(&destination)?,
            },
        );
    }
    // The pending copies are private inodes. Only their fully durable receipt is published.
    let receipt = Receipt {
        kind: KIND.to_owned(),
        format_version: FORMAT,
        complete: true,
        key: identity.key.clone(),
        identity: identity.inputs.clone(),
        files,
    };
    let mut receipt_file = fs::File::create(temporary.path().join(RECEIPT))?;
    receipt_file.write_all(&serde_json::to_vec_pretty(&receipt)?)?;
    receipt_file.sync_all()?;
    drop(receipt_file);
    sync_directory(temporary.path())?;
    cancelled(cancellation)?;
    let destination = root.join(&identity.key);
    if fs::symlink_metadata(&destination).is_ok() {
        // The exact key and exact converter-only layout also recognize a bundle whose receipt
        // was damaged. Extra files or links make ownership uncertain and are preserved.
        ensure!(
            owned_bundle(&destination, &identity.key)
                || owned_layout(&destination, identity.reader),
            "refusing to replace an unowned database-cache directory"
        );
        let backup = tempfile::Builder::new()
            .prefix(".database-replaced-")
            .tempdir_in(root)?;
        fs::remove_dir(backup.path())?;
        fs::rename(&destination, backup.path())?;
        let backup = backup.keep();
        if let Err(error) = fs::rename(temporary.path(), &destination) {
            let _ = fs::rename(&backup, &destination);
            return Err(error.into());
        }
        sync_directory(root)?;
        // The complete new bundle is durable before removing its old generation.
        fs::remove_dir_all(&backup)?;
    } else {
        fs::rename(temporary.path(), &destination)?;
        sync_directory(root)?;
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let key = entry.file_name().to_string_lossy().into_owned();
        if key != identity.key && owned_bundle(&entry.path(), &key) {
            fs::remove_dir_all(entry.path())?;
        }
    }
    sync_directory(root)?;
    Ok(())
}

pub(super) fn mark_reused_profile(
    staging: &Path,
    identity: &Identity,
    restore_ms: u128,
    validation_ms: u128,
) -> Result<()> {
    if identity.reader != RecordReader::Inhouse {
        return Ok(());
    }
    let path = staging.join(inhouse::DATABASE_PROFILE_FILE);
    let mut profile: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let historical = profile["elapsed_seconds"].take();
    profile["producer_elapsed_seconds"] = historical;
    profile["elapsed_seconds"] = serde_json::json!({
        "database_cache_restore": restore_ms as f64 / 1000.0,
        "database_validation": validation_ms as f64 / 1000.0,
    });
    profile["cache_hit"] = serde_json::json!(true);
    profile["cache_key"] = serde_json::json!(identity.key);
    profile["timing_source"] = serde_json::json!("cache_restore");
    // Restored profile bytes were copied, and replacement keeps that guarantee explicit.
    fs::remove_file(&path)?;
    fs::write(path, serde_json::to_vec_pretty(&profile)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esm::EsmParser;

    struct Fixture {
        directory: tempfile::TempDir,
        config: PipelineConfig,
        plugins: Vec<PathBuf>,
        staging: PathBuf,
        cache: PathBuf,
        identity: Identity,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let staging = directory.path().join("producer");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(staging.join("vfs")).unwrap();
        let plugin = data.join("Skyrim.esm");
        fs::write(
            &plugin,
            dummy_content::esm::plugin(&dummy_content::esm::Plugin {
                author: "DatabaseCache",
                worldspace: "CacheWorld",
                cells: &[dummy_content::esm::Cell {
                    grid_x: 0,
                    grid_y: 0,
                }],
                model_path: "test/cache.nif",
                diffuse: "test/cache.dds",
                normal_texture: "test/cache_n.dds",
            })
            .unwrap(),
        )
        .unwrap();
        let plugins = vec![plugin];
        let mut config = PipelineConfig::new(&data, directory.path().join("published"));
        config.record_reader = RecordReader::Inhouse;
        config.no_lod = true;
        let identity = identity(
            &config,
            &plugins,
            &staging.join("vfs"),
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
        let records = EsmParser::convert_plugins_with_reader(
            &plugins,
            &staging.join("skyrim_world.db"),
            config.record_reader,
            &staging.join("vfs"),
        )
        .unwrap();
        inhouse::write_terrain_caches(&records, &staging.join("skyrim_world.db"), &staging)
            .unwrap();
        inhouse::write_reader_identity(&staging, config.record_reader).unwrap();
        let cache = directory.path().join("cache");
        Fixture {
            directory,
            config,
            plugins,
            staging,
            cache,
            identity,
        }
    }

    fn destination(fixture: &Fixture, name: &str) -> PathBuf {
        let path = fixture.directory.path().join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn database_cache_recheck_rejects_inputs_changed_after_restore() {
        let fixture = fixture();
        let cancellation = Cancellation::new();
        store(
            &fixture.cache,
            &fixture.identity,
            &fixture.staging,
            &cancellation,
        )
        .unwrap();
        let restored = destination(&fixture, "restored");
        restore(&fixture.cache, &fixture.identity, &restored, &cancellation)
            .unwrap()
            .unwrap();
        assert!(
            inputs_unchanged(
                &fixture.identity,
                &fixture.config,
                &fixture.plugins,
                &fixture.staging.join("vfs"),
                &cancellation,
            )
            .unwrap()
        );
        let mut bytes = fs::read(&fixture.plugins[0]).unwrap();
        bytes.push(0);
        fs::write(&fixture.plugins[0], bytes).unwrap();
        assert!(
            !inputs_unchanged(
                &fixture.identity,
                &fixture.config,
                &fixture.plugins,
                &fixture.staging.join("vfs"),
                &cancellation,
            )
            .unwrap()
        );
    }

    #[test]
    fn database_bundle_preserves_provenance_and_restores_private_pristine_database_bytes() {
        let fixture = fixture();
        let cancellation = Cancellation::new();
        store(
            &fixture.cache,
            &fixture.identity,
            &fixture.staging,
            &cancellation,
        )
        .unwrap();
        let bundle = fixture.cache.join(&fixture.identity.key);
        let pristine = hash_file(&bundle.join("skyrim_world.db")).unwrap();
        let first = destination(&fixture, "first");
        let restored = restore(&fixture.cache, &fixture.identity, &first, &cancellation)
            .unwrap()
            .unwrap();
        assert_eq!(restored.files, outputs(RecordReader::Inhouse).len() as u64);
        for name in outputs(RecordReader::Inhouse) {
            assert_eq!(
                fs::read(first.join(name)).unwrap(),
                fs::read(fixture.staging.join(name)).unwrap()
            );
        }
        let old_database = first.join("skyrim_world.db");
        {
            let connection = Connection::open(&old_database).unwrap();
            assert!(
                connection
                    .query_row::<u64, _, _>(
                        "SELECT count(*) FROM inhouse_source_records",
                        [],
                        |row| row.get(0)
                    )
                    .unwrap()
                    > 0
            );
            connection
                .execute("UPDATE statics SET bounds_valid=1,bounds_min_x=99", [])
                .unwrap();
        }
        let previous_bytes = hash_file(&old_database).unwrap();
        assert_ne!(previous_bytes, pristine);
        let second = destination(&fixture, "second");
        fs::hard_link(&old_database, second.join("skyrim_world.db")).unwrap();
        restore(&fixture.cache, &fixture.identity, &second, &cancellation)
            .unwrap()
            .unwrap();
        assert_eq!(
            hash_file(&second.join("skyrim_world.db")).unwrap(),
            pristine
        );
        assert_eq!(hash_file(&old_database).unwrap(), previous_bytes);
        {
            let connection = Connection::open(second.join("skyrim_world.db")).unwrap();
            assert_eq!(
                connection
                    .query_row::<u64, _, _>("SELECT sum(bounds_valid) FROM statics", [], |row| row
                        .get(0))
                    .unwrap(),
                0
            );
            // Represents the later per-run bounds integration, which must not write the bundle.
            connection
                .execute("UPDATE statics SET bounds_valid=1,bounds_min_x=12", [])
                .unwrap();
        }
        assert_eq!(
            hash_file(&bundle.join("skyrim_world.db")).unwrap(),
            pristine
        );
        assert_eq!(hash_file(&old_database).unwrap(), previous_bytes);
        let third = destination(&fixture, "third");
        restore(&fixture.cache, &fixture.identity, &third, &cancellation)
            .unwrap()
            .unwrap();
        assert_eq!(hash_file(&third.join("skyrim_world.db")).unwrap(), pristine);
    }

    #[test]
    fn database_bundle_corruption_partial_receipts_and_old_formats_miss_and_recover() {
        let fixture = fixture();
        let cancellation = Cancellation::new();
        store(
            &fixture.cache,
            &fixture.identity,
            &fixture.staging,
            &cancellation,
        )
        .unwrap();
        let bundle = fixture.cache.join(&fixture.identity.key);
        for (index, damage) in [
            "database",
            "terrain",
            "receipt",
            "old-format",
            "incomplete",
            "missing",
        ]
        .into_iter()
        .enumerate()
        {
            match damage {
                "database" => fs::write(bundle.join("skyrim_world.db"), b"not a database").unwrap(),
                "terrain" => {
                    fs::write(bundle.join("cell_cache_preserved.rkyv"), b"broken").unwrap()
                }
                "receipt" => fs::write(bundle.join(RECEIPT), b"{truncated").unwrap(),
                "old-format" | "incomplete" => {
                    let mut receipt: serde_json::Value =
                        serde_json::from_slice(&fs::read(bundle.join(RECEIPT)).unwrap()).unwrap();
                    if damage == "old-format" {
                        receipt["format_version"] = serde_json::json!(0);
                    } else {
                        receipt["complete"] = serde_json::json!(false);
                    }
                    fs::write(bundle.join(RECEIPT), serde_json::to_vec(&receipt).unwrap()).unwrap();
                }
                "missing" => {
                    fs::remove_file(bundle.join("inhouse-reader-diagnostics.json")).unwrap()
                }
                _ => unreachable!(),
            }
            let target = destination(&fixture, &format!("damaged-{index}"));
            fs::write(target.join("untouched"), b"staged work").unwrap();
            assert!(
                restore(&fixture.cache, &fixture.identity, &target, &cancellation)
                    .unwrap()
                    .is_none()
            );
            assert!(!target.join("skyrim_world.db").exists());
            store(
                &fixture.cache,
                &fixture.identity,
                &fixture.staging,
                &cancellation,
            )
            .unwrap();
            assert!(
                restore(&fixture.cache, &fixture.identity, &target, &cancellation)
                    .unwrap()
                    .is_some()
            );
            assert_eq!(fs::read(target.join("untouched")).unwrap(), b"staged work");
        }
    }

    #[test]
    fn database_bundle_rejects_corrupt_sqlite_or_rkyv_even_if_receipt_hashes_match() {
        let fixture = fixture();
        let cancellation = Cancellation::new();
        for name in [
            "skyrim_world.db",
            "cell_cache.rkyv",
            "cell_cache_preserved.rkyv",
        ] {
            store(
                &fixture.cache,
                &fixture.identity,
                &fixture.staging,
                &cancellation,
            )
            .unwrap();
            let bundle = fixture.cache.join(&fixture.identity.key);
            let path = bundle.join(name);
            fs::write(&path, b"invalid encoded payload").unwrap();
            let mut receipt: Receipt =
                serde_json::from_slice(&fs::read(bundle.join(RECEIPT)).unwrap()).unwrap();
            receipt.files.insert(
                name.to_owned(),
                Payload {
                    size: fs::metadata(&path).unwrap().len(),
                    sha256: hash_file(&path).unwrap(),
                },
            );
            fs::write(bundle.join(RECEIPT), serde_json::to_vec(&receipt).unwrap()).unwrap();
            let target = destination(&fixture, &format!("bad-{name}"));
            assert!(
                restore(&fixture.cache, &fixture.identity, &target, &cancellation)
                    .unwrap()
                    .is_none()
            );
            assert!(!target.join("skyrim_world.db").exists());
        }
    }

    #[test]
    fn database_identity_tracks_ordered_plugins_reader_and_effective_localized_bank_coverage() {
        let fixture = fixture();
        let cancellation = Cancellation::new();
        let key = |config: &PipelineConfig, plugins: &[PathBuf]| {
            identity(config, plugins, &fixture.staging.join("vfs"), &cancellation)
                .unwrap()
                .unwrap()
                .key
        };
        let original = key(&fixture.config, &fixture.plugins);
        let mut irrelevant = fixture.config.clone();
        irrelevant.texture_fallback_quality += 1;
        irrelevant.no_lod = false;
        assert_eq!(key(&irrelevant, &fixture.plugins), original);
        let mut legacy = fixture.config.clone();
        legacy.record_reader = RecordReader::Legacy;
        assert_ne!(key(&legacy, &fixture.plugins), original);
        let second = fixture.config.data_dir.join("Other.esm");
        fs::copy(&fixture.plugins[0], &second).unwrap();
        let ordered = [fixture.plugins[0].clone(), second.clone()];
        let reversed = [second.clone(), fixture.plugins[0].clone()];
        assert_ne!(
            key(&fixture.config, &ordered),
            key(&fixture.config, &reversed)
        );
        let original_plugin = fs::read(&fixture.plugins[0]).unwrap();
        fs::write(&fixture.plugins[0], b"changed plugin bytes").unwrap();
        assert_ne!(key(&fixture.config, &fixture.plugins), original);
        fs::write(&fixture.plugins[0], original_plugin).unwrap();
        let vfs_strings = fixture.staging.join("vfs/Strings");
        fs::create_dir(&vfs_strings).unwrap();
        let bank = vfs_strings.join("Skyrim_english.strings");
        fs::write(&bank, b"malformed but present").unwrap();
        let malformed = key(&fixture.config, &fixture.plugins);
        assert_ne!(malformed, original);
        fs::write(&bank, b"other malformed bytes").unwrap();
        assert_ne!(key(&fixture.config, &fixture.plugins), malformed);
        let loose = fixture.config.data_dir.join("Strings");
        fs::create_dir(&loose).unwrap();
        fs::write(loose.join("Skyrim_english.strings"), b"loose wins").unwrap();
        let loose_key = key(&fixture.config, &fixture.plugins);
        fs::write(&bank, b"ignored archive bytes").unwrap();
        assert_eq!(key(&fixture.config, &fixture.plugins), loose_key);
        fs::remove_file(loose.join("Skyrim_english.strings")).unwrap();
        assert_ne!(key(&fixture.config, &fixture.plugins), loose_key);
        fs::remove_file(bank).unwrap();
        assert_eq!(key(&fixture.config, &fixture.plugins), original);
    }

    #[test]
    fn database_bundle_rejects_extras_and_cancellation_preserves_previous_cache() {
        let fixture = fixture();
        let cancellation = Cancellation::new();
        store(
            &fixture.cache,
            &fixture.identity,
            &fixture.staging,
            &cancellation,
        )
        .unwrap();
        let bundle = fixture.cache.join(&fixture.identity.key);
        fs::write(bundle.join("unrelated.txt"), b"preserve me").unwrap();
        let target = destination(&fixture, "extra");
        assert!(
            restore(&fixture.cache, &fixture.identity, &target, &cancellation)
                .unwrap()
                .is_none()
        );
        assert!(
            store(
                &fixture.cache,
                &fixture.identity,
                &fixture.staging,
                &cancellation
            )
            .is_err()
        );
        assert_eq!(
            fs::read(bundle.join("unrelated.txt")).unwrap(),
            b"preserve me"
        );
        fs::remove_file(bundle.join("unrelated.txt")).unwrap();
        let receipt_before = fs::read(bundle.join(RECEIPT)).unwrap();
        cancellation.cancel();
        assert!(
            store(
                &fixture.cache,
                &fixture.identity,
                &fixture.staging,
                &cancellation
            )
            .unwrap_err()
            .downcast_ref::<Interrupted>()
            .is_some()
        );
        assert!(
            restore(&fixture.cache, &fixture.identity, &target, &cancellation)
                .unwrap_err()
                .downcast_ref::<Interrupted>()
                .is_some()
        );
        assert_eq!(fs::read(bundle.join(RECEIPT)).unwrap(), receipt_before);
    }

    #[test]
    fn database_cache_preserves_foreign_directory_at_an_allowlisted_payload_name() {
        let fixture = fixture();
        let cancellation = Cancellation::new();
        store(
            &fixture.cache,
            &fixture.identity,
            &fixture.staging,
            &cancellation,
        )
        .unwrap();
        let foreign = fixture
            .cache
            .join(&fixture.identity.key)
            .join("skyrim_world.db");
        fs::remove_file(&foreign).unwrap();
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("foreign-notes"), b"preserve").unwrap();
        let target = destination(&fixture, "foreign-directory");
        assert!(
            restore(&fixture.cache, &fixture.identity, &target, &cancellation)
                .unwrap()
                .is_none()
        );
        assert!(
            store(
                &fixture.cache,
                &fixture.identity,
                &fixture.staging,
                &cancellation
            )
            .is_err()
        );
        assert_eq!(
            fs::read(foreign.join("foreign-notes")).unwrap(),
            b"preserve"
        );
    }

    #[test]
    fn database_cache_retention_removes_only_obsolete_owned_bundles() {
        let fixture = fixture();
        let cancellation = Cancellation::new();
        store(
            &fixture.cache,
            &fixture.identity,
            &fixture.staging,
            &cancellation,
        )
        .unwrap();
        let foreign = fixture.cache.join("unrelated");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("notes"), b"keep").unwrap();
        let mut next = fixture.identity.clone();
        next.inputs["test_identity_change"] = serde_json::json!(true);
        next.key = hash_bytes(&serde_json::to_vec(&next.inputs).unwrap());
        store(&fixture.cache, &next, &fixture.staging, &cancellation).unwrap();
        assert!(!fixture.cache.join(&fixture.identity.key).exists());
        assert!(fixture.cache.join(&next.key).is_dir());
        assert_eq!(fs::read(foreign.join("notes")).unwrap(), b"keep");
    }
}
