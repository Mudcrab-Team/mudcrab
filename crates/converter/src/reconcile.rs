//! Read-only retail-versus-pack count reconciliation for LOD data (#931, phase 1).
//!
//! For each worldspace and block level, this counts the retail LOD files in the game `Data`
//! folder and compares them with the pack's LOD tables and `lod-manifest.json`. The rule is
//! `retail - pack - explained = 0`. An explanation is a skip class the converter reported. A count
//! that cannot be computed is `unknown`, never zero. The command writes only counts and reason
//! classes to its JSON. Paths and near-match text stay on the terminal.
//!
//! Retail files are listed once through the object LOD provider search
//! ([`crate::lod::object_stage::providers`]), which already walks every `.bsa` and the loose
//! `meshes/terrain` and `textures/terrain` trees. Terrain files are classified by the same rules
//! the terrain importer uses ([`crate::lod::retail_terrain_source::classify`]). WRLD `RNAM`
//! memberships are read from the selected plugins with the existing plugin parser, streamed one
//! record at a time. Nothing here writes to the game folder or to the pack. The file-name and
//! atlas rules are documented in
//! [the LOD compiler spec](../../../docs/specs/converters/lod-compiler.md).

use crate::{
    esm::{
        binary::{parse_plugin_metadata, read_world_rnam},
        large_references::members,
        load_order::{LoadOrder, normalize_plugin_name},
    },
    lod::{
        near_match::FileIndex,
        object_stage::{atlas_paths, providers},
        reconciliation::{COLOUR_BIT, LodReconciliation, MESH_BIT, NO_RUNTIME_TIER, NORMAL_BIT},
        retail::name::{RetailLodKind, parse_lod_file_name},
        retail_terrain::tier_for_level,
        retail_terrain_source::{BlockId, Kind, canonical_path, classify},
        reuse::LodManifest,
    },
    records::source_identity::{header_runtime_id, resolve},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use shared::lod::{ChunkAnchor, ChunkKey, LodTier, chunk_payload_path};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Format tag of the JSON report.
pub const FORMAT: &str = "lod-count-reconciliation/1";

/// Inputs of `converter reconcile`.
#[derive(Debug, Clone)]
pub struct Options {
    /// The Skyrim `Data` folder. Read only.
    pub data: PathBuf,
    /// The pack root, holding `skyrim_world.db` and `lod-manifest.json`. Read only.
    pub pack: PathBuf,
    /// Where to write the machine-readable report. `None` writes no file.
    pub json: Option<PathBuf>,
}

/// Result of one category at one world and level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// `retail - pack - explained = 0` and no pack-only entry.
    Balanced,
    /// A non-zero difference the converter did not explain.
    Unexplained,
    /// A count that cannot be computed. Not zero.
    Unknown,
    /// A category the pack does not claim to hold, with a recorded reason.
    OutOfScope,
}

/// One row of the reconciliation table.
#[derive(Debug, Clone, Serialize)]
pub struct CategoryReport {
    /// `terrain_mesh`, `terrain_colour`, `terrain_normal`, `object_block`, `object_atlas` or
    /// `large_reference`.
    pub category: &'static str,
    /// Worldspace editor id, or `*` for a category-wide row.
    pub world: String,
    /// Block level. `None` for an atlas, a large-reference row or a category-wide row.
    pub level: Option<u32>,
    /// Count in the game data, or `None` when unknown.
    pub retail: Option<u64>,
    /// Pack count, or `None` when unknown.
    pub pack: Option<u64>,
    /// Count explained by converter skip classes, or `None` when unknown.
    pub explained: Option<u64>,
    /// `retail - pack - explained`. Negative means the pack holds more than retail.
    pub unexplained: Option<i64>,
    /// Skip classes and how many entries each explains.
    pub explained_by_class: BTreeMap<String, u64>,
    pub status: Status,
    /// Why the row is unknown or out of scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Operator-only example text. Never serialized: it can hold local paths.
    #[serde(skip)]
    pub examples: Vec<String>,
}

/// Counts of rows by status.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub rows: usize,
    pub balanced: usize,
    pub unexplained: usize,
    pub unknown: usize,
    pub out_of_scope: usize,
    /// Sum of the absolute non-zero differences.
    pub difference: u64,
}

/// The reconciliation report.
#[derive(Debug, Clone, Serialize)]
pub struct ReconcileReport {
    pub format: &'static str,
    /// A reader must not turn `null` into zero.
    pub null_is_unknown: bool,
    pub totals: Totals,
    pub categories: Vec<CategoryReport>,
}

impl ReconcileReport {
    fn new() -> Self {
        Self {
            format: FORMAT,
            null_is_unknown: true,
            totals: Totals::default(),
            categories: Vec::new(),
        }
    }

    /// Whether every category that claims to hold data is balanced. Unknown counts fail the check
    /// because the rule cannot be proven; out-of-scope categories do not.
    pub fn passed(&self) -> bool {
        self.totals.unexplained == 0 && self.totals.unknown == 0
    }

    fn push(&mut self, row: CategoryReport) {
        self.totals.rows += 1;
        match row.status {
            Status::Balanced => self.totals.balanced += 1,
            Status::Unexplained => {
                self.totals.unexplained += 1;
                if let Some(value) = row.unexplained {
                    self.totals.difference += value.unsigned_abs();
                }
            }
            Status::Unknown => self.totals.unknown += 1,
            Status::OutOfScope => self.totals.out_of_scope += 1,
        }
        self.categories.push(row);
    }
}

/// Runs the check. Reads the game folder and the pack; writes only `options.json` when asked.
pub fn run(options: &Options) -> Result<ReconcileReport> {
    if let Some(json) = &options.json {
        reject_output_inside("pack", &options.pack, json)?;
        reject_output_inside("game data folder", &options.data, json)?;
    }
    let mut report = ReconcileReport::new();
    let pack = Pack::open(&options.pack)?;
    let retail = match Retail::list(&options.data) {
        Ok(retail) => Some(retail),
        Err(error) => {
            eprintln!("Retail LOD listing failed: {error:#}");
            None
        }
    };
    let (rnam_retail, rnam_reason) = match retail_rnam(&options.data, &pack.plugins) {
        Ok(scan) => (Some(scan), None),
        Err(error) => {
            eprintln!("Retail WRLD RNAM scan failed: {error:#}");
            let reason = pack.plugins_error.unwrap_or(if pack.plugins.is_empty() {
                "no_selected_plugins"
            } else {
                "retail_rnam_unreadable"
            });
            (None, Some(reason))
        }
    };

    let selected = rnam_retail.as_ref().map(|scan| &scan.selected);
    if let Some(retail) = &retail {
        compare_terrain(&mut report, retail, &pack, selected);
        compare_objects(&mut report, retail, &pack, selected);
        compare_atlases(&mut report, retail, &pack, selected);
    } else {
        for category in [
            "terrain_mesh",
            "terrain_colour",
            "terrain_normal",
            "object_block",
            "object_atlas",
        ] {
            report.push(unknown_row(category, "*", None, "retail_listing_failed"));
        }
    }
    compare_large_references(&mut report, &pack, rnam_retail.as_ref(), rnam_reason);

    Ok(report)
}

/// Prints the human table and the operator-only examples.
pub fn print_report(report: &ReconcileReport) {
    println!("| category | world | level | retail | pack | explained | unexplained | status |");
    println!("|---|---|---|---|---|---|---|---|");
    for row in &report.categories {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            row.category,
            row.world,
            row.level
                .map_or_else(|| "-".to_owned(), |level| level.to_string()),
            count(row.retail),
            count(row.pack),
            count(row.explained),
            row.unexplained
                .map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
            status(row.status),
        );
    }
    let totals = &report.totals;
    println!(
        "\nReconciliation: {} balanced, {} unexplained, {} unknown, {} out of scope ({} row(s))",
        totals.balanced, totals.unexplained, totals.unknown, totals.out_of_scope, totals.rows
    );
    if let Some(reason) = report
        .categories
        .iter()
        .find(|row| row.status == Status::Unknown)
        .and_then(|row| row.reason.as_deref())
    {
        eprintln!("Unknown counts have a reason; first: {reason}");
    }
    let mut printed = 0usize;
    for row in report
        .categories
        .iter()
        .filter(|row| row.status == Status::Unexplained)
    {
        for example in row.examples.iter().take(5) {
            if printed >= 40 {
                eprintln!("  ... more examples omitted");
                return;
            }
            if printed == 0 {
                println!("\nUnexplained examples:");
            }
            println!("  {} {}: {example}", row.category, row.world);
            printed += 1;
        }
    }
}

fn count(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

fn status(status: Status) -> &'static str {
    match status {
        Status::Balanced => "balanced",
        Status::Unexplained => "unexplained",
        Status::Unknown => "unknown",
        Status::OutOfScope => "out of scope",
    }
}

/// Writes the JSON report. Counts and reason classes only; `examples` is not serialized. The
/// destination must not exist, so a hard link outside the pack cannot truncate a pack file.
pub fn write_json(path: &Path, report: &ReconcileReport) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(report).wrap_err("cannot serialize the report")?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .wrap_err_with(|| {
            format!(
                "cannot create {}; the report path must not exist",
                path.display()
            )
        })?;
    file.write_all(&bytes)
        .wrap_err_with(|| format!("cannot write {}", path.display()))
}

/// Refuses an output file inside an input tree. The check must not change the pack or the game
/// folder, even when the caller asks for the report there.
fn reject_output_inside(label: &str, root: &Path, output: &Path) -> Result<()> {
    ensure!(
        !canonical_target(output).starts_with(canonical_target(root)),
        "refused: --json must be outside the {label}"
    );
    Ok(())
}

/// Resolve symlinks and `..` for the output and its root. The output may not exist yet, so
/// canonicalize its parent and append its file name. A lexical fallback still refuses an obvious
/// prefix when neither exists.
fn canonical_target(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_owned(), |cwd| cwd.join(path))
    };
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => {
            fs::canonicalize(parent).map_or_else(|_| absolute.clone(), |parent| parent.join(name))
        }
        _ => absolute,
    }
}

fn report_row(
    category: &'static str,
    world: &str,
    level: Option<u32>,
    counts: (Option<u64>, Option<u64>, Option<u64>, Option<i64>),
    classes: BTreeMap<String, u64>,
    status: Status,
    reason: Option<String>,
) -> CategoryReport {
    CategoryReport {
        category,
        world: world.to_owned(),
        level,
        retail: counts.0,
        pack: counts.1,
        explained: counts.2,
        unexplained: counts.3,
        explained_by_class: classes,
        status,
        reason,
        examples: Vec::new(),
    }
}

fn unknown_row(
    category: &'static str,
    world: &str,
    level: Option<u32>,
    reason: &str,
) -> CategoryReport {
    report_row(
        category,
        world,
        level,
        (None, None, None, None),
        BTreeMap::new(),
        Status::Unknown,
        Some(reason.to_owned()),
    )
}

// ---------------------------------------------------------------------------------------------
// Retail listing
// ---------------------------------------------------------------------------------------------

/// One retail terrain block's files.
#[derive(Debug, Clone, Copy, Default)]
struct Presence {
    mesh: bool,
    colour: bool,
    normal: bool,
}

impl Presence {
    fn of(kind: Kind) -> impl Fn(&Self) -> bool {
        move |presence| match kind {
            Kind::Mesh => presence.mesh,
            Kind::Colour => presence.colour,
            Kind::Normal => presence.normal,
        }
    }
}

/// Retail terrain blocks by world, level and cell.
type TerrainBlocks = BTreeMap<String, BTreeMap<u32, BTreeMap<(i32, i32), Presence>>>;
/// Retail object blocks by world and level.
type ObjectBlocks = BTreeMap<String, BTreeMap<u32, BTreeSet<(i32, i32)>>>;

