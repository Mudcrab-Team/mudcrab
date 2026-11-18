//! Derive `actor-outfits.json` into an APFS clone of a registered pack, without a conversion.
//!
//! The worn-outfit catalog reads only data the pack already holds: the winning `NPC_`, `RACE`,
//! `OTFT`, `ARMO`, `ARMA` and `LVLI` records in `skyrim_world.db`, the published `actor_models`
//! rows, and the converted `.glb` files. This command reads no Skyrim game data. It writes two new
//! files into the clone, the catalog and a receipt, and nothing else.
//!
//! The command fails closed. It refuses a target that is a registered pack, that is inside or
//! contains one, that is not an exact copy of the named registered pack's manifests, or whose
//! database lacks the tables it reads. Every refusal names its reason.

use crate::{
    actor_models::{ActorModelCatalog, ActorModelRow},
    actor_outfits::resolve_actor_outfits,
    cache::hash_file,
    esm::records::RawRecord,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use shared::actor_outfits::{ActorOutfitCatalog, PATH as CATALOG_FILE};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

/// Name of the receipt written beside the catalog.
pub const RECEIPT_FILE: &str = "actor-outfits-receipt.json";
/// Receipt format name, so a reader can recognise the file.
pub const RECEIPT_FORMAT: &str = "actor-outfits-receipt/1";

/// Record types the resolver follows. `LVLI` is loaded only so a leveled list is counted as one.
const RECORD_TYPES: &str = "'NPC_','RACE','OTFT','ARMO','ARMA','LVLI'";

/// Inputs of `derive-outfits`.
#[derive(Debug, Clone)]
pub struct DeriveOutfitsOptions {
    /// The APFS clone that receives the overlay.
    pub target: PathBuf,
    /// `packs.json` of the shared storage. The command only reads it.
    pub registry: PathBuf,
    /// Registered pack that `target` must be a clone of.
    pub clone_of: String,
    /// Replace an existing catalog and receipt in the target.
    pub replace: bool,
    /// Git head of the converter build, recorded in the receipt. The caller supplies it.
    pub converter_head: Option<String>,
    /// The command line as the caller ran it, recorded in the receipt.
    pub command: Vec<String>,
}

/// What a successful run produced.
#[derive(Debug)]
pub struct DeriveOutfitsReport {
    pub catalog: PathBuf,
    pub receipt: PathBuf,
    pub bytes_added: u64,
    pub elapsed_ms: u128,
    pub actors_with_outfit: usize,
    pub parts: usize,
    pub skipped: BTreeMap<String, usize>,
}

#[derive(Debug, Deserialize)]
struct RegisteredPack {
    path: PathBuf,
    #[serde(default)]
    manifests: BTreeMap<String, String>,
}

/// Facts about the registered pack the target copies, recorded in the receipt.
struct PackFacts {
    name: String,
    path: PathBuf,
    manifests: BTreeMap<String, String>,
    manifest_schema_version: Option<u64>,
}

/// Derive the catalog into `options.target`. See the module documentation for the rules.
pub fn run(options: &DeriveOutfitsOptions) -> Result<DeriveOutfitsReport> {
    let started = Instant::now();
    let pack = check_target(options)?;
    let database = options.target.join("skyrim_world.db");
    let connection = open_database_read_only(&database)?;
    let schema = check_world_inputs(&connection)?;

    let protected = protected_files(&options.target)?;
    let before = top_level_names(&options.target)?;
    let (records, headers) = load_records(&connection)?;
    let models = load_models(&connection)?;
    let target = options.target.clone();
    let catalog = resolve_actor_outfits(&records, &models, &|scene| target.join(scene).is_file());
    catalog
        .validate()
        .map_err(|reason| color_eyre::eyre::eyre!("the derived catalog is invalid: {reason}"))?;

    let catalog_path = options.target.join(CATALOG_FILE);
    let receipt_path = options.target.join(RECEIPT_FILE);
    let text = serde_json::to_string_pretty(&catalog)? + "\n";
    write_atomic(&catalog_path, text.as_bytes())?;
    // A partial overlay would pass for a complete one at the next engine start. Remove it when
    // anything after the write goes wrong.
    let finish = verify_after_write(&options.target, &before, &protected);
    if let Err(error) = finish {
        let _ = fs::remove_file(&catalog_path);
        return Err(error);
    }

    let elapsed_ms = started.elapsed().as_millis();
    let parts: usize = catalog
        .outfits
        .iter()
        .map(|outfit| outfit.parts.len())
        .sum();
    let receipt = serde_json::json!({
        "format": RECEIPT_FORMAT,
        "command": options.command,
        "converter_head": options.converter_head,
        "game_data_read": false,
        "clone_of": {
            "name": pack.name,
            "path": pack.path,
            "manifests": pack.manifests,
            "manifest_schema_version": pack.manifest_schema_version,
        },
        "world_database_schema": schema,
        "inputs": {
            "records_loaded": records.len(),
            "record_flags_from_source_headers": headers,
            "actor_models": models.models.len(),
        },
        "catalog": {
            "file": CATALOG_FILE,
            "version": catalog.version,
            "sha256": hash_file(&catalog_path)?,
            "bytes": text.len(),
            "actors_with_outfit": catalog.outfits.len(),
            "parts": parts,
            "skipped": catalog.skipped,
        },
        "elapsed_ms": elapsed_ms,
        "scope": "Default outfit (DOFT) only. Leveled lists, inventory armor (CNTO) and the sleeping outfit are not resolved.",
    });
    let receipt_text = serde_json::to_string_pretty(&receipt)? + "\n";
    if let Err(error) = fs::write(&receipt_path, &receipt_text) {
        let _ = fs::remove_file(&catalog_path);
        return Err(error).wrap_err("failed to write the overlay receipt");
    }
    Ok(DeriveOutfitsReport {
        catalog: catalog_path,
        receipt: receipt_path,
        bytes_added: (text.len() + receipt_text.len()) as u64,
        elapsed_ms,
        actors_with_outfit: catalog.outfits.len(),
        parts,
        skipped: catalog.skipped,
    })
}

/// Write `bytes` to `path` through a temporary file, so a reader never sees half a catalog.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)
        .wrap_err_with(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, path).wrap_err_with(|| format!("failed to publish {}", path.display()))
}

