//! Derive `grass_cache.rkyv` into an APFS clone of a registered pack, without a conversion.
//!
//! The grass bake reads only data that the pack already holds: its world database
//! (`skyrim_world.db`), its preserved land cache (`cell_cache_preserved.rkyv`) and its GLB
//! models. This command reads nothing from the Skyrim game data. It writes two new files into
//! the clone, `grass_cache.rkyv` and `grass-overlay-receipt.json`, and nothing else.
//!
//! The command fails closed. It refuses a target that is a registered pack, that is inside or
//! contains one, that is not an exact copy of the named registered pack's manifests, or whose
//! database or land cache cannot feed the bake. Every refusal names its reason.

use crate::{
    cache::{CONVERTER_SCHEMA_VERSION, ConversionManifest, hash_file},
    esm::{
        cell_cache::validate_cell_cache,
        grass_cache::{GrassBakeReport, validate_grass_cache, write_grass_cache},
    },
};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rkyv::rancor::Error as RkyvError;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

/// Name of the grass cache that the engine reads from the asset directory.
pub const GRASS_CACHE_FILE: &str = "grass_cache.rkyv";
/// Name of the receipt written beside the cache.
pub const RECEIPT_FILE: &str = "grass-overlay-receipt.json";
/// Receipt format name, so a reader can recognise the file.
pub const RECEIPT_FORMAT: &str = "grass-overlay-receipt/1";

/// Inputs of `derive-grass`.
#[derive(Debug, Clone)]
pub struct DeriveGrassOptions {
    /// The APFS clone that receives the overlay.
    pub target: PathBuf,
    /// `packs.json` of the shared storage. The command only reads it.
    pub registry: PathBuf,
    /// Registered pack that `target` must be a clone of.
    pub clone_of: String,
    /// Replace an existing `grass_cache.rkyv` and receipt in the target.
    pub replace: bool,
    /// Git head of the converter build, recorded in the receipt. The caller supplies it.
    pub converter_head: Option<String>,
    /// The command line as the caller ran it, recorded in the receipt.
    pub command: Vec<String>,
}

/// What a successful run produced.
#[derive(Debug)]
pub struct DeriveGrassReport {
    pub cache: PathBuf,
    pub receipt: PathBuf,
    pub bytes_added: u64,
    pub elapsed_ms: u128,
    pub bake: GrassBakeReport,
}

#[derive(Debug, Deserialize)]
struct RegisteredPack {
    path: PathBuf,
    #[serde(default)]
    manifests: BTreeMap<String, String>,
}

/// Tables and columns the grass bake reads from the world database. Legacy optional columns
/// (`references.header_flags` and the enable-parent columns, `landscape_texture_grasses.ordinal`)
/// are not listed because the bake has a fallback for each.
const REQUIRED_COLUMNS: &[(&str, &[&str])] = &[
    (
        "grass_types",
        &[
            "id",
            "model_path",
            "density",
            "min_slope",
            "max_slope",
            "units_from_water",
            "water_comparison",
            "position_range",
            "height_range",
            "color_range",
            "wave_period",
            "flags",
        ],
    ),
    (
        "cells",
        &["id", "worldspace_id", "grid_x", "grid_y", "flags"],
    ),
    (
        "references",
        &[
            "id",
            "base_form_id",
            "worldspace_id",
            "cell_id",
            "is_exterior",
            "pos_x",
            "pos_y",
            "pos_z",
            "rot_x",
            "rot_y",
            "rot_z",
            "scale",
        ],
    ),
    (
        "statics",
        &[
            "id",
            "model_path",
            "flags",
            "bounds_valid",
            "bounds_min_x",
            "bounds_min_y",
            "bounds_min_z",
            "bounds_max_x",
            "bounds_max_y",
            "bounds_max_z",
        ],
    ),
    ("records", &["form_id", "record_type", "cell_id", "data"]),
    ("landscape_texture_grasses", &["ltex_id", "gras_id"]),
];