/// Retail LOD files found in the game data.
struct Retail {
    index: FileIndex,
    terrain: TerrainBlocks,
    objects: ObjectBlocks,
    atlases: BTreeMap<String, u64>,
    /// `.bto` files the converter does not read, by world, level and reason.
    object_out_of_scope: BTreeMap<(String, Option<u32>), (u64, &'static str)>,
}

impl Retail {
    /// Lists every `.bsa` and the loose terrain trees once. Every path is kept in the file index,
    /// so a missing or unexplained file can name its near matches.
    fn list(data: &Path) -> Result<Self> {
        let wanted =
            |path: &str| path.ends_with(".btr") || path.ends_with(".bto") || path.ends_with(".dds");
        let (found, index) = providers(data, &wanted)?;
        let mut retail = Self {
            index,
            terrain: BTreeMap::new(),
            objects: BTreeMap::new(),
            atlases: BTreeMap::new(),
            object_out_of_scope: BTreeMap::new(),
        };
        for path in found.keys() {
            if let Some((id, kind)) = classify(path) {
                let (world, level, x, y) = id;
                let presence = retail
                    .terrain
                    .entry(world.clone())
                    .or_default()
                    .entry(level)
                    .or_default()
                    .entry((x, y))
                    .or_default();
                match kind {
                    Kind::Mesh => presence.mesh = true,
                    Kind::Colour => presence.colour = true,
                    Kind::Normal => presence.normal = true,
                }
                continue;
            }
            if let Some(world) = atlas_world(path) {
                *retail.atlases.entry(world.clone()).or_default() += 1;
                continue;
            }
            if path.ends_with(".bto")
                && let Some(name) = parse_lod_file_name(path)
                && name.kind == RetailLodKind::Object
                && let Some(level) = name.level
            {
                let reason = if !path.starts_with("meshes/") {
                    Some("object_block_outside_meshes")
                } else if !crate::lod::object_stage::LEVELS.contains(&level) {
                    Some("object_level_not_compiled")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    let entry = retail
                        .object_out_of_scope
                        .entry((name.world.clone(), Some(level)))
                        .or_insert((0, reason));
                    entry.0 += 1;
                    continue;
                }
                if let Some(cell) = name.cell {
                    retail
                        .objects
                        .entry(name.world.clone())
                        .or_default()
                        .entry(level)
                        .or_default()
                        .insert(cell);
                }
            }
        }
        Ok(retail)
    }
}

/// The world of an atlas path, if it is one of the two documented name forms.
fn atlas_world(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 5 || parts[0] != "textures" || parts[1] != "terrain" || parts[3] != "objects"
    {
        return None;
    }
    let world = parts[2];
    atlas_paths(world)
        .iter()
        .any(|candidate| candidate == path)
        .then(|| world.to_owned())
}

// ---------------------------------------------------------------------------------------------
// Pack
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PackWorld {
    id: u32,
    editor_id: String,
    origin: Option<(i32, i32)>,
}

/// Pack-side terrain counts: retail chunks only, by world id and tier.
#[derive(Debug)]
enum TerrainState {
    Retail(BTreeMap<(u32, u32), BTreeSet<(i32, i32)>>),
    SelfBuilt,
    Unknown(&'static str),
}

/// Pack-side object counts.
#[derive(Debug)]
enum ObjectState {
    Tables {
        chunks: BTreeMap<(u32, u32), BTreeSet<(i32, i32)>>,
        atlases: BTreeSet<u32>,
    },
    Unknown(&'static str),
}

/// Pack-side large-reference counts.
#[derive(Debug)]
enum RnamState {
    Tables(BTreeMap<u32, PackRnam>),
    Unknown(&'static str),
}

#[derive(Debug, Clone, Copy, Default)]
struct PackRnam {
    projected: u64,
    omitted: u64,
    removed: u64,
}

impl PackRnam {
    fn explained(self) -> u64 {
        self.omitted + self.removed
    }
}

struct Pack {
    worlds: BTreeMap<String, PackWorld>,
    worlds_by_id: BTreeMap<u32, PackWorld>,
    terrain: TerrainState,
    objects: ObjectState,
    rnam: RnamState,
    reconciliation: Option<LodReconciliation>,
    /// The producer could not list the retail terrain files.
    terrain_listing_failed: bool,
    /// World file names whose terrain skips the producer recorded.
    terrain_recorded_worlds: BTreeSet<String>,
    /// World file names whose object skips the producer recorded.
    objects_recorded_worlds: BTreeSet<String>,
    plugins: Vec<(String, i64)>,
    /// Why the selected plugin list cannot be read, when it cannot.
    plugins_error: Option<&'static str>,
}

impl Pack {
    fn open(root: &Path) -> Result<Self> {
        ensure!(
            root.is_dir(),
            "refused: {} is not a pack directory",
            root.display()
        );
        let database = root.join("skyrim_world.db");
        ensure!(
            database.is_file(),
            "refused: {} has no skyrim_world.db",
            root.display()
        );
        let connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .wrap_err_with(|| format!("cannot open {}", database.display()))?;
        let (worlds, worlds_by_id) = read_worlds(&connection)
            .wrap_err("the pack has no readable worldspace table; it is not a converter pack")?;
        let manifest = read_manifest(root);
        let (terrain, reconciliation) = match &manifest {
            Ok(Some(manifest)) => (
                terrain_state(manifest, root, &connection, &worlds_by_id),
                manifest
                    .reconciliation
                    .clone()
                    .filter(|counts| counts.format == crate::lod::reconciliation::FORMAT),
            ),
            Ok(None) => (TerrainState::Unknown("no_lod_manifest"), None),
            Err(error) => {
                eprintln!("lod-manifest.json cannot be read: {error:#}");
                (TerrainState::Unknown("lod_manifest_unreadable"), None)
            }
        };
        let terrain_listing_failed = reconciliation
            .as_ref()
            .is_some_and(|counts| counts.terrain_listing_failed);
        let terrain_recorded_worlds = reconciliation
            .as_ref()
            .map_or_else(BTreeSet::new, |counts| {
                counts.terrain_recorded_worlds.clone()
            });
        let objects_recorded_worlds = reconciliation
            .as_ref()
            .map_or_else(BTreeSet::new, |counts| {
                counts.objects_recorded_worlds.clone()
            });
        let objects = match object_state(root, &connection, &worlds_by_id) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("Object LOD pack counts cannot be read: {error:#}");
                ObjectState::Unknown("object_lod_package_invalid")
            }
        };
        let rnam = match rnam_state(&connection, &worlds_by_id) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("Large-reference pack counts cannot be read: {error:#}");
                RnamState::Unknown("large_reference_unreadable")
            }
        };
        let (plugins, plugins_error) = match read_plugins(&connection) {
            Ok(plugins) => (plugins, None),
            Err(error) => {
                eprintln!("Selected plugin list cannot be read: {error:#}");
                (Vec::new(), Some("plugins_table_unreadable"))
            }
        };
        Ok(Self {
            worlds,
            worlds_by_id,
            terrain,
            objects,
            rnam,
            reconciliation,
            terrain_listing_failed,
            terrain_recorded_worlds,
            objects_recorded_worlds,
            plugins,
            plugins_error,
        })
    }
}

fn read_manifest(root: &Path) -> Result<Option<LodManifest>> {
    let path = root.join("lod-manifest.json");
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path).wrap_err_with(|| format!("cannot read {}", path.display()))?;
    let manifest = serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("{} is not valid", path.display()))?;
    Ok(Some(manifest))
}

fn read_worlds(
    connection: &Connection,
) -> Result<(BTreeMap<String, PackWorld>, BTreeMap<u32, PackWorld>)> {
    let mut statement = connection
        .prepare("SELECT id, editor_id, lod_origin_x, lod_origin_y FROM worldspaces ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok(PackWorld {
            id: row.get(0)?,
            editor_id: row.get(1)?,
            origin: match (row.get::<_, Option<i32>>(2)?, row.get::<_, Option<i32>>(3)?) {
                (Some(x), Some(y)) => Some((x, y)),
                _ => None,
            },
        })
    })?;
    let mut by_name = BTreeMap::new();
    let mut by_id = BTreeMap::new();
    for row in rows {
        let world = row?;
        ensure!(
            by_name
                .insert(world.editor_id.to_ascii_lowercase(), world.clone())
                .is_none(),
            "duplicate worldspace editor id {}",
            world.editor_id
        );
        by_id.insert(world.id, world);
    }
    Ok((by_name, by_id))
}

fn terrain_state(
    manifest: &LodManifest,
    root: &Path,
    connection: &Connection,
    worlds_by_id: &BTreeMap<u32, PackWorld>,
) -> TerrainState {
    let Some(source) = manifest.terrain_lod_source.as_deref() else {
        return TerrainState::Unknown("lod_manifest_has_no_terrain_source");
    };
    if source == crate::lod::reuse::TERRAIN_SOURCE_SELF_BUILT {
        return TerrainState::SelfBuilt;
    }
    if !source.starts_with(crate::lod::reuse::TERRAIN_SOURCE_RETAIL) {
        return TerrainState::Unknown("unknown_terrain_lod_source");
    }
    let has_table = match has_table(connection, "lod_chunks") {
        Ok(has_table) => has_table,
        Err(error) => {
            eprintln!("Terrain LOD row check failed: {error:#}");
            return TerrainState::Unknown("lod_chunks_unreadable");
        }
    };
    if !has_table {
        return TerrainState::Unknown("no_lod_chunks_table");
    }
    let mut statement = match connection.prepare(
        "SELECT worldspace_id, tier, anchor_x, anchor_y, payload_path, content_hash \
         FROM lod_chunks",
    ) {
        Ok(statement) => statement,
        Err(error) => {
            eprintln!("Terrain LOD rows cannot be read: {error:#}");
            return TerrainState::Unknown("lod_chunks_unreadable");
        }
    };
    let rows = match statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, i32>(1)?,
            row.get::<_, i32>(2)?,
            row.get::<_, i32>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("Terrain LOD rows cannot be read: {error:#}");
            return TerrainState::Unknown("lod_chunks_unreadable");
        }
    };
    let mixed = source != crate::lod::reuse::TERRAIN_SOURCE_RETAIL;
    let mut chunks: BTreeMap<(u32, u32), BTreeSet<(i32, i32)>> = BTreeMap::new();
    let mut row_paths = BTreeSet::new();
    for row in rows {
        let (world_id, tier, anchor_x, anchor_y, path, hash) = match row {
            Ok(row) => row,
            Err(error) => {
                eprintln!("Terrain LOD row cannot be read: {error:#}");
                return TerrainState::Unknown("lod_chunks_unreadable");
            }
        };
        let Some(tier) = LodTier::from_side_cells(tier) else {
            return TerrainState::Unknown("terrain_chunk_tier_invalid");
        };
        let key = ChunkKey::new(world_id, tier, ChunkAnchor::new(anchor_x, anchor_y));
        if chunk_payload_path(key) != path {
            return TerrainState::Unknown("terrain_chunk_path_not_canonical");
        }
        let Some(world) = worlds_by_id.get(&world_id) else {
            return TerrainState::Unknown("terrain_chunk_world_missing");
        };
        let Some((origin_x, origin_y)) = world.origin else {
            return TerrainState::Unknown("terrain_chunk_world_has_no_lod_origin");
        };
        let actual = match crate::cache::hash_file(&root.join(&path)) {
            Ok(actual) => actual,
            Err(error) => {
                eprintln!("Terrain chunk {path} cannot be hashed: {error:#}");
                return TerrainState::Unknown("terrain_chunk_payload_missing");
            }
        };
        if actual != hash {
            return TerrainState::Unknown("terrain_chunk_hash_mismatch");
        }
        row_paths.insert(path.clone());
        if mixed && !manifest.retail_terrain_inputs.contains_key(&path) {
            continue;
        }
        let side = tier.side_cells();
        chunks
            .entry((world_id, u32::try_from(side).unwrap_or(u32::MAX)))
            .or_default()
            .insert((origin_x + anchor_x * side, origin_y + anchor_y * side));
    }
    if mixed {
        if let Some(path) = manifest
            .retail_terrain_inputs
            .keys()
            .find(|path| !row_paths.contains(*path))
        {
            eprintln!("Terrain chunk {path} is listed in lod-manifest.json but has no row");
            return TerrainState::Unknown("terrain_manifest_row_missing");
        }
    } else if manifest.retail_terrain_inputs.len() != row_paths.len()
        || manifest
            .retail_terrain_inputs
            .keys()
            .any(|path| !row_paths.contains(path))
    {
        return TerrainState::Unknown("terrain_manifest_row_mismatch");
    }
    TerrainState::Retail(chunks)
}

fn object_state(
    root: &Path,
    connection: &Connection,
    worlds_by_id: &BTreeMap<u32, PackWorld>,
) -> Result<ObjectState> {
    if !has_table(connection, "lod_object_chunks")? {
        return Ok(ObjectState::Unknown("no_object_lod_tables"));
    }
    // The package validator already checks manifest counts, canonical paths, payload existence,
    // recorded SHA-256 and the shared-atlas contract. Its failure is an unknown count, never a
    // smaller pack that passes.
    if let Err(error) = crate::lod::objects::validate_object_lod_package(root) {
        eprintln!("Object LOD package check failed: {error:#}");
        return Ok(ObjectState::Unknown("object_lod_package_invalid"));
    }
    let mut statement = connection.prepare(
        "SELECT worldspace_id, tier, anchor_x, anchor_y FROM lod_object_chunks ORDER BY worldspace_id, tier, anchor_x, anchor_y",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, i32>(1)?,
            row.get::<_, i32>(2)?,
            row.get::<_, i32>(3)?,
        ))
    })?;
    let mut chunks: BTreeMap<(u32, u32), BTreeSet<(i32, i32)>> = BTreeMap::new();
    for row in rows {
        let (world_id, tier, anchor_x, anchor_y) = row?;
        let tier = LodTier::from_side_cells(tier)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid object LOD tier {tier}"))?;
        let world = worlds_by_id.get(&world_id).ok_or_else(|| {
            color_eyre::eyre::eyre!("object chunk world {world_id:08X} is absent")
        })?;
        let (origin_x, origin_y) = world.origin.ok_or_else(|| {
            color_eyre::eyre::eyre!("object chunk world {world_id:08X} has no LOD origin")
        })?;
        let side = tier.side_cells();
        chunks
            .entry((world_id, u32::try_from(side)?))
            .or_default()
            .insert((origin_x + anchor_x * side, origin_y + anchor_y * side));
    }
    // The package validator returns early when there are no object chunks, so it can miss an
    // orphan or wrongly named atlas row. Check the atlas rows here as well.
    let mut atlases = BTreeSet::new();
    if has_table(connection, "lod_object_atlases")? {
        let mut statement = connection.prepare(
            "SELECT worldspace_id, payload_path, content_hash FROM lod_object_atlases \
             ORDER BY worldspace_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (world, path, hash) = row?;
            ensure!(
                path == shared::lod::object_atlas_path(world),
                "object atlas row {path} is not the canonical path of worldspace {world:08X}"
            );
            ensure!(
                crate::cache::hash_file(&root.join(&path))? == hash,
                "object atlas {path} does not match its recorded SHA-256"
            );
            atlases.insert(world);
        }
    }
    for world in chunks.keys() {
        ensure!(
            atlases.contains(&world.0),
            "object chunks of worldspace {:08X} have no atlas row",
            world.0
        );
    }
    for world in &atlases {
        ensure!(
            chunks.keys().any(|(world_id, _)| world_id == world),
            "the object atlas of worldspace {world:08X} has no chunk that uses it"
        );
    }
    Ok(ObjectState::Tables { chunks, atlases })
}

