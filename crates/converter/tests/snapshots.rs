//! Snapshot tests for the converter's own output.
//!
//! Every value here comes from the deterministic `dummy-content` fixtures
//! through the real conversion code, so a change to what the converter produces
//! shows up as a readable diff in review instead of as a wrong render later:
//!
//! * [`converted_static_model_gltf_matches_snapshot`] - the glTF JSON chunk of
//!   a converted static model: node tree, mesh primitives and their attributes,
//!   the material with its extras, and accessor bounds. The binary chunk is
//!   deliberately not part of the snapshot.
//! * [`world_database_tables_match_snapshot`] - every row of every table in the
//!   exported `skyrim_world.db`, one sorted line per row, so a published value
//!   change appears as a row diff instead of as a wrong render later.
//! * [`conversion_manifest_matches_snapshot`] - the published
//!   `conversion-manifest.json`: schema version, completeness, input counts and
//!   one entry per converted asset.
//! * [`pipeline_report_matches_snapshot`] - the report the pipeline returns:
//!   counts, artifacts, completeness and the integration gate.
//!
//! To accept an intentional change run `cargo insta review` and approve it, or
//! read the `*.snap.new` file the failing run writes into
//! `crates/converter/tests/snapshots/` and rename it to `*.snap` once it holds
//! what the converter is meant to produce. The failing output is a unified diff
//! of the changed rows.
//!
//! A pull request that changes
//! `snapshots__world_database_tables_match_snapshot.snap` must list each changed
//! row and the reason in its body. See `docs/contributing/writing-prs.md`.

mod common;

use common::cpu_lod_config;
use dummy_content::{esm, layout};
use insta::{assert_json_snapshot, assert_snapshot};
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

/// The bytes `dummy-content gen --with-interior` writes: one exterior cell,
/// its auto-load door into one interior cell, and the return door. The spec is
/// assembled from the same `layout` constants the command uses, so this file
/// cannot drift from the plugin the command publishes.
fn preset_plugin() -> Vec<u8> {
    let cells = [esm::PRESET_EXTERIOR_CELL];
    esm::plugin_with_interior(
        &esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &cells,
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        },
        &esm::PRESET_INTERIOR,
    )
    .unwrap()
}

/// Writes the plugin [`preset_plugin`] describes into a fresh `Skyrim.esm`.
fn write_preset_plugin(directory: &Path) -> PathBuf {
    let path = directory.join("Skyrim.esm");
    fs::write(&path, preset_plugin()).unwrap();
    path
}

/// The glTF JSON chunk of a GLB, without its binary chunk: the same document
/// the engine reads for nodes, meshes, materials and bounds.
fn glb_json(path: &Path) -> Value {
    let bytes = fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"glTF", "{} is not a GLB", path.display());
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize,
        bytes.len(),
        "GLB length is inconsistent"
    );
    let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    assert_eq!(&bytes[16..20], b"JSON");
    let json = serde_json::from_slice(&bytes[20..20 + json_length]).unwrap();
    assert!(
        bytes.len() > 20 + json_length,
        "the snapshot is meant to cover the JSON chunk of a GLB with geometry"
    );
    json
}

/// Columns the world database dump normalizes before comparison, with the
/// reason. The machine and the wall clock set them, not the converter. The
/// snapshot header names them, so no exclusion is silent, and the snapshot test
/// fails if one leaves the schema.
const NORMALIZED_COLUMNS: [(&str, &str, &str); 3] = [
    (
        "conversion_cache",
        "plugin_path",
        "the fixture's temporary directory, which differs between runs and machines",
    ),
    (
        "conversion_cache",
        "last_converted",
        "the wall-clock time when the cache row was written",
    ),
    (
        "record_overrides",
        "source_path",
        "the plugin path in the fixture's temporary directory",
    ),
];

/// Every table the converter publishes, sorted by name. SQLite's internal
/// tables are skipped. Shadow tables are skipped too: they are the R-tree blocks
/// behind a virtual table, not rows the converter writes. The virtual table's
/// own rows stay, because they are the published spatial index.
fn database_tables(connection: &Connection) -> Vec<String> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM pragma_table_list
             WHERE schema = 'main'
               AND type IN ('table', 'virtual')
               AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap()
}