/// Derive the grass cache into `options.target`. See the module documentation for the rules.
pub fn run(options: &DeriveGrassOptions) -> Result<DeriveGrassReport> {
    let started = Instant::now();
    let pack = check_target(options)?;
    let database = options.target.join("skyrim_world.db");
    let terrain = options.target.join("cell_cache_preserved.rkyv");
    let connection = open_database_read_only(&database)?;
    let land = check_world_inputs(&connection, &terrain)?;

    let cache = options.target.join(GRASS_CACHE_FILE);
    let receipt = options.target.join(RECEIPT_FILE);
    let protected = protected_files(&options.target)?;
    let before: BTreeSet<_> = top_level_names(&options.target)?;
    let bake = write_grass_cache(&connection, &options.target, &terrain, None, &cache)
        .wrap_err("grass bake failed; the clone has no new grass cache that can be trusted")?;
    // A partial cache would pass for a complete one at the next engine start. Remove it when
    // anything after the bake goes wrong.
    let finish = verify_after_bake(&options.target, &cache, &before, &protected);
    let (cache_bytes, cache_facts) = match finish {
        Ok(done) => done,
        Err(error) => {
            let _ = fs::remove_file(&cache);
            return Err(error);
        }
    };

    let elapsed_ms = started.elapsed().as_millis();
    let document = receipt_document(
        options,
        &pack,
        &land,
        &bake,
        cache_bytes,
        cache_facts,
        &cache,
        elapsed_ms,
    )?;
    let text = serde_json::to_string_pretty(&document)? + "\n";
    if let Err(error) = fs::write(&receipt, &text) {
        let _ = fs::remove_file(&cache);
        return Err(error).wrap_err("failed to write the overlay receipt");
    }
    Ok(DeriveGrassReport {
        cache,
        receipt,
        bytes_added: cache_bytes + text.len() as u64,
        elapsed_ms,
        bake,
    })
}

/// Check the clone after the bake: the protected files are unchanged, `grass_cache.rkyv` is the
/// only new top-level entry, and the new cache validates. Returns its size and a summary.
fn verify_after_bake(
    target: &Path,
    cache: &Path,
    before: &BTreeSet<String>,
    protected: &[(PathBuf, u64, std::time::SystemTime)],
) -> Result<(u64, serde_json::Value)> {
    ensure_unchanged(protected)?;
    let after = top_level_names(target)?;
    let added: BTreeSet<_> = after.difference(before).cloned().collect();
    let allowed: BTreeSet<_> = [GRASS_CACHE_FILE.to_string()].into();
    ensure!(
        added.is_subset(&allowed),
        "the bake changed the clone beyond {GRASS_CACHE_FILE}: new entries {added:?}"
    );
    let map = validate_grass_cache(cache).wrap_err("the new grass cache is invalid")?;
    let archived = rkyv::access::<shared::grass::ArchivedGrassCache, RkyvError>(&map)?;
    let facts = serde_json::json!({
        "version": archived.version.to_native(),
        "types": archived.types.len(),
        "cells": archived.cells.len(),
    });
    Ok((fs::metadata(cache)?.len(), facts))
}

/// Facts about the registered pack that the target copies, recorded in the receipt.
struct PackFacts {
    name: String,
    path: PathBuf,
    manifests: BTreeMap<String, String>,
    schema_version: u32,
    nif_configuration_hash: Option<String>,
}

/// Facts about the land and world inputs, recorded in the receipt.
struct LandFacts {
    world_database_schema: u32,
    terrain_cells: usize,
    terrain_cells_in_database: usize,
    grass_types: i64,
    exterior_references: i64,
    land_records: i64,
}