fn rnam_state(
    connection: &Connection,
    worlds_by_id: &BTreeMap<u32, PackWorld>,
) -> Result<RnamState> {
    if !has_table(connection, "large_references")?
        || !has_table(connection, "large_reference_omissions")?
    {
        return Ok(RnamState::Unknown("no_large_reference_tables"));
    }
    let mut counts: BTreeMap<u32, PackRnam> = BTreeMap::new();
    let mut statement = connection
        .prepare("SELECT worldspace_id, count(*) FROM large_references GROUP BY worldspace_id")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, i64>(1)?)))?;
    for row in rows {
        let (world, projected) = row?;
        counts.entry(world).or_default().projected = u64::try_from(projected)?;
    }
    let mut statement = connection.prepare(
        "SELECT worldspace_id, \
                COALESCE(sum(omitted_count), 0), COALESCE(sum(confirmed_removed_count), 0) \
         FROM large_reference_omissions GROUP BY worldspace_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (world, omitted, removed) = row?;
        let entry = counts.entry(world).or_default();
        entry.omitted = u64::try_from(omitted)?;
        entry.removed = u64::try_from(removed)?;
    }
    for world in counts.keys() {
        if !worlds_by_id.contains_key(world) {
            eprintln!("large_reference row names absent worldspace {world:08X}");
        }
    }
    Ok(RnamState::Tables(counts))
}

fn read_plugins(connection: &Connection) -> Result<Vec<(String, i64)>> {
    if !has_table(connection, "plugins")? {
        return Ok(Vec::new());
    }
    let mut statement =
        connection.prepare("SELECT name, priority FROM plugins ORDER BY priority")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

fn has_table(connection: &Connection, name: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )?)
}

// ---------------------------------------------------------------------------------------------
// Retail WRLD RNAM
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct RnamUnits {
    memberships: u64,
    unresolved: u64,
    malformed: u64,
    overrides: u64,
}

impl RnamUnits {
    fn total(self) -> u64 {
        self.memberships + self.unresolved + self.malformed + self.overrides
    }
}

/// A retail WRLD identity. An EDID is the normal key; a WRLD without one is keyed by the runtime
/// FormID the converter projected, so its omissions can still explain its RNAM.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RnamWorld {
    EditorId(String),
    RuntimeId(u32),
}

struct RnamRetail {
    worlds: BTreeMap<RnamWorld, RnamUnits>,
    /// Runtime FormID to display name, from the first record that carries an EDID.
    labels: BTreeMap<u32, String>,
    unattributed: u64,
    selected: BTreeSet<String>,
}

/// Counts the WRLD `RNAM` memberships of the selected plugins, in the same units the converter's
/// omission table uses: one unit per qualifying tuple, one per malformed defining `RNAM`, and one
/// per override `RNAM` subrecord.
fn retail_rnam(data: &Path, plugins: &[(String, i64)]) -> Result<RnamRetail> {
    ensure!(!plugins.is_empty(), "the pack lists no selected plugins");
    let mut names = Vec::with_capacity(plugins.len());
    let mut paths = Vec::with_capacity(plugins.len());
    let mut metadata = Vec::with_capacity(plugins.len());
    for (name, _) in plugins {
        let path = find_plugin(data, name)?.ok_or_else(|| {
            color_eyre::eyre::eyre!("selected plugin {name} is not in the data folder")
        })?;
        names.push(name.clone());
        metadata.push(parse_plugin_metadata(&path)?);
        paths.push(path);
    }
    let order = LoadOrder::from_metadata(names, metadata)?;
    let mut worlds: BTreeMap<RnamWorld, RnamUnits> = BTreeMap::new();
    let mut memberships: BTreeMap<RnamWorld, BTreeSet<(i32, i32, u32)>> = BTreeMap::new();
    let mut labels: BTreeMap<u32, String> = BTreeMap::new();
    let mut unattributed = 0u64;
    let mut selected = BTreeSet::new();
    for (supplier, path) in paths.iter().enumerate() {
        let defining_name = normalize_plugin_name(&order.names[supplier]);
        let scan = read_world_rnam(path)?;
        if scan.incomplete {
            color_eyre::eyre::bail!(
                "the WRLD RNAM scan of a selected plugin is incomplete: {}",
                scan.warnings.join("; ")
            );
        }
        for record in scan.worlds {
            let defining = resolve(b"WRLD", record.form_id, supplier, &order)
                .original_owner
                .as_deref()
                == Some(defining_name.as_str());
            let runtime = header_runtime_id(b"WRLD", record.form_id, supplier, &order);
            let editor_id = record.editor_id.map(|id| id.to_ascii_lowercase());
            let world = match (runtime, editor_id) {
                (Some(id), Some(label)) => {
                    labels.entry(id).or_insert_with(|| label.clone());
                    selected.insert(label);
                    RnamWorld::RuntimeId(id)
                }
                (Some(id), None) => RnamWorld::RuntimeId(id),
                (None, Some(label)) => {
                    selected.insert(label.clone());
                    RnamWorld::EditorId(label)
                }
                (None, None) => {
                    for rnam in &record.rnam {
                        unattributed += units_for(rnam, defining);
                    }
                    continue;
                }
            };
            let entry = worlds.entry(world.clone()).or_default();
            for rnam in &record.rnam {
                if !defining {
                    entry.overrides += 1;
                } else if let Some(found) = members(rnam) {
                    for (x, y, raw_id) in found {
                        match header_runtime_id(b"REFR", raw_id, supplier, &order) {
                            Some(id) if id != 0 => {
                                memberships
                                    .entry(world.clone())
                                    .or_default()
                                    .insert((x, y, id));
                            }
                            _ => entry.unresolved += 1,
                        }
                    }
                } else {
                    entry.malformed += 1;
                }
            }
        }
    }
    for (world, found) in memberships {
        worlds.entry(world).or_default().memberships = u64::try_from(found.len())?;
    }
    Ok(RnamRetail {
        worlds,
        labels,
        unattributed,
        selected,
    })
}

fn units_for(rnam: &[u8], defining: bool) -> u64 {
    if !defining {
        return 1;
    }
    members(rnam).map_or(1, |members| {
        u64::try_from(members.count()).unwrap_or(u64::MAX)
    })
}

fn find_plugin(data: &Path, name: &str) -> Result<Option<PathBuf>> {
    let wanted = name.to_ascii_lowercase();
    let mut found = None;
    for entry in fs::read_dir(data)
        .wrap_err_with(|| format!("cannot read the game data folder {}", data.display()))?
    {
        let entry = entry?;
        if entry.file_name().to_string_lossy().to_ascii_lowercase() == wanted {
            ensure!(
                found.is_none(),
                "more than one file in the data folder has the name {name}"
            );
            found = Some(entry.path());
        }
    }
    Ok(found)
}

// ---------------------------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------------------------

/// How a retail world maps into the pack.
enum WorldMatch<'a> {
    Pack(&'a PackWorld),
    NotSelected,
    MissingFromPack,
    SelectionUnknown,
}

fn world_match<'a>(
    retail_world: &str,
    pack: &'a Pack,
    selected: Option<&BTreeSet<String>>,
) -> WorldMatch<'a> {
    if let Some(world) = pack.worlds.get(retail_world) {
        return WorldMatch::Pack(world);
    }
    match selected {
        Some(selected) if selected.contains(retail_world) => WorldMatch::MissingFromPack,
        Some(_) => WorldMatch::NotSelected,
        None => WorldMatch::SelectionUnknown,
    }
}

