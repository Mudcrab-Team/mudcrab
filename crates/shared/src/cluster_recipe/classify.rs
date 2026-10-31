//! Reference classifier of the cluster stage (ADR-0013 section 5, reference rules).
//!
//! One classifier decides which placed references the cluster path may own: the same rules the
//! baked object chunks use, so a reference is never in both paths and never in neither by
//! accident. It reads the world database written earlier in the same conversion and records the
//! first failing rule per reference as a counted reason.

use super::overrides::{Guard, guards};
use crate::{
    coordinates::{creation_euler_to_runtime_quaternion, creation_to_runtime_vector},
    lod::{LodOrigin, LodTier},
};
use color_eyre::Result;
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub use crate::terrain::CELL_SIZE;

/// `REFR` record header flag: initially disabled.
const FLAG_INITIALLY_DISABLED: u32 = 0x800;
/// `REFR` record header flag: deleted.
const FLAG_DELETED: u32 = 0x20;
/// `STAT` record header flag: has distant LOD.
const STATIC_FLAG_HAS_DISTANT_LOD: u32 = 1 << 15;
/// Base record header flag: editor-only marker, regardless of the model filename.
pub const STATIC_FLAG_IS_MARKER: u32 = 0x00800000;
/// STAT record header flag: has tree LOD.
const STATIC_FLAG_HAS_TREE_LOD: u32 = 1 << 6;

/// One reference the cluster path may own.
#[derive(Debug, Clone)]
pub struct WorldReference {
    pub reference_id: u32,
    pub worldspace_id: u32,
    pub base_form_id: u32,
    /// The child worldspace the reference is placed in when `worldspace_id` is its parent whose
    /// block grid it joins; 0 for a reference placed in `worldspace_id` itself.
    pub source_worldspace_id: u32,
    /// The base object's snow material (`statics.material_object`, a `MATO` form id); 0 when the
    /// database has no such column (converter schema before 16) or the object has none.
    pub snow_material: u32,
    /// The cell the reference stands in, from its position (persistent references sit in the
    /// worldspace's persistent cell whatever their position is).
    pub cell_x: i32,
    pub cell_y: i32,
    /// Path of the converted GLB, `meshes/.../x.glb`.
    pub glb_path: String,
    /// 3x4 row-major, mesh space to world space, both in runtime axes.
    pub transform: [f32; 12],
}

/// Base types whose converted resting model can participate in cluster replacement.
/// State-changing types remain in the mapping scope so their refusal is explicit.
/// They are not eligible until production state writers revoke their cluster representation.
/// Actor geometry is excluded; tree inclusion remains an explicit recipe setting.
pub const RESTING_MODEL_TYPES: &[&str] = &[
    "STAT", "MSTT", "ACTI", "FLOR", "FURN", "MISC", "CONT", "ALCH", "WEAP", "INGR", "ARMO", "BOOK",
    "AMMO", "SLGM", "SCRL", "KEYM", "APPA", "TACT", "ADDN", "ARTO",
];

/// Whether this type belongs to the declared resting-model scope.
pub fn base_type_in_scope(record_type: &str, include_trees: bool) -> bool {
    RESTING_MODEL_TYPES.contains(&record_type) || (include_trees && record_type == "TREE")
}

/// One reference in the declared scope, with everything the digest, the verdict and the mapping
/// need. The stage and the authority both build it from the same row, so the recipe is one.
#[derive(Debug, Clone)]
pub struct ScopedReference {
    pub reference_id: u32,
    /// The worldspace whose block grid the reference joins.
    pub block_worldspace_id: u32,
    /// The worldspace that places the reference: the child when it joins its parent's blocks.
    pub placed_worldspace_id: u32,
    /// The cell record that owns the reference (`references.cell_id`).
    pub source_cell_id: u32,
    pub base_form_id: u32,
    pub record_type: String,
    /// Tree base type or the static tree flag. Runtime metadata only.
    pub is_tree: bool,
    /// The static distant-LOD flag, when this schema stores static flags for this base.
    /// Legacy schemas without flags leave it unknown. Runtime metadata only.
    pub base_has_distant_lod: Option<bool>,
    /// The base record's `MODL` as stored, before the engine's path rules.
    pub model_path: Option<String>,
    /// Authoritative base marker flag, also retained when cluster geometry is refused.
    pub base_is_marker: bool,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: f32,
    pub header_flags: u32,
    pub enable_parent: Option<u32>,
    pub snow_material: u32,
    /// The grid cell of the position.
    pub cell: (i32, i32),
    /// The anchor of the object block the reference belongs to, on the block worldspace's grid:
    /// the LOD origin and the cell decide it, whatever worldspace places the reference.
    pub block_anchor: (i32, i32),
    /// SHA-256 (hex) of the reference's raw subrecord blob, `None` when the column is NULL.
    pub data_hash: Option<String>,
    /// SHA-256 (hex) of the base record's raw subrecord blob.
    pub base_record_hash: Option<String>,
    pub reference_guard: Guard,
    pub base_guard: Guard,
    /// The classifier's first failing rule, `None` when it is a candidate.
    pub refusal: Option<String>,
    /// The candidate the stage builds blocks from, when the classifier accepted it.
    pub candidate: Option<WorldReference>,
}