/// Refuse a target that is not a clone of the named registered pack, or that is a registered pack.
fn check_target(options: &DeriveGrassOptions) -> Result<PackFacts> {
    let registry_bytes = fs::read(&options.registry)
        .wrap_err_with(|| format!("cannot read pack registry {}", options.registry.display()))?;
    let registry: BTreeMap<String, RegisteredPack> = serde_json::from_slice(&registry_bytes)
        .wrap_err_with(|| format!("pack registry {} is not valid", options.registry.display()))?;
    let Some(source) = registry.get(&options.clone_of) else {
        bail!(
            "refused: `{}` is not a registered pack in {}",
            options.clone_of,
            options.registry.display()
        );
    };

    let metadata = fs::symlink_metadata(&options.target).wrap_err_with(|| {
        format!(
            "refused: target {} does not exist",
            options.target.display()
        )
    })?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "refused: target {} must be a real directory, not a file or a symlink",
        options.target.display()
    );
    let target = fs::canonicalize(&options.target)?;
    for (name, registered) in &registry {
        // A registered pack that is missing on disk cannot contain the target.
        let Ok(path) = fs::canonicalize(&registered.path) else {
            continue;
        };
        ensure!(
            target != path,
            "refused: target {} is the registered pack `{name}`; derive into an APFS clone, never into a registered pack",
            target.display()
        );
        ensure!(
            !target.starts_with(&path),
            "refused: target {} is inside the registered pack `{name}` ({})",
            target.display(),
            path.display()
        );
        ensure!(
            !path.starts_with(&target),
            "refused: target {} contains the registered pack `{name}` ({})",
            target.display(),
            path.display()
        );
    }

    ensure!(
        !source.manifests.is_empty(),
        "refused: the registry records no manifest hashes for `{}`, so the clone cannot be proven",
        options.clone_of
    );
    for (file, expected) in &source.manifests {
        let candidate = target.join(file);
        ensure!(
            candidate.is_file(),
            "refused: target has no {file}, so it is not a clone of `{}`",
            options.clone_of
        );
        let actual = hash_file(&candidate)?;
        ensure!(
            &actual == expected,
            "refused: target {file} hash {actual} differs from the registered `{}` hash {expected}; the target is not an exact clone",
            options.clone_of
        );
        // A hard link or a symlink back to the registered file would share storage with the pack.
        let original = source.path.join(file);
        if original.is_file() {
            ensure!(
                !same_file::is_same_file(&candidate, &original).unwrap_or(false),
                "refused: target {file} is the same file as the registered pack's; use `cp -c -R` (an APFS clone) to make the target"
            );
        }
    }

    let manifest = ConversionManifest::load(&target.join("conversion-manifest.json"))
        .wrap_err("refused: the target conversion manifest cannot be read")?;
    ensure!(
        manifest.schema_version == CONVERTER_SCHEMA_VERSION,
        "refused: the target manifest has schema {}, this converter writes schema {CONVERTER_SCHEMA_VERSION}",
        manifest.schema_version
    );
    // `load` clears `complete` for a foreign schema, so test the schema first for a clear reason.
    ensure!(
        manifest.complete,
        "refused: the target conversion manifest is not complete"
    );
    for name in ["skyrim_world.db", "cell_cache_preserved.rkyv"] {
        ensure!(
            target.join(name).is_file(),
            "refused: the target has no {name}, which the grass bake needs"
        );
    }
    for name in [GRASS_CACHE_FILE, RECEIPT_FILE] {
        ensure!(
            options.replace || !target.join(name).exists(),
            "refused: the target already has {name}; pass --replace to overwrite it"
        );
    }
    Ok(PackFacts {
        name: options.clone_of.clone(),
        path: source.path.clone(),
        manifests: source.manifests.clone(),
        schema_version: manifest.schema_version,
        nif_configuration_hash: manifest.asset_configuration_hashes.get("nif").cloned(),
    })
}

/// Open the database without any chance to write, or to create `-wal`, `-shm` or journal files.
fn open_database_read_only(path: &Path) -> Result<Connection> {
    let text = path
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("refused: database path is not UTF-8"))?;
    let mut uri = String::from("file:");
    for byte in text.bytes() {
        match byte {
            b'%' | b'?' | b'#' | b' ' => uri.push_str(&format!("%{byte:02X}")),
            _ => uri.push(char::from(byte)),
        }
    }
    uri.push_str("?immutable=1");
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .wrap_err_with(|| format!("refused: cannot open {} read-only", path.display()))
}