fn compare_terrain(
    report: &mut ReconcileReport,
    retail: &Retail,
    pack: &Pack,
    selected: Option<&BTreeSet<String>>,
) {
    let mut pack_failure_reported = false;
    let mut worlds: BTreeSet<String> = retail.terrain.keys().cloned().collect();
    if let TerrainState::Retail(chunks) = &pack.terrain {
        for (world_id, _) in chunks.keys() {
            if let Some(world) = pack.worlds_by_id.get(world_id) {
                worlds.insert(world.editor_id.to_ascii_lowercase());
            }
        }
    }
    for world_file in worlds {
        let match_ = world_match(&world_file, pack, selected);
        let label = match &match_ {
            WorldMatch::Pack(world) => world.editor_id.clone(),
            _ => world_file.clone(),
        };
        let pack_world = match &match_ {
            WorldMatch::Pack(world) => Some(*world),
            _ => None,
        };
        let mut levels: BTreeSet<u32> = retail
            .terrain
            .get(&world_file)
            .map(|levels| levels.keys().copied().collect())
            // silent-ok: a world with no retail level of this kind has no blocks
            .unwrap_or_default();
        if let (Some(world), TerrainState::Retail(chunks)) = (pack_world, &pack.terrain) {
            levels.extend(
                chunks
                    .keys()
                    .filter(|(world_id, _)| *world_id == world.id)
                    .map(|(_, level)| *level),
            );
        }
        if matches!(match_, WorldMatch::NotSelected) {
            for (kind, key) in [
                (Kind::Mesh, "terrain_mesh"),
                (Kind::Colour, "terrain_colour"),
                (Kind::Normal, "terrain_normal"),
            ] {
                let present = Presence::of(kind);
                for level in &levels {
                    let count = retail
                        .terrain
                        .get(&world_file)
                        .and_then(|levels| levels.get(level))
                        .map_or(0, |blocks| {
                            u64::try_from(blocks.values().filter(|block| present(block)).count())
                                .unwrap_or(u64::MAX)
                        });
                    if count > 0 {
                        report.push(report_row(
                            key,
                            &label,
                            Some(*level),
                            (Some(count), Some(0), Some(0), None),
                            BTreeMap::new(),
                            Status::OutOfScope,
                            Some("world_not_in_selected_plugins".to_owned()),
                        ));
                    }
                }
            }
            continue;
        }
        if matches!(match_, WorldMatch::SelectionUnknown) {
            for (kind, key) in [
                (Kind::Mesh, "terrain_mesh"),
                (Kind::Colour, "terrain_colour"),
                (Kind::Normal, "terrain_normal"),
            ] {
                let present = Presence::of(kind);
                for level in &levels {
                    let count = retail
                        .terrain
                        .get(&world_file)
                        .and_then(|levels| levels.get(level))
                        .map_or(0, |blocks| {
                            u64::try_from(blocks.values().filter(|block| present(block)).count())
                                .unwrap_or(u64::MAX)
                        });
                    if count > 0 {
                        let mut row =
                            unknown_row(key, &label, Some(*level), "plugin_selection_unknown");
                        row.retail = Some(count);
                        report.push(row);
                    }
                }
            }
            continue;
        }
        for (kind, key) in [
            (Kind::Mesh, "terrain_mesh"),
            (Kind::Colour, "terrain_colour"),
            (Kind::Normal, "terrain_normal"),
        ] {
            let present = Presence::of(kind);
            for level in &levels {
                let retail_blocks = retail
                    .terrain
                    .get(&world_file)
                    .and_then(|levels| levels.get(level));
                let retail_set: BTreeSet<(i32, i32)> = retail_blocks
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|(_, block)| present(block))
                            .map(|(cell, _)| *cell)
                            .collect()
                    })
                    // silent-ok: a world with no retail block at this level has no files
                    .unwrap_or_default();
                let shown = match &match_ {
                    WorldMatch::MissingFromPack => {
                        if retail_set.is_empty() {
                            continue;
                        }
                        compare_missing_world(key, &label, Some(*level), retail_set, |cell| {
                            terrain_example(&retail.index, &world_file, *level, cell, kind)
                        })
                    }
                    _ => {
                        let pack_cells = terrain_pack_cells(pack, pack_world, *level);
                        match terrain_row(
                            key,
                            &label,
                            &world_file,
                            *level,
                            &retail_set,
                            &pack_cells,
                            retail_blocks,
                            pack,
                            &retail.index,
                            kind,
                        ) {
                            Ok(row) => row,
                            Err(reason) => {
                                let mut row = unknown_row(key, &label, Some(*level), reason);
                                row.retail =
                                    Some(u64::try_from(retail_set.len()).unwrap_or(u64::MAX));
                                row
                            }
                        }
                    }
                };
                if shown.retail.unwrap_or(0) > 0
                    || shown.pack.unwrap_or(0) > 0
                    || shown.status != Status::Balanced
                {
                    if let TerrainState::Unknown(reason) = &pack.terrain
                        && shown.status == Status::Unknown
                        && shown.reason.as_deref() == Some(reason)
                    {
                        pack_failure_reported = true;
                    }
                    report.push(shown);
                }
            }
        }
    }
    if let TerrainState::Unknown(reason) = &pack.terrain
        && !pack_failure_reported
    {
        for category in ["terrain_mesh", "terrain_colour", "terrain_normal"] {
            report.push(unknown_row(category, "*", None, reason));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn terrain_row(
    category: &'static str,
    label: &str,
    world_file: &str,
    level: u32,
    retail_set: &BTreeSet<(i32, i32)>,
    pack_cells: &BTreeSet<(i32, i32)>,
    retail_blocks: Option<&BTreeMap<(i32, i32), Presence>>,
    pack: &Pack,
    index: &FileIndex,
    kind: Kind,
) -> std::result::Result<CategoryReport, &'static str> {
    if matches!(pack.terrain, TerrainState::SelfBuilt) {
        return Ok(out_of_scope_row(
            category,
            label,
            Some(level),
            u64::try_from(retail_set.len()).unwrap_or(u64::MAX),
            "terrain_lod_source=self-built",
        ));
    }
    if let TerrainState::Unknown(reason) = &pack.terrain {
        return Err(reason);
    }
    let TerrainState::Retail(_) = &pack.terrain else {
        return Err("terrain_lod_source_unknown");
    };

    let recorded = pack.terrain_recorded_worlds.contains(world_file);
    let bit = kind_bit(kind);
    let mut explained: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut class_cells: BTreeMap<String, BTreeSet<(i32, i32)>> = BTreeMap::new();
    let mut unknown_reason = None;
    if pack.terrain_listing_failed {
        unknown_reason = Some("terrain_listing_failed");
    } else if tier_for_level(level).is_none() {
        // A level with no runtime tier cannot be in the pack. This is a converter rule, not a
        // recorded skip, so a file that appeared after the conversion is still explained.
        explained.extend(retail_set.iter().copied());
        class_cells.insert(NO_RUNTIME_TIER.to_owned(), retail_set.clone());
    } else if recorded
        && let Some(counts) = &pack.reconciliation
        && let Some(levels) = counts.terrain_skips.get(world_file)
        && let Some(by_class) = levels.get(&level)
    {
        for (class, skip) in by_class {
            if class == NO_RUNTIME_TIER {
                continue;
            }
            if !skip.is_consistent() {
                unknown_reason = Some("incomplete_terrain_reconciliation");
                continue;
            }
            let cells: BTreeSet<(i32, i32)> = skip
                .cells
                .iter()
                .filter(|(x, y, mask)| {
                    mask & bit != 0 && current_presence(retail_blocks, (*x, *y), kind)
                })
                .map(|(x, y, _)| (*x, *y))
                .collect();
            if !cells.is_empty() {
                class_cells.insert(class.clone(), cells.clone());
                explained.extend(cells);
            }
        }
    }

    let missing: Vec<(i32, i32)> = retail_set
        .difference(pack_cells)
        .filter(|cell| !explained.contains(cell))
        .copied()
        .collect();
    let excess: Vec<(i32, i32)> = pack_cells.difference(retail_set).copied().collect();
    if unknown_reason.is_none() && !recorded && !missing.is_empty() {
        unknown_reason = Some("terrain_world_not_recorded");
    }
    if let Some(reason) = unknown_reason {
        let mut row = unknown_row(category, label, Some(level), reason);
        row.retail = Some(u64::try_from(retail_set.len()).unwrap_or(u64::MAX));
        row.pack = Some(u64::try_from(pack_cells.len()).unwrap_or(u64::MAX));
        return Ok(row);
    }
    let explained_only: BTreeSet<(i32, i32)> = explained.difference(pack_cells).copied().collect();
    let classes = class_counts(&class_cells, &explained_only);
    let difference = i64::try_from(missing.len()).unwrap_or(i64::MAX)
        - i64::try_from(excess.len()).unwrap_or(i64::MAX);
    let mut examples: Vec<String> = missing
        .iter()
        .take(5)
        .map(|cell| terrain_example(index, world_file, level, *cell, kind))
        .collect();
    for cell in excess.iter().take(5usize.saturating_sub(examples.len())) {
        examples.push(format!(
            "pack holds a chunk with no retail source: {}",
            terrain_example(index, world_file, level, *cell, kind)
        ));
    }
    let mut row = report_row(
        category,
        label,
        Some(level),
        (
            Some(u64::try_from(retail_set.len()).unwrap_or(u64::MAX)),
            Some(u64::try_from(pack_cells.len()).unwrap_or(u64::MAX)),
            Some(u64::try_from(explained_only.len()).unwrap_or(u64::MAX)),
            Some(difference),
        ),
        classes,
        if missing.is_empty() && excess.is_empty() {
            Status::Balanced
        } else {
            Status::Unexplained
        },
        None,
    );
    row.examples = examples;
    Ok(row)
}

/// Counts each explained cell under exactly one class. The first class in map order wins, so the
/// class counts sum to `explained_only` and never include a cell that the pack holds.
fn class_counts(
    class_cells: &BTreeMap<String, BTreeSet<(i32, i32)>>,
    explained_only: &BTreeSet<(i32, i32)>,
) -> BTreeMap<String, u64> {
    let mut classes = BTreeMap::new();
    let mut assigned = BTreeSet::new();
    for (class, cells) in class_cells {
        let count = cells
            .iter()
            .filter(|cell| explained_only.contains(*cell) && !assigned.contains(*cell))
            .count();
        if count > 0 {
            classes.insert(class.clone(), u64::try_from(count).unwrap_or(u64::MAX));
        }
        assigned.extend(cells.iter().copied());
    }
    classes
}

fn current_presence(
    retail_blocks: Option<&BTreeMap<(i32, i32), Presence>>,
    cell: (i32, i32),
    kind: Kind,
) -> bool {
    retail_blocks
        .and_then(|blocks| blocks.get(&cell))
        .is_some_and(|presence| match kind {
            Kind::Mesh => presence.mesh,
            Kind::Colour => presence.colour,
            Kind::Normal => presence.normal,
        })
}

fn kind_bit(kind: Kind) -> u8 {
    match kind {
        Kind::Mesh => MESH_BIT,
        Kind::Colour => COLOUR_BIT,
        Kind::Normal => NORMAL_BIT,
    }
}

fn out_of_scope_row(
    category: &'static str,
    label: &str,
    level: Option<u32>,
    retail: u64,
    reason: &str,
) -> CategoryReport {
    report_row(
        category,
        label,
        level,
        (Some(retail), None, None, None),
        BTreeMap::new(),
        Status::OutOfScope,
        Some(reason.to_owned()),
    )
}

fn terrain_pack_cells(pack: &Pack, world: Option<&PackWorld>, level: u32) -> BTreeSet<(i32, i32)> {
    let (Some(world), TerrainState::Retail(chunks)) = (world, &pack.terrain) else {
        return BTreeSet::new();
    };
    // silent-ok: a world with no chunk at this level has none
    chunks.get(&(world.id, level)).cloned().unwrap_or_default()
}

fn compare_missing_world(
    category: &'static str,
    label: &str,
    level: Option<u32>,
    retail_set: BTreeSet<(i32, i32)>,
    example: impl Fn((i32, i32)) -> String,
) -> CategoryReport {
    let count = u64::try_from(retail_set.len()).unwrap_or(u64::MAX);
    let mut row = report_row(
        category,
        label,
        level,
        (
            Some(count),
            Some(0),
            Some(0),
            Some(i64::try_from(count).unwrap_or(i64::MAX)),
        ),
        BTreeMap::new(),
        Status::Unexplained,
        Some("world_is_selected_but_absent_from_the_pack".to_owned()),
    );
    row.examples = retail_set
        .iter()
        .take(5)
        .map(|cell| example(*cell))
        .collect();
    row
}

fn terrain_example(
    index: &FileIndex,
    world_file: &str,
    level: u32,
    cell: (i32, i32),
    kind: Kind,
) -> String {
    let id: BlockId = (world_file.to_owned(), level, cell.0, cell.1);
    index.describe_missing(&[&canonical_path(&id, kind)], Some(world_file))
}

fn compare_objects(
    report: &mut ReconcileReport,
    retail: &Retail,
    pack: &Pack,
    selected: Option<&BTreeSet<String>>,
) {
    for ((world_file, level), (count, reason)) in &retail.object_out_of_scope {
        let match_ = world_match(world_file, pack, selected);
        let label = match &match_ {
            WorldMatch::Pack(world) => world.editor_id.clone(),
            _ => world_file.clone(),
        };
        report.push(out_of_scope_row(
            "object_block",
            &label,
            *level,
            *count,
            reason,
        ));
    }
    let mut pack_failure_reported = false;
    let mut worlds: BTreeSet<String> = retail.objects.keys().cloned().collect();
    if let ObjectState::Tables { chunks, .. } = &pack.objects {
        for (world_id, _) in chunks.keys() {
            if let Some(world) = pack.worlds_by_id.get(world_id) {
                worlds.insert(world.editor_id.to_ascii_lowercase());
            }
        }
    }
    for world_file in worlds {
        let match_ = world_match(&world_file, pack, selected);
        let label = match &match_ {
            WorldMatch::Pack(world) => world.editor_id.clone(),
            _ => world_file.clone(),
        };
        let pack_world = match &match_ {
            WorldMatch::Pack(world) => Some(*world),
            _ => None,
        };
        let mut levels: BTreeSet<u32> = retail
            .objects
            .get(&world_file)
            .map(|levels| levels.keys().copied().collect())
            // silent-ok: a world with no retail level of this kind has no blocks
            .unwrap_or_default();
        if let (Some(world), ObjectState::Tables { chunks, .. }) = (pack_world, &pack.objects) {
            levels.extend(
                chunks
                    .keys()
                    .filter(|(world_id, _)| *world_id == world.id)
                    .map(|(_, level)| *level),
            );
        }
        if matches!(match_, WorldMatch::NotSelected) {
            for level in &levels {
                let count = retail
                    .objects
                    .get(&world_file)
                    .and_then(|levels| levels.get(level))
                    .map_or(0, |cells| u64::try_from(cells.len()).unwrap_or(u64::MAX));
                if count > 0 {
                    report.push(report_row(
                        "object_block",
                        &label,
                        Some(*level),
                        (Some(count), Some(0), Some(0), None),
                        BTreeMap::new(),
                        Status::OutOfScope,
                        Some("world_not_in_selected_plugins".to_owned()),
                    ));
                }
            }
            continue;
        }
        if matches!(match_, WorldMatch::SelectionUnknown) {
            for level in &levels {
                let count = retail
                    .objects
                    .get(&world_file)
                    .and_then(|levels| levels.get(level))
                    .map_or(0, |cells| u64::try_from(cells.len()).unwrap_or(u64::MAX));
                if count > 0 {
                    let mut row = unknown_row(
                        "object_block",
                        &label,
                        Some(*level),
                        "plugin_selection_unknown",
                    );
                    row.retail = Some(count);
                    report.push(row);
                }
            }
            continue;
        }
        for level in &levels {
            let retail_set = retail
                .objects
                .get(&world_file)
                .and_then(|levels| levels.get(level))
                .cloned()
                // silent-ok: a world with no retail object block at this level has none
                .unwrap_or_default();
            let shown = match &match_ {
                WorldMatch::MissingFromPack => {
                    if retail_set.is_empty() {
                        continue;
                    }
                    compare_missing_world(
                        "object_block",
                        &label,
                        Some(*level),
                        retail_set,
                        |cell| object_example(&retail.index, &world_file, *level, cell),
                    )
                }
                _ => object_row(
                    &label,
                    &world_file,
                    *level,
                    &retail_set,
                    object_pack_cells(pack, pack_world, *level),
                    pack,
                    &retail.index,
                ),
            };
            if shown.retail.unwrap_or(0) > 0
                || shown.pack.unwrap_or(0) > 0
                || shown.status != Status::Balanced
            {
                if let ObjectState::Unknown(reason) = &pack.objects
                    && shown.status == Status::Unknown
                    && shown.reason.as_deref() == Some(reason)
                {
                    pack_failure_reported = true;
                }
                report.push(shown);
            }
        }
    }
    if let ObjectState::Unknown(reason) = &pack.objects
        && !pack_failure_reported
    {
        report.push(unknown_row("object_block", "*", None, reason));
    }
}

fn object_row(
    label: &str,
    world_file: &str,
    level: u32,
    retail_set: &BTreeSet<(i32, i32)>,
    pack_cells: BTreeSet<(i32, i32)>,
    pack: &Pack,
    index: &FileIndex,
) -> CategoryReport {
    let ObjectState::Tables { .. } = &pack.objects else {
        let ObjectState::Unknown(reason) = &pack.objects else {
            return unknown_row("object_block", label, Some(level), "object_lod_unknown");
        };
        let mut row = unknown_row("object_block", label, Some(level), reason);
        row.retail = Some(u64::try_from(retail_set.len()).unwrap_or(u64::MAX));
        return row;
    };
    let recorded = pack.objects_recorded_worlds.contains(world_file);
    let mut explained: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut class_cells: BTreeMap<String, BTreeSet<(i32, i32)>> = BTreeMap::new();
    let mut unknown_reason = None;
    if tier_for_level(level).is_none() {
        explained.extend(retail_set.iter().copied());
        class_cells.insert(NO_RUNTIME_TIER.to_owned(), retail_set.clone());
    } else if recorded && let Some(counts) = &pack.reconciliation {
        if let Some(skipped) = counts.object_world_skips.get(world_file)
            && skipped == crate::lod::object_stage::MISSING_ATLAS
        {
            match counts.object_world_skip_blocks.get(world_file) {
                Some(blocks) if !blocks.is_empty() => {
                    let covered: BTreeSet<(i32, i32)> = blocks
                        .iter()
                        .filter(|(recorded_level, x, y)| {
                            *recorded_level == level && retail_set.contains(&(*x, *y))
                        })
                        .map(|(_, x, y)| (*x, *y))
                        .collect();
                    if !covered.is_empty() {
                        explained.extend(covered.iter().copied());
                        class_cells.insert(skipped.clone(), covered);
                    }
                }
                _ => unknown_reason = Some("missing_atlas_coverage_unknown"),
            }
        }
        if explained.is_empty()
            && let Some(levels) = counts.object_skips.get(world_file)
            && let Some(by_class) = levels.get(&level)
        {
            for (class, skip) in by_class {
                if class == NO_RUNTIME_TIER {
                    continue;
                }
                // silent-ok: an incomplete record marks the category unknown below
                if u64::try_from(skip.cells.len()).ok() != Some(skip.blocks) {
                    unknown_reason = Some("incomplete_object_reconciliation");
                    continue;
                }
                let cells: BTreeSet<(i32, i32)> =
                    skip.cells.intersection(retail_set).copied().collect();
                if !cells.is_empty() {
                    class_cells.insert(class.clone(), cells.clone());
                    explained.extend(cells);
                }
            }
        }
    }
    let missing: Vec<(i32, i32)> = retail_set
        .difference(&pack_cells)
        .filter(|cell| !explained.contains(cell))
        .copied()
        .collect();
    let excess: Vec<(i32, i32)> = pack_cells.difference(retail_set).copied().collect();
    if unknown_reason.is_none() && !recorded && !missing.is_empty() {
        unknown_reason = Some("object_world_not_recorded");
    }
    if let Some(reason) = unknown_reason {
        let mut row = unknown_row("object_block", label, Some(level), reason);
        row.retail = Some(u64::try_from(retail_set.len()).unwrap_or(u64::MAX));
        row.pack = Some(u64::try_from(pack_cells.len()).unwrap_or(u64::MAX));
        return row;
    }
    let explained_only: BTreeSet<(i32, i32)> = explained.difference(&pack_cells).copied().collect();
    let classes = class_counts(&class_cells, &explained_only);
    let difference = i64::try_from(missing.len()).unwrap_or(i64::MAX)
        - i64::try_from(excess.len()).unwrap_or(i64::MAX);
    let mut examples: Vec<String> = missing
        .iter()
        .take(5)
        .map(|cell| object_example(index, world_file, level, *cell))
        .collect();
    for cell in excess.iter().take(5usize.saturating_sub(examples.len())) {
        examples.push(format!(
            "pack holds a chunk with no retail source: {}",
            object_example(index, world_file, level, *cell)
        ));
    }
    let mut row = report_row(
        "object_block",
        label,
        Some(level),
        (
            Some(u64::try_from(retail_set.len()).unwrap_or(u64::MAX)),
            Some(u64::try_from(pack_cells.len()).unwrap_or(u64::MAX)),
            Some(u64::try_from(explained_only.len()).unwrap_or(u64::MAX)),
            Some(difference),
        ),
        classes,
        if missing.is_empty() && excess.is_empty() {
            Status::Balanced
        } else {
            Status::Unexplained
        },
        None,
    );
    row.examples = examples;
    row
}

fn object_pack_cells(pack: &Pack, world: Option<&PackWorld>, level: u32) -> BTreeSet<(i32, i32)> {
    let (Some(world), ObjectState::Tables { chunks, .. }) = (world, &pack.objects) else {
        return BTreeSet::new();
    };
    // silent-ok: a world with no object chunk at this level has none
    chunks.get(&(world.id, level)).cloned().unwrap_or_default()
}

fn object_example(index: &FileIndex, world_file: &str, level: u32, cell: (i32, i32)) -> String {
    let path = format!(
        "meshes/terrain/{world_file}/objects/{world_file}.{level}.{}.{}.bto",
        cell.0, cell.1
    );
    index.describe_missing(&[&path], Some(world_file))
}

fn compare_atlases(
    report: &mut ReconcileReport,
    retail: &Retail,
    pack: &Pack,
    selected: Option<&BTreeSet<String>>,
) {
    let mut pack_failure_reported = false;
    let mut entries: Vec<(String, u64, bool)> = Vec::new();
    for (world_file, count) in &retail.atlases {
        let pack_count = atlas_pack_count(pack, world_file);
        entries.push((world_file.clone(), *count, pack_count));
    }
    if let ObjectState::Tables { atlases, .. } = &pack.objects {
        for world_id in atlases {
            if let Some(world) = pack.worlds_by_id.get(world_id) {
                let file = world.editor_id.to_ascii_lowercase();
                if !retail.atlases.contains_key(&file) {
                    entries.push((file, 0, true));
                }
            }
        }
    }
    for (world_file, retail_count, pack_has) in entries {
        let match_ = world_match(&world_file, pack, selected);
        let label = match &match_ {
            WorldMatch::Pack(world) => world.editor_id.clone(),
            _ => world_file.clone(),
        };
        if matches!(match_, WorldMatch::NotSelected) {
            report.push(report_row(
                "object_atlas",
                &label,
                None,
                (Some(retail_count), Some(0), Some(0), None),
                BTreeMap::new(),
                Status::OutOfScope,
                Some("world_not_in_selected_plugins".to_owned()),
            ));
            continue;
        }
        if matches!(match_, WorldMatch::SelectionUnknown) {
            report.push(unknown_row(
                "object_atlas",
                &label,
                None,
                "plugin_selection_unknown",
            ));
            continue;
        }
        if let ObjectState::Unknown(reason) = &pack.objects {
            let mut row = unknown_row("object_atlas", &label, None, reason);
            row.retail = Some(retail_count);
            pack_failure_reported = true;
            report.push(row);
            continue;
        }
        let pack_count = u64::from(pack_has);
        let has_blocks = retail
            .objects
            .get(&world_file)
            .is_some_and(|levels| !levels.is_empty());
        let recorded = pack.objects_recorded_worlds.contains(&world_file);
        let mut explained = 0u64;
        let mut classes = BTreeMap::new();
        let mut status = if retail_count == pack_count {
            Status::Balanced
        } else if retail_count > 0 && pack_count == 0 && !has_blocks {
            Status::OutOfScope
        } else {
            Status::Unexplained
        };
        let mut reason =
            (status == Status::OutOfScope).then(|| "retail_atlas_without_object_blocks".to_owned());
        if status == Status::Unexplained
            && retail_count > 0
            && pack_count == 0
            && has_blocks
            && !recorded
        {
            status = Status::Unknown;
            reason = Some("object_world_not_recorded".to_owned());
        } else if status == Status::Unexplained
            && retail_count > 0
            && pack_count == 0
            && has_blocks
            && object_chunk_count(pack, &world_file) == 0
            && retail_count == 1
            && all_object_blocks_skipped(pack, &world_file, retail.objects.get(&world_file))
        {
            // The stage publishes an atlas only when at least one object block compiled. Every
            // source block was skipped, so the absent atlas is explained by that outcome.
            status = Status::Balanced;
            explained = 1;
            classes.insert("no_object_chunk_compiled".to_owned(), 1);
        }
        let difference = i64::try_from(retail_count).unwrap_or(i64::MAX)
            - i64::try_from(pack_count.saturating_add(explained)).unwrap_or(i64::MAX);
        let mut row = report_row(
            "object_atlas",
            &label,
            None,
            (
                Some(retail_count),
                Some(pack_count),
                Some(explained),
                Some(difference),
            ),
            classes,
            status,
            reason,
        );
        if status == Status::Unexplained {
            let names = atlas_paths(&world_file);
            if retail_count > 1 {
                row.examples.push(format!(
                    "the game data has both {} and {}; the converter refuses an ambiguous atlas",
                    names[0], names[1]
                ));
            } else {
                row.examples.push(
                    retail
                        .index
                        .describe_missing(&[&names[0], &names[1]], Some(&world_file)),
                );
            }
        }
        report.push(row);
    }
    if let ObjectState::Unknown(reason) = &pack.objects
        && !pack_failure_reported
    {
        report.push(unknown_row("object_atlas", "*", None, reason));
    }
}

fn atlas_pack_count(pack: &Pack, world_file: &str) -> bool {
    let (ObjectState::Tables { atlases, .. }, Some(world)) =
        (&pack.objects, pack.worlds.get(world_file))
    else {
        return false;
    };
    atlases.contains(&world.id)
}

fn object_chunk_count(pack: &Pack, world_file: &str) -> usize {
    let (ObjectState::Tables { chunks, .. }, Some(world)) =
        (&pack.objects, pack.worlds.get(world_file))
    else {
        return 0;
    };
    chunks
        .keys()
        .filter(|(world_id, _)| *world_id == world.id)
        .count()
}

fn all_object_blocks_skipped(
    pack: &Pack,
    world_file: &str,
    blocks: Option<&BTreeMap<u32, BTreeSet<(i32, i32)>>>,
) -> bool {
    let Some(blocks) = blocks else {
        return false;
    };
    let current: BTreeSet<(u32, i32, i32)> = blocks
        .iter()
        .flat_map(|(level, cells)| cells.iter().map(|cell| (*level, cell.0, cell.1)))
        .collect();
    if current.is_empty() {
        return false;
    }
    let Some(counts) = &pack.reconciliation else {
        return false;
    };
    let Some(by_level) = counts.object_skips.get(world_file) else {
        return false;
    };
    let mut covered = BTreeSet::new();
    for (level, classes) in by_level {
        for skip in classes.values() {
            // silent-ok: an incomplete record cannot explain the absent atlas
            let consistent = u64::try_from(skip.cells.len()).ok() == Some(skip.blocks);
            if skip.blocks > 0 && consistent {
                for cell in &skip.cells {
                    covered.insert((*level, cell.0, cell.1));
                }
            }
        }
    }
    current.is_subset(&covered)
}

fn compare_large_references(
    report: &mut ReconcileReport,
    pack: &Pack,
    retail: Option<&RnamRetail>,
    reason: Option<&'static str>,
) {
    if let RnamState::Unknown(unknown) = &pack.rnam {
        report.push(unknown_row("large_reference", "*", None, unknown));
    }
    let Some(retail) = retail else {
        if matches!(pack.rnam, RnamState::Tables(_)) {
            report.push(unknown_row(
                "large_reference",
                "*",
                None,
                reason.unwrap_or("retail_rnam_unreadable"),
            ));
        }
        return;
    };
    let RnamState::Tables(pack_counts) = &pack.rnam else {
        return;
    };
    let mut reported_ids = BTreeSet::new();
    for (world, units) in &retail.worlds {
        let (label, counts) = match world {
            RnamWorld::RuntimeId(id) => {
                reported_ids.insert(*id);
                // silent-ok: a world without large-reference rows has none
                let counts = pack_counts.get(id).copied().unwrap_or_default();
                let label = pack
                    .worlds_by_id
                    .get(id)
                    .map(|world| world.editor_id.clone())
                    .or_else(|| retail.labels.get(id).cloned())
                    .unwrap_or_else(|| format!("world {id:08x}"));
                (label, counts)
            }
            RnamWorld::EditorId(editor_id) => {
                let match_ = world_match(editor_id, pack, Some(&retail.selected));
                let label = match &match_ {
                    WorldMatch::Pack(world) => world.editor_id.clone(),
                    _ => editor_id.clone(),
                };
                if matches!(match_, WorldMatch::NotSelected) {
                    if units.total() > 0 {
                        report.push(out_of_scope_row(
                            "large_reference",
                            &label,
                            None,
                            units.total(),
                            "world_not_in_selected_plugins",
                        ));
                    }
                    continue;
                }
                let pack_world = pack.worlds.get(editor_id);
                if let Some(world) = pack_world {
                    reported_ids.insert(world.id);
                }
                let counts = pack_world
                    .and_then(|world| pack_counts.get(&world.id))
                    .copied()
                    // silent-ok: a world without large-reference rows has none
                    .unwrap_or_default();
                (label, counts)
            }
        };
        if units.total() == 0 && counts.projected == 0 && counts.explained() == 0 {
            continue;
        }
        report.push(rnam_row(&label, units.total(), counts));
    }
    let mut ids: BTreeSet<u32> = pack_counts.keys().copied().collect();
    ids.retain(|id| !reported_ids.contains(id));
    for world_id in ids {
        // silent-ok: a world without large-reference rows has none
        let counts = pack_counts.get(&world_id).copied().unwrap_or_default();
        if counts.projected == 0 && counts.explained() == 0 {
            continue;
        }
        let label = pack.worlds_by_id.get(&world_id).map_or_else(
            || format!("world {world_id:08x}"),
            |world| world.editor_id.clone(),
        );
        report.push(rnam_row(&label, 0, counts));
    }
    if retail.unattributed > 0 {
        let mut row = report_row(
            "large_reference",
            "(unattributed)",
            None,
            (
                Some(retail.unattributed),
                Some(0),
                Some(0),
                Some(i64::try_from(retail.unattributed).unwrap_or(i64::MAX)),
            ),
            BTreeMap::new(),
            Status::Unexplained,
            Some("rnam_world_without_edid_or_runtime_id".to_owned()),
        );
        row.examples.push(
            "WRLD RNAM without an EDID or a resolvable runtime FormID cannot be compared"
                .to_owned(),
        );
        report.push(row);
    }
}

fn rnam_row(label: &str, units: u64, counts: PackRnam) -> CategoryReport {
    let explained = counts.explained();
    let difference = i64::try_from(units).unwrap_or(i64::MAX)
        - i64::try_from(counts.projected.saturating_add(explained)).unwrap_or(i64::MAX);
    let mut classes = BTreeMap::new();
    if counts.omitted > 0 {
        classes.insert("rnam_omitted".to_owned(), counts.omitted);
    }
    if counts.removed > 0 {
        classes.insert("confirmed_deleted".to_owned(), counts.removed);
    }
    let status = if difference == 0 {
        Status::Balanced
    } else {
        Status::Unexplained
    };
    let mut row = report_row(
        "large_reference",
        label,
        None,
        (
            Some(units),
            Some(counts.projected),
            Some(explained),
            Some(difference),
        ),
        classes,
        status,
        None,
    );
    if difference != 0 {
        row.examples.push(format!(
            "WRLD RNAM units {units}, large_references {}, omitted {}, confirmed deleted {}",
            counts.projected, counts.omitted, counts.removed
        ));
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lod::{
        objects::{
            OBJECT_COMPILER_VERSION, compile_object_chunk_shared_atlas, publish_object_chunk,
        },
        retail::bto::{ObjectLodFile, ObjectLodSegment, ObjectLodShape, ObjectLodVertex},
    };
    use dummy_content::{Entry, bsa};
    use rusqlite::Connection;
    use serde_json::{Value, json};
    use shared::lod::{ChunkAnchor, ChunkKey, LodOrigin, LodTier};
    use std::fs;
    use tempfile::TempDir;

    fn terrain_block(
        data: &Path,
        world: &str,
        level: u32,
        cell: (i32, i32),
        mesh: bool,
        colour: bool,
        normal: bool,
    ) {
        let meshes = data.join(format!("meshes/terrain/{world}"));
        let textures = data.join(format!("textures/terrain/{world}"));
        fs::create_dir_all(&meshes).unwrap();
        fs::create_dir_all(&textures).unwrap();
        if mesh {
            fs::write(
                meshes.join(format!("{world}.{level}.{}.{}.btr", cell.0, cell.1)),
                b"mesh",
            )
            .unwrap();
        }
        if colour {
            fs::write(
                textures.join(format!("{world}.{level}.{}.{}.dds", cell.0, cell.1)),
                b"colour",
            )
            .unwrap();
        }
        if normal {
            fs::write(
                textures.join(format!("{world}.{level}.{}.{}_n.dds", cell.0, cell.1)),
                b"normal",
            )
            .unwrap();
        }
    }

    fn object_block(data: &Path, world: &str, level: u32, cell: (i32, i32), atlas: Option<&str>) {
        let folder = data.join(format!("meshes/terrain/{world}/objects"));
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(format!("{world}.{level}.{}.{}.bto", cell.0, cell.1)),
            b"object",
        )
        .unwrap();
        if let Some(atlas) = atlas {
            let textures = data.join(format!("textures/terrain/{world}/objects"));
            fs::create_dir_all(&textures).unwrap();
            fs::write(textures.join(atlas), b"atlas").unwrap();
        }
    }

    fn full_data() -> TempDir {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "w", 4, (0, 0), true, true, true);
        object_block(data.path(), "w", 16, (0, 0), Some("w.objects.dds"));
        data
    }

    fn pack_with(worlds: &[(u32, &str)]) -> TempDir {
        let pack = tempfile::tempdir().unwrap();
        let connection = Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        for (id, name) in worlds {
            connection
                .execute(
                    "INSERT INTO worldspaces(id, editor_id, flags, lod_origin_x, lod_origin_y) \
                     VALUES (?1, ?2, 0, 0, 0)",
                    rusqlite::params![id, name],
                )
                .unwrap();
        }
        crate::lod::objects::create_object_chunk_tables(&connection).unwrap();
        pack
    }

    fn pack_dir() -> TempDir {
        pack_with(&[(1, "W"), (2, "V")])
    }

    fn add_plugins(pack: &Path) {
        let connection = Connection::open(pack.join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO plugins(id, name, priority, checksum) VALUES (0, 'Test.esm', 0, X'00')",
                [],
            )
            .unwrap();
    }

    fn add_plugin(pack: &Path) {
        add_plugins(pack);
        let connection = Connection::open(pack.join("skyrim_world.db")).unwrap();
        connection
            .execute("INSERT INTO large_references VALUES (1, 0, 0, 7)", [])
            .unwrap();
    }

    fn write_manifest(pack: &Path, manifest: &Value) {
        fs::write(
            pack.join("lod-manifest.json"),
            serde_json::to_vec_pretty(manifest).unwrap(),
        )
        .unwrap();
    }

    fn retail_manifest(worlds: &[&str]) -> Value {
        json!({
            "build_identity": "0".repeat(64),
            "converter_schema": 28,
            "world_database_schema": 9,
            "chunks": 0,
            "terrain_sources": {},
            "terrain_lod_source": "retail",
            "retail_terrain_import_version": 1,
            "retail_terrain_inputs": {},
            "object_chunks": 0,
            "objects_compiler_version": 1,
            "object_chunk_inputs": {},
            "object_atlases": {},
            "reconciliation": {
                "format": crate::lod::reconciliation::FORMAT,
                "terrain_recorded_worlds": worlds,
                "objects_recorded_worlds": worlds,
            },
        })
    }

    fn payload(pack: &Path, relative: &str, bytes: &[u8]) -> String {
        let path = pack.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        crate::cache::hash_bytes(bytes)
    }

    fn add_terrain_chunk(
        pack: &Path,
        manifest: &mut Value,
        world: u32,
        level: u32,
        cell: (i32, i32),
    ) -> String {
        let side = i32::try_from(level).unwrap();
        let (anchor_x, anchor_y) = (cell.0 / side, cell.1 / side);
        let path = format!("lod/{world:08x}/{level}/cell_{anchor_x}_{anchor_y}.glb");
        let hash = payload(pack, &path, b"terrain");
        let connection = Connection::open(pack.join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO lod_chunks(worldspace_id, tier, anchor_x, anchor_y, payload_path, \
                 content_hash, bounds_min_x, bounds_min_y, bounds_min_z, bounds_max_x, \
                 bounds_max_y, bounds_max_z, source_cells) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 0, 1, 1, 1, '0,0')",
                rusqlite::params![world, level, anchor_x, anchor_y, path, hash],
            )
            .unwrap();
        manifest["retail_terrain_inputs"][&path] = json!("input");
        manifest["chunks"] = json!(manifest["chunks"].as_u64().unwrap_or(0) + 1);
        path
    }

    fn test_shape() -> ObjectLodShape {
        let vertex = |position| ObjectLodVertex {
            position,
            normal: Some([0.0, 0.0, 1.0]),
            uv: Some([0.5, 0.5]),
            color: Some([255; 4]),
        };
        ObjectLodShape {
            large_reference: false,
            block_index: 1,
            flags: 0,
            translation: [0.0, -65_536.0, 0.0],
            rotation_columns: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            scale: 16.0,
            shader_property: u32::MAX,
            vertices: vec![
                vertex([0.0, 0.0, 0.0]),
                vertex([10.0, 0.0, 0.0]),
                vertex([0.0, 10.0, 0.0]),
            ],
            triangles: vec![[0, 1, 2]],
            segments: vec![ObjectLodSegment {
                flag: 0,
                start_index: 0,
                num_primitives: 1,
            }],
        }
    }

    fn test_block() -> ObjectLodFile {
        ObjectLodFile {
            user_version: 12,
            bethesda_version: 100,
            block_types: BTreeMap::new(),
            unhandled_blocks: BTreeMap::new(),
            shapes: vec![test_shape()],
            large_ref_blocks: Vec::new(),
        }
    }

    fn add_object_chunk(
        pack: &Path,
        manifest: &mut Value,
        world: u32,
        level: u32,
        cell: (i32, i32),
    ) -> String {
        let side = i32::try_from(level).unwrap();
        let (anchor_x, anchor_y) = (cell.0 / side, cell.1 / side);
        let tier = LodTier::from_side_cells(side).unwrap();
        let key = ChunkKey::new(world, tier, ChunkAnchor::new(anchor_x, anchor_y));
        let chunk = compile_object_chunk_shared_atlas(key, LodOrigin::new(0, 0), &test_block())
            .expect("the test object chunk compiles");
        let connection = Connection::open(pack.join("skyrim_world.db")).unwrap();
        publish_object_chunk(&connection, pack, &chunk).expect("the test object chunk publishes");
        let path = shared::lod::object_chunk_payload_path(key);
        manifest["object_chunks"] = json!(manifest["object_chunks"].as_u64().unwrap_or(0) + 1);
        manifest["object_chunk_inputs"][&path] = json!("input");
        manifest["objects_compiler_version"] = json!(OBJECT_COMPILER_VERSION);
        path
    }

    fn add_atlas(pack: &Path, manifest: &mut Value, world: u32) -> String {
        let dds = dummy_content::dds::generate(
            &dummy_content::dds::Spec::new(dummy_content::dds::Format::X8R8G8B8, 8, 8),
            &mut dummy_content::rng::Rng::new(0),
        )
        .unwrap();
        let ktx2 = crate::texture::TextureConverter::convert(
            &dds,
            crate::texture::TextureEncoding::ColorSrgb,
        )
        .unwrap();
        let path = shared::lod::object_atlas_path(world);
        let hash = payload(pack, &path, &ktx2);
        let connection = Connection::open(pack.join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO lod_object_atlases VALUES (?1, ?2, ?3)",
                rusqlite::params![world, path, hash],
            )
            .unwrap();
        manifest["object_atlases"][&path] = json!(hash);
        path
    }

    fn record(tag: &[u8; 4], form_id: u32, subs: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut payload = Vec::new();
        for (tag, data) in subs {
            payload.extend_from_slice(*tag);
            payload.extend_from_slice(&u16::try_from(data.len()).unwrap().to_le_bytes());
            payload.extend_from_slice(data);
        }
        let mut record = Vec::new();
        record.extend_from_slice(tag);
        record.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
        record.extend_from_slice(&0u32.to_le_bytes());
        record.extend_from_slice(&form_id.to_le_bytes());
        record.extend_from_slice(&0u32.to_le_bytes());
        record.extend_from_slice(&44u16.to_le_bytes());
        record.extend_from_slice(&0u16.to_le_bytes());
        record.extend_from_slice(&payload);
        record
    }

    fn rnam(key: [u8; 4], members: &[(u32, [u8; 4])]) -> Vec<u8> {
        let mut bytes = key.to_vec();
        bytes.extend(u32::try_from(members.len()).unwrap().to_le_bytes());
        for (id, placement) in members {
            bytes.extend(id.to_le_bytes());
            bytes.extend(placement);
        }
        bytes
    }

    fn write_plugin_with(data: &Path, editor_id: Option<&[u8]>, members: &[(u32, [u8; 4])]) {
        let mut subs = Vec::new();
        if let Some(editor_id) = editor_id {
            subs.push((b"EDID", editor_id.to_vec()));
        }
        subs.push((b"RNAM", rnam([0; 4], members)));
        let mut plugin = record(b"TES4", 0, &[]);
        plugin.extend(record(b"WRLD", 1, &subs));
        fs::write(data.join("Test.esm"), plugin).unwrap();
    }

    fn write_plugin(data: &Path) {
        write_plugin_with(data, Some(b"W\0"), &[(7, [0; 4])]);
    }

    /// A master WLRD with an EDID and an override of the same FormID without one.
    fn write_override_plugin_pair(data: &Path) {
        let master_world = record(
            b"WRLD",
            1,
            &[
                (b"EDID", b"W\0".to_vec()),
                (b"RNAM", rnam([0; 4], &[(7, [0; 4])])),
            ],
        );
        fs::write(
            data.join("Test.esm"),
            [record(b"TES4", 0, &[]), master_world].concat(),
        )
        .unwrap();
        let override_world = record(b"WRLD", 1, &[(b"RNAM", rnam([0; 4], &[(7, [0; 4])]))]);
        let header = record(b"TES4", 0, &[(b"MAST", b"Test.esm\0".to_vec())]);
        fs::write(data.join("Override.esm"), [header, override_world].concat()).unwrap();
    }

    fn check(data: &Path, pack: &Path) -> ReconcileReport {
        let report = run(&Options {
            data: data.to_owned(),
            pack: pack.to_owned(),
            json: None,
        })
        .unwrap();
        assert_eq!(report.format, FORMAT);
        report
    }

    fn row<'a>(
        report: &'a ReconcileReport,
        category: &str,
        world: &str,
        level: Option<u32>,
    ) -> &'a CategoryReport {
        report
            .categories
            .iter()
            .find(|row| row.category == category && row.world == world && row.level == level)
            .unwrap_or_else(|| {
                panic!(
                    "no {category} row for {world} level {level:?}; rows: {:#?}",
                    report.categories
                )
            })
    }

    #[test]
    fn rnam_overrides_aggregate_under_one_runtime_world() {
        let data = full_data();
        write_override_plugin_pair(data.path());
        let pack = pack_dir();
        let connection = Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO plugins(id, name, priority, checksum) VALUES \
                 (0, 'Test.esm', 0, X'00'), (1, 'Override.esm', 1, X'00')",
                [],
            )
            .unwrap();
        connection
            .execute("INSERT INTO large_references VALUES (1, 0, 0, 7)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO large_reference_omissions VALUES (1, NULL, NULL, 1, 0)",
                [],
            )
            .unwrap();
        drop(connection);
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "large_reference", "W", None);
        assert_eq!(row.retail, Some(2), "{:#?}", row);
        assert_eq!(row.pack, Some(1));
        assert_eq!(row.explained, Some(1));
        assert_eq!(row.status, Status::Balanced);
    }

    #[test]
    fn a_balanced_pack_has_no_unexplained_difference() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(report.passed(), "{:#?}", report.categories);
        assert_eq!(
            row(&report, "terrain_mesh", "W", Some(4)).status,
            Status::Balanced
        );
        assert_eq!(
            row(&report, "object_block", "W", Some(16)).status,
            Status::Balanced
        );
        assert_eq!(
            row(&report, "object_atlas", "W", None).status,
            Status::Balanced
        );
        assert_eq!(
            row(&report, "large_reference", "W", None).status,
            Status::Balanced
        );
    }

    #[test]
    fn an_unexplained_missing_file_fails_and_names_the_source() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "w", 4, (0, 0), true, true, true);
        terrain_block(data.path(), "w", 4, (4, 0), true, true, true);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(!report.passed());
        let mesh = row(&report, "terrain_mesh", "W", Some(4));
        assert_eq!(mesh.status, Status::Unexplained);
        assert_eq!(mesh.retail, Some(2));
        assert_eq!(mesh.pack, Some(1));
        assert_eq!(mesh.unexplained, Some(1));
        assert!(
            mesh.examples
                .iter()
                .any(|example| example.contains("w.4.4.0.btr")),
            "{:?}",
            mesh.examples
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("meshes/"), "{json}");
        assert!(!json.contains(".btr"), "{json}");
    }

    #[test]
    fn an_explained_skip_balances_the_category() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "w", 4, (0, 0), true, true, true);
        terrain_block(data.path(), "w", 4, (4, 0), true, false, true);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        manifest["reconciliation"]["terrain_skips"] = json!({
            "w": {
                "4": {
                    "missing_colour_map": {
                        "blocks": 1,
                        "mesh": 1,
                        "colour": 0,
                        "normal": 1,
                        "cells": [[4, 0, 5]]
                    }
                }
            }
        });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(report.passed(), "{:#?}", report.categories);
        let mesh = row(&report, "terrain_mesh", "W", Some(4));
        assert_eq!(mesh.retail, Some(2));
        assert_eq!(mesh.pack, Some(1));
        assert_eq!(mesh.explained, Some(1));
        assert_eq!(mesh.explained_by_class["missing_colour_map"], 1);
        let colour = row(&report, "terrain_colour", "W", Some(4));
        assert_eq!(colour.retail, Some(1));
        assert_eq!(colour.explained, Some(0));
        let normal = row(&report, "terrain_normal", "W", Some(4));
        assert_eq!(normal.retail, Some(2));
        assert_eq!(normal.explained, Some(1));
    }

    #[test]
    fn a_file_that_appeared_after_conversion_is_not_explained() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "w", 4, (0, 0), true, true, true);
        terrain_block(data.path(), "w", 4, (4, 0), true, false, false);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        manifest["reconciliation"]["terrain_skips"] = json!({
            "w": { "4": { "missing_colour_map": {
                "blocks": 1, "mesh": 1, "colour": 0, "normal": 0, "cells": [[4, 0, 1]]
            }}}
        });
        write_manifest(pack.path(), &manifest);
        // The colour map appears after the conversion. The recorded mask still says it was absent,
        // so it must not explain this new file.
        terrain_block(data.path(), "w", 4, (4, 0), true, true, false);

        let report = check(data.path(), pack.path());
        let colour = row(&report, "terrain_colour", "W", Some(4));
        assert_eq!(colour.status, Status::Unexplained);
        assert_eq!(colour.retail, Some(2));
        assert_eq!(colour.explained, Some(0));
    }

    #[test]
    fn both_atlas_name_forms_are_balanced() {
        let data = tempfile::tempdir().unwrap();
        object_block(data.path(), "w", 16, (0, 0), Some("w.objects.dds"));
        object_block(data.path(), "v", 16, (0, 0), Some("vobjectslod.dds"));
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w", "v"]);
        add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_object_chunk(pack.path(), &mut manifest, 2, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        add_atlas(pack.path(), &mut manifest, 2);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(report.passed(), "{:#?}", report.categories);
        assert_eq!(row(&report, "object_atlas", "W", None).retail, Some(1));
        assert_eq!(row(&report, "object_atlas", "V", None).retail, Some(1));
    }

    #[test]
    fn both_atlas_forms_in_one_world_are_unexplained() {
        let data = tempfile::tempdir().unwrap();
        object_block(data.path(), "w", 16, (0, 0), Some("w.objects.dds"));
        object_block(data.path(), "w", 16, (0, 0), Some("wobjectslod.dds"));
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let manifest = retail_manifest(&["w"]);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let atlas = row(&report, "object_atlas", "W", None);
        assert_eq!(atlas.status, Status::Unexplained);
        assert_eq!(atlas.retail, Some(2));
        assert!(
            atlas
                .examples
                .iter()
                .any(|example| example.contains("both")),
            "{:?}",
            atlas.examples
        );
    }

    #[test]
    fn an_uncountable_pack_is_unknown_not_zero() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let report = check(data.path(), pack.path());
        assert!(!report.passed());
        let mesh = row(&report, "terrain_mesh", "W", Some(4));
        assert_eq!(mesh.status, Status::Unknown);
        assert_eq!(mesh.retail, Some(1));
        assert_eq!(mesh.pack, None);
        assert_eq!(mesh.unexplained, None);
        assert_eq!(mesh.reason.as_deref(), Some("no_lod_manifest"));
    }

    #[test]
    fn archives_are_counted_with_the_dotted_and_objectslod_names() {
        let data = tempfile::tempdir().unwrap();
        let bytes = bsa::v105(
            &[
                Entry::new("Meshes/Terrain/W/W.4.0.0.btr", b"mesh"),
                Entry::new("Textures/Terrain/W/W.4.0.0.dds", b"colour"),
                Entry::new("Textures/Terrain/W/W.4.0.0_n.dds", b"normal"),
                Entry::new("Meshes/Terrain/W/Objects/W.16.0.0.bto", b"object"),
                Entry::new("Textures/Terrain/W/Objects/W.objects.dds", b"atlas"),
            ],
            bsa::Compression::None,
        )
        .unwrap();
        fs::write(data.path().join("Terrain.bsa"), bytes).unwrap();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(report.passed(), "{:#?}", report.categories);
        assert_eq!(row(&report, "terrain_mesh", "W", Some(4)).retail, Some(1));
    }

    #[test]
    fn a_loose_file_overrides_the_archive_count() {
        let data = tempfile::tempdir().unwrap();
        let bytes = bsa::v105(
            &[
                Entry::new("Meshes/Terrain/W/W.4.0.0.btr", b"archive mesh"),
                Entry::new("Textures/Terrain/W/W.4.0.0.dds", b"archive colour"),
                Entry::new("Textures/Terrain/W/W.4.0.0_n.dds", b"archive normal"),
            ],
            bsa::Compression::None,
        )
        .unwrap();
        fs::write(data.path().join("Terrain.bsa"), bytes).unwrap();
        terrain_block(data.path(), "w", 4, (0, 0), true, true, true);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(report.passed(), "{:#?}", report.categories);
        assert_eq!(row(&report, "terrain_mesh", "W", Some(4)).retail, Some(1));
    }

    #[test]
    fn the_same_path_in_two_archives_is_counted_once() {
        let data = tempfile::tempdir().unwrap();
        let entries = [
            Entry::new("Meshes/Terrain/W/W.4.0.0.btr", b"mesh"),
            Entry::new("Textures/Terrain/W/W.4.0.0.dds", b"colour"),
            Entry::new("Textures/Terrain/W/W.4.0.0_n.dds", b"normal"),
        ];
        fs::write(
            data.path().join("One.bsa"),
            bsa::v105(&entries, bsa::Compression::None).unwrap(),
        )
        .unwrap();
        fs::write(
            data.path().join("Two.bsa"),
            bsa::v105(&entries, bsa::Compression::None).unwrap(),
        )
        .unwrap();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(report.passed(), "{:#?}", report.categories);
        assert_eq!(row(&report, "terrain_mesh", "W", Some(4)).retail, Some(1));
    }

    #[test]
    fn a_retail_listing_failure_is_unknown() {
        let data = tempfile::tempdir().unwrap();
        fs::write(data.path().join("Bad.bsa"), b"not an archive").unwrap();
        let pack = pack_dir();
        let report = check(data.path(), pack.path());
        let row = report
            .categories
            .iter()
            .find(|row| row.reason.as_deref() == Some("retail_listing_failed"))
            .expect("a retail_listing_failed row");
        assert_eq!(row.status, Status::Unknown);
        assert_eq!(row.retail, None);
    }

    #[test]
    fn a_plugin_selection_that_cannot_be_read_is_unknown() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "v", 4, (0, 0), true, true, true);
        let pack = pack_with(&[(1, "W")]);
        let manifest = retail_manifest(&[]);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "terrain_mesh", "v", Some(4));
        assert_eq!(row.status, Status::Unknown);
        assert_eq!(row.reason.as_deref(), Some("plugin_selection_unknown"));
        assert_eq!(row.retail, Some(1));
    }

    #[test]
    fn a_world_outside_the_selected_plugins_is_out_of_scope() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "v", 4, (0, 0), true, true, true);
        write_plugin(data.path());
        let pack = pack_with(&[(1, "W")]);
        add_plugin(pack.path());
        let manifest = retail_manifest(&["w"]);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "terrain_mesh", "v", Some(4));
        assert_eq!(row.status, Status::OutOfScope);
        assert_eq!(row.reason.as_deref(), Some("world_not_in_selected_plugins"));
    }

    #[test]
    fn a_selected_world_missing_from_the_pack_is_unexplained() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "w", 4, (0, 0), true, false, false);
        write_plugin(data.path());
        let pack = pack_with(&[(2, "V")]);
        add_plugins(pack.path());
        let manifest = retail_manifest(&[]);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "terrain_mesh", "w", Some(4));
        assert_eq!(row.status, Status::Unexplained);
        assert_eq!(
            row.reason.as_deref(),
            Some("world_is_selected_but_absent_from_the_pack")
        );
        assert!(
            report.categories.iter().all(|row| {
                row.category != "terrain_colour" && row.category != "terrain_normal"
            }),
            "{:#?}",
            report.categories
        );
    }

    #[test]
    fn an_incomplete_skip_is_unknown() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "w", 4, (0, 0), true, true, true);
        terrain_block(data.path(), "w", 4, (4, 0), true, false, false);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        manifest["reconciliation"]["terrain_skips"] = json!({
            "w": { "4": { "missing_colour_map": {
                "blocks": 2, "mesh": 2, "colour": 0, "normal": 0, "cells": [[4, 0, 1]]
            }}}
        });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "terrain_mesh", "W", Some(4));
        assert_eq!(row.status, Status::Unknown);
        assert_eq!(
            row.reason.as_deref(),
            Some("incomplete_terrain_reconciliation")
        );
    }

    #[test]
    fn a_skip_does_not_count_a_packed_cell_as_explained() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "w", 4, (0, 0), true, true, true);
        terrain_block(data.path(), "w", 4, (4, 0), true, true, true);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (4, 0));
        manifest["reconciliation"]["terrain_skips"] = json!({
            "w": { "4": { "block cannot be read": {
                "blocks": 1, "mesh": 1, "colour": 1, "normal": 1, "cells": [[4, 0, 7]]
            }}}
        });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let mesh = row(&report, "terrain_mesh", "W", Some(4));
        assert_eq!(mesh.explained, Some(0));
        assert!(mesh.explained_by_class.is_empty(), "{mesh:#?}");
        assert_eq!(mesh.status, Status::Balanced);
    }

    #[test]
    fn a_cell_is_credited_to_one_reason_class() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "w", 4, (0, 0), true, true, true);
        terrain_block(data.path(), "w", 4, (4, 0), true, false, false);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        manifest["reconciliation"]["terrain_skips"] = json!({
            "w": { "4": {
                "block cannot be read": {
                    "blocks": 1, "mesh": 1, "colour": 0, "normal": 0, "cells": [[4, 0, 1]]
                },
                "missing_colour_map": {
                    "blocks": 1, "mesh": 1, "colour": 0, "normal": 0, "cells": [[4, 0, 1]]
                }
            }}
        });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let mesh = row(&report, "terrain_mesh", "W", Some(4));
        assert_eq!(mesh.explained, Some(1));
        assert_eq!(mesh.explained_by_class.values().sum::<u64>(), 1);
        assert_eq!(mesh.status, Status::Balanced);
    }

    #[test]
    fn a_level_without_a_runtime_tier_is_explained() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "w", 32, (0, 0), true, true, true);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        manifest["reconciliation"]["terrain_skips"] = json!({
            "w": { "32": { "no_runtime_tier": {
                "blocks": 1, "mesh": 1, "colour": 1, "normal": 1, "cells": [[0, 0, 7]]
            }}}
        });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(report.passed(), "{:#?}", report.categories);
        let mesh = row(&report, "terrain_mesh", "W", Some(32));
        assert_eq!(mesh.status, Status::Balanced);
        assert_eq!(mesh.explained_by_class["no_runtime_tier"], 1);
    }

    #[test]
    fn object_skips_explain_the_absent_atlas_when_no_chunk_compiled() {
        let data = tempfile::tempdir().unwrap();
        object_block(data.path(), "w", 16, (0, 0), Some("w.objects.dds"));
        object_block(data.path(), "w", 16, (4, 0), Some("w.objects.dds"));
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        manifest["reconciliation"]["object_skips"] = json!({
            "w": { "16": { "block cannot be read": { "blocks": 2, "cells": [[0, 0], [4, 0]] } } }
        });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(report.passed(), "{:#?}", report.categories);
        assert_eq!(
            row(&report, "object_block", "W", Some(16)).status,
            Status::Balanced
        );
        let atlas = row(&report, "object_atlas", "W", None);
        assert_eq!(atlas.status, Status::Balanced);
        assert_eq!(atlas.explained_by_class["no_object_chunk_compiled"], 1);
    }

    #[test]
    fn a_missing_atlas_is_explained_for_every_object_block() {
        let data = tempfile::tempdir().unwrap();
        object_block(data.path(), "w", 16, (0, 0), None);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        manifest["reconciliation"]["object_world_skips"] = json!({ "w": "missing_atlas" });
        manifest["reconciliation"]["object_world_skip_blocks"] = json!({ "w": [[16, 0, 0]] });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "object_block", "W", Some(16));
        assert_eq!(row.status, Status::Balanced);
        assert_eq!(row.explained_by_class["missing_atlas"], 1);
    }

    #[test]
    fn a_missing_atlas_does_not_explain_a_new_block() {
        let data = tempfile::tempdir().unwrap();
        object_block(data.path(), "w", 16, (0, 0), None);
        object_block(data.path(), "w", 16, (4, 0), None);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        manifest["reconciliation"]["object_world_skips"] = json!({ "w": "missing_atlas" });
        manifest["reconciliation"]["object_world_skip_blocks"] = json!({ "w": [[16, 0, 0]] });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "object_block", "W", Some(16));
        assert_eq!(row.status, Status::Unexplained);
        assert_eq!(row.retail, Some(2));
        assert_eq!(row.explained, Some(1));
        assert_eq!(row.unexplained, Some(1));
    }

    #[test]
    fn a_legacy_missing_atlas_coverage_is_unknown() {
        let data = tempfile::tempdir().unwrap();
        object_block(data.path(), "w", 16, (0, 0), None);
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        manifest["reconciliation"]["object_world_skips"] = json!({ "w": "missing_atlas" });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "object_block", "W", Some(16));
        assert_eq!(row.status, Status::Unknown);
        assert_eq!(
            row.reason.as_deref(),
            Some("missing_atlas_coverage_unknown")
        );
    }

    #[test]
    fn object_files_outside_meshes_or_levels_are_out_of_scope() {
        let data = tempfile::tempdir().unwrap();
        let textures = data.path().join("textures/terrain/w/objects");
        fs::create_dir_all(&textures).unwrap();
        fs::write(textures.join("w.16.0.0.bto"), b"object").unwrap();
        let meshes = data.path().join("meshes/terrain/w/objects");
        fs::create_dir_all(&meshes).unwrap();
        fs::write(meshes.join("w.32.0.0.bto"), b"object").unwrap();
        let pack = pack_dir();
        let manifest = retail_manifest(&[]);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let rows: Vec<&CategoryReport> = report
            .categories
            .iter()
            .filter(|row| row.category == "object_block" && row.status == Status::OutOfScope)
            .collect();
        assert_eq!(rows.len(), 2, "{:#?}", report.categories);
        assert!(
            rows.iter()
                .any(|row| row.reason.as_deref() == Some("object_block_outside_meshes"))
        );
        assert!(
            rows.iter()
                .any(|row| row.reason.as_deref() == Some("object_level_not_compiled"))
        );
    }

    #[test]
    fn a_manifest_terrain_listing_failure_is_unknown() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        manifest["reconciliation"]["terrain_listing_failed"] = json!(true);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "terrain_mesh", "W", Some(4));
        assert_eq!(row.status, Status::Unknown);
        assert_eq!(row.reason.as_deref(), Some("terrain_listing_failed"));
    }

    #[test]
    fn a_plugins_table_error_is_unknown() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        let connection = Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        connection
            .execute_batch("DROP TABLE plugins; CREATE TABLE plugins (wrong TEXT);")
            .unwrap();
        drop(connection);
        let manifest = retail_manifest(&["w"]);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = report
            .categories
            .iter()
            .find(|row| row.reason.as_deref() == Some("plugins_table_unreadable"))
            .expect("a plugins_table_unreadable row");
        assert_eq!(row.category, "large_reference");
        assert_eq!(row.status, Status::Unknown);
    }

    #[test]
    fn a_missing_atlas_payload_is_unknown() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        let atlas = add_atlas(pack.path(), &mut manifest, 1);
        fs::remove_file(pack.path().join(&atlas)).unwrap();
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "object_atlas", "W", None);
        assert_eq!(row.status, Status::Unknown);
        assert_eq!(row.reason.as_deref(), Some("object_lod_package_invalid"));
    }

    #[test]
    fn a_pack_validation_failure_without_source_files_is_unknown() {
        let data = tempfile::tempdir().unwrap();
        let pack = pack_dir();
        let mut manifest = retail_manifest(&["w"]);
        let terrain = add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        let object = add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        fs::remove_file(pack.path().join(&terrain)).unwrap();
        fs::remove_file(pack.path().join(&object)).unwrap();
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        for category in ["terrain_mesh", "object_block", "object_atlas"] {
            let row = report
                .categories
                .iter()
                .find(|row| row.category == category && row.world == "*")
                .unwrap_or_else(|| panic!("no category-wide {category} row"));
            assert_eq!(row.status, Status::Unknown, "{row:#?}");
        }
        assert!(!report.passed());
    }

    #[test]
    fn both_atlas_forms_with_all_blocks_skipped_are_unexplained() {
        let data = tempfile::tempdir().unwrap();
        object_block(data.path(), "w", 16, (0, 0), Some("w.objects.dds"));
        object_block(data.path(), "w", 16, (0, 0), Some("wobjectslod.dds"));
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        manifest["reconciliation"]["object_skips"] = json!({
            "w": { "16": { "block cannot be read": { "blocks": 1, "cells": [[0, 0]] } } }
        });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let atlas = row(&report, "object_atlas", "W", None);
        assert_eq!(atlas.status, Status::Unexplained);
        assert_eq!(atlas.retail, Some(2));
        assert_eq!(atlas.explained, Some(0));
    }

    #[test]
    fn a_removed_block_skip_does_not_explain_a_new_block_atlas() {
        let data = tempfile::tempdir().unwrap();
        object_block(data.path(), "w", 16, (4, 0), Some("w.objects.dds"));
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        manifest["reconciliation"]["object_skips"] = json!({
            "w": { "16": { "block cannot be read": { "blocks": 1, "cells": [[0, 0]] } } }
        });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let object = row(&report, "object_block", "W", Some(16));
        assert_eq!(object.status, Status::Unexplained);
        let atlas = row(&report, "object_atlas", "W", None);
        assert_eq!(atlas.status, Status::Unexplained);
    }

    #[test]
    fn a_pack_failure_is_unknown_when_all_source_files_are_out_of_scope() {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "v", 4, (0, 0), true, true, true);
        object_block(data.path(), "v", 16, (0, 0), Some("v.objects.dds"));
        write_plugin(data.path());
        let pack = pack_with(&[(1, "W")]);
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        let terrain = add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        let object = add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        fs::remove_file(pack.path().join(&terrain)).unwrap();
        fs::remove_file(pack.path().join(&object)).unwrap();
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(
            report
                .categories
                .iter()
                .any(|row| row.world == "v" && row.status == Status::OutOfScope),
            "{:#?}",
            report.categories
        );
        for category in ["terrain_mesh", "object_block", "object_atlas"] {
            let row = report
                .categories
                .iter()
                .find(|row| row.category == category && row.world == "*")
                .unwrap_or_else(|| {
                    panic!("no category-wide {category} row: {:#?}", report.categories)
                });
            assert_eq!(row.status, Status::Unknown, "{row:#?}");
        }
    }

    #[test]
    fn a_terrain_only_record_does_not_claim_object_skips() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        manifest["reconciliation"]["objects_recorded_worlds"] = json!([]);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let object = row(&report, "object_block", "W", Some(16));
        assert_eq!(object.status, Status::Unknown);
        assert_eq!(object.reason.as_deref(), Some("object_world_not_recorded"));
    }

    #[test]
    fn a_missing_terrain_chunk_payload_is_unknown() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        let path = add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        fs::remove_file(pack.path().join(&path)).unwrap();
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "terrain_mesh", "W", Some(4));
        assert_eq!(row.status, Status::Unknown);
        assert_eq!(row.reason.as_deref(), Some("terrain_chunk_payload_missing"));
    }

    #[test]
    fn a_terrain_chunk_hash_mismatch_is_unknown() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        let path = add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        fs::write(pack.path().join(&path), b"changed").unwrap();
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "terrain_mesh", "W", Some(4));
        assert_eq!(row.status, Status::Unknown);
        assert_eq!(row.reason.as_deref(), Some("terrain_chunk_hash_mismatch"));
    }

    #[test]
    fn a_terrain_manifest_row_mismatch_is_unknown() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        manifest["retail_terrain_inputs"] = json!({});
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "terrain_mesh", "W", Some(4));
        assert_eq!(row.status, Status::Unknown);
        assert_eq!(row.reason.as_deref(), Some("terrain_manifest_row_mismatch"));
    }

    #[test]
    fn a_missing_object_payload_is_unknown() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        let path = add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        fs::remove_file(pack.path().join(&path)).unwrap();
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "object_block", "W", Some(16));
        assert_eq!(row.status, Status::Unknown);
        assert_eq!(row.reason.as_deref(), Some("object_lod_package_invalid"));
    }

    #[test]
    fn a_wrong_world_atlas_path_is_unknown() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        write_manifest(pack.path(), &manifest);
        let connection = Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO lod_object_atlases VALUES (1, ?1, 'h')",
                [shared::lod::object_atlas_path(2)],
            )
            .unwrap();
        drop(connection);

        let report = check(data.path(), pack.path());
        let atlas = row(&report, "object_atlas", "W", None);
        assert_eq!(atlas.status, Status::Unknown);
        assert_eq!(atlas.reason.as_deref(), Some("object_lod_package_invalid"));
    }

    #[test]
    fn an_omitted_rnam_is_an_explained_difference() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugins(pack.path());
        let connection = Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO large_reference_omissions VALUES (1, NULL, NULL, 1, 0)",
                [],
            )
            .unwrap();
        drop(connection);
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "large_reference", "W", None);
        assert_eq!(row.retail, Some(1));
        assert_eq!(row.pack, Some(0));
        assert_eq!(row.explained, Some(1));
        assert_eq!(row.explained_by_class["rnam_omitted"], 1);
        assert_eq!(row.status, Status::Balanced);
    }

    #[test]
    fn an_unexplained_rnam_fails_the_category() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugins(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "large_reference", "W", None);
        assert_eq!(row.retail, Some(1));
        assert_eq!(row.unexplained, Some(1));
        assert_eq!(row.status, Status::Unexplained);
    }

    #[test]
    fn duplicate_rnam_tuples_are_counted_once() {
        let data = full_data();
        write_plugin_with(data.path(), Some(b"W\0"), &[(7, [0; 4]), (7, [0; 4])]);
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "large_reference", "W", None);
        assert_eq!(row.retail, Some(1));
        assert_eq!(row.pack, Some(1));
        assert_eq!(row.status, Status::Balanced);
    }

    #[test]
    fn an_edid_less_world_rnam_is_explained_by_its_omissions() {
        let data = full_data();
        write_plugin_with(data.path(), None, &[(7, [0; 4])]);
        let pack = pack_dir();
        add_plugins(pack.path());
        let connection = Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO large_reference_omissions VALUES (1, NULL, NULL, 1, 0)",
                [],
            )
            .unwrap();
        drop(connection);
        let mut manifest = retail_manifest(&["w"]);
        add_terrain_chunk(pack.path(), &mut manifest, 1, 4, (0, 0));
        add_object_chunk(pack.path(), &mut manifest, 1, 16, (0, 0));
        add_atlas(pack.path(), &mut manifest, 1);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "large_reference", "W", None);
        assert_eq!(row.retail, Some(1));
        assert_eq!(row.explained, Some(1));
        assert_eq!(row.status, Status::Balanced);
    }

    #[cfg(unix)]
    #[test]
    fn a_hard_link_to_a_pack_file_is_not_overwritten() {
        let pack = pack_dir();
        let manifest_path = pack.path().join("lod-manifest.json");
        fs::write(&manifest_path, b"pack manifest").unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = outside.path().join("report.json");
        fs::hard_link(&manifest_path, &link).unwrap();
        let report = ReconcileReport::new();
        let error = write_json(&link, &report).unwrap_err();
        assert!(error.to_string().contains("must not exist"), "{error}");
        assert_eq!(fs::read(&manifest_path).unwrap(), b"pack manifest");
        // A new path outside both inputs still works.
        write_json(&outside.path().join("fresh.json"), &report).unwrap();
    }

    #[test]
    fn the_report_cannot_be_written_inside_a_pack_or_the_game_folder() {
        let data = full_data();
        let pack = pack_dir();
        let error = run(&Options {
            data: data.path().to_owned(),
            pack: pack.path().to_owned(),
            json: Some(pack.path().join("report.json")),
        })
        .unwrap_err();
        assert!(error.to_string().contains("outside the pack"), "{error}");
        let error = run(&Options {
            data: data.path().to_owned(),
            pack: pack.path().to_owned(),
            json: Some(data.path().join("report.json")),
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("outside the game data folder"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_into_the_pack_is_refused() {
        let data = full_data();
        let pack = pack_dir();
        let links = tempfile::tempdir().unwrap();
        let link = links.path().join("link");
        std::os::unix::fs::symlink(pack.path(), &link).unwrap();
        let error = run(&Options {
            data: data.path().to_owned(),
            pack: pack.path().to_owned(),
            json: Some(link.join("report.json")),
        })
        .unwrap_err();
        assert!(error.to_string().contains("outside the pack"), "{error}");
    }
}