/// Check the clone after the write: the protected files are unchanged and the catalog is the only
/// new top-level entry, read back and valid.
fn verify_after_write(
    target: &Path,
    before: &BTreeSet<String>,
    protected: &[(PathBuf, u64, SystemTime)],
) -> Result<()> {
    for (path, length, modified) in protected {
        let metadata = fs::metadata(path)?;
        ensure!(
            metadata.len() == *length && metadata.modified()? == *modified,
            "the derive changed {}, which it must not touch",
            path.display()
        );
    }
    let after = top_level_names(target)?;
    let added: BTreeSet<_> = after.difference(before).cloned().collect();
    let allowed: BTreeSet<_> = [CATALOG_FILE.to_string()].into();
    ensure!(
        added.is_subset(&allowed),
        "the derive changed the clone beyond {CATALOG_FILE}: new entries {added:?}"
    );
    let written: ActorOutfitCatalog = serde_json::from_slice(&fs::read(target.join(CATALOG_FILE))?)
        .wrap_err("the new catalog does not read back")?;
    written
        .validate()
        .map_err(|reason| color_eyre::eyre::eyre!("the new catalog is invalid: {reason}"))
}

fn top_level_names(directory: &Path) -> Result<BTreeSet<String>> {
    fs::read_dir(directory)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect()
}

/// Length and modification time of the files the derive must leave alone.
fn protected_files(target: &Path) -> Result<Vec<(PathBuf, u64, SystemTime)>> {
    ["skyrim_world.db", "conversion-manifest.json"]
        .iter()
        .map(|name| {
            let path = target.join(name);
            let metadata = fs::metadata(&path)
                .wrap_err_with(|| format!("refused: the target has no {name}"))?;
            Ok((path, metadata.len(), metadata.modified()?))
        })
        .collect()
}

