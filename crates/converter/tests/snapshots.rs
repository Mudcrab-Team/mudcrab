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
//! * [`legacy_world_database_tables_match_snapshot`] - the 11 tables the older
//!   JSON snapshot covered, exported through the legacy reader, so that path
//!   stays visible while the default reader is the in-house one.
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
//! of the changed rows. A missing snapshot fails even under the update command
//! unless `MUDCRAB_SNAPSHOT_CREATE=1` permits creating it.
//!
//! A pull request that changes either world database snapshot must list each
//! changed row, each added or removed table, and each header change, and say
//! why. See `docs/contributing/writing-prs.md`.

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

/// Every object the dump leaves out, with its `pragma_table_list` type and the
/// reason. The snapshot header names them, and the test fails if the discovered
/// set differs, so a new shadow table, view or other object cannot be excluded
/// without a decision. The virtual tables themselves are dumped: their rows are
/// the published spatial index.
const OMITTED_OBJECTS: [(&str, &str, &str); 6] = [
    (
        "exterior_spatial_node",
        "shadow",
        "R-tree node block of exterior_spatial",
    ),
    (
        "exterior_spatial_parent",
        "shadow",
        "R-tree parent block of exterior_spatial",
    ),
    (
        "exterior_spatial_rowid",
        "shadow",
        "R-tree rowid map of exterior_spatial",
    ),
    (
        "lod_chunks_spatial_node",
        "shadow",
        "R-tree node block of lod_chunks_spatial",
    ),
    (
        "lod_chunks_spatial_parent",
        "shadow",
        "R-tree parent block of lod_chunks_spatial",
    ),
    (
        "lod_chunks_spatial_rowid",
        "shadow",
        "R-tree rowid map of lod_chunks_spatial",
    ),
];

