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
        reconciliation::{LodReconciliation, NO_RUNTIME_TIER},
        retail::name::{RetailLodKind, parse_lod_file_name},
        retail_terrain::tier_for_level,
        retail_terrain_source::{BlockId, Kind, canonical_path, classify},
        reuse::LodManifest,
    },
    records::source_identity::resolve,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use shared::lod::{ChunkAnchor, ChunkKey, LodTier, chunk_payload_path, object_chunk_payload_path};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
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
    let rnam_retail = match retail_rnam(&options.data, &pack.plugins) {
        Ok(scan) => Some(scan),
        Err(error) => {
            eprintln!("Retail WRLD RNAM scan failed: {error:#}");
            None
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
    compare_large_references(&mut report, &pack, rnam_retail.as_ref());

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

/// Writes the JSON report. Counts and reason classes only; `examples` is not serialized.
pub fn write_json(path: &Path, report: &ReconcileReport) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(report).wrap_err("cannot serialize the report")?;
    fs::write(path, bytes).wrap_err_with(|| format!("cannot write {}", path.display()))
}

/// Refuses an output file inside an input tree. The check must not change the pack or the game
/// folder, even when the caller asks for the report there.
fn reject_output_inside(label: &str, root: &Path, output: &Path) -> Result<()> {
    let absolute = |path: &Path| {
        if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir().map_or_else(|_| path.to_owned(), |cwd| cwd.join(path))
        }
    };
    ensure!(
        !absolute(output).starts_with(absolute(root)),
        "refused: --json must be outside the {label}"
    );
    Ok(())
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
    worlds: BTreeSet<String>,
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
            worlds: BTreeSet::new(),
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
                retail.worlds.insert(world);
                continue;
            }
            if let Some(world) = atlas_world(path) {
                *retail.atlases.entry(world.clone()).or_default() += 1;
                retail.worlds.insert(world);
                continue;
            }
            if path.ends_with(".bto")
                && let Some(name) = parse_lod_file_name(path)
                && name.kind == RetailLodKind::Object
                && let (Some(level), Some(cell)) = (name.level, name.cell)
            {
                retail
                    .objects
                    .entry(name.world.clone())
                    .or_default()
                    .entry(level)
                    .or_default()
                    .insert(cell);
                retail.worlds.insert(name.world);
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
    /// The manifest carries a complete terrain skip record.
    terrain_recorded: bool,
    /// The manifest carries a complete object skip record.
    objects_recorded: bool,
    plugins: Vec<(String, i64)>,
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
        let (terrain, reconciliation, reconciliation_ok) = match &manifest {
            Ok(Some(manifest)) => {
                let ok = manifest
                    .reconciliation
                    .as_ref()
                    .is_some_and(|counts| counts.format == crate::lod::reconciliation::FORMAT);
                (
                    terrain_state(manifest, &worlds_by_id),
                    manifest.reconciliation.clone(),
                    ok,
                )
            }
            Ok(None) => (TerrainState::Unknown("no_lod_manifest"), None, false),
            Err(error) => {
                eprintln!("lod-manifest.json cannot be read: {error:#}");
                (
                    TerrainState::Unknown("lod_manifest_unreadable"),
                    None,
                    false,
                )
            }
        };
        let terrain_recorded = reconciliation.as_ref().is_some_and(|counts| {
            reconciliation_ok
                && counts.terrain_recorded
                && counts.format == crate::lod::reconciliation::FORMAT
        });
        let objects_recorded = reconciliation.as_ref().is_some_and(|counts| {
            reconciliation_ok
                && counts.objects_recorded
                && counts.format == crate::lod::reconciliation::FORMAT
        });
        let objects = match object_state(&connection, &worlds_by_id) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("Object LOD pack counts cannot be read: {error:#}");
                ObjectState::Unknown("object_lod_unreadable")
            }
        };
        let rnam = match rnam_state(&connection, &worlds_by_id) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("Large-reference pack counts cannot be read: {error:#}");
                RnamState::Unknown("large_reference_unreadable")
            }
        };
        let plugins = match read_plugins(&connection) {
            Ok(plugins) => plugins,
            Err(error) => {
                eprintln!("Selected plugin list cannot be read: {error:#}");
                Vec::new()
            }
        };
        Ok(Self {
            worlds,
            worlds_by_id,
            terrain,
            objects,
            rnam,
            reconciliation,
            terrain_recorded,
            objects_recorded,
            plugins,
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

fn terrain_state(manifest: &LodManifest, worlds_by_id: &BTreeMap<u32, PackWorld>) -> TerrainState {
    let Some(source) = manifest.terrain_lod_source.as_deref() else {
        return TerrainState::Unknown("lod_manifest_has_no_terrain_source");
    };
    if source == crate::lod::reuse::TERRAIN_SOURCE_SELF_BUILT {
        return TerrainState::SelfBuilt;
    }
    if !source.starts_with(crate::lod::reuse::TERRAIN_SOURCE_RETAIL) {
        return TerrainState::Unknown("unknown_terrain_lod_source");
    }
    let mut chunks: BTreeMap<(u32, u32), BTreeSet<(i32, i32)>> = BTreeMap::new();
    for path in manifest.retail_terrain_inputs.keys() {
        let Some((world_id, tier, anchor)) = parse_cell_payload(path) else {
            return TerrainState::Unknown("invalid_retail_chunk_path");
        };
        let Some(world) = worlds_by_id.get(&world_id) else {
            return TerrainState::Unknown("retail_chunk_world_missing");
        };
        let Some((origin_x, origin_y)) = world.origin else {
            return TerrainState::Unknown("retail_chunk_world_has_no_lod_origin");
        };
        let Ok(side) = i32::try_from(tier) else {
            return TerrainState::Unknown("invalid_retail_chunk_tier");
        };
        chunks
            .entry((world_id, tier))
            .or_default()
            .insert((origin_x + anchor.x * side, origin_y + anchor.y * side));
    }
    TerrainState::Retail(chunks)
}

fn object_state(
    connection: &Connection,
    worlds_by_id: &BTreeMap<u32, PackWorld>,
) -> Result<ObjectState> {
    if !has_table(connection, "lod_object_chunks")? {
        return Ok(ObjectState::Unknown("no_object_lod_tables"));
    }
    let mut statement = connection.prepare(
        "SELECT worldspace_id, tier, anchor_x, anchor_y, payload_path FROM lod_object_chunks",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, i32>(1)?,
            row.get::<_, i32>(2)?,
            row.get::<_, i32>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut chunks: BTreeMap<(u32, u32), BTreeSet<(i32, i32)>> = BTreeMap::new();
    for row in rows {
        let (world_id, tier, anchor_x, anchor_y, path) = row?;
        let tier = LodTier::from_side_cells(tier)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid object LOD tier {tier}"))?;
        let key = ChunkKey::new(world_id, tier, ChunkAnchor::new(anchor_x, anchor_y));
        ensure!(
            object_chunk_payload_path(key) == path,
            "object chunk path {path} is not canonical"
        );
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
    let mut atlases = BTreeSet::new();
    if has_table(connection, "lod_object_atlases")? {
        let mut statement = connection
            .prepare("SELECT worldspace_id FROM lod_object_atlases ORDER BY worldspace_id")?;
        let rows = statement.query_map([], |row| row.get::<_, u32>(0))?;
        for row in rows {
            atlases.insert(row?);
        }
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

/// `lod/<world:08x>/<tier>/cell_<anchor_x>_<anchor_y>.glb`.
fn parse_cell_payload(path: &str) -> Option<(u32, u32, ChunkAnchor)> {
    let mut parts = path.split('/');
    if parts.next()? != "lod" {
        return None;
    }
    // silent-ok: a non-hex world field is not a chunk payload
    let world = u32::from_str_radix(parts.next()?, 16).ok()?;
    // silent-ok: a non-numeric tier field is not a chunk payload
    let tier = u32::try_from(parts.next()?.parse::<i32>().ok()?).ok()?;
    // silent-ok: a tier outside 4, 8 and 16 is not a chunk payload
    let tier = LodTier::from_side_cells(i32::try_from(tier).ok()?)?;
    let file = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let rest = file.strip_prefix("cell_")?.strip_suffix(".glb")?;
    let (x, y) = rest.split_once('_')?;
    // silent-ok: non-numeric anchor fields are not a chunk payload
    let anchor = ChunkAnchor::new(x.parse().ok()?, y.parse().ok()?);
    let key = ChunkKey::new(world, tier, anchor);
    // silent-ok: a side outside u32 is not a chunk payload
    let side = u32::try_from(tier.side_cells()).ok()?;
    (chunk_payload_path(key) == path).then_some((world, side, anchor))
}

// ---------------------------------------------------------------------------------------------
// Retail WRLD RNAM
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct RnamUnits {
    memberships: u64,
    malformed: u64,
    overrides: u64,
}

impl RnamUnits {
    fn total(self) -> u64 {
        self.memberships + self.malformed + self.overrides
    }
}

struct RnamRetail {
    worlds: BTreeMap<String, RnamUnits>,
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
    let mut worlds: BTreeMap<String, RnamUnits> = BTreeMap::new();
    let mut unattributed = 0u64;
    let mut selected = BTreeSet::new();
    for (supplier, path) in paths.iter().enumerate() {
        let defining_name = normalize_plugin_name(&order.names[supplier]);
        for record in read_world_rnam(path)? {
            let defining = resolve(b"WRLD", record.form_id, supplier, &order)
                .original_owner
                .as_deref()
                == Some(defining_name.as_str());
            let Some(editor_id) = record.editor_id else {
                for rnam in &record.rnam {
                    unattributed += units_for(rnam, defining);
                }
                continue;
            };
            let world = editor_id.to_ascii_lowercase();
            selected.insert(world.clone());
            let entry = worlds.entry(world).or_default();
            for rnam in &record.rnam {
                if !defining {
                    entry.overrides += 1;
                } else if let Some(members) = members(rnam) {
                    entry.memberships += u64::try_from(members.count())?;
                } else {
                    entry.malformed += 1;
                }
            }
        }
    }
    Ok(RnamRetail {
        worlds,
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
                            &present,
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
                    report.push(shown);
                }
            }
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
    present: impl Fn(&Presence) -> bool,
    pack: &Pack,
    index: &FileIndex,
    kind: Kind,
) -> std::result::Result<CategoryReport, &'static str> {
    if matches!(pack.terrain, TerrainState::SelfBuilt) {
        let mut row = report_row(
            category,
            label,
            Some(level),
            (
                Some(u64::try_from(retail_set.len()).unwrap_or(u64::MAX)),
                None,
                None,
                None,
            ),
            BTreeMap::new(),
            Status::OutOfScope,
            Some("terrain_lod_source=self-built".to_owned()),
        );
        row.examples = Vec::new();
        return Ok(row);
    }
    if let TerrainState::Unknown(reason) = &pack.terrain {
        return Err(reason);
    }
    let TerrainState::Retail(_) = &pack.terrain else {
        return Err("terrain_lod_source_unknown");
    };

    let mut explained: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut classes: BTreeMap<String, u64> = BTreeMap::new();
    let mut unknown = false;
    if tier_for_level(level).is_none() {
        explained.extend(retail_set.iter().copied());
        let count = u64::try_from(retail_set.len()).unwrap_or(u64::MAX);
        if count > 0 {
            classes.insert(NO_RUNTIME_TIER.to_owned(), count);
        }
    } else if pack.terrain_recorded
        && let Some(counts) = &pack.reconciliation
        && let Some(levels) = counts.terrain_skips.get(world_file)
        && let Some(by_class) = levels.get(&level)
    {
        for (class, skip) in by_class {
            if class == NO_RUNTIME_TIER {
                continue;
            }
            // silent-ok: an incomplete count marks the manifest unknown below
            if u64::try_from(skip.cells.len()).ok() != Some(skip.blocks) {
                unknown = true;
                continue;
            }
            let cells: BTreeSet<(i32, i32)> = skip
                .cells
                .iter()
                .filter(|cell| {
                    retail_blocks
                        .and_then(|blocks| blocks.get(*cell))
                        .is_some_and(&present)
                })
                .copied()
                .collect();
            if !cells.is_empty() {
                classes.insert(
                    class.clone(),
                    u64::try_from(cells.len()).unwrap_or(u64::MAX),
                );
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
    if !missing.is_empty() && !pack.terrain_recorded {
        unknown = true;
    }
    if unknown {
        let mut row = unknown_row(
            category,
            label,
            Some(level),
            "lod_manifest_has_no_terrain_reconciliation",
        );
        row.retail = Some(u64::try_from(retail_set.len()).unwrap_or(u64::MAX));
        row.pack = Some(u64::try_from(pack_cells.len()).unwrap_or(u64::MAX));
        return Ok(row);
    }
    let explained_only: BTreeSet<(i32, i32)> = explained.difference(pack_cells).copied().collect();
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
                WorldMatch::MissingFromPack => compare_missing_world(
                    "object_block",
                    &label,
                    Some(*level),
                    retail_set,
                    |cell| object_example(&retail.index, &world_file, *level, cell),
                ),
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
                report.push(shown);
            }
        }
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
    let mut explained: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut classes: BTreeMap<String, u64> = BTreeMap::new();
    let mut unknown = false;
    if tier_for_level(level).is_none() {
        explained.extend(retail_set.iter().copied());
        let count = u64::try_from(retail_set.len()).unwrap_or(u64::MAX);
        if count > 0 {
            classes.insert(NO_RUNTIME_TIER.to_owned(), count);
        }
    } else if pack.objects_recorded
        && let Some(counts) = &pack.reconciliation
    {
        if let Some(skipped) = counts.object_world_skips.get(world_file)
            && skipped == crate::lod::object_stage::MISSING_ATLAS
        {
            explained.extend(retail_set.iter().copied());
            let count = u64::try_from(retail_set.len()).unwrap_or(u64::MAX);
            if count > 0 {
                classes.insert(skipped.clone(), count);
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
                // silent-ok: an incomplete count marks the manifest unknown below
                if u64::try_from(skip.cells.len()).ok() != Some(skip.blocks) {
                    unknown = true;
                    continue;
                }
                let cells: BTreeSet<(i32, i32)> =
                    skip.cells.intersection(retail_set).copied().collect();
                if !cells.is_empty() {
                    classes.insert(
                        class.clone(),
                        u64::try_from(cells.len()).unwrap_or(u64::MAX),
                    );
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
    if !missing.is_empty() && !pack.objects_recorded {
        unknown = true;
    }
    if unknown {
        let mut row = unknown_row(
            "object_block",
            label,
            Some(level),
            "lod_manifest_has_no_object_reconciliation",
        );
        row.retail = Some(u64::try_from(retail_set.len()).unwrap_or(u64::MAX));
        row.pack = Some(u64::try_from(pack_cells.len()).unwrap_or(u64::MAX));
        return row;
    }
    let explained_only: BTreeSet<(i32, i32)> = explained.difference(&pack_cells).copied().collect();
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
            report.push(row);
            continue;
        }
        let pack_count = u64::from(pack_has);
        let has_blocks = retail
            .objects
            .get(&world_file)
            .is_some_and(|levels| !levels.is_empty());
        let status = if retail_count == pack_count {
            Status::Balanced
        } else if retail_count > 0 && pack_count == 0 && !has_blocks {
            Status::OutOfScope
        } else {
            Status::Unexplained
        };
        let reason =
            (status == Status::OutOfScope).then(|| "retail_atlas_without_object_blocks".to_owned());
        let difference = i64::try_from(retail_count).unwrap_or(i64::MAX)
            - i64::try_from(pack_count).unwrap_or(i64::MAX);
        let mut row = report_row(
            "object_atlas",
            &label,
            None,
            (
                Some(retail_count),
                Some(pack_count),
                Some(0),
                Some(difference),
            ),
            BTreeMap::new(),
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
}

fn atlas_pack_count(pack: &Pack, world_file: &str) -> bool {
    let (ObjectState::Tables { atlases, .. }, Some(world)) =
        (&pack.objects, pack.worlds.get(world_file))
    else {
        return false;
    };
    atlases.contains(&world.id)
}

fn compare_large_references(
    report: &mut ReconcileReport,
    pack: &Pack,
    retail: Option<&RnamRetail>,
) {
    if let RnamState::Unknown(reason) = &pack.rnam {
        report.push(unknown_row("large_reference", "*", None, reason));
    }
    let Some(retail) = retail else {
        if matches!(pack.rnam, RnamState::Tables(_)) {
            report.push(unknown_row(
                "large_reference",
                "*",
                None,
                "retail_rnam_unreadable",
            ));
        }
        return;
    };
    let RnamState::Tables(pack_counts) = &pack.rnam else {
        return;
    };
    let mut worlds: BTreeSet<String> = retail.worlds.keys().cloned().collect();
    for world_id in pack_counts.keys() {
        if let Some(world) = pack.worlds_by_id.get(world_id) {
            worlds.insert(world.editor_id.to_ascii_lowercase());
        }
    }
    let selected = Some(&retail.selected);
    for world_file in worlds {
        let match_ = world_match(&world_file, pack, selected);
        let label = match &match_ {
            WorldMatch::Pack(world) => world.editor_id.clone(),
            _ => world_file.clone(),
        };
        if matches!(match_, WorldMatch::NotSelected) {
            continue;
        }
        let units = retail
            .worlds
            .get(&world_file)
            .map_or(0, |units| units.total());
        let counts = pack
            .worlds
            .get(&world_file)
            .and_then(|world| pack_counts.get(&world.id))
            .copied()
            // silent-ok: a world without large-reference rows has none
            .unwrap_or_default();
        if units == 0 && counts.projected == 0 && counts.explained() == 0 {
            continue;
        }
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
            &label,
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
        report.push(row);
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
            Some("rnam_world_without_edid".to_owned()),
        );
        row.examples
            .push("WRLD RNAM without an EDID cannot be mapped to a pack worldspace".to_owned());
        report.push(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    /// A Data folder with one full level-4 terrain block and one level-16 object block.
    fn full_data() -> TempDir {
        let data = tempfile::tempdir().unwrap();
        terrain_block(data.path(), "w", 4, (0, 0), true, true, true);
        object_block(data.path(), "w", 16, (0, 0), "w.objects.dds");
        data
    }

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

    fn object_block(data: &Path, world: &str, level: u32, cell: (i32, i32), atlas: &str) {
        let folder = data.join(format!("meshes/terrain/{world}/objects"));
        let textures = data.join(format!("textures/terrain/{world}/objects"));
        fs::create_dir_all(&folder).unwrap();
        fs::create_dir_all(&textures).unwrap();
        fs::write(
            folder.join(format!("{world}.{level}.{}.{}.bto", cell.0, cell.1)),
            b"object",
        )
        .unwrap();
        fs::write(textures.join(atlas), b"atlas").unwrap();
    }

    /// A pack with worldspaces `W` and `V`, both at origin 0,0.
    fn pack_dir() -> TempDir {
        let pack = tempfile::tempdir().unwrap();
        let connection = Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO worldspaces(id, editor_id, flags, lod_origin_x, lod_origin_y) VALUES \
                 (1, 'W', 0, 0, 0), (2, 'V', 0, 0, 0)",
                [],
            )
            .unwrap();
        crate::lod::objects::create_object_chunk_tables(&connection).unwrap();
        pack
    }

    fn add_plugin(pack: &Path) {
        let connection = Connection::open(pack.join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO plugins(id, name, priority, checksum) VALUES (0, 'Test.esm', 0, X'00')",
                [],
            )
            .unwrap();
        connection
            .execute("INSERT INTO large_references VALUES (1, 0, 0, 7)", [])
            .unwrap();
    }

    fn write_manifest(pack: &Path, manifest: &serde_json::Value) {
        fs::write(
            pack.join("lod-manifest.json"),
            serde_json::to_vec_pretty(manifest).unwrap(),
        )
        .unwrap();
    }

    fn base_manifest() -> serde_json::Value {
        json!({
            "build_identity": "0".repeat(64),
            "converter_schema": 28,
            "world_database_schema": 9,
            "chunks": 0,
            "terrain_sources": {},
            "reconciliation": {
                "format": crate::lod::reconciliation::FORMAT,
                "terrain_recorded": true,
                "objects_recorded": true,
            },
        })
    }

    fn retail_manifest() -> serde_json::Value {
        let mut manifest = base_manifest();
        manifest["terrain_lod_source"] = json!("retail");
        manifest["retail_terrain_import_version"] = json!(1);
        manifest["retail_terrain_inputs"] = json!({ "lod/00000001/4/cell_0_0.glb": "h" });
        manifest
    }

    fn add_object_chunk(pack: &Path, world: u32, level: u32, cell: (i32, i32)) {
        let connection = Connection::open(pack.join("skyrim_world.db")).unwrap();
        let path = format!("lod/{world:08x}/{level}/objects_{}_{}.glb", cell.0, cell.1);
        connection
            .execute(
                "INSERT INTO lod_object_chunks(worldspace_id, tier, anchor_x, anchor_y, \
                 payload_path, content_hash, bounds_min_x, bounds_min_y, bounds_min_z, \
                 bounds_max_x, bounds_max_y, bounds_max_z, source_cells) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'h', 0, 0, 0, 1, 1, 1, '0,0')",
                rusqlite::params![world, level, cell.0, cell.1, path],
            )
            .unwrap();
    }

    fn add_atlas(pack: &Path, world: u32) {
        let connection = Connection::open(pack.join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO lod_object_atlases VALUES (?1, ?2, 'h')",
                rusqlite::params![
                    world,
                    format!("lod/{world:08x}/objects_atlas.opensky-srgb.ktx2")
                ],
            )
            .unwrap();
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

    fn write_plugin(data: &Path) {
        let mut plugin = record(b"TES4", 0, &[]);
        plugin.extend(record(
            b"WRLD",
            1,
            &[
                (b"EDID", b"W\0".to_vec()),
                (b"RNAM", rnam([0; 4], &[(7, [0; 4])])),
            ],
        ));
        fs::write(data.join("Test.esm"), plugin).unwrap();
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
    fn a_balanced_pack_has_no_unexplained_difference() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        add_object_chunk(pack.path(), 1, 16, (0, 0));
        add_atlas(pack.path(), 1);
        let mut manifest = retail_manifest();
        manifest["object_chunks"] = json!(1);
        manifest["object_chunk_inputs"] = json!({ "lod/00000001/16/objects_0_0.glb": "h" });
        manifest["object_atlases"] = json!({ "lod/00000001/objects_atlas.opensky-srgb.ktx2": "h" });
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
        let manifest = retail_manifest();
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
        // The JSON is public-safe: no retail paths.
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
        let mut manifest = retail_manifest();
        manifest["reconciliation"] = json!({
            "format": crate::lod::reconciliation::FORMAT,
            "terrain_recorded": true,
            "objects_recorded": true,
            "terrain_skips": {
                "w": {
                    "4": {
                        "missing_colour_map": {
                            "blocks": 1,
                            "mesh": 1,
                            "colour": 0,
                            "normal": 1,
                            "cells": [[4, 0]],
                        }
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
    fn both_atlas_name_forms_are_balanced() {
        let data = tempfile::tempdir().unwrap();
        object_block(data.path(), "w", 16, (0, 0), "w.objects.dds");
        object_block(data.path(), "v", 16, (0, 0), "vobjectslod.dds");
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        add_object_chunk(pack.path(), 1, 16, (0, 0));
        add_object_chunk(pack.path(), 2, 16, (0, 0));
        add_atlas(pack.path(), 1);
        add_atlas(pack.path(), 2);
        let mut manifest = base_manifest();
        manifest["terrain_lod_source"] = json!("retail");
        manifest["retail_terrain_inputs"] = json!({});
        manifest["object_chunks"] = json!(2);
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(report.passed(), "{:#?}", report.categories);
        assert_eq!(row(&report, "object_atlas", "W", None).retail, Some(1));
        assert_eq!(row(&report, "object_atlas", "V", None).retail, Some(1));
    }

    #[test]
    fn a_terrain_only_record_does_not_claim_object_skips() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        let mut manifest = retail_manifest();
        manifest["reconciliation"] = json!({
            "format": crate::lod::reconciliation::FORMAT,
            "terrain_recorded": true,
            "objects_recorded": false,
        });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let object = row(&report, "object_block", "W", Some(16));
        assert_eq!(object.status, Status::Unknown);
        assert_eq!(object.retail, Some(1));
        assert_eq!(object.pack, Some(0));
        assert_eq!(
            object.reason.as_deref(),
            Some("lod_manifest_has_no_object_reconciliation")
        );
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

    #[test]
    fn an_uncountable_pack_is_unknown_not_zero() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        add_plugin(pack.path());
        // No lod-manifest.json and no object LOD tables: neither count is computable.
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
        use dummy_content::{Entry, bsa};
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
        add_object_chunk(pack.path(), 1, 16, (0, 0));
        add_atlas(pack.path(), 1);
        let mut manifest = retail_manifest();
        manifest["object_chunks"] = json!(1);
        manifest["object_atlases"] = json!({ "lod/00000001/objects_atlas.opensky-srgb.ktx2": "h" });
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        assert!(report.passed(), "{:#?}", report.categories);
        assert_eq!(row(&report, "terrain_mesh", "W", Some(4)).retail, Some(1));
    }

    #[test]
    fn an_omitted_rnam_is_an_explained_difference() {
        let data = full_data();
        write_plugin(data.path());
        let pack = pack_dir();
        let connection = Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO plugins(id, name, priority, checksum) VALUES (0, 'Test.esm', 0, X'00')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO large_reference_omissions VALUES (1, NULL, NULL, 1, 0)",
                [],
            )
            .unwrap();
        drop(connection);
        add_object_chunk(pack.path(), 1, 16, (0, 0));
        let manifest = retail_manifest();
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
        let connection = Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "INSERT INTO plugins(id, name, priority, checksum) VALUES (0, 'Test.esm', 0, X'00')",
                [],
            )
            .unwrap();
        drop(connection);
        add_object_chunk(pack.path(), 1, 16, (0, 0));
        let manifest = retail_manifest();
        write_manifest(pack.path(), &manifest);

        let report = check(data.path(), pack.path());
        let row = row(&report, "large_reference", "W", None);
        assert_eq!(row.retail, Some(1));
        assert_eq!(row.unexplained, Some(1));
        assert_eq!(row.status, Status::Unexplained);
    }
}