/// Refuse a target that is not a clone of the named registered pack, or that is a registered pack.
fn check_target(options: &DeriveOutfitsOptions) -> Result<PackFacts> {
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

    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(target.join("conversion-manifest.json"))
            .wrap_err("refused: the target conversion manifest cannot be read")?,
    )
    .wrap_err("refused: the target conversion manifest is not valid JSON")?;
    ensure!(
        manifest["complete"].as_bool() == Some(true),
        "refused: the target conversion manifest is not complete"
    );
    ensure!(
        target.join("skyrim_world.db").is_file(),
        "refused: the target has no skyrim_world.db, which the derive reads"
    );
    for name in [CATALOG_FILE, RECEIPT_FILE] {
        ensure!(
            options.replace || !target.join(name).exists(),
            "refused: the target already has {name}; pass --replace to overwrite it"
        );
    }
    Ok(PackFacts {
        name: options.clone_of.clone(),
        path: source.path.clone(),
        manifests: source.manifests.clone(),
        manifest_schema_version: manifest["schema_version"].as_u64(),
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

/// Tables and columns the derive reads.
const REQUIRED_COLUMNS: &[(&str, &[&str])] = &[
    ("records", &["form_id", "record_type", "load_order", "data"]),
    ("actor_models", &["id", "race_id", "sex", "resolution"]),
];

/// Refuse a database that cannot feed the derive. Returns its schema version.
fn check_world_inputs(connection: &Connection) -> Result<u32> {
    let schema: u32 = connection
        .query_row("SELECT version FROM schema_info LIMIT 1", [], |row| {
            row.get(0)
        })
        .wrap_err("refused: the world database has no schema_info version")?;
    ensure!(
        shared::supports_runtime_world_database_schema(schema),
        "refused: world database schema {schema} is outside the runtime range {} to {}",
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
            "refused: the world database has no `{table}` table, which the derive reads"
        );
        let missing: Vec<_> = columns
            .iter()
            .filter(|column| !present.contains(**column))
            .collect();
        ensure!(
            missing.is_empty(),
            "refused: world database table `{table}` lacks columns {missing:?}, which the derive reads"
        );
    }
    let count = |sql: &str| -> Result<i64> { Ok(connection.query_row(sql, [], |row| row.get(0))?) };
    ensure!(
        count("SELECT count(*) FROM actor_models")? > 0,
        "refused: the world database has no actor_models rows"
    );
    ensure!(
        count("SELECT count(*) FROM records WHERE record_type='OTFT'")? > 0,
        "refused: the world database has no OTFT records, so there is no outfit to derive"
    );
    Ok(schema)
}

/// The winning actor and armor records with their exact header flags.
///
/// Header flags come from `inhouse_source_records`. A pack without that table has no flags, so a
/// deleted record there reads as present; the receipt records which source was used.
fn load_records(connection: &Connection) -> Result<(HashMap<u32, RawRecord>, bool)> {
    let has_headers: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='inhouse_source_records')",
        [],
        |row| row.get(0),
    )?;
    let sql = if has_headers {
        format!(
            "SELECT r.form_id, r.record_type, r.load_order, r.data, COALESCE(h.flags, 0) \
             FROM records r LEFT JOIN inhouse_source_records h \
             ON h.form_id = r.form_id AND h.load_order = r.load_order AND h.record_type = r.record_type \
             WHERE r.record_type IN ({RECORD_TYPES})"
        )
    } else {
        format!(
            "SELECT form_id, record_type, load_order, data, 0 FROM records \
             WHERE record_type IN ({RECORD_TYPES})"
        )
    };
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, Vec<u8>>(3)?,
            row.get::<_, u32>(4)?,
        ))
    })?;
    let mut records = HashMap::new();
    for row in rows {
        let (id, kind, load_order, bytes, flags) = row?;
        let data =
            rkyv::from_bytes::<shared::records::ArchivedRecordData, rkyv::rancor::Error>(&bytes)
                .map_err(|error| color_eyre::eyre::eyre!("record {id:08X} {kind}: {error}"))?;
        let record_type: [u8; 4] = kind
            .as_bytes()
            .try_into()
            .map_err(|_| color_eyre::eyre::eyre!("record {id:08X} has type `{kind}`"))?;
        records.insert(
            id,
            RawRecord {
                form_id: id,
                record_type,
                flags,
                subrecords: data
                    .subrecords
                    .into_iter()
                    .map(|field| (field.tag.to_vec(), field.data))
                    .collect(),
                cell_form_id: None,
                worldspace_form_id: None,
                load_order,
            },
        );
    }
    Ok((records, has_headers))
}

/// The published `actor_models` rows: race, sex and resolution are read, not recomputed, so the
/// outfit follows exactly the actors the engine spawns.
fn load_models(connection: &Connection) -> Result<ActorModelCatalog> {
    let mut statement =
        connection.prepare("SELECT id, race_id, sex, resolution FROM actor_models")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut catalog = ActorModelCatalog::default();
    for row in rows {
        let (id, race_id, sex, resolution) = row?;
        let sex = match sex.as_str() {
            "male" => "male",
            "female" => "female",
            _ => continue,
        };
        let resolution = match resolution.as_str() {
            "body" => "body",
            "body_head" => "body_head",
            "unresolved" => "unresolved",
            _ => continue,
        };
        catalog.models.push(ActorModelRow {
            id,
            race_id,
            sex,
            skin_armo_id: None,
            head_model: None,
            head_source: None,
            resolution,
            skin_tone: None,
            weight: None,
        });
    }
    catalog.models.sort_by_key(|row| row.id);
    Ok(catalog)
}

#[cfg(test)]
mod tests;