/// The tables the older JSON snapshot covered, sorted by name. The legacy
/// reader still publishes them, so its snapshot keeps them reviewable while the
/// in-house reader is the default everywhere else.
const LEGACY_SNAPSHOT_TABLES: [&str; 11] = [
    "actor_model_parts",
    "actor_models",
    "cells",
    "formid_map",
    "land",
    "landscape_textures",
    "npcs",
    "references",
    "statics",
    "texture_sets",
    "worldspaces",
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

/// The objects in the database that the dump leaves out, with their type, sorted
/// by name: R-tree shadow tables and views. SQLite's own `sqlite_%` objects are
/// not published, so they are not counted.
fn omitted_database_objects(connection: &Connection) -> Vec<(String, String)> {
    let mut statement = connection
        .prepare(
            "SELECT name, type FROM pragma_table_list
             WHERE schema = 'main'
               AND name NOT LIKE 'sqlite_%'
               AND type NOT IN ('table', 'virtual')
             ORDER BY name",
        )
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<(String, String)>>>()
        .unwrap()
}

/// Fails when a new shadow table, view or other object would be left out
/// silently: the discovered set must equal the named set.
fn assert_omitted_objects_are_named(connection: &Connection) {
    let mut discovered = omitted_database_objects(connection);
    let mut named: Vec<(String, String)> = OMITTED_OBJECTS
        .iter()
        .map(|(name, kind, _)| ((*name).to_owned(), (*kind).to_owned()))
        .collect();
    discovered.sort();
    named.sort();
    assert_eq!(
        discovered, named,
        "the database's omitted objects changed; name them in OMITTED_OBJECTS and the snapshot \
         header, or include them in the dump"
    );
}

/// One SQLite value as a stable token. NULL is `NULL`, text is a JSON string so
/// a quote or newline cannot break the line, integers are decimal, reals use
/// [`real_token`], and a blob is its SHA-256 prefix and byte length.
fn value_token(value: &SqlValue) -> String {
    match value {
        SqlValue::Null => "NULL".to_owned(),
        SqlValue::Integer(number) => number.to_string(),
        SqlValue::Real(number) => real_token(*number),
        SqlValue::Text(text) => serde_json::to_string(text).unwrap(),
        SqlValue::Blob(bytes) => blob_token(bytes),
    }
}

/// A REAL as a token that always parses back to the same `f64`. Nine decimals
/// read well and are exact for most of this fixture; a value they cannot
/// round-trip falls back to Rust's shortest round-trip form, so a change below
/// the ninth decimal still appears as a row diff.
fn real_token(number: f64) -> String {
    let fixed = format!("{number:.9}");
    match fixed.parse::<f64>() {
        Ok(parsed) if parsed.to_bits() == number.to_bits() => fixed,
        _ => format!("{number:?}"),
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
/// `1, 2, 10` instead of `1, 10, 2`. `f64::total_cmp` is a total order for every
/// float, including `-0.0` and NaN, so two rows that differ only in the sign of
/// zero keep one fixed order. Two numerically equal numbers with different
/// tokens, such as INTEGER `1` and REAL `1.0`, order by token.
fn value_order(left: &SqlValue, right: &SqlValue) -> std::cmp::Ordering {
    use SqlValue::{Blob, Integer, Null, Real, Text};
    use std::cmp::Ordering;

    match (left, right) {
        (Null, Null) => Ordering::Equal,
        (Integer(_) | Real(_), Integer(_) | Real(_)) => {
            number_order(left, right).then_with(|| value_token(left).cmp(&value_token(right)))
        }
        (Text(a), Text(b)) => a.cmp(b),
        (Blob(a), Blob(b)) => a.cmp(b),
        _ => value_rank(left).cmp(&value_rank(right)),
    }
}

/// Numbers compare as floats, with INTEGER values widened. `total_cmp` orders
/// `-0.0` before `0.0` and keeps NaN deterministic instead of reporting two
/// different values as equal.
fn number_order(left: &SqlValue, right: &SqlValue) -> std::cmp::Ordering {
    use SqlValue::{Integer, Real};

    match (left, right) {
        (Integer(a), Integer(b)) => a.cmp(b),
        (Real(a), Real(b)) => a.total_cmp(b),
        (Integer(a), Real(b)) => (*a as f64).total_cmp(b),
        (Real(a), Integer(b)) => a.total_cmp(&(*b as f64)),
        _ => std::cmp::Ordering::Equal,
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
fn path_token(label: &str, path: &str, fixture_roots: &[String]) -> String {
    let path = normalize_path(path);
    let path = fixture_roots
        .iter()
        .find_map(|root| {
            path.strip_prefix(root)
                .map(|rest| format!("<fixture>{rest}"))
        })
        .unwrap_or(path);
    assert_relative_path(label, &path);
    serde_json::to_string(&path).unwrap()
}

/// Panics if a text value is an absolute path: a Unix path, a UNC path, or a
/// drive-qualified Windows path. The fixture writes relative paths and paths
/// under its own temporary directory only, so an absolute one means a
/// normalization rule is missing.
fn assert_relative_path(label: &str, path: &str) {
    let normalized = normalize_path(path);
    assert!(
        !normalized.starts_with('/') && !normalized.contains(":/"),
        "{label} carries an absolute path: {path:?}"
    );
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
            path_token(&format!("{table}.{column}"), path, fixture_roots)
        }
        ("conversion_cache", "last_converted", _) => "<timestamp>".to_owned(),
        (_, _, SqlValue::Text(text)) => {
            assert_relative_path(&format!("{table}.{column}"), text);
            serde_json::to_string(text).unwrap()
        }
        (_, _, value) => value_token(value),
    }
}

/// One table as a header line and one sorted line per row:
/// `table|column=value|...`. Sorting the typed rows removes SQLite row order
/// and the `HashMap` iteration that fills many tables as sources of churn.
fn dump_table(
    snapshot: &mut String,
    connection: &Connection,
    table: &str,
    fixture_roots: &[String],
) {
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
        let mut line = table.to_owned();
        for (column, value) in columns.iter().zip(&row) {
            line.push('|');
            line.push_str(column);
            line.push('=');
            line.push_str(&cell_token(table, column, value, fixture_roots));
        }
        snapshot.push_str(&line);
        snapshot.push('\n');
    }
}

/// Every table with one sorted line per row: `table|column=value|...`. The
/// header names every normalized column and every omitted object, and the test
/// fails if the omitted set changes.
fn world_database_dump(connection: &Connection, fixture: &Path) -> String {
    assert_omitted_objects_are_named(connection);
    let fixture_roots = fixture_roots(fixture);
    let mut snapshot = String::new();
    snapshot.push_str(
        "# Rows of a world database built from dummy-content's preset interior\n\
         # plugin (one exterior cell, one interior cell and their load-door pair)\n\
         # through the default in-house reader. Every table is listed, including\n\
         # empty ones. One line per row: table|column=value|column=value.\n\
         # Rows are sorted by column values before formatting, so SQLite row order\n\
         # and HashMap iteration cannot churn. NULL is NULL, text is a JSON string,\n\
         # a REAL shows nine decimals when that token parses back to the same\n\
         # value and Rust's round-trip form otherwise, and a BLOB is\n\
         # blob:<sha256 prefix>:<length>.\n\
         # Normalized columns, because the machine or clock sets them:\n",
    );
    for (table, column, reason) in NORMALIZED_COLUMNS {
        writeln!(snapshot, "#   {table}.{column}: {reason}").unwrap();
    }
    snapshot.push_str("# Omitted objects, named so no exclusion is silent:\n");
    for (name, kind, reason) in OMITTED_OBJECTS {
        writeln!(snapshot, "#   {name} ({kind}): {reason}").unwrap();
    }
    snapshot.push_str(
        "# The virtual tables exterior_spatial and lod_chunks_spatial are included:\n\
         # their rows are the published spatial index. The test fails if the omitted\n\
         # set differs from this list.\n",
    );

    for table in database_tables(connection) {
        dump_table(&mut snapshot, connection, &table, &fixture_roots);
    }
    snapshot
}

/// The 11 tables the older JSON snapshot covered, exported through the legacy
/// reader, so that path keeps a readable row record.
fn legacy_world_database_dump(connection: &Connection, fixture: &Path) -> String {
    let fixture_roots = fixture_roots(fixture);
    let mut snapshot = String::new();
    snapshot.push_str(
        "# Rows of the 11 tables the older JSON snapshot covered, built from the\n\
         # same preset plugin through the legacy reader (convert_plugins). The dump\n\
         # above covers every table through the default in-house reader. One line\n\
         # per row, in the same format.\n",
    );
    for table in LEGACY_SNAPSHOT_TABLES {
        dump_table(&mut snapshot, connection, table, &fixture_roots);
    }
    snapshot
}

/// Fails a dump that carries the fixture's temporary directory. Text cells are
/// checked for absolute paths as they are written; this is the backstop.
fn assert_no_machine_paths_in_dump(snapshot: &str, fixture: &Path) {
    for root in fixture_roots(fixture) {
        assert!(
            !snapshot.contains(&root),
            "snapshot carries the fixture directory: {}",
            snapshot
                .lines()
                .find(|line| line.contains(&root))
                .unwrap_or_default()
        );
    }
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
                    !normalized.starts_with('/') && !normalized.contains(":/"),
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

/// Builds the fixture database through `reader` and returns the directory that
/// owns it with an open connection.
fn fixture_database(reader: converter::config::RecordReader) -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let plugin_path = write_preset_plugin(directory.path());
    let database = directory.path().join("skyrim_world.db");
    converter::EsmParser::convert_plugins_with_reader(
        std::slice::from_ref(&plugin_path),
        &database,
        reader,
        directory.path(),
    )
    .unwrap();
    let connection = Connection::open(&database).unwrap();
    (directory, connection)
}

/// The committed snapshot for `name`, under the test's own snapshot directory.
fn snapshot_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("snapshots__{name}.snap"))
}

