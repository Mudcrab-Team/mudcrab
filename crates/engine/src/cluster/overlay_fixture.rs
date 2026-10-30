//! Test support: base + overlay fixtures for the cluster readers.
//!
//! The converter's stage and finalizers still write one folder (core tables, cluster tables, payloads
//! and both manifests together). [`split_full_copy`] turns that folder into the derived-overlay layout the engine
//! reads: the folder stays the base without any cluster table, extension entry or payload, and the
//! cluster side moves to `derived/clusters-<settings-id>` with an `overlay.json` bound to the base.
//! The recorded build settings are the producer's own (its builder identity and the threshold in
//! its `cluster_build` row) plus a synthetic converter version, and the folder id is their hash, so
//! the fixture satisfies the same metadata checks as a derived overlay. Rows,
//! columns and owned files are moved untouched, so the certified extension hashes stay valid. The
//! engine reads only the split layout; no reader accepts the single-folder form.

use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::Connection;
use serde_json::Value;
use shared::{
    cluster_overlay::{
        OVERLAY_DATABASE_FILE, OVERLAY_KIND, OVERLAY_METADATA_FILE, OVERLAY_SCHEMA_VERSION,
        OverlayMetadata, ResolvedBuildSettings, base_core_identity, base_fingerprint,
        open_overlay_database, overlay_dir, read_metadata,
    },
    extensions::{CLUSTER_TERRAIN, CLUSTERS, ExtensionEntry},
};

use super::{envelope, reader::CertifiedPackage};

/// `ResolvedBuildSettings::converter_version` of every fixture overlay.
pub(crate) const FIXTURE_CONVERTER_VERSION: &str = "fixture";

/// The fixture overlay folder of `base`: the only `clusters-*` folder under `derived`.
pub(crate) fn overlay_path(base: &Path) -> PathBuf {
    let derived = shared::cluster_overlay::derived_dir(base);
    let mut found: Vec<PathBuf> = fs::read_dir(&derived)
        .unwrap_or_else(|error| panic!("reading {}: {error}", derived.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(shared::cluster_overlay::OVERLAY_DIR_PREFIX))
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected one fixture overlay under {}, found {found:?}",
        derived.display()
    );
    found.remove(0)
}

/// Certifies the fixture overlay of `base`.
pub(crate) fn open_fixture(base: &Path, terrain: bool) -> color_eyre::Result<CertifiedPackage> {
    CertifiedPackage::open(base, &overlay_path(base), terrain)
}

/// The bound, read-only view: the base with the overlay attached as `clusters`.
pub(crate) fn read_db(base: &Path) -> Connection {
    open_overlay_database(base, &overlay_path(base)).unwrap()
}

/// A writable connection on the overlay's own cluster tables.
pub(crate) fn overlay_db(base: &Path) -> Connection {
    Connection::open(overlay_path(base).join(OVERLAY_DATABASE_FILE)).unwrap()
}

/// A writable connection on the base's core tables.
pub(crate) fn base_db(base: &Path) -> Connection {
    Connection::open(base.join("skyrim_world.db")).unwrap()
}

/// Edits the cluster extension entries recorded in `overlay.json`.
pub(crate) fn rewrite_entries(base: &Path, edit: impl FnOnce(&mut Vec<ExtensionEntry>)) {
    let overlay = overlay_path(base);
    let mut metadata = read_metadata(&overlay).unwrap();
    edit(&mut metadata.extensions);
    write_metadata(&overlay, &metadata);
}

/// Recomputes every recorded extension hash from the overlay's present tables and files, as a
/// producer that legitimately rebuilt the overlay would.
pub(crate) fn rehash_entries(base: &Path) {
    let overlay = overlay_path(base);
    let connection = overlay_db(base);
    rewrite_entries(base, |entries| {
        for entry in entries {
            entry.sha256 = envelope::recompute(&connection, &overlay, entry).unwrap();
        }
    });
}

fn write_metadata(overlay: &Path, metadata: &OverlayMetadata) {
    fs::write(
        overlay.join(OVERLAY_METADATA_FILE),
        serde_json::to_vec_pretty(metadata).unwrap(),
    )
    .unwrap();
}

fn is_cluster_extension(entry: &Value) -> bool {
    matches!(entry["id"].as_str(), Some(CLUSTERS) | Some(CLUSTER_TERRAIN))
}