#[derive(Debug, Clone, Copy)]
pub struct WorldspaceOrigin {
    pub worldspace_id: u32,
    pub origin: LodOrigin,
}

/// Counted outcome of the reference rules.
#[derive(Debug, Clone, Default)]
pub struct ClassifiedWorld {
    pub worldspaces: Vec<WorldspaceOrigin>,
    pub eligible: Vec<WorldReference>,
    /// First failing rule per reference, counted. Keys are stable report strings.
    pub rejected: BTreeMap<String, u64>,
    pub examined: u64,
    /// Child worldspaces that were asked for but have no parent with a LOD origin.
    pub skipped_children: Vec<u32>,
    /// Every reference in the declared scope (see [`base_type_in_scope`]), refused or not.
    pub scoped: Vec<ScopedReference>,
}

/// The block (LOD chunk) a cell belongs to, on the block grid of `origin`.
pub fn block_of(origin: LodOrigin, cell_x: i32, cell_y: i32) -> (i32, i32) {
    let anchor = origin.chunk_for_cell(LodTier::Tier16, cell_x, cell_y);
    (anchor.x, anchor.y)
}

/// Classifies every exterior reference of every worldspace that has a valid LOD origin. A
/// worldspace without one gets no cluster instances, as it gets no terrain LOD, unless it is
/// named in `children`: then its references join the block grid of its parent worldspace (which
/// needs the origin), on the assumption that the child's coordinates are the parent's. A child
/// whose parent has no origin lands in `skipped_children`.
pub fn classify_references(
    connection: &Connection,
    children: &[u32],
    include_trees: bool,
) -> Result<ClassifiedWorld> {
    let mut world = ClassifiedWorld::default();
    let sources = read_sources(connection, children)?;
    world.worldspaces = sources.worldspaces;
    world.skipped_children = sources.skipped_children;
    let sql = reference_sql(
        sources.snow_source,
        sources.has_static_flags,
        "r.is_exterior = 1 AND r.worldspace_id = ?1",
    );
    let mut references = connection.prepare(&sql)?;
    for &(source, block_worldspace) in &sources.sources {
        let rows = references.query_map([source], row_from_sql)?;
        for row in rows {
            let row = row?;
            world.examined += 1;
            let child_source = if source == block_worldspace {
                0
            } else {
                source
            };
            let outcome = classify_row(&row, block_worldspace, child_source, include_trees);
            let origin = origin_of(&world.worldspaces, block_worldspace);
            if let Some(scoped) = scoped_reference(
                &row,
                block_worldspace,
                source,
                &outcome,
                include_trees,
                origin,
            ) {
                world.scoped.push(scoped);
            }
            match outcome {
                Ok(reference) => world.eligible.push(reference),
                Err(reason) => *world.rejected.entry(reason).or_default() += 1,
            }
        }
    }
    Ok(world)
}

/// The scoped reference with this id, built the way [`classify_references`] builds it, or `None`
/// when it is not an exterior reference of a worldspace (or included child) that has a LOD
/// origin, or its base type is out of scope. For the authority, which asks reference by reference.
pub fn scoped_reference_by_id(
    connection: &Connection,
    sources: &Sources,
    reference_id: u32,
    include_trees: bool,
) -> Result<Option<ScopedReference>> {
    let sql = reference_sql(
        sources.snow_source,
        sources.has_static_flags,
        "r.is_exterior = 1 AND r.id = ?1",
    );
    let Some((row, world)) = connection
        .query_row(&sql, [reference_id], |row| {
            Ok((row_from_sql(row)?, row.get::<_, Option<u32>>(18)?))
        })
        .optional()?
    else {
        return Ok(None);
    };
    Ok(scoped_reference_in_world(
        &row,
        world,
        sources,
        include_trees,
    ))
}

/// Reads a set of references with one prepared query, using the same classifier and row recipe
/// as [`scoped_reference_by_id`]. Missing or out-of-scope rows have no scoped reference.
/// Query, storage and row-conversion errors are returned to the authority's fallible constructor;
/// they must never be converted into an unknown reference during certification.
pub(super) fn scoped_references_by_ids(
    connection: &Connection,
    sources: &Sources,
    reference_ids: &BTreeSet<u32>,
    include_trees: bool,
) -> Result<BTreeMap<u32, Option<ScopedReference>>> {
    let sql = reference_sql(
        sources.snow_source,
        sources.has_static_flags,
        "r.is_exterior = 1 AND r.id IN (SELECT value FROM json_each(?1))",
    );
    let mut statement = connection.prepare(&sql)?;
    let ids_json = serde_json::to_string(reference_ids)?;
    let mut rows = statement.query([ids_json])?;
    let mut references: BTreeMap<_, _> = reference_ids.iter().map(|&id| (id, None)).collect();
    let mut seen = BTreeSet::new();
    while let Some(row) = rows.next()? {
        let id: u32 = row.get(0)?;
        // query_row uses the first matching row, even if malformed. Preserve that behavior if a
        // malformed database has duplicate joined base rows.
        if !seen.insert(id) {
            continue;
        }
        let reference = row_from_sql(row)?;
        let world = row.get::<_, Option<u32>>(18)?;
        let scoped = scoped_reference_in_world(&reference, world, sources, include_trees);
        references.insert(id, scoped);
    }
    Ok(references)
}