/// A missing snapshot fails even under `INSTA_UPDATE=always`, because that
/// command would otherwise recreate a deleted file and pass with no review.
/// `MUDCRAB_SNAPSHOT_CREATE=1` is the explicit permission to create it.
fn assert_snapshot_exists(name: &str) {
    let path = snapshot_path(name);
    assert!(
        path.exists() || std::env::var_os("MUDCRAB_SNAPSHOT_CREATE").is_some(),
        "{} is missing; restore it from Git. To create it on purpose, set \
         MUDCRAB_SNAPSHOT_CREATE=1 as well as INSTA_UPDATE=always.",
        path.display()
    );
}

/// Two rows that differ only in the sign of zero must not depend on input
/// order, because SQLite row order and `HashMap` iteration are not stable.
#[test]
fn reals_order_totally_across_zero_signs() {
    let negative = SqlValue::Real(-0.0);
    let positive = SqlValue::Real(0.0);
    assert!(value_order(&negative, &positive).is_lt());
    assert!(value_order(&positive, &negative).is_gt());

    let mut forward = [positive.clone(), negative.clone(), positive, negative];
    let mut backward = forward.clone();
    backward.reverse();
    forward.sort_by(value_order);
    backward.sort_by(value_order);
    assert_eq!(
        forward.map(|value| value_token(&value)),
        backward.map(|value| value_token(&value))
    );
}