/// One SQLite value as a stable token. NULL is `NULL`, text is a JSON string so
/// a quote or newline cannot break the line, integers are decimal, reals use
/// nine fixed decimals (enough for an `f32` in a `REAL` column), and a blob is
/// its SHA-256 prefix and byte length.
fn value_token(value: &SqlValue) -> String {
    match value {
        SqlValue::Null => "NULL".to_owned(),
        SqlValue::Integer(number) => number.to_string(),
        SqlValue::Real(number) => format!("{number:.9}"),
        SqlValue::Text(text) => serde_json::to_string(text).unwrap(),
        SqlValue::Blob(bytes) => blob_token(bytes),
    }
}

fn blob_token(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hash = String::with_capacity(16);
    for byte in &digest[..8] {
        write!(hash, "{byte:02x}").unwrap();
    }
    format!("blob:{hash}:{}", bytes.len())
}

/// Values sort by kind (NULL, number, text, blob) and then by value. Numbers
/// compare numerically across INTEGER and REAL, so a FormID column reads
/// `1, 2, 10` instead of `1, 10, 2`. This is a total order for every value
/// SQLite stores here.
fn value_order(left: &SqlValue, right: &SqlValue) -> std::cmp::Ordering {
    use SqlValue::{Blob, Integer, Null, Real, Text};
    use std::cmp::Ordering;

    match (left, right) {
        (Null, Null) => Ordering::Equal,
        (Integer(a), Integer(b)) => a.cmp(b),
        (Real(a), Real(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (Integer(a), Real(b)) => (*a as f64).partial_cmp(b).unwrap_or(Ordering::Equal),
        (Real(a), Integer(b)) => a.partial_cmp(&(*b as f64)).unwrap_or(Ordering::Equal),
        (Text(a), Text(b)) => a.cmp(b),
        (Blob(a), Blob(b)) => a.cmp(b),
        _ => value_rank(left).cmp(&value_rank(right)),
    }
}

fn value_rank(value: &SqlValue) -> u8 {
    match value {
        SqlValue::Null => 0,
        SqlValue::Integer(_) | SqlValue::Real(_) => 1,
        SqlValue::Text(_) => 2,
        SqlValue::Blob(_) => 3,
    }
}

/// A path with the fixture's temporary directory replaced by `<fixture>`, so a
/// run on another machine or in another temporary directory produces the same
/// token. macOS returns `/var/...` from `tempdir` while the converter may
/// canonicalize it to `/private/var/...`. Windows canonical paths carry a
/// verbatim prefix. Both forms are accepted, and forward slashes make Windows
/// and Unix paths read the same way.
fn path_token(path: &str, fixture_roots: &[String]) -> String {
    let path = normalize_path(path);
    let path = fixture_roots
        .iter()
        .find_map(|root| {
            path.strip_prefix(root)
                .map(|rest| format!("<fixture>{rest}"))
        })
        .unwrap_or(path);
    serde_json::to_string(&path).unwrap()
}

fn normalize_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    match path.strip_prefix("//?/") {
        Some(path) => path.to_owned(),
        None => path,
    }
}

/// The temporary directory and its canonical form, without duplicates.
fn fixture_roots(fixture: &Path) -> Vec<String> {
    let mut roots = vec![normalize_path(&fixture.to_string_lossy())];
    if let Ok(canonical) = fixture.canonicalize() {
        let canonical = normalize_path(&canonical.to_string_lossy());
        if !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }
    roots
}

/// Format one cell, replacing the values the machine owns. `fixture_roots` are
/// normalized to forward slashes, like the paths they replace.
fn cell_token(table: &str, column: &str, value: &SqlValue, fixture_roots: &[String]) -> String {
    match (table, column, value) {
        ("conversion_cache", "plugin_path", SqlValue::Text(path))
        | ("record_overrides", "source_path", SqlValue::Text(path)) => {
            path_token(path, fixture_roots)
        }
        ("conversion_cache", "last_converted", _) => "<timestamp>".to_owned(),
        (_, _, value) => value_token(value),
    }
}

/// Every table with one sorted line per row: `table|column=value|...`. Sorting
/// the typed rows removes SQLite row order and the `HashMap` iteration that
/// fills many tables as sources of churn.
fn world_database_dump(connection: &Connection, fixture: &Path) -> String {
    let fixture_roots = fixture_roots(fixture);
    let mut snapshot = String::new();
    snapshot.push_str(
        "# Rows of a world database built from dummy-content's preset interior\n\
         # plugin (one exterior cell, one interior cell and their load-door pair)\n\
         # through the default in-house reader. Every table is listed, including\n\
         # empty ones. One line per row: table|column=value|column=value.\n\
         # Rows are sorted by column values before formatting, so SQLite row order\n\
         # and HashMap iteration cannot churn. NULL is NULL, text is a JSON string,\n\
         # REAL has nine fixed decimals, and a BLOB is blob:<sha256 prefix>:<length>.\n\
         # Normalized columns, because the machine or clock sets them:\n",
    );
    for (table, column, reason) in NORMALIZED_COLUMNS {
        writeln!(snapshot, "#   {table}.{column}: {reason}").unwrap();
    }
    snapshot.push_str(
        "# Omitted tables: the shadow tables of a virtual table hold SQLite's\n\
         # R-tree blocks; the virtual tables (exterior_spatial,\n\
         # lod_chunks_spatial) are included.\n",
    );

    for table in database_tables(connection) {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{table}\""))
            .unwrap();
        let columns: Vec<String> = statement.column_names().map(str::to_owned).collect();
        let mut rows: Vec<Vec<SqlValue>> = statement
            .query_map([], |row| {
                (0..columns.len())
                    .map(|index| row.get::<_, SqlValue>(index))
                    .collect()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows.sort_by(|left, right| {
            left.iter()
                .zip(right)
                .map(|(left, right)| value_order(left, right))
                .find(|order| !order.is_eq())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        writeln!(snapshot, "## {table} ({} rows)", rows.len()).unwrap();
        for row in rows {
            let mut line = table.clone();
            for (column, value) in columns.iter().zip(&row) {
                line.push('|');
                line.push_str(column);
                line.push('=');
                line.push_str(&cell_token(&table, column, value, &fixture_roots));
            }
            snapshot.push_str(&line);
            snapshot.push('\n');
        }
    }
    snapshot
}

/// Fails any snapshot that carries a machine-specific path, so none can be
/// accepted with one in it. `root` is the directory the fixture ran in.
fn assert_no_machine_paths(value: &Value, root: &Path) {
    let root = root.to_string_lossy().replace('\\', "/");
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            Value::String(text) => {
                let normalized = text.replace('\\', "/");
                assert!(
                    !normalized.contains(&root),
                    "snapshot text {text:?} carries the fixture directory"
                );
                assert!(
                    !normalized.contains(":/"),
                    "snapshot text {text:?} looks like an absolute path"
                );
            }
            Value::Array(items) => stack.extend(items),
            Value::Object(entries) => stack.extend(entries.values()),
            _ => {}
        }
    }
}

/// Runs the generated `Data/` tree through the real pipeline and hands back the
/// directory that owns it, the output directory and the report.
async fn convert_generated_data() -> (tempfile::TempDir, PathBuf, converter::PipelineReport) {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(&data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();

    let output = directory.path().join("modern");
    let config = cpu_lod_config(&data, &output);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = converter::AssetPipeline::run_async(config, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    (directory, output, report)
}

#[tokio::test]
async fn converted_static_model_gltf_matches_snapshot() {
    let (directory, output, _) = convert_generated_data().await;
    let document = glb_json(&output.join("meshes/generated.glb"));

    assert_no_machine_paths(&document, directory.path());
    // The model the pipeline publishes, built from the fixture alone: no field
    // here differs between two runs or two machines, so nothing is redacted.
    // `OPEN_SKYRIM_material` is absent because the material stage writes it only
    // for a material with an extra texture slot or an alpha flag, and the
    // fixture's quad has neither (`crates/converter/src/material.rs`).
    assert_json_snapshot!(document);
}

#[test]
fn world_database_tables_match_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let plugin_path = write_preset_plugin(directory.path());
    let database = directory.path().join("skyrim_world.db");
    converter::EsmParser::convert_plugins_with_reader(
        std::slice::from_ref(&plugin_path),
        &database,
        converter::config::RecordReader::Inhouse,
        directory.path(),
    )
    .unwrap();

    let connection = Connection::open(&database).unwrap();
    // A schema rename must not turn a normalization rule into a silent no-op.
    for (table, column, _) in NORMALIZED_COLUMNS {
        connection
            .prepare(&format!("SELECT {column} FROM \"{table}\" LIMIT 0"))
            .unwrap_or_else(|error| {
                panic!("normalized column {table}.{column} is missing: {error}")
            });
    }
    let snapshot = world_database_dump(&connection, directory.path());

    // The machine that runs the test is not part of what the converter
    // publishes. A temporary directory or a drive-qualified path here means a
    // normalization rule is missing.
    for root in fixture_roots(directory.path()) {
        assert!(
            !snapshot.contains(&root),
            "snapshot carries the fixture directory: {}",
            snapshot
                .lines()
                .find(|line| line.contains(&root))
                .unwrap_or_default()
        );
    }
    assert!(
        !snapshot.contains(":/"),
        "snapshot carries an absolute path"
    );

    assert_snapshot!(snapshot);
}

#[tokio::test]
async fn conversion_manifest_matches_snapshot() {
    let (directory, output, _) = convert_generated_data().await;
    let manifest: Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();

    assert_no_machine_paths(&manifest, directory.path());
    assert_json_snapshot!(manifest, {
        // Content digests carry nothing a reviewer can read, and a texture's
        // digest depends on the basis-universal encoder rather than on the
        // converter; entry keys, output paths and sizes are the reviewable
        // part. The converted meshes and the database have their own snapshots
        // above.
        ".**.source_hash" => "[source_hash]",
        ".**.output_hash" => "[output_hash]",
        ".**.hash" => "[hash]",
    });
}

#[tokio::test]
async fn pipeline_report_matches_snapshot() {
    let (directory, _, report) = convert_generated_data().await;
    let database_cache_key = report
        .database_cache_key
        .as_deref()
        .expect("a fresh database bundle has a cache identity");
    assert_eq!(database_cache_key.len(), 64);
    assert!(
        database_cache_key
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    let mut report = serde_json::to_value(&report).unwrap();
    // Normalize duration values explicitly: an insta map wildcard also
    // redacts keys, which would hide changes to the measured phase names.
    for field in ["phase_elapsed_ms", "publication_phase_elapsed_ms"] {
        for elapsed in report[field].as_object_mut().unwrap().values_mut() {
            assert!(elapsed.is_u64());
            *elapsed = Value::from("[elapsed_ms]");
        }
    }

    assert_no_machine_paths(&report, directory.path());
    assert_json_snapshot!(report, {
        // Wall-clock time.
        ".elapsed_ms" => "[elapsed_ms]",
        ".lod_elapsed_ms" => "[lod_elapsed_ms]",
        ".publication_elapsed_ms" => "[publication_elapsed_ms]",
        ".archive_timings.*.source_hash_seconds" => "[seconds]",
        ".archive_timings.*.extraction_seconds" => "[seconds]",
        ".archive_timings.*.cache_restore_seconds" => "[seconds]",
        ".archive_timings.*.cache_link_seconds" => "[seconds]",
        ".archive_timings.*.sync_seconds" => "[seconds]",
        ".archive_timings.*.sync_worker_seconds" => "[seconds]",
        ".archive_timings.*.checkpoint_sync_worker_seconds" => "[seconds]",
        // The identity includes the executable and platform. Keep its shape
        // checked above while retaining the cache outcome and work counters.
        ".database_cache_key" => "[database_cache_key]",
        // Artifacts are appended in the order the parallel conversions finish,
        // which is not stable between runs; sorting keeps the same set of
        // published files readable.
        ".artifacts" => insta::sorted_redaction(),
    });
}