fn scoped_reference_in_world(
    row: &Row,
    world: Option<u32>,
    sources: &Sources,
    include_trees: bool,
) -> Option<ScopedReference> {
    let world = world?;
    let &(source, block_worldspace) = sources
        .sources
        .iter()
        .find(|(source, _)| *source == world)?;
    let child_source = if source == block_worldspace {
        0
    } else {
        source
    };
    let outcome = classify_row(row, block_worldspace, child_source, include_trees);
    let origin = origin_of(&sources.worldspaces, block_worldspace);
    scoped_reference(
        row,
        block_worldspace,
        source,
        &outcome,
        include_trees,
        origin,
    )
}

/// The id, grid cell and block worldspace of one reference in the declared scope.
pub type ScopedId = (u32, (i32, i32), u32);

/// The ids of every reference in the declared scope, for the authority's examined set.
pub fn scoped_reference_ids(
    connection: &Connection,
    sources: &Sources,
    include_trees: bool,
) -> Result<Vec<ScopedId>> {
    if sources.sources.len() > 1
        && let Ok(ids) = scoped_reference_ids_batched(connection, sources, include_trees)
    {
        return Ok(ids);
    }
    // Empty sources still prepare the original query. Any fast-path SQL/storage/decode
    // failure replays the original reads, preserving the first refusal in source order.
    scoped_reference_ids_sequential(connection, sources, include_trees)
}

fn scoped_reference_ids_batched(
    connection: &Connection,
    sources: &Sources,
    include_trees: bool,
) -> Result<Vec<ScopedId>> {
    let worlds: BTreeSet<_> = sources.sources.iter().map(|&(source, _)| source).collect();
    let placeholders = vec!["?"; worlds.len()].join(",");
    let mut statement = connection.prepare(&format!(
        "SELECT r.id, r.pos_x, r.pos_y, rec.record_type, r.worldspace_id
         FROM \"references\" r LEFT JOIN records rec ON rec.form_id = r.base_form_id
         WHERE r.is_exterior = 1 AND r.worldspace_id IN ({placeholders}) ORDER BY r.id"
    ))?;
    let mut rows = statement.query(rusqlite::params_from_iter(worlds))?;
    // Retain only compact identities and cells, not row strings or fallible column buffers.
    let mut buckets: BTreeMap<u32, Vec<_>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let id: u32 = row.get(0)?;
        let x: f32 = row.get(1)?;
        let y: f32 = row.get(2)?;
        let record_type: Option<String> = row.get(3)?;
        let world: u32 = row.get(4)?;
        if record_type
            .as_deref()
            .is_some_and(|record_type| base_type_in_scope(record_type, include_trees))
        {
            buckets.entry(world).or_default().push((
                id,
                (
                    (x / CELL_SIZE).floor() as i32,
                    (y / CELL_SIZE).floor() as i32,
                ),
            ));
        }
    }
    let mut ids = Vec::new();
    // A requested source may occur repeatedly, even with different block-world mappings.
    for &(source, block_worldspace) in &sources.sources {
        if let Some(bucket) = buckets.get(&source) {
            ids.extend(
                bucket
                    .iter()
                    .map(|&(id, cell)| (id, cell, block_worldspace)),
            );
        }
    }
    Ok(ids)
}

fn scoped_reference_ids_sequential(
    connection: &Connection,
    sources: &Sources,
    include_trees: bool,
) -> Result<Vec<ScopedId>> {
    let mut statement = connection.prepare(
        "SELECT r.id, r.pos_x, r.pos_y, rec.record_type
         FROM \"references\" r LEFT JOIN records rec ON rec.form_id = r.base_form_id
         WHERE r.is_exterior = 1 AND r.worldspace_id = ?1 ORDER BY r.id",
    )?;
    let mut ids = Vec::new();
    for &(source, block_worldspace) in &sources.sources {
        let rows = statement.query_map([source], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, f32>(1)?,
                row.get::<_, f32>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (id, x, y, record_type) = row?;
            if record_type
                .as_deref()
                .is_some_and(|record_type| base_type_in_scope(record_type, include_trees))
            {
                ids.push((
                    id,
                    (
                        (x / CELL_SIZE).floor() as i32,
                        (y / CELL_SIZE).floor() as i32,
                    ),
                    block_worldspace,
                ));
            }
        }
    }
    Ok(ids)
}

/// Where a reference's snow material (`MATO`) comes from in this database. The core `statics`
/// table no longer carries it: the directional-snow extension owns `static_materials`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnowSource {
    /// `static_materials(id, material_object, ...)`, joined on the base form id (current schema).
    StaticMaterials,
    /// The older core column `statics.material_object`, read only when the extension table is
    /// absent (databases from before the move, such as old benchmark inputs).
    StaticsColumn,
    /// Neither exists: every reference has snow material 0, and the scope says so.
    #[default]
    None,
}

impl SnowSource {
    /// The name written into the declared scope.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StaticMaterials => "static_materials",
            Self::StaticsColumn => "statics.material_object",
            Self::None => "none",
        }
    }
}