/// A new view must fail the omitted-set check instead of leaving the dump
/// without a note.
#[test]
#[should_panic(expected = "the database's omitted objects changed")]
fn a_new_view_fails_the_omitted_set_check() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE VIRTUAL TABLE exterior_spatial USING rtree(
                 id, min_x, max_x, min_y, max_y
             );
             CREATE VIRTUAL TABLE lod_chunks_spatial USING rtree(
                 id, min_x, max_x, min_y, max_y
             );
             CREATE VIEW unexpected_view AS SELECT 1;",
        )
        .unwrap();
    assert_omitted_objects_are_named(&connection);
}

/// A REAL token must always parse back to the same value, or a change below the
/// printed precision would not appear as a diff.
#[test]
fn real_tokens_round_trip() {
    let values = [
        0.0,
        -0.0,
        1.0,
        -1.5,
        1.1_f32 as f64,
        1.0 / 3.0,
        2048.0,
        1e-10,
        1e-12,
        f64::MIN_POSITIVE,
        f64::MAX,
    ];
    for number in values {
        let token = real_token(number);
        let parsed: f64 = token
            .parse()
            .unwrap_or_else(|error| panic!("{number:?} printed {token:?}: {error}"));
        assert_eq!(
            parsed.to_bits(),
            number.to_bits(),
            "{number:?} printed {token:?}, which parses to {parsed:?}"
        );
    }
    assert_ne!(
        real_token(0.000_000_000_1),
        real_token(0.000_000_000_2),
        "two REALs below the ninth decimal must not share a token"
    );
}

#[test]
fn world_database_tables_match_snapshot() {
    let (directory, connection) = fixture_database(converter::config::RecordReader::Inhouse);
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
    // publishes. A temporary directory or an absolute path here means a
    // normalization rule is missing.
    assert_no_machine_paths_in_dump(&snapshot, directory.path());

    assert_snapshot_exists("world_database_tables_match_snapshot");
    assert_snapshot!(snapshot);
}

#[test]
fn legacy_world_database_tables_match_snapshot() {
    let (directory, connection) = fixture_database(converter::config::RecordReader::Legacy);
    let snapshot = legacy_world_database_dump(&connection, directory.path());

    assert_no_machine_paths_in_dump(&snapshot, directory.path());

    assert_snapshot_exists("legacy_world_database_tables_match_snapshot");
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