/// Drops every view and table of `connection` for which `doomed(name)` holds. Virtual tables go
/// first so their shadow tables leave with them.
fn drop_where(connection: &Connection, doomed: impl Fn(&str) -> bool) {
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .unwrap();
    let list = |sql: &str| -> Vec<(String, String)> {
        let mut statement = connection.prepare(sql).unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    for (name, _) in list("SELECT name, sql FROM sqlite_master WHERE type = 'view'") {
        if doomed(&name) {
            connection
                .execute_batch(&format!("DROP VIEW \"{name}\""))
                .unwrap();
        }
    }
    for (name, sql) in list("SELECT name, sql FROM sqlite_master WHERE type = 'table'") {
        if sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE") && doomed(&name) {
            connection
                .execute_batch(&format!("DROP TABLE \"{name}\""))
                .unwrap();
        }
    }
    for (name, _) in list("SELECT name, sql FROM sqlite_master WHERE type = 'table'") {
        if !name.starts_with("sqlite_") && doomed(&name) {
            connection
                .execute_batch(&format!("DROP TABLE \"{name}\""))
                .unwrap();
        }
    }
}

/// Splits a single-folder converter result at `root` into base (`root`) and fixture overlay.
pub(crate) fn split_full_copy(root: &Path) {
    // The producer's own settings, read before anything moves.
    let builder_identity =
        serde_json::from_slice::<Value>(&fs::read(root.join("cluster-manifest.json")).unwrap())
            .unwrap()["builder_identity"]
            .as_str()
            .unwrap()
            .to_owned();
    let triangle_threshold: u32 = Connection::open(root.join("skyrim_world.db"))
        .unwrap()
        .query_row(
            "SELECT triangle_threshold FROM cluster_build WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let terrain = root.join("cluster-terrain-manifest.json").exists();
    let build_settings = ResolvedBuildSettings::new(
        FIXTURE_CONVERTER_VERSION,
        builder_identity.clone(),
        triangle_threshold,
        terrain,
        false,
    );
    build_settings.validate().unwrap();
    let overlay = overlay_dir(root, &build_settings.settings_id());
    fs::create_dir_all(&overlay).unwrap();

    // The manifest keeps every non-cluster extension; the cluster entries move to overlay.json.
    let manifest_path = root.join("conversion-manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let all = manifest["extensions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let (cluster, rest): (Vec<Value>, Vec<Value>) = all.into_iter().partition(is_cluster_extension);
    manifest["extensions"] = Value::Array(rest);
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let extensions: Vec<ExtensionEntry> = cluster
        .into_iter()
        .map(|entry| serde_json::from_value(entry).unwrap())
        .collect();

    // Payloads and manifests move as they are.
    for name in [
        "clusters",
        "cluster-manifest.json",
        "cluster-terrain-manifest.json",
    ] {
        let from = root.join(name);
        if from.exists() {
            fs::rename(from, overlay.join(name)).unwrap();
        }
    }

    // One database becomes two by dropping the other side's tables from a copy of the file, so
    // every kept row and column is the producer's own.
    let clusters_db = overlay.join(OVERLAY_DATABASE_FILE);
    fs::copy(root.join("skyrim_world.db"), &clusters_db).unwrap();
    let side = Connection::open(&clusters_db).unwrap();
    drop_where(&side, |name| !name.starts_with("cluster_"));
    drop(side);
    let core = Connection::open(root.join("skyrim_world.db")).unwrap();
    drop_where(&core, |name| name.starts_with("cluster_"));
    drop(core);

    write_metadata(
        &overlay,
        &OverlayMetadata {
            schema_version: OVERLAY_SCHEMA_VERSION,
            kind: OVERLAY_KIND.into(),
            label: "fixture".into(),
            settings_id: build_settings.settings_id(),
            build_settings,
            terrain_sources: None,
            base_core_identity: base_core_identity(root).unwrap(),
            base_fingerprint: base_fingerprint(root).unwrap(),
            base_failure_count: 0,
            builder_identity,
            created: "2026-10-20T00:00:00Z".into(),
            owner: "engine cluster test fixture; tempfile-owned synthetic overlay".into(),
            extensions,
        },
    );
}