/// Finds the snow source of `connection` without querying a table that is not there: the
/// extension table first, then the legacy column, otherwise none.
pub fn detect_snow_source(connection: &Connection) -> Result<SnowSource> {
    // The base may be an attached database (a cluster stage's connection has the cluster database
    // as `main`), so the table is looked for in every schema, not only `main`.
    let has_extension_table =
        crate::extensions::locate_object(connection, "table", "static_materials")?.is_some();
    if has_extension_table {
        return Ok(SnowSource::StaticMaterials);
    }
    let has_column = connection
        .prepare("SELECT name FROM pragma_table_info('statics') WHERE name = 'material_object'")?
        .exists([])?;
    Ok(if has_column {
        SnowSource::StaticsColumn
    } else {
        SnowSource::None
    })
}

/// The worldspaces with a LOD origin and the (reference worldspace, block worldspace) pairs a run
/// reads: every worldspace with an origin joins its own blocks, and each requested child whose
/// parent has an origin joins its parent's.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub worldspaces: Vec<WorldspaceOrigin>,
    pub sources: Vec<(u32, u32)>,
    pub skipped_children: Vec<u32>,
    /// Where snow materials are read from in this database.
    pub snow_source: SnowSource,
    /// Whether the core statics schema stores its record flags.
    pub has_static_flags: bool,
}