/// Refuse a database or land cache that cannot feed the grass bake. Each failure names its cause.
fn check_world_inputs(connection: &Connection, terrain: &Path) -> Result<LandFacts> {
    let world_database_schema: u32 = connection
        .query_row("SELECT version FROM schema_info LIMIT 1", [], |row| {
            row.get(0)
        })
        .wrap_err("refused: the world database has no schema_info version")?;
    ensure!(
        shared::supports_runtime_world_database_schema(world_database_schema),
        "refused: world database schema {world_database_schema} is outside the runtime range {} to {}",
        shared::MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION,
        shared::WORLD_DATABASE_SCHEMA_VERSION
    );
    for (table, columns) in REQUIRED_COLUMNS {
        let mut statement = connection.prepare("SELECT name FROM pragma_table_info(?1)")?;
        let present: BTreeSet<String> = statement
            .query_map([table], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ensure!(
            !present.is_empty(),
            "refused: the world database has no `{table}` table, which the grass bake reads"
        );
        let missing: Vec<_> = columns
            .iter()
            .filter(|column| !present.contains(**column))
            .collect();
        ensure!(
            missing.is_empty(),
            "refused: world database table `{table}` lacks columns {missing:?}, which the grass bake reads"
        );
    }
    let count = |sql: &str| -> Result<i64> { Ok(connection.query_row(sql, [], |row| row.get(0))?) };
    let grass_types = count(
        "SELECT count(*) FROM grass_types WHERE model_path IS NOT NULL AND model_path <> ''",
    )?;
    ensure!(
        grass_types > 0,
        "refused: the world database has no grass type with a model, so no grass can be placed"
    );
    let links = count("SELECT count(*) FROM landscape_texture_grasses")?;
    ensure!(
        links > 0,
        "refused: the world database has no landscape texture to grass links"
    );
    let exterior_references = count("SELECT count(*) FROM \"references\" WHERE is_exterior=1")?;
    ensure!(
        exterior_references > 0,
        "refused: the world database has no exterior references"
    );
    let land_records = count("SELECT count(*) FROM records WHERE record_type='LAND'")?;
    ensure!(
        land_records > 0,
        "refused: the world database has no LAND records"
    );

    let map = validate_cell_cache(terrain).wrap_err_with(|| {
        format!(
            "refused: land input {} is not a valid cell cache",
            terrain.display()
        )
    })?;
    let cache = rkyv::access::<shared::ArchivedCellCache, RkyvError>(&map)
        .wrap_err("refused: the land cache cannot be read")?;
    let terrain_cells = cache.cells.len();
    ensure!(terrain_cells > 0, "refused: the land cache has no cells");
    let known: BTreeSet<u32> = {
        let mut statement = connection.prepare(
            "SELECT id FROM cells WHERE worldspace_id IS NOT NULL AND grid_x IS NOT NULL AND grid_y IS NOT NULL AND (flags & 1)=0",
        )?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let terrain_cells_in_database = cache
        .cells
        .iter()
        .filter(|land| known.contains(&land.cell_id.to_native()))
        .count();
    ensure!(
        terrain_cells_in_database > 0,
        "refused: none of the {terrain_cells} land cache cells has an exterior cell row in the world database, so the land cache does not belong to this database"
    );
    // A small mismatch is normal (interior or unplaced land). A large one means the files differ.
    ensure!(
        terrain_cells_in_database * 10 >= terrain_cells * 9,
        "refused: only {terrain_cells_in_database} of {terrain_cells} land cache cells have an exterior cell row in the world database; the land cache and database do not match"
    );
    Ok(LandFacts {
        world_database_schema,
        terrain_cells,
        terrain_cells_in_database,
        grass_types,
        exterior_references,
        land_records,
    })
}

/// Files that the bake must leave as they are: length and modification time before and after.
fn protected_files(target: &Path) -> Result<Vec<(PathBuf, u64, std::time::SystemTime)>> {
    let mut files = Vec::new();
    for name in [
        "skyrim_world.db",
        "cell_cache_preserved.rkyv",
        "conversion-manifest.json",
        "integration-report.json",
    ] {
        let path = target.join(name);
        if let Ok(metadata) = fs::metadata(&path) {
            files.push((path, metadata.len(), metadata.modified()?));
        }
    }
    Ok(files)
}

fn ensure_unchanged(files: &[(PathBuf, u64, std::time::SystemTime)]) -> Result<()> {
    for (path, length, modified) in files {
        let now = fs::metadata(path)
            .wrap_err_with(|| format!("{} vanished during the bake", path.display()))?;
        ensure!(
            now.len() == *length && now.modified()? == *modified,
            "{} changed during the bake",
            path.display()
        );
    }
    Ok(())
}

fn top_level_names(target: &Path) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(target)? {
        names.insert(entry?.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

#[allow(clippy::too_many_arguments)]
fn receipt_document(
    options: &DeriveGrassOptions,
    pack: &PackFacts,
    land: &LandFacts,
    bake: &GrassBakeReport,
    cache_bytes: u64,
    cache_facts: serde_json::Value,
    cache: &Path,
    elapsed_ms: u128,
) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "format": RECEIPT_FORMAT,
        "command": options.command.join(" "),
        "converter_head": options.converter_head.clone().unwrap_or_else(|| "unknown".into()),
        "converter_crate_version": env!("CARGO_PKG_VERSION"),
        "converter_schema": CONVERTER_SCHEMA_VERSION,
        "grass_cache_format": shared::grass::GRASS_CACHE_VERSION,
        "elapsed_ms": elapsed_ms,
        "overlay": options.target.display().to_string(),
        "base_pack": {
            "name": pack.name,
            "path": pack.path.display().to_string(),
            "manifest_hashes": pack.manifests,
            "manifest_schema": pack.schema_version,
            "nif_asset_configuration_hash": pack.nif_configuration_hash,
        },
        "game_data_read": false,
        "inputs": {
            "world_database_schema": land.world_database_schema,
            "terrain_cells": land.terrain_cells,
            "terrain_cells_in_database": land.terrain_cells_in_database,
            "grass_types_with_model": land.grass_types,
            "exterior_references": land.exterior_references,
            "land_records": land.land_records,
        },
        "grass_cache": {
            "file": GRASS_CACHE_FILE,
            "bytes": cache_bytes,
            "sha256": hash_file(cache)?,
            "contents": cache_facts,
        },
        "bake": {
            "baked_cells": bake.baked_cells,
            "reused_cells": bake.reused_cells,
            "skipped_cells": bake.skipped_cells,
            "skipped_rules": bake.skipped_rules,
            "modelless_rules": bake.modelless_rules,
            "skipped_texture_lists": bake.skipped_texture_lists,
            "unbounded_references": bake.unbounded_references,
            "skipped_obstruction_models": bake.skipped_obstruction_models,
            "initially_hidden_references": bake.initially_hidden_references,
            "unresolved_enable_references": bake.unresolved_enable_references,
            "notices": bake.notices(),
            "warnings": bake.warnings(),
        },
        "scope": [
            "Placement only. The overlay validates grass placement, density, slopes, obstruction masks, streaming and budgets.",
            "The clone keeps the base pack's models and textures. It has no TANGENT frames and no lossless normal maps, so grass shading uses the engine's derivative fallback.",
            "conversion-manifest.json is not changed and does not list grass_cache.rkyv.",
            "This is a test overlay. It is not a conversion and it is not a registered pack.",
        ],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a registered pack, an exact clone of it, and a registry that names the pack `base`.
    /// The clone files are copies, so they are different inodes from the registered files.
    fn fixture() -> (tempfile::TempDir, DeriveGrassOptions) {
        let root = tempfile::tempdir().unwrap();
        let registered = root.path().join("registered");
        let clone = root.path().join("clone");
        fs::create_dir_all(&registered).unwrap();
        fs::create_dir_all(&clone).unwrap();
        let manifest = ConversionManifest {
            schema_version: CONVERTER_SCHEMA_VERSION,
            complete: true,
            ..Default::default()
        };
        manifest
            .save(&registered.join("conversion-manifest.json"))
            .unwrap();
        fs::write(registered.join("integration-report.json"), b"report\n").unwrap();
        for name in ["conversion-manifest.json", "integration-report.json"] {
            fs::copy(registered.join(name), clone.join(name)).unwrap();
        }
        fs::write(clone.join("skyrim_world.db"), b"").unwrap();
        fs::write(clone.join("cell_cache_preserved.rkyv"), b"").unwrap();
        let registry = root.path().join("packs.json");
        record_hashes(&registry, &registered, &registered);
        let options = DeriveGrassOptions {
            target: clone,
            registry,
            clone_of: "base".to_string(),
            replace: false,
            converter_head: None,
            command: Vec::new(),
        };
        (root, options)
    }

    /// Write a one-entry registry: `base` lives at `registered`, and its recorded manifest
    /// hashes are those of the files in `hashes_from`.
    fn record_hashes(registry: &Path, registered: &Path, hashes_from: &Path) {
        let manifests = serde_json::json!({
            "conversion-manifest.json": hash_file(&hashes_from.join("conversion-manifest.json")).unwrap(),
            "integration-report.json": hash_file(&hashes_from.join("integration-report.json")).unwrap(),
        });
        let document = serde_json::json!({
            "base": {"path": registered.display().to_string(), "manifests": manifests}
        });
        fs::write(registry, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    }

    /// Refusal reason of `check_target`, or a panic when the target was accepted.
    fn refusal(options: &DeriveGrassOptions) -> String {
        match check_target(options) {
            Ok(_) => panic!("check_target accepted a target that it must refuse"),
            Err(error) => format!("{error:#}"),
        }
    }

    /// Refusal reason of `check_world_inputs`, or a panic when the inputs were accepted.
    fn world_refusal(connection: &Connection, terrain: &Path) -> String {
        match check_world_inputs(connection, terrain) {
            Ok(_) => panic!("check_world_inputs accepted inputs that it must refuse"),
            Err(error) => format!("{error:#}"),
        }
    }

    /// An in-memory world database with schema 9 and every table and column the bake reads.
    fn world_database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_info(version INTEGER); INSERT INTO schema_info VALUES (9);",
            )
            .unwrap();
        for (table, columns) in REQUIRED_COLUMNS {
            connection
                .execute_batch(&format!("CREATE TABLE \"{table}\"({})", columns.join(",")))
                .unwrap();
        }
        connection
    }

    #[test]
    fn accepts_an_exact_clone() {
        let (_root, options) = fixture();
        let facts = check_target(&options).expect("the exact clone was refused");
        assert_eq!(facts.name, "base");
    }

    #[test]
    fn unknown_pack_name() {
        let (_root, mut options) = fixture();
        options.clone_of = "nope".to_string();
        assert!(refusal(&options).contains("is not a registered pack"));
    }

    #[test]
    fn target_is_the_registered_pack() {
        let (root, mut options) = fixture();
        options.target = root.path().join("registered");
        assert!(refusal(&options).contains("is the registered pack"));
    }

    #[test]
    fn target_inside_the_registered_pack() {
        let (root, mut options) = fixture();
        let inner = root.path().join("registered").join("inner");
        fs::create_dir_all(&inner).unwrap();
        options.target = inner;
        assert!(refusal(&options).contains("is inside the registered pack"));
    }

    #[test]
    fn target_contains_the_registered_pack() {
        let (_root, options) = fixture();
        let nested = options.target.join("nested-pack");
        fs::create_dir_all(&nested).unwrap();
        record_hashes(&options.registry, &nested, &options.target);
        assert!(refusal(&options).contains("contains the registered pack"));
    }

    #[cfg(unix)]
    #[test]
    fn target_is_a_symlink() {
        let (_root, mut options) = fixture();
        let link = options.target.with_file_name("clone-link");
        std::os::unix::fs::symlink(&options.target, &link).unwrap();
        options.target = link;
        assert!(refusal(&options).contains("must be a real directory"));
    }

    #[test]
    fn target_missing() {
        let (root, mut options) = fixture();
        options.target = root.path().join("missing");
        assert!(refusal(&options).contains("does not exist"));
    }

    #[test]
    fn manifest_hash_mismatch() {
        let (_root, options) = fixture();
        let report = options.target.join("integration-report.json");
        let mut bytes = fs::read(&report).unwrap();
        bytes.push(b'!');
        fs::write(&report, bytes).unwrap();
        assert!(refusal(&options).contains("is not an exact clone"));
    }

    #[test]
    fn manifest_missing_in_target() {
        let (_root, options) = fixture();
        fs::remove_file(options.target.join("integration-report.json")).unwrap();
        assert!(refusal(&options).contains("has no integration-report.json"));
    }

    #[test]
    fn incomplete_manifest() {
        let (root, options) = fixture();
        let registered = root.path().join("registered");
        let manifest = ConversionManifest {
            schema_version: CONVERTER_SCHEMA_VERSION,
            complete: false,
            ..Default::default()
        };
        manifest
            .save(&options.target.join("conversion-manifest.json"))
            .unwrap();
        record_hashes(&options.registry, &registered, &options.target);
        assert!(refusal(&options).contains("is not complete"));
    }

    #[test]
    fn wrong_manifest_schema() {
        let (root, options) = fixture();
        let registered = root.path().join("registered");
        let manifest = ConversionManifest {
            schema_version: CONVERTER_SCHEMA_VERSION + 1,
            complete: true,
            ..Default::default()
        };
        manifest
            .save(&options.target.join("conversion-manifest.json"))
            .unwrap();
        record_hashes(&options.registry, &registered, &options.target);
        let error = refusal(&options);
        assert!(error.contains("has schema"), "{error}");
    }

    #[test]
    fn missing_database() {
        let (_root, options) = fixture();
        fs::remove_file(options.target.join("skyrim_world.db")).unwrap();
        assert!(refusal(&options).contains("has no skyrim_world.db"));
    }

    #[test]
    fn existing_cache_needs_replace() {
        let (_root, mut options) = fixture();
        fs::write(options.target.join(GRASS_CACHE_FILE), b"").unwrap();
        assert!(refusal(&options).contains("pass --replace"));
        options.replace = true;
        assert!(check_target(&options).is_ok());
    }

    #[test]
    fn registry_without_manifest_hashes() {
        let (root, options) = fixture();
        let document = serde_json::json!({
            "base": {
                "path": root.path().join("registered").display().to_string(),
                "manifests": {},
            }
        });
        fs::write(
            &options.registry,
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
        assert!(refusal(&options).contains("records no manifest hashes"));
    }

    #[test]
    fn world_database_without_tables() {
        let bare = Connection::open_in_memory().unwrap();
        bare.execute_batch(
            "CREATE TABLE schema_info(version INTEGER); INSERT INTO schema_info VALUES (9);",
        )
        .unwrap();
        let error = world_refusal(&bare, Path::new("unused"));
        assert!(error.contains("has no `grass_types` table"), "{error}");

        let unversioned = Connection::open_in_memory().unwrap();
        let error = world_refusal(&unversioned, Path::new("unused"));
        assert!(error.contains("no schema_info version"), "{error}");

        let future = Connection::open_in_memory().unwrap();
        future
            .execute_batch(
                "CREATE TABLE schema_info(version INTEGER); INSERT INTO schema_info VALUES (99);",
            )
            .unwrap();
        let error = world_refusal(&future, Path::new("unused"));
        assert!(error.contains("outside the runtime range"), "{error}");
    }

    #[test]
    fn world_database_missing_columns() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_info(version INTEGER); INSERT INTO schema_info VALUES (9);",
            )
            .unwrap();
        for (table, columns) in REQUIRED_COLUMNS {
            let sql_columns = if *table == "grass_types" {
                "id".to_string()
            } else {
                columns.join(",")
            };
            connection
                .execute_batch(&format!("CREATE TABLE \"{table}\"({sql_columns})"))
                .unwrap();
        }
        let error = world_refusal(&connection, Path::new("unused"));
        assert!(error.contains("lacks columns"), "{error}");
    }

    #[test]
    fn world_database_without_grass_types() {
        let connection = world_database();
        let error = world_refusal(&connection, Path::new("unused"));
        assert!(error.contains("has no grass type with a model"), "{error}");
    }

    #[test]
    fn land_cache_invalid() {
        let connection = world_database();
        connection
            .execute_batch(
                "INSERT INTO grass_types(id, model_path) VALUES (1, 'x');
                 INSERT INTO landscape_texture_grasses(ltex_id, gras_id) VALUES (1, 1);
                 INSERT INTO \"references\"(id, is_exterior) VALUES (1, 1);
                 INSERT INTO records(form_id, record_type) VALUES (1, 'LAND');",
            )
            .unwrap();
        let terrain = tempfile::tempdir().unwrap();
        let path = terrain.path().join("cell_cache_preserved.rkyv");
        fs::write(&path, b"junk").unwrap();
        let error = world_refusal(&connection, &path);
        assert!(error.contains("is not a valid cell cache"), "{error}");
    }

    /// Write a land cache of `count` cells whose cell FormIDs are 1000 and up.
    fn land_cache(dir: &Path, count: u32) -> PathBuf {
        use crate::esm::records::RawRecord;
        let records: std::collections::HashMap<u32, RawRecord> = (0..count)
            .map(|index| {
                let mut texture = 30u32.to_le_bytes().to_vec();
                texture.extend([0, 0, 0, 0]);
                (
                    100 + index,
                    RawRecord {
                        form_id: 100 + index,
                        record_type: *b"LAND",
                        flags: 0,
                        subrecords: vec![(b"BTXT".to_vec(), texture)],
                        cell_form_id: Some(1000 + index),
                        worldspace_form_id: Some(1),
                        load_order: 0,
                    },
                )
            })
            .collect();
        let path = dir.join("cell_cache_preserved.rkyv");
        crate::esm::cell_cache::write_cell_cache(&records, &path).unwrap();
        path
    }

    /// A database that passes every check except the land match, with `rows` exterior cell rows
    /// for the first land cells.
    fn database_with_cell_rows(rows: u32) -> Connection {
        let connection = world_database();
        connection
            .execute_batch(
                "INSERT INTO grass_types(id, model_path) VALUES (1, 'x');
                 INSERT INTO landscape_texture_grasses(ltex_id, gras_id) VALUES (1, 1);
                 INSERT INTO \"references\"(id, is_exterior) VALUES (1, 1);
                 INSERT INTO records(form_id, record_type) VALUES (1, 'LAND');",
            )
            .unwrap();
        for index in 0..rows {
            connection
                .execute(
                    "INSERT INTO cells(id, worldspace_id, grid_x, grid_y, flags) VALUES (?1, 1, ?2, 0, 0)",
                    [1000 + index, index],
                )
                .unwrap();
        }
        connection
    }

    #[test]
    fn land_cache_below_ninety_percent_match_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let terrain = land_cache(dir.path(), 10);
        let error = world_refusal(&database_with_cell_rows(8), &terrain);
        assert!(error.contains("only 8 of 10 land cache cells"), "{error}");
    }

    #[test]
    fn land_cache_without_any_match_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let terrain = land_cache(dir.path(), 3);
        let error = world_refusal(&database_with_cell_rows(0), &terrain);
        assert!(error.contains("none of the 3 land cache cells"), "{error}");
    }

    #[test]
    fn land_cache_at_ninety_percent_match_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let terrain = land_cache(dir.path(), 10);
        let facts = check_world_inputs(&database_with_cell_rows(9), &terrain)
            .expect("a 9 of 10 match was refused");
        assert_eq!(
            (facts.terrain_cells, facts.terrain_cells_in_database),
            (10, 9)
        );
    }

    /// A clone directory with the protected files and a recorded top-level listing.
    fn bake_target() -> (
        tempfile::TempDir,
        BTreeSet<String>,
        Vec<(PathBuf, u64, std::time::SystemTime)>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "skyrim_world.db",
            "cell_cache_preserved.rkyv",
            "conversion-manifest.json",
        ] {
            fs::write(dir.path().join(name), b"data").unwrap();
        }
        let before = top_level_names(dir.path()).unwrap();
        let protected = protected_files(dir.path()).unwrap();
        (dir, before, protected)
    }

    #[test]
    fn post_bake_check_refuses_a_stray_new_file() {
        let (dir, before, protected) = bake_target();
        let cache = dir.path().join(GRASS_CACHE_FILE);
        fs::write(&cache, b"x").unwrap();
        fs::write(dir.path().join("skyrim_world.db-wal"), b"x").unwrap();
        let error = verify_after_bake(dir.path(), &cache, &before, &protected)
            .expect_err("a stray file was accepted");
        let text = format!("{error:#}");
        assert!(text.contains("beyond grass_cache.rkyv"), "{text}");
        assert!(text.contains("skyrim_world.db-wal"), "{text}");
    }

    #[test]
    fn post_bake_check_refuses_a_changed_protected_file() {
        let (dir, before, protected) = bake_target();
        let cache = dir.path().join(GRASS_CACHE_FILE);
        fs::write(&cache, b"x").unwrap();
        fs::write(dir.path().join("skyrim_world.db"), b"changed data").unwrap();
        let text = format!(
            "{:#}",
            verify_after_bake(dir.path(), &cache, &before, &protected)
                .expect_err("a changed database was accepted")
        );
        assert!(
            text.contains("skyrim_world.db") && text.contains("changed during the bake"),
            "{text}"
        );
    }

    #[test]
    fn post_bake_check_refuses_an_invalid_cache() {
        let (dir, before, protected) = bake_target();
        let cache = dir.path().join(GRASS_CACHE_FILE);
        fs::write(&cache, b"junk").unwrap();
        let text = format!(
            "{:#}",
            verify_after_bake(dir.path(), &cache, &before, &protected)
                .expect_err("an invalid cache was accepted")
        );
        assert!(text.contains("the new grass cache is invalid"), "{text}");
    }

    #[test]
    fn post_bake_check_accepts_a_valid_cache_and_only_that() {
        let (dir, before, protected) = bake_target();
        let cache = dir.path().join(GRASS_CACHE_FILE);
        let bytes = rkyv::to_bytes::<RkyvError>(&shared::grass::GrassCache {
            version: shared::grass::GRASS_CACHE_VERSION,
            types: Vec::new(),
            cells: Vec::new(),
        })
        .unwrap();
        fs::write(&cache, &bytes).unwrap();
        let (length, facts) = verify_after_bake(dir.path(), &cache, &before, &protected)
            .expect("a valid cache was refused");
        assert_eq!(length, bytes.len() as u64);
        assert_eq!(facts["version"], shared::grass::GRASS_CACHE_VERSION);
    }
}