/// Reads the [`Sources`] of a run.
pub fn read_sources(connection: &Connection, children: &[u32]) -> Result<Sources> {
    let mut statement = connection.prepare(
        "SELECT id, lod_origin_x, lod_origin_y FROM worldspaces
         WHERE lod_origin_x IS NOT NULL AND lod_origin_y IS NOT NULL ORDER BY id",
    )?;
    let worldspaces = statement
        .query_map([], |row| {
            Ok(WorldspaceOrigin {
                worldspace_id: row.get(0)?,
                origin: LodOrigin::new(row.get(1)?, row.get(2)?),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut sources: Vec<(u32, u32)> = worldspaces
        .iter()
        .map(|worldspace| (worldspace.worldspace_id, worldspace.worldspace_id))
        .collect();
    let mut skipped_children = Vec::new();
    for &child in children {
        let parent: Option<u32> = connection
            .query_row(
                "SELECT parent_world FROM worldspaces WHERE id = ?1",
                [child],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        match parent {
            Some(parent) if worldspaces.iter().any(|w| w.worldspace_id == parent) => {
                sources.push((child, parent));
            }
            _ => skipped_children.push(child),
        }
    }
    Ok(Sources {
        worldspaces,
        sources,
        skipped_children,
        snow_source: detect_snow_source(connection)?,
        has_static_flags: connection
            .prepare("SELECT name FROM pragma_table_info('statics') WHERE name = 'flags'")?
            .exists([])?,
    })
}

/// The reference query: the columns [`row_from_sql`] reads, plus the reference's own worldspace as
/// column 18. The two blobs are only read for in-scope base types.
fn reference_sql(snow_source: SnowSource, has_static_flags: bool, condition: &str) -> String {
    let (snow_expression, snow_join) = match snow_source {
        SnowSource::StaticMaterials => (
            "COALESCE(sm.material_object, 0)",
            "LEFT JOIN static_materials sm ON sm.id = r.base_form_id",
        ),
        SnowSource::StaticsColumn => ("COALESCE(s.material_object, 0)", ""),
        SnowSource::None => ("0", ""),
    };
    let static_flags = if has_static_flags { "s.flags" } else { "NULL" };
    format!(
        "SELECT r.id, r.base_form_id, r.pos_x, r.pos_y, r.pos_z, r.rot_x, r.rot_y, r.rot_z,
                r.scale, r.header_flags, r.enable_parent_id, rec.record_type, s.model_path,
                EXISTS(SELECT 1 FROM scripts sc WHERE sc.form_id = r.id OR sc.form_id = r.base_form_id),
                {snow_expression}, r.cell_id,
                r.data,
                rec.data,
                r.worldspace_id, {static_flags}
         FROM \"references\" r
         LEFT JOIN records rec ON rec.form_id = r.base_form_id
         LEFT JOIN statics s ON s.id = r.base_form_id
         {snow_join}
         WHERE {condition}
         ORDER BY r.id"
    )
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let reference_blob: Option<Vec<u8>> = row.get(16)?;
    let base_blob: Option<Vec<u8>> = row.get(17)?;
    let (reference_guard, base_guard) = guards(reference_blob.as_deref(), base_blob.as_deref());
    let hash = |blob: &Option<Vec<u8>>| {
        blob.as_ref()
            .map(|bytes| crate::cluster::hex(&Sha256::digest(bytes)))
    };
    Ok(Row {
        id: row.get(0)?,
        base_form_id: row.get(1)?,
        position: [row.get(2)?, row.get(3)?, row.get(4)?],
        rotation: [row.get(5)?, row.get(6)?, row.get(7)?],
        scale: row.get(8)?,
        header_flags: row.get(9)?,
        enable_parent: row.get::<_, Option<u32>>(10)?,
        record_type: row.get::<_, Option<String>>(11)?,
        static_flags: row.get::<_, Option<u32>>(19)?,
        model_path: row.get::<_, Option<String>>(12)?,
        scripted: row.get::<_, bool>(13)?,
        snow_material: row.get::<_, i64>(14)? as u32,
        cell_id: row.get::<_, Option<u32>>(15)?.unwrap_or_default(),
        data_hash: hash(&reference_blob),
        base_record_hash: hash(&base_blob),
        reference_guard,
        base_guard,
    })
}

/// The LOD origin of a block worldspace; every block worldspace of a run has one.
fn origin_of(worldspaces: &[WorldspaceOrigin], worldspace_id: u32) -> LodOrigin {
    worldspaces
        .iter()
        .find(|world| world.worldspace_id == worldspace_id)
        .map(|world| world.origin)
        .expect("a block worldspace has a LOD origin")
}

fn scoped_reference(
    row: &Row,
    block_worldspace: u32,
    source_worldspace: u32,
    outcome: &Result<WorldReference, String>,
    include_trees: bool,
    origin: LodOrigin,
) -> Option<ScopedReference> {
    let record_type = row.record_type.as_deref()?;
    if !base_type_in_scope(record_type, include_trees) {
        return None;
    }
    Some(ScopedReference {
        reference_id: row.id,
        block_worldspace_id: block_worldspace,
        placed_worldspace_id: source_worldspace,
        source_cell_id: row.cell_id,
        base_form_id: row.base_form_id,
        record_type: record_type.to_owned(),
        is_tree: record_type == "TREE"
            || (record_type == "STAT"
                && row
                    .static_flags
                    .is_some_and(|flags| flags & STATIC_FLAG_HAS_TREE_LOD != 0)),
        base_has_distant_lod: row
            .static_flags
            .map(|flags| flags & STATIC_FLAG_HAS_DISTANT_LOD != 0),
        model_path: row.model_path.clone(),
        base_is_marker: row
            .static_flags
            .is_some_and(|flags| flags & STATIC_FLAG_IS_MARKER != 0),
        position: row.position,
        rotation: row.rotation,
        scale: row.scale,
        header_flags: row.header_flags,
        enable_parent: row.enable_parent,
        snow_material: row.snow_material,
        cell: (
            (row.position[0] / CELL_SIZE).floor() as i32,
            (row.position[1] / CELL_SIZE).floor() as i32,
        ),
        block_anchor: block_of(
            origin,
            (row.position[0] / CELL_SIZE).floor() as i32,
            (row.position[1] / CELL_SIZE).floor() as i32,
        ),
        data_hash: row.data_hash.clone(),
        base_record_hash: row.base_record_hash.clone(),
        reference_guard: row.reference_guard,
        base_guard: row.base_guard,
        refusal: outcome.as_ref().err().cloned(),
        candidate: outcome.as_ref().ok().cloned(),
    })
}

struct Row {
    id: u32,
    base_form_id: u32,
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
    header_flags: u32,
    enable_parent: Option<u32>,
    record_type: Option<String>,
    static_flags: Option<u32>,
    model_path: Option<String>,
    scripted: bool,
    snow_material: u32,
    cell_id: u32,
    data_hash: Option<String>,
    base_record_hash: Option<String>,
    reference_guard: Guard,
    base_guard: Guard,
}

fn classify_row(
    row: &Row,
    worldspace_id: u32,
    source_worldspace_id: u32,
    include_trees: bool,
) -> Result<WorldReference, String> {
    match row.record_type.as_deref() {
        Some(kind) if base_type_in_scope(kind, include_trees) => {}
        Some(other) => return Err(format!("base_not_static:{other}")),
        None => return Err("base_unresolved".into()),
    }
    if row
        .static_flags
        .is_some_and(|flags| flags & STATIC_FLAG_IS_MARKER != 0)
    {
        return Err("base_is_marker".into());
    }
    if row.header_flags & FLAG_DELETED != 0 {
        return Err("deleted".into());
    }
    if row.header_flags & FLAG_INITIALLY_DISABLED != 0 {
        return Err(super::refusal::INITIALLY_DISABLED.into());
    }
    if row.enable_parent.is_some() {
        return Err(super::refusal::ENABLE_PARENT.into());
    }
    if row.scripted {
        return Err(super::refusal::SCRIPTED.into());
    }
    let glb_path = row
        .model_path
        .as_deref()
        .ok_or_else(|| "no_model".to_string())?;
    if !matches!(row.record_type.as_deref(), Some("STAT" | "TREE")) {
        return Err(super::refusal::STATE_CHANGE_WRITER_UNAVAILABLE.into());
    }
    let glb_path = model_glb_path(glb_path).ok_or_else(|| "model_path_excluded".to_string())?;
    if !row
        .position
        .iter()
        .chain(&row.rotation)
        .chain([&row.scale])
        .all(|value| value.is_finite())
        || row.scale <= 0.0
    {
        return Err("invalid_transform".into());
    }
    Ok(WorldReference {
        reference_id: row.id,
        worldspace_id,
        base_form_id: row.base_form_id,
        source_worldspace_id,
        snow_material: row.snow_material,
        cell_x: (row.position[0] / CELL_SIZE).floor() as i32,
        cell_y: (row.position[1] / CELL_SIZE).floor() as i32,
        glb_path,
        transform: reference_transform(row.position, row.rotation, row.scale),
    })
}

/// 3x4 row-major transform of a placed reference in runtime axes: the engine spawns the model
/// with the translation, rotation and scale of the reference (`streaming.rs`), on top of the GLB's
/// own node transforms, which `glb_input` already baked into the vertices.
pub fn reference_transform(position: [f32; 3], rotation: [f32; 3], scale: f32) -> [f32; 12] {
    let [qx, qy, qz, qw] = creation_euler_to_runtime_quaternion(rotation);
    let [tx, ty, tz] = creation_to_runtime_vector(position);
    let r = [
        [
            1.0 - 2.0 * (qy * qy + qz * qz),
            2.0 * (qx * qy - qz * qw),
            2.0 * (qx * qz + qy * qw),
        ],
        [
            2.0 * (qx * qy + qz * qw),
            1.0 - 2.0 * (qx * qx + qz * qz),
            2.0 * (qy * qz - qx * qw),
        ],
        [
            2.0 * (qx * qz - qy * qw),
            2.0 * (qy * qz + qx * qw),
            1.0 - 2.0 * (qx * qx + qy * qy),
        ],
    ];
    [
        r[0][0] * scale,
        r[0][1] * scale,
        r[0][2] * scale,
        tx,
        r[1][0] * scale,
        r[1][1] * scale,
        r[1][2] * scale,
        ty,
        r[2][0] * scale,
        r[2][1] * scale,
        r[2][2] * scale,
        tz,
    ]
}

/// The converted model a placed reference draws. The base record's `Is Marker` flag,
/// not a filename substring, makes the entire reference editor-only.
pub fn spawned_model_path(model_path: Option<&str>, base_is_marker: bool) -> Option<String> {
    model_path
        .filter(|_| !base_is_marker)
        .and_then(model_glb_path)
}

/// The converted GLB path of a base record's `MODL`, or `None` for excluded folders
/// (sky, markers, effects) and unsafe paths. Record-aware callers use [`spawned_model_path`].
pub fn model_glb_path(model_path: &str) -> Option<String> {
    let normalized = model_path.replace('\\', "/").to_ascii_lowercase();
    if normalized.starts_with("meshes/sky/")
        || normalized.starts_with("sky/")
        || normalized.starts_with("meshes/markers/")
        || normalized.starts_with("markers/")
        || normalized.starts_with("meshes/effects/")
        || normalized.starts_with("effects/")
    {
        return None;
    }
    let without_prefix = normalized.strip_prefix("meshes/").unwrap_or(&normalized);
    if without_prefix.is_empty()
        || std::path::Path::new(without_prefix)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let mut path = std::path::PathBuf::from("meshes").join(without_prefix);
    path.set_extension("glb");
    Some(path.to_string_lossy().replace('\\', "/"))
}

/// Applies a 3x4 row-major transform to a point.
pub fn transform_point(transform: &[f32; 12], point: [f32; 3]) -> [f32; 3] {
    [
        transform[0] * point[0] + transform[1] * point[1] + transform[2] * point[2] + transform[3],
        transform[4] * point[0] + transform[5] * point[1] + transform[6] * point[2] + transform[7],
        transform[8] * point[0]
            + transform[9] * point[1]
            + transform[10] * point[2]
            + transform[11],
    ]
}

/// World-space axis-aligned bounds of mesh-space bounds under `transform`.
pub fn transform_bounds(
    transform: &[f32; 12],
    (min, max): ([f32; 3], [f32; 3]),
) -> ([f32; 3], [f32; 3]) {
    let mut out_min = [f32::MAX; 3];
    let mut out_max = [f32::MIN; 3];
    for corner in 0..8 {
        let point = [
            if corner & 1 == 0 { min[0] } else { max[0] },
            if corner & 2 == 0 { min[1] } else { max[1] },
            if corner & 4 == 0 { min[2] } else { max[2] },
        ];
        let world = transform_point(transform, point);
        for axis in 0..3 {
            out_min[axis] = out_min[axis].min(world[axis]);
            out_max[axis] = out_max[axis].max(world[axis]);
        }
    }
    (out_min, out_max)
}

#[cfg(test)]
mod scoped_id_tests {
    use super::*;
    use rusqlite::limits::Limit;

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE records (form_id INTEGER PRIMARY KEY, record_type TEXT);
                 CREATE TABLE \"references\" (
                     id INTEGER PRIMARY KEY, base_form_id INTEGER, pos_x, pos_y,
                     is_exterior INTEGER, worldspace_id
                 );
                 INSERT INTO records VALUES (101, 'STAT'), (102, 'TREE'), (103, 'NPC_');
                 INSERT INTO \"references\" VALUES
                     (1, 101, 4096, -1, 1, 10),
                     (2, 102, -1, 8192, 1, 20),
                     (3, 103, 0, 0, 1, 10),
                     (4, 101, 8192, 0, 1, 20),
                     (5, 101, 0, -4096, 1, 30),
                     (6, 101, 0, 0, 1, 99),
                     (7, 101, 0, 0, 0, 10);",
            )
            .unwrap();
        connection
    }

    fn sources(pairs: &[(u32, u32)]) -> Sources {
        Sources {
            sources: pairs.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn batching_preserves_source_order_duplicates_child_mapping_and_tree_scope() {
        let connection = database();
        let sources = sources(&[(30, 100), (20, 200), (10, 10), (30, 300), (20, 201)]);
        for include_trees in [false, true] {
            let original =
                scoped_reference_ids_sequential(&connection, &sources, include_trees).unwrap();
            let mut expected = vec![(5, (0, -1), 100)];
            if include_trees {
                expected.push((2, (-1, 2), 200));
            }
            expected.extend([(4, (2, 0), 200), (1, (1, -1), 10), (5, (0, -1), 300)]);
            if include_trees {
                expected.push((2, (-1, 2), 201));
            }
            expected.push((4, (2, 0), 201));
            assert_eq!(original, expected);
            assert_eq!(
                scoped_reference_ids_batched(&connection, &sources, include_trees).unwrap(),
                original
            );
            assert_eq!(
                scoped_reference_ids(&connection, &sources, include_trees).unwrap(),
                original
            );
        }
    }

    #[test]
    fn out_of_scope_rows_keep_their_original_decode_refusal() {
        let connection = database();
        connection
            .execute("UPDATE \"references\" SET pos_x='bad' WHERE id=3", [])
            .unwrap();
        let sources = sources(&[(20, 20), (10, 10)]);
        let original = scoped_reference_ids_sequential(&connection, &sources, false)
            .unwrap_err()
            .to_string();
        assert_eq!(
            scoped_reference_ids(&connection, &sources, false)
                .unwrap_err()
                .to_string(),
            original
        );
    }

    #[test]
    fn competing_decode_failures_replay_the_original_world_order() {
        let connection = database();
        connection
            .execute_batch(
                "UPDATE \"references\" SET pos_x='bad' WHERE id=1;
                 UPDATE \"references\" SET pos_y='bad' WHERE id=4;",
            )
            .unwrap();
        let sources = sources(&[(20, 20), (10, 10)]);
        let original = scoped_reference_ids_sequential(&connection, &sources, false)
            .unwrap_err()
            .to_string();
        assert_ne!(
            scoped_reference_ids_batched(&connection, &sources, false)
                .unwrap_err()
                .to_string(),
            original,
            "the fixture must distinguish scan order from source refusal order"
        );
        assert_eq!(
            scoped_reference_ids(&connection, &sources, false)
                .unwrap_err()
                .to_string(),
            original
        );
    }

    #[test]
    fn a_connection_variable_limit_replays_single_world_queries() {
        let connection = database();
        connection.set_limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 1);
        let sources = sources(&[(20, 20), (10, 10)]);
        assert!(scoped_reference_ids_batched(&connection, &sources, true).is_err());
        assert_eq!(
            scoped_reference_ids(&connection, &sources, true).unwrap(),
            scoped_reference_ids_sequential(&connection, &sources, true).unwrap()
        );
    }

    #[test]
    fn extra_world_column_decode_failure_replays_original_success() {
        let connection = database();
        connection
            .execute(
                "UPDATE \"references\" SET worldspace_id=20.0 WHERE id=4",
                [],
            )
            .unwrap();
        let sources = sources(&[(20, 20), (10, 10)]);
        assert!(scoped_reference_ids_batched(&connection, &sources, true).is_err());
        assert_eq!(
            scoped_reference_ids(&connection, &sources, true).unwrap(),
            scoped_reference_ids_sequential(&connection, &sources, true).unwrap()
        );
    }

    #[test]
    fn empty_sources_still_refuse_a_missing_reference_table() {
        let connection = Connection::open_in_memory().unwrap();
        let sources = Sources::default();
        assert_eq!(
            scoped_reference_ids(&connection, &sources, false)
                .unwrap_err()
                .to_string(),
            scoped_reference_ids_sequential(&connection, &sources, false)
                .unwrap_err()
                .to_string()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Row {
        Row {
            id: 7,
            base_form_id: 9,
            position: [4096.0 * 3.0 + 10.0, -4096.0 + 5.0, 100.0],
            rotation: [0.0, 0.0, 0.0],
            scale: 1.0,
            header_flags: 0,
            enable_parent: None,
            record_type: Some("STAT".into()),
            static_flags: None,
            model_path: Some("Architecture\\Whiterun\\WRBuilding01.nif".into()),
            scripted: false,
            snow_material: 0x25129,
            cell_id: 0x30,
            data_hash: None,
            base_record_hash: None,
            reference_guard: Guard::Clear,
            base_guard: Guard::Clear,
        }
    }

    #[test]
    fn an_ordinary_static_is_eligible_in_its_position_cell() {
        let reference = classify_row(&row(), 0x3c, 0, false).unwrap();
        assert_eq!(
            reference.glb_path,
            "meshes/architecture/whiterun/wrbuilding01.glb"
        );
        assert_eq!((reference.cell_x, reference.cell_y), (3, -1));
        assert_eq!(reference.snow_material, 0x25129);
        // Identity rotation: translation is the runtime-axes position.
        assert_eq!(reference.transform[3], 4096.0 * 3.0 + 10.0);
        assert_eq!(reference.transform[7], 100.0);
        assert_eq!(reference.transform[11], -(-4096.0 + 5.0));
    }

    #[test]
    fn flagged_ordinary_model_is_refused_and_has_no_retained_drawable_path() {
        for scripted in [false, true] {
            let mut source = row();
            source.model_path = Some("Architecture/Wall.nif".into());
            source.static_flags = Some(0x00800000);
            source.scripted = scripted;
            let outcome = classify_row(&source, 1, 0, false);
            assert_eq!(outcome.as_ref().unwrap_err(), "base_is_marker");
            let retained =
                scoped_reference(&source, 1, 1, &outcome, false, LodOrigin::new(0, 0)).unwrap();
            assert!(retained.base_is_marker);
            assert!(retained.candidate.is_none());
            assert_eq!(
                spawned_model_path(retained.model_path.as_deref(), retained.base_is_marker),
                None
            );
        }
    }

    #[test]
    fn unflagged_marker_filename_keeps_drawable_path_when_eligible_or_refused() {
        for scripted in [false, true] {
            let mut source = row();
            source.model_path = Some("Clutter/CivilWar/CWMapMarkers01.nif".into());
            source.static_flags = Some(0);
            source.scripted = scripted;
            let outcome = classify_row(&source, 1, 0, false);
            if scripted {
                assert_eq!(outcome.as_ref().unwrap_err(), "scripted");
            } else {
                assert_eq!(
                    outcome.as_ref().unwrap().glb_path,
                    "meshes/clutter/civilwar/cwmapmarkers01.glb"
                );
            }
            let retained =
                scoped_reference(&source, 1, 1, &outcome, false, LodOrigin::new(0, 0)).unwrap();
            assert!(!retained.base_is_marker);
            assert_eq!(
                spawned_model_path(retained.model_path.as_deref(), retained.base_is_marker)
                    .as_deref(),
                Some("meshes/clutter/civilwar/cwmapmarkers01.glb")
            );
        }
    }

    #[test]
    fn shared_drawable_resolver_preserves_folder_and_traversal_guards() {
        for source in [
            "Markers/XMarker.nif",
            "Sky/Cloud.nif",
            "Effects/Water.nif",
            "meshes/../../escape.nif",
            "meshes//root.nif",
            "meshes/",
        ] {
            assert_eq!(spawned_model_path(Some(source), false), None, "{source}");
        }
        assert_eq!(spawned_model_path(None, false), None);
        assert_eq!(
            spawned_model_path(Some("Furniture/SitLedgeMarker.nif"), false).as_deref(),
            Some("meshes/furniture/sitledgemarker.glb")
        );
    }

    #[test]
    fn each_rule_names_its_reason() {
        type Mutation = Box<dyn Fn(&mut Row)>;
        let cases: Vec<(Mutation, &str)> = vec![
            (
                Box::new(|r| r.record_type = Some("TREE".into())),
                "base_not_static:TREE",
            ),
            (Box::new(|r| r.record_type = None), "base_unresolved"),
            (Box::new(|r| r.header_flags = 0x800), "initially_disabled"),
            (Box::new(|r| r.header_flags = 0x20), "deleted"),
            (Box::new(|r| r.enable_parent = Some(5)), "enable_parent"),
            (Box::new(|r| r.scripted = true), "scripted"),
            (Box::new(|r| r.model_path = None), "no_model"),
            (
                Box::new(|r| r.model_path = Some("Markers\\XMarker.nif".into())),
                "model_path_excluded",
            ),
            (Box::new(|r| r.scale = 0.0), "invalid_transform"),
        ];
        for (mutate, reason) in cases {
            let mut candidate = row();
            mutate(&mut candidate);
            assert_eq!(classify_row(&candidate, 1, 0, false).unwrap_err(), reason);
        }
    }

    #[test]
    fn state_changing_types_require_production_state_writers() {
        for &kind in RESTING_MODEL_TYPES {
            let mut candidate = row();
            candidate.record_type = Some(kind.into());
            let result = classify_row(&candidate, 1, 0, false);
            if kind == "STAT" {
                assert!(result.is_ok());
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    "state_change_writer_unavailable",
                    "{kind}"
                );
            }
        }
        for kind in ["NPC_", "ACHR", "DOOR"] {
            assert!(!base_type_in_scope(kind, true), "{kind}");
        }
        assert!(!base_type_in_scope("TREE", false));
        assert!(base_type_in_scope("TREE", true));
    }

    #[test]
    fn nonstatic_header_bits_do_not_claim_a_native_tree_relation() {
        let mut candidate = row();
        candidate.record_type = Some("ACTI".into());
        candidate.static_flags = Some(STATIC_FLAG_HAS_TREE_LOD);
        let outcome = classify_row(&candidate, 1, 0, true);
        let scoped =
            scoped_reference(&candidate, 1, 1, &outcome, true, LodOrigin::new(0, 0)).unwrap();
        assert!(!scoped.is_tree);
        candidate.record_type = Some("STAT".into());
        let outcome = classify_row(&candidate, 1, 0, true);
        let scoped =
            scoped_reference(&candidate, 1, 1, &outcome, true, LodOrigin::new(0, 0)).unwrap();
        assert!(scoped.is_tree);
    }

    #[test]
    fn yaw_rotates_the_transform() {
        let transform = reference_transform([0.0; 3], [0.0, 0.0, std::f32::consts::FRAC_PI_2], 2.0);
        let moved = transform_point(&transform, [1.0, 0.0, 0.0]);
        // A quarter turn about the vertical axis with scale 2 puts the x axis on the z axis.
        assert!(
            moved[0].abs() < 1e-4 && moved[1].abs() < 1e-4 && (moved[2].abs() - 2.0).abs() < 1e-4
        );
    }

    #[test]
    fn bounds_follow_the_transform() {
        let transform = reference_transform([100.0, 0.0, 0.0], [0.0; 3], 1.0);
        let (min, max) = transform_bounds(&transform, ([-1.0; 3], [1.0; 3]));
        assert_eq!((min[0], max[0]), (99.0, 101.0));
    }
}
