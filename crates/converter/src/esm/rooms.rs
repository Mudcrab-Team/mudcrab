//! Interior room graph: room boxes, portals, room links and room membership.
//!
//! Skyrim marks the rooms of an interior with `RoomMarker` references and the doorways between
//! them with `PortalMarker` references. This module decodes both into the optional tables of
//! `docs/specs/converters/db-schema.md` §15 and records which room boxes contain each placed
//! object. The tables hold facts only; which objects to draw is the engine's policy.
//!
//! Design and byte layouts: `docs/research/interior-room-graph-converter.md`.

use crate::esm::{
    extractors::SubrecordView,
    load_order::LoadOrder,
    projection_omissions::{OmissionCategory, ProjectionOmissions},
    records::RawRecord,
    types::ArchivedRecordData,
};
use crate::records::{DecodedRecord, ReadResult};
use rusqlite::{Connection, OptionalExtension, Result, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

mod occluders;
pub use occluders::{NativeOccluderChange, NativeOccluderShape, apply_native_occluders};
pub(super) use occluders::{update_authored_subset, update_subset as update_subset_occluders};

/// `RoomMarker` base form in `Skyrim.esm`, which is always load-order slot 0.
pub const ROOM_MARKER_BASE: u32 = 0x1F;
/// `PortalMarker` base form in `Skyrim.esm`.
pub const PORTAL_MARKER_BASE: u32 = 0x20;
/// `PlaneMarker` base form, used by both plane and box occlusion primitives.
pub const OCCLUSION_MARKER_BASE: u32 = 0x17;
/// `XPRM` shape value of a box room.
pub(super) const SHAPE_BOX: u32 = 1;
pub(super) const SHAPE_PORTAL: u32 = 3;

/// Rejected REFRs with unreadable or missing NAME fields can hide PlaneMarker links.
pub(crate) fn may_name_occlusion_marker<'a>(
    names: impl Iterator<Item = &'a [u8]>,
    plugin: usize,
    order: &LoadOrder,
) -> bool {
    let mut names = names.peekable();
    names.peek().is_none()
        || names.any(|bytes| {
            let Ok(word) = <[u8; 4]>::try_from(bytes) else {
                return true;
            };
            crate::records::source_identity::header_runtime_id(
                b"REFR",
                u32::from_le_bytes(word),
                plugin,
                order,
            )
            .is_none_or(|id| id == OCCLUSION_MARKER_BASE)
        })
}

/// A malformed XORD can hide a target. An authored four-slot all-null XORD cannot.
pub(crate) fn xord_may_link(data: &[u8]) -> bool {
    data != [0; 16]
}

/// Tables created by `create_tables` so an older reused database gains them.
pub const ROOM_TABLES_SQL: &str = r#"
         CREATE TABLE IF NOT EXISTS room_bounds (
             ref_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
             pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
             rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
             half_x REAL NOT NULL, half_y REAL NOT NULL, half_z REAL NOT NULL,
             lighting_template_id INTEGER, image_space_id INTEGER,
             flags INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_room_bounds_cell ON room_bounds(cell_id);
         CREATE TABLE IF NOT EXISTS room_portals (
             ref_id INTEGER NOT NULL, pair_index INTEGER NOT NULL DEFAULT 0,
             cell_id INTEGER NOT NULL,
             pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
             rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
             half_x REAL NOT NULL, half_z REAL NOT NULL,
             room_a INTEGER, room_b INTEGER,
             unbounded_a INTEGER NOT NULL DEFAULT 0,
             unbounded_b INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (ref_id, pair_index)
         );
         CREATE INDEX IF NOT EXISTS idx_room_portals_cell ON room_portals(cell_id);
         CREATE TABLE IF NOT EXISTS room_occluders (
             ref_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
             shape INTEGER NOT NULL CHECK(shape IN (1,3)),
             pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
             rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
             half_x REAL NOT NULL, half_y REAL NOT NULL, half_z REAL NOT NULL,
             scale REAL NOT NULL, header_flags INTEGER NOT NULL,
             has_enable_parent INTEGER NOT NULL, has_links INTEGER NOT NULL,
             target_ref INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_room_occluders_cell ON room_occluders(cell_id);
         CREATE TABLE IF NOT EXISTS room_occluder_links (
             ref_id INTEGER NOT NULL, link_index INTEGER NOT NULL,
             linked_ref INTEGER NOT NULL,
             single_target_verified INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (ref_id, link_index)
         );
         CREATE TABLE IF NOT EXISTS room_links (
             room_ref INTEGER NOT NULL, linked_ref INTEGER NOT NULL,
             link_index INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (room_ref, linked_ref)
         );
         CREATE TABLE IF NOT EXISTS room_reference_rules (
             reference_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
             assignment TEXT NOT NULL CHECK(assignment IN ('auto','none','target','unknown')),
             target_ref INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_room_reference_rules_cell ON room_reference_rules(cell_id);
         CREATE TABLE IF NOT EXISTS room_model_bounds (
             base_form_id INTEGER PRIMARY KEY,
             center_x REAL NOT NULL, center_y REAL NOT NULL, center_z REAL NOT NULL,
             radius REAL NOT NULL
         );
         CREATE TABLE IF NOT EXISTS room_graph_cells (
             cell_id INTEGER PRIMARY KEY, complete INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS reference_rooms (
             reference_id INTEGER NOT NULL, room_ref INTEGER NOT NULL,
             contained INTEGER NOT NULL,
             PRIMARY KEY (reference_id, room_ref)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS idx_reference_rooms_room ON reference_rooms(room_ref);
         CREATE TABLE IF NOT EXISTS room_graph_stats (
             name TEXT PRIMARY KEY, value INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS room_link_anomalies (
             plugin TEXT PRIMARY KEY, count INTEGER NOT NULL,
             first_marker INTEGER NOT NULL, first_cell INTEGER NOT NULL
         );
"#;

/// Old NULL endpoints are ambiguous. Adding columns never promotes them to authored outside.
pub fn migrate_room_metadata(conn: &Connection) -> Result<()> {
    let outermost = conn.is_autocommit();
    conn.execute_batch("SAVEPOINT portal_endpoints")?;
    let result = (|| {
        for column in ["unbounded_a", "unbounded_b"] {
            let present: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('room_portals') WHERE name=?1)",
                [column],
                |row| row.get(0),
            )?;
            if !present {
                conn.execute_batch(&format!(
                    "ALTER TABLE room_portals ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0"
                ))?;
            }
        }
        let ordered_links: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('room_links') WHERE name='link_index')",
            [],
            |row| row.get(0),
        )?;
        if !ordered_links {
            conn.execute_batch(
                "ALTER TABLE room_links ADD COLUMN link_index INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        migrate_occluder_links(conn)?;
        conn.execute_batch("RELEASE portal_endpoints")
    })();
    if result.is_err() {
        let _ = conn.execute_batch(if outermost {
            "ROLLBACK"
        } else {
            "ROLLBACK TO portal_endpoints; RELEASE portal_endpoints"
        });
    }
    result
}

fn migrate_occluder_links(conn: &Connection) -> Result<()> {
    let verified_plane_links: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('room_occluder_links') WHERE name='single_target_verified')",
            [],
            |row| row.get(0),
        )?;
    if !verified_plane_links {
        conn.execute_batch(
                "ALTER TABLE room_occluder_links ADD COLUMN single_target_verified INTEGER NOT NULL DEFAULT 0",
            )?;
    }
    Ok(())
}

/// The typed reader can clear broken links to zero. Restore outside provenance from source
/// framing before publication, without changing the canonical archive or its diagnostics.
pub(crate) fn restore_portal_endpoint_provenance<'a>(
    conn: &Connection,
    sources: impl IntoIterator<Item = (u32, Option<&'a [u8]>)>,
) -> Result<()> {
    let mut clear = conn
        .prepare_cached("UPDATE room_portals SET unbounded_a=0, unbounded_b=0 WHERE ref_id=?1")?;
    let mut set = conn.prepare_cached(
        "UPDATE room_portals SET unbounded_a=(room_a IS NULL AND ?3), unbounded_b=(room_b IS NULL AND ?4) WHERE ref_id=?1 AND pair_index=?2",
    )?;
    let mut incomplete = conn.prepare_cached(
        "INSERT INTO room_graph_cells(cell_id,complete)
         SELECT DISTINCT cell_id,0 FROM room_portals WHERE ref_id=?1 AND
          (?2 OR (room_a IS NULL AND NOT unbounded_a) OR (room_b IS NULL AND NOT unbounded_b))
         ON CONFLICT(cell_id) DO UPDATE SET complete=0",
    )?;
    for (id, source) in sources {
        if clear.execute([id])? == 0 {
            continue;
        }
        let pairs = source.filter(|bytes| !bytes.is_empty() && bytes.len().is_multiple_of(8));
        if let Some(bytes) = pairs {
            for (index, pair) in bytes.as_chunks::<8>().0.iter().enumerate() {
                set.execute(params![
                    id,
                    index as i64,
                    u32_at(pair, 0) == Some(0),
                    u32_at(pair, 4) == Some(0)
                ])?;
            }
        }
        incomplete.execute(params![id, pairs.is_none()])?;
    }
    Ok(())
}

/// Canonical typed fields omit rejected data. Keep that uncertainty in derived room metadata
/// without adding synthetic fields to the canonical archive.
pub(crate) fn restore_rejected_room_fields<'a>(
    conn: &Connection,
    sources: impl IntoIterator<Item = &'a crate::records::DecodedRecord>,
) -> Result<()> {
    let mut assignment = conn.prepare_cached(
        "UPDATE room_reference_rules SET assignment='unknown', target_ref=NULL WHERE reference_id=?1",
    )?;
    let mut incomplete =
        conn.prepare_cached("UPDATE room_graph_cells SET complete=0 WHERE cell_id=?1")?;
    for record in sources {
        if record.rejected_fields.contains(b"XMBR") {
            assignment.execute([record.form_id])?;
        }
        if !record.payload_complete
            || record.rejected_fields.contains(b"XLRM")
            || record.rejected_fields.contains(b"XRMR")
        {
            incomplete.execute([record.cell_form_id])?;
        }
    }
    Ok(())
}

/// Migrates a `room_portals` table of the older shape (`ref_id` alone as the key, no
/// `pair_index`) to the current one in place: every existing row is copied with `pair_index` 0.
/// `CREATE TABLE IF NOT EXISTS` cannot change an existing table's key, and dropping the table
/// would leave it empty beside `room_bounds` after a subset export or a failed full export.
/// Runs in one savepoint, so it is atomic whether or not a transaction is already open. Call
/// before executing `ROOM_TABLES_SQL`.
pub fn migrate_room_portals(conn: &Connection) -> Result<()> {
    let has_table: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='room_portals'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if has_table.is_none() {
        return Ok(());
    }
    let has_pair_index: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('room_portals') WHERE name='pair_index'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if has_pair_index.is_some() {
        return Ok(());
    }
    // Outside a transaction the savepoint opens one, and RELEASE then commits it; that
    // commit can fail (SQLITE_BUSY while another connection reads the file).
    let outermost = conn.is_autocommit();
    conn.execute_batch("SAVEPOINT room_portals_migration")?;
    let migrated = conn.execute_batch(
        "DROP TABLE IF EXISTS room_portals_new;
         CREATE TABLE room_portals_new (
             ref_id INTEGER NOT NULL, pair_index INTEGER NOT NULL DEFAULT 0,
             cell_id INTEGER NOT NULL,
             pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
             rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
             half_x REAL NOT NULL, half_z REAL NOT NULL,
             room_a INTEGER, room_b INTEGER,
             PRIMARY KEY (ref_id, pair_index)
         );
         INSERT INTO room_portals_new (ref_id, pair_index, cell_id, pos_x, pos_y, pos_z,
                                       rot_x, rot_y, rot_z, half_x, half_z, room_a, room_b)
             SELECT ref_id, 0, cell_id, pos_x, pos_y, pos_z,
                    rot_x, rot_y, rot_z, half_x, half_z, room_a, room_b
             FROM room_portals;
         DROP TABLE room_portals;
         ALTER TABLE room_portals_new RENAME TO room_portals;
         CREATE INDEX IF NOT EXISTS idx_room_portals_cell ON room_portals(cell_id);",
    );
    match migrated.and_then(|()| conn.execute_batch("RELEASE room_portals_migration")) {
        Ok(()) => Ok(()),
        Err(error) => {
            // Undo the partial migration and close the savepoint, including after a failed
            // RELEASE, so no transaction is left open; report the original error. When the
            // savepoint opened the transaction, ROLLBACK ends it (a second RELEASE could fail
            // the same way); inside the caller's transaction, releasing a nested savepoint
            // commits nothing and cannot be busy.
            let cleanup = if outermost {
                "ROLLBACK"
            } else {
                "ROLLBACK TO room_portals_migration; RELEASE room_portals_migration"
            };
            let _ = conn.execute_batch(cleanup);
            Err(error)
        }
    }
}

/// Counters for the conversion report (`integration-report.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomGraphReport {
    pub rooms: u64,
    /// Portal markers written, one count per marker however many `room_portals` rows it has.
    pub portals: u64,
    pub links: u64,
    /// `room_portals` rows whose `room_a` or `room_b` is NULL (0, missing or not a room of the
    /// same cell). A portal with several pairs adds one per such row.
    pub portals_with_missing_side: u64,
    /// Portal markers whose `XPOD` holds more than one (origin, destination) pair.
    pub portals_with_extra_pairs: u64,
    /// Portal markers whose `XPOD` length is not a multiple of 8; the trailing bytes are ignored.
    pub portals_with_trailing_xpod_bytes: u64,
    /// Interior room markers that are not boxes; they get no row and their objects count as
    /// being in no room.
    pub non_box_rooms_skipped: u64,
    /// Portal markers without an `XPRM` (no size); they get no row.
    pub portals_without_size_skipped: u64,
    /// Markers skipped for invalid geometry or ambiguous portal fields.
    pub invalid_markers_skipped: u64,
    /// Objects in a cell with rooms whose pivot is in no room box.
    pub objects_in_no_room: u64,
    /// Objects in a cell with rooms whose pivot is in more than one room box.
    pub objects_in_several_rooms: u64,
    /// `reference_rooms` rows with `contained = 0`, added after model bounds are known.
    pub overlap_rows: u64,
    /// Supported occlusion primitives exported, including authored state flags.
    #[serde(default)]
    pub occluders: u64,
    /// Occlusion markers with missing, malformed or unsupported geometry.
    #[serde(default)]
    pub invalid_occluders_skipped: u64,
    /// Markers with invalid source `XRMR` blocks or link counts, grouped by supplying plugin.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub room_link_anomalies: BTreeMap<String, RoomLinkAnomalies>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomLinkAnomalies {
    pub count: u64,
    /// Lowest FormID among affected markers from this plugin.
    pub first_marker: u32,
    pub first_cell: u32,
}

const STAT_NAMES: [&str; 14] = [
    "rooms",
    "portals",
    "links",
    "portals_with_missing_side",
    "portals_with_extra_pairs",
    "portals_with_trailing_xpod_bytes",
    "non_box_rooms_skipped",
    "portals_without_size_skipped",
    "invalid_markers_skipped",
    "objects_in_no_room",
    "objects_in_several_rooms",
    "overlap_rows",
    "occluders",
    "invalid_occluders_skipped",
];

impl RoomGraphReport {
    fn values(&self) -> [u64; 14] {
        [
            self.rooms,
            self.portals,
            self.links,
            self.portals_with_missing_side,
            self.portals_with_extra_pairs,
            self.portals_with_trailing_xpod_bytes,
            self.non_box_rooms_skipped,
            self.portals_without_size_skipped,
            self.invalid_markers_skipped,
            self.objects_in_no_room,
            self.objects_in_several_rooms,
            self.overlap_rows,
            self.occluders,
            self.invalid_occluders_skipped,
        ]
    }

    fn write(&self, conn: &Connection) -> Result<()> {
        conn.execute("DELETE FROM room_graph_stats", [])?;
        for (name, value) in STAT_NAMES.iter().zip(self.values()) {
            conn.execute(
                "INSERT INTO room_graph_stats(name, value) VALUES (?1, ?2)",
                params![name, value as i64],
            )?;
        }
        let mut statement = conn.prepare(
            "INSERT INTO room_link_anomalies(plugin,count,first_marker,first_cell) VALUES (?1,?2,?3,?4)",
        )?;
        for (plugin, anomalies) in &self.room_link_anomalies {
            statement.execute(params![
                plugin,
                anomalies.count,
                anomalies.first_marker,
                anomalies.first_cell
            ])?;
        }
        Ok(())
    }

    /// Reads the counters of the last export; all zero when the tables are absent.
    pub fn read(conn: &Connection) -> Result<Self> {
        let present: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='room_graph_stats'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let mut report = Self::default();
        if present.is_none() {
            return Ok(report);
        }
        let mut statement = conn.prepare("SELECT name, value FROM room_graph_stats")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        for row in rows {
            let (name, value) = row?;
            match name.as_str() {
                "rooms" => report.rooms = value,
                "portals" => report.portals = value,
                "links" => report.links = value,
                "portals_with_missing_side" => report.portals_with_missing_side = value,
                "portals_with_extra_pairs" => report.portals_with_extra_pairs = value,
                "portals_with_trailing_xpod_bytes" => {
                    report.portals_with_trailing_xpod_bytes = value
                }
                "non_box_rooms_skipped" => report.non_box_rooms_skipped = value,
                "portals_without_size_skipped" => report.portals_without_size_skipped = value,
                "invalid_markers_skipped" => report.invalid_markers_skipped = value,
                "objects_in_no_room" => report.objects_in_no_room = value,
                "objects_in_several_rooms" => report.objects_in_several_rooms = value,
                "overlap_rows" => report.overlap_rows = value,
                "occluders" => report.occluders = value,
                "invalid_occluders_skipped" => report.invalid_occluders_skipped = value,
                _ => {}
            }
        }
        if table_exists(conn, "room_link_anomalies")? {
            let mut statement = conn
                .prepare("SELECT plugin,count,first_marker,first_cell FROM room_link_anomalies")?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get(0)?,
                    RoomLinkAnomalies {
                        count: row.get(1)?,
                        first_marker: row.get(2)?,
                        first_cell: row.get(3)?,
                    },
                ))
            })?;
            report.room_link_anomalies = rows.collect::<Result<_>>()?;
        }
        Ok(report)
    }
}

/// Deletes every row of the room tables. Called at the start of a full export.
pub fn clear(conn: &Connection) -> Result<()> {
    for table in [
        "room_bounds",
        "room_portals",
        "room_occluders",
        "room_occluder_links",
        "room_links",
        "room_reference_rules",
        "room_graph_cells",
        "room_model_bounds",
        "reference_rooms",
        "room_graph_stats",
        "room_link_anomalies",
    ] {
        conn.execute(&format!("DELETE FROM {table}"), [])?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Geometry (Creation space: Z-up, units as in the database)
// ---------------------------------------------------------------------------------------------

type Mat3 = [[f64; 3]; 3];

fn matmul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (column, cell) in out_row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[row][k] * b[k][column]).sum();
        }
    }
    out
}

fn axis_rotation(axis: usize, angle: f64) -> Mat3 {
    let (s, c) = angle.sin_cos();
    match axis {
        0 => [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
        1 => [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
        _ => [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
    }
}

/// Rotation matrix, in Creation space, that a reference with `REFR/DATA` angles `rot` applies
/// to its model: `Rx(-x) * Ry(-y) * Rz(-z)`. This is the composition
/// `shared::coordinates::creation_euler_to_runtime_quaternion` documents (Creation angles turn
/// clockwise), expressed before the Creation-to-runtime basis change.
pub fn creation_rotation(rot: [f32; 3]) -> [[f64; 3]; 3] {
    let [x, y, z] = rot.map(f64::from);
    matmul(
        &axis_rotation(0, -x),
        &matmul(&axis_rotation(1, -y), &axis_rotation(2, -z)),
    )
}

fn transpose(m: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (column, cell) in out_row.iter_mut().enumerate() {
            *cell = m[column][row];
        }
    }
    out
}

fn apply(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|row| (0..3).map(|k| m[row][k] * v[k]).sum())
}

/// A room box: centre, orientation and half-extents, all in Creation space.
#[derive(Debug, Clone, Copy)]
pub struct RoomBox {
    pub center: [f64; 3],
    /// Inverse (transpose) of `creation_rotation(rot)`: world offset to room-local offset.
    to_local: Mat3,
    pub half: [f64; 3],
}

impl RoomBox {
    pub fn new(position: [f32; 3], rotation: [f32; 3], half: [f32; 3]) -> Self {
        Self {
            center: position.map(f64::from),
            to_local: transpose(&creation_rotation(rotation)),
            half: half.map(f64::from),
        }
    }

    fn local(&self, world: [f64; 3]) -> [f64; 3] {
        apply(
            &self.to_local,
            [0, 1, 2].map(|axis| world[axis] - self.center[axis]),
        )
    }

    /// Whether a point (an object's pivot) lies inside the box, edges included.
    pub fn contains(&self, world: [f64; 3]) -> bool {
        let local = self.local(world);
        (0..3).all(|axis| local[axis].abs() <= self.half[axis])
    }

    /// Conservative overlap test for a convex set of world points (an object's bounding-box
    /// corners): the points' axis-aligned bounds in the room frame against the half-extents.
    /// It can report an overlap that does not exist (never miss one), which only makes the
    /// engine draw more.
    pub fn overlaps_points(&self, world_points: &[[f64; 3]]) -> bool {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for point in world_points {
            let local = self.local(*point);
            for axis in 0..3 {
                min[axis] = min[axis].min(local[axis]);
                max[axis] = max[axis].max(local[axis]);
            }
        }
        (0..3).all(|axis| min[axis] <= self.half[axis] && max[axis] >= -self.half[axis])
    }
}

/// World-space (Creation) corners of a model's bounds placed by a reference.
///
/// `bounds_min`/`bounds_max` are `statics.bounds_*`, which `finalize_world_database` reads from
/// the converted GLB: glTF axes (Y-up, `[x, z, -y]` of Creation) in **Creation units** (the
/// converter applies no metre scale; the engine uses the same numbers as Bevy units). The corner
/// is mapped back to Creation axes, scaled, rotated by the reference's rotation and moved to its
/// position, which is what the engine's reference transform does after the basis change.
pub fn placed_bounds_corners(
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
) -> [[f64; 3]; 8] {
    let rotation = creation_rotation(rotation);
    let scale = f64::from(scale);
    let mut corners = [[0.0; 3]; 8];
    for (index, corner) in corners.iter_mut().enumerate() {
        let pick = |axis: usize| {
            if index >> axis & 1 == 0 {
                bounds_min[axis]
            } else {
                bounds_max[axis]
            }
        };
        let runtime = [pick(0), pick(1), pick(2)];
        let creation = shared::coordinates::runtime_to_creation_vector(runtime).map(f64::from);
        let rotated = apply(&rotation, creation.map(|value| value * scale));
        *corner = [0, 1, 2].map(|axis| rotated[axis] + f64::from(position[axis]));
    }
    corners
}

// ---------------------------------------------------------------------------------------------
// ESM export stage
// ---------------------------------------------------------------------------------------------

struct Room {
    ref_id: u32,
    shape: RoomBox,
}

fn f32_at(data: &[u8], offset: usize) -> Option<f32> {
    data.get(offset..offset + 4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four bytes")))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")))
}

fn transform_of(data: &[u8]) -> Option<([f32; 3], [f32; 3])> {
    let (words, _) = data.first_chunk::<24>()?.as_chunks::<4>();
    let [x, y, z, rx, ry, rz] = std::array::from_fn(|index| f32::from_le_bytes(words[index]));
    Some(([x, y, z], [rx, ry, rz]))
}

fn unique_marker_geometry<'a>(
    sources: &Option<RoomLinkSources<'a>>,
    form_id: u32,
    view: &SubrecordView<'a>,
    tag: &[u8; 4],
) -> Option<&'a [u8]> {
    if let Some(RoomLinkSources::Typed(decoded)) = sources
        && let Some(record) = decoded.records.get(&form_id)
    {
        return record.unique_source_subrecord(tag);
    }
    let mut fields = view.find_all(tag);
    let data = fields.next()?;
    fields.next().is_none().then_some(data)
}

fn is_placed(record_type: &[u8; 4]) -> bool {
    super::is_placed_record_type(record_type)
}

#[derive(Clone, Copy)]
pub(super) enum RoomLinkSources<'a> {
    Legacy(&'a HashMap<u32, RoomLinkLayout>),
    Typed(&'a ReadResult),
}

#[derive(Clone, Default)]
pub(super) struct RoomLinkLayout {
    flags: i64,
    lighting: bool,
    image_space: bool,
    // Ordinals among four-byte XLRMs. Either reader can discard malformed widths.
    links: Range<usize>,
    four_byte_links: usize,
    anomaly: Option<String>,
    /// The `XRMR` header is present but cannot be read (short, repeated or unframed), so the
    /// flags are unknown. An absent header is different: no flag bits were authored.
    flags_unreadable: bool,
}

fn is_room_link_field(tag: &[u8]) -> bool {
    matches!(tag, b"XRMR" | b"LNAM" | b"INAM" | b"XLRM")
}

impl RoomLinkLayout {
    pub(super) fn from_raw(record: &RawRecord) -> Self {
        Self::read(
            record
                .subrecords
                .iter()
                .map(|(tag, data)| (tag.as_slice(), data.as_slice())),
        )
    }

    fn from_decoded(record: &DecodedRecord) -> Self {
        match record.framed_source_subrecords() {
            Some(fields) => Self::read(fields.iter().map(|(tag, data)| (tag.as_slice(), *data))),
            None => Self {
                anomaly: Some("has incomplete source subrecord framing".into()),
                flags_unreadable: true,
                ..Self::default()
            },
        }
    }

    fn read<'a>(fields: impl Iterator<Item = (&'a [u8], &'a [u8])> + Clone) -> Self {
        let mut layout = Self::default();
        let mut headers = fields
            .clone()
            .enumerate()
            .filter(|(_, (tag, _))| *tag == b"XRMR");
        let Some((header_index, (_, header))) = headers.next() else {
            if fields.clone().any(|(tag, _)| is_room_link_field(tag)) {
                layout.anomaly = Some("has room-link fields without an XRMR header".into());
            }
            return layout;
        };
        if headers.next().is_some() {
            layout.anomaly = Some("has repeated XRMR headers".into());
            layout.flags_unreadable = true;
            return layout;
        }
        let &[low, high, master, padding] = header else {
            layout.anomaly = Some("has a malformed XRMR header".into());
            layout.flags_unreadable = true;
            return layout;
        };
        layout.flags = i64::from(high) | (i64::from(u16::from_le_bytes([master, padding])) << 8);
        let declared = usize::from(u16::from_le_bytes([low, high]) & 0x3fff);
        let is_four_byte_link = |(tag, data): &(&[u8], &[u8])| *tag == b"XLRM" && data.len() == 4;
        layout.four_byte_links = fields.clone().filter(is_four_byte_link).count();
        let before_header = fields.clone().take(header_index);
        let first_link = before_header.clone().filter(is_four_byte_link).count();
        let mut block = fields.clone().skip(header_index + 1).peekable();
        for (flag, tag, publish) in [
            (0x80, b"LNAM", &mut layout.lighting),
            (0x40, b"INAM", &mut layout.image_space),
        ] {
            if high & flag == 0 {
                continue;
            }
            if block
                .next_if(|(found, data)| *found == tag && data.len() == 4)
                .is_none()
            {
                layout.anomaly = Some(format!(
                    "is missing the flagged four-byte {} slot",
                    String::from_utf8_lossy(tag)
                ));
                break;
            }
            *publish = fields.clone().filter(|(found, _)| *found == tag).count() == 1;
        }
        if layout.anomaly.is_some() {
            return layout;
        }

        let mut valid = 0;
        while valid < declared && block.next_if(is_four_byte_link).is_some() {
            valid += 1;
        }
        layout.links = first_link..first_link + valid;
        let actual = fields.filter(|(tag, _)| *tag == b"XLRM").count();
        if actual != declared {
            layout.anomaly = Some(format!(
                "has {actual} XLRM entries but XRMR declares {declared}"
            ));
        } else if before_header
            .chain(block)
            .any(|(tag, _)| is_room_link_field(tag))
        {
            layout.anomaly = Some("has misplaced or malformed room-link fields".into());
        }
        layout
    }
}

/// Rebuilds the room tables from the merged records. `interior_cells` are the CELL records
/// with no worldspace and no grid position; markers elsewhere are ignored.
///
/// Reads parsed subrecords only, never the stored blob. Writes `contained = 1` membership rows
/// (the object's pivot is inside the box); `add_overlap_rows` adds the rest after integration.
/// References with missing or short 24-byte DATA have no usable placement and cannot become
/// room boxes, portals or pivot memberships. The exporter reports those omissions once.
pub fn export_room_graph(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    interior_cells: &HashSet<u32>,
) -> Result<RoomGraphReport> {
    export_room_graph_with_sources(
        tx,
        master,
        interior_cells,
        None,
        None,
        None,
        &mut ProjectionOmissions::default(),
    )
}

pub(super) fn export_room_graph_with_sources(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    interior_cells: &HashSet<u32>,
    sources: Option<RoomLinkSources<'_>>,
    order: Option<&LoadOrder>,
    damaged_placement_cells: Option<&HashSet<u32>>,
    omissions: &mut ProjectionOmissions,
) -> Result<RoomGraphReport> {
    clear(tx)?;
    let occluder_sources = match &sources {
        Some(RoomLinkSources::Typed(decoded)) => Some(*decoded).zip(order),
        _ => None,
    };
    let mut occluders = occluders::export(tx, master, interior_cells, occluder_sources, omissions)?;
    let links_verified = !occluders.unverified_link_targets;
    occluders
        .incomplete_cells
        .extend(damaged_placement_cells.into_iter().flatten().copied());
    let report = project_room_graph(
        tx,
        master,
        interior_cells,
        sources,
        order.map_or(&[][..], |order| &order.names),
        occluders,
        None,
        omissions,
    )?;
    if let Some(RoomLinkSources::Typed(decoded)) = sources {
        restore_rejected_room_fields(
            tx,
            decoded
                .records
                .values()
                .filter(|r| is_placed(&r.record_type)),
        )?;
    }
    report.write(tx)?;
    tx.execute(
        "INSERT INTO room_graph_stats(name,value) VALUES ('visibility_version',2),('assignment_version',1)",
        [],
    )?;
    if occluder_sources.is_some() {
        tx.execute(
            "INSERT INTO room_graph_stats(name,value) VALUES ('occlusion_links_verified',?1)",
            [links_verified],
        )?;
    }
    Ok(report)
}

struct PlacementRefresh<'a> {
    changed: &'a HashSet<u32>,
    previous_outside: HashMap<(u32, i64), [bool; 2]>,
}

/// Reproject a touched interior using the effective runtime reference rows. Earlier source
/// rejections may be absent from their canonical blobs. Retain unchanged ownership rules and
/// outside proof, and require a valid pre-edit certificate for cells that already existed.
pub(super) fn refresh_placement_cells(
    tx: &Transaction<'_>,
    cells: &HashSet<u32>,
    fresh_cells: &HashSet<u32>,
    changed: &HashSet<u32>,
    layouts: &HashMap<u32, RoomLinkLayout>,
    plugin_names: &[String],
) -> Result<Vec<u32>> {
    let mut occluders = occluders::Exported::default();
    let has_occluders = table_exists(tx, "room_occluders")?;
    let mut previous_outside = HashMap::new();
    let mut records = HashMap::new();
    for &cell in cells {
        let certificate = tx
            .query_row(
                "SELECT complete FROM room_graph_cells WHERE cell_id=?1",
                [cell],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if certificate.unwrap_or(i64::from(fresh_cells.contains(&cell))) != 1 {
            occluders.incomplete_cells.insert(cell);
        }
        if has_occluders
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM room_occluders WHERE cell_id=?1)",
                [cell],
                |row| row.get::<_, bool>(0),
            )?
        {
            occluders.cells.insert(cell);
        }
        let mut portals = tx.prepare_cached(
            "SELECT ref_id,pair_index,unbounded_a,unbounded_b FROM room_portals WHERE cell_id=?1",
        )?;
        for row in portals.query_map([cell], |row| {
            Ok(((row.get(0)?, row.get(1)?), [row.get(2)?, row.get(3)?]))
        })? {
            let (key, outside) = row?;
            previous_outside.insert(key, outside);
        }
        let mut references = tx.prepare_cached(
            "SELECT r.id,r.header_flags,r.data,coalesce(c.record_type,'REFR')
             FROM [references] r LEFT JOIN records c ON c.form_id=r.id WHERE r.cell_id=?1",
        )?;
        for row in references.query_map([cell], |row| {
            let form_id = row.get(0)?;
            let Some(blob) = row.get::<_, Option<Vec<u8>>>(2)? else {
                return Ok(None);
            };
            let record = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&blob)
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?;
            let record_type: String = row.get(3)?;
            Ok(Some(RawRecord {
                form_id,
                record_type: record_type.as_bytes().try_into().unwrap_or(*b"UNKN"),
                flags: row.get(1)?,
                subrecords: record
                    .subrecords
                    .into_iter()
                    .map(|field| (field.tag.to_vec(), field.data))
                    .collect(),
                cell_form_id: Some(cell),
                worldspace_form_id: None,
                load_order: 0,
            }))
        })? {
            if let Some(record) = row? {
                records.insert(record.form_id, record);
            } else {
                // Older packs can have a placement without enough data to certify its graph.
                occluders.incomplete_cells.insert(cell);
            }
        }
        for sql in [
            "DELETE FROM reference_rooms WHERE room_ref IN (SELECT ref_id FROM room_bounds WHERE cell_id=?1)",
            "DELETE FROM room_portals WHERE cell_id=?1",
            "DELETE FROM room_graph_cells WHERE cell_id=?1",
        ] {
            tx.prepare_cached(sql)?.execute([cell])?;
        }
    }
    for &id in changed {
        tx.prepare_cached("DELETE FROM room_links WHERE room_ref=?1")?
            .execute([id])?;
        tx.prepare_cached("DELETE FROM room_bounds WHERE ref_id=?1")?
            .execute([id])?;
    }
    project_room_graph(
        tx,
        &records,
        cells,
        Some(RoomLinkSources::Legacy(layouts)),
        plugin_names,
        occluders,
        Some(&PlacementRefresh {
            changed,
            previous_outside,
        }),
        // A placement refresh has no integration report; the marker's cell stays incomplete.
        &mut ProjectionOmissions::default(),
    )?;
    let mut certified = Vec::new();
    for &cell in cells {
        // The projection writes every negative certificate, including cells without rooms.
        // A remaining empty cell had complete input and may receive its first room later.
        tx.prepare_cached(
            "INSERT OR IGNORE INTO room_graph_cells(cell_id,complete) VALUES (?1,1)",
        )?
        .execute([cell])?;
        write_overlap_rows(tx, Some(cell))?;
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM room_graph_cells WHERE cell_id=?1 AND complete=1)",
            [cell],
            |row| row.get(0),
        )? {
            certified.push(cell);
        }
    }
    certified.sort_unstable();
    Ok(certified)
}

#[allow(clippy::too_many_arguments)]
fn project_room_graph(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    interior_cells: &HashSet<u32>,
    sources: Option<RoomLinkSources<'_>>,
    plugin_names: &[String],
    occluders: occluders::Exported,
    refresh: Option<&PlacementRefresh<'_>>,
    omissions: &mut ProjectionOmissions,
) -> Result<RoomGraphReport> {
    let mut report = RoomGraphReport::default();
    let mut link_anomalies: BTreeMap<&str, (RoomLinkAnomalies, String)> = BTreeMap::new();
    report.occluders = occluders.count;
    report.invalid_occluders_skipped = occluders.invalid;
    let mut incomplete = occluders.incomplete_cells;
    if let Some(RoomLinkSources::Typed(decoded)) = &sources {
        incomplete.extend(&decoded.damaged_cell_groups);
        // A winning placement can lose fields before reaching the runtime projection.
        incomplete.extend(decoded.records.values().filter_map(|record| {
            (is_placed(&record.record_type)
                && (!record.payload_complete
                    || record.rejected_fields.contains(b"XLRM")
                    || record.rejected_fields.contains(b"XRMR")))
            .then_some(record.cell_form_id)
            .flatten()
        }));
    }
    let mut ordered: Vec<(&u32, &RawRecord)> = master
        .iter()
        .filter(|(_, record)| {
            !record.is_deleted()
                && is_placed(&record.record_type)
                && record
                    .cell_form_id
                    .is_some_and(|cell| interior_cells.contains(&cell))
        })
        .collect();
    ordered.sort_unstable_by_key(|(form_id, _)| **form_id);

    let base_of = |record: &RawRecord| {
        SubrecordView::new(&record.subrecords)
            .unique_form_id(b"NAME")
            .unwrap_or(0)
    };
    ordered.retain(|(_, record)| {
        let valid = base_of(record) != 0;
        if !valid {
            incomplete.extend(record.cell_form_id);
        }
        valid
    });

    // Keep unchanged room geometry and links exactly as the source reader published them.
    let mut rooms_by_cell = BTreeMap::new();
    if refresh.is_some() {
        for &cell in interior_cells {
            rooms_by_cell.extend(stored_room_boxes(tx, Some(cell))?);
        }
    }
    for &(&form_id, record) in &ordered {
        if &record.record_type != b"REFR"
            || base_of(record) != ROOM_MARKER_BASE
            || refresh.is_some_and(|refresh| !refresh.changed.contains(&form_id))
        {
            continue;
        }
        let cell = record.cell_form_id.unwrap_or(0);
        let view = SubrecordView::new(&record.subrecords);
        let layout = match &sources {
            Some(RoomLinkSources::Legacy(layouts)) => {
                // silent-ok: An absent legacy entry supplies no room-link slots or flags.
                layouts.get(&form_id).cloned().unwrap_or_default()
            }
            Some(RoomLinkSources::Typed(decoded)) => decoded
                .records
                .get(&form_id)
                .map(RoomLinkLayout::from_decoded)
                // silent-ok: An absent decoded record supplies no room-link slots or flags.
                .unwrap_or_default(),
            None => RoomLinkLayout::from_raw(record),
        };
        let links = record.subrecords.iter().filter_map(|(tag, data)| {
            // silent-ok: Only four-byte XLRMs occupy slots in the source layout.
            let bytes: [u8; 4] = data.as_slice().try_into().ok()?;
            (tag == b"XLRM").then(|| u32::from_le_bytes(bytes))
        });
        // Match the resolved IDs by source occurrence. If either reader omitted a four-byte
        // occurrence, the remaining IDs cannot be assigned to their source slots by ordinal.
        let matching_occurrences = links.clone().count() == layout.four_byte_links;
        let anomaly = layout.anomaly.as_deref().or_else(|| {
            if !matching_occurrences {
                Some("lost a four-byte XLRM occurrence during decoding")
            } else {
                links
                    .clone()
                    .any(|id| id == 0)
                    .then_some("has an unresolved XLRM target")
            }
        });
        if let Some(anomaly) = anomaly {
            incomplete.insert(cell);
            let plugin = plugin_names
                .get(record.load_order as usize)
                .map_or("<unknown plugin>", String::as_str);
            let (summary, _) = link_anomalies.entry(plugin).or_insert_with(|| {
                (
                    RoomLinkAnomalies {
                        count: 0,
                        first_marker: form_id,
                        first_cell: cell,
                    },
                    anomaly.to_owned(),
                )
            });
            summary.count += 1;
        }
        if layout.flags_unreadable {
            // The room flags (master bit and link header) are unknown. Storing flags 0 would
            // pass an unreadable field off as authored data, so the box gets no row. The cell
            // is already incomplete and falls back to whole-cell visibility.
            omissions.record(
                OmissionCategory::RoomMarkerUnreadableFlags,
                form_id,
                format!(
                    "room marker {}; no room_bounds row",
                    layout
                        .anomaly
                        .as_deref()
                        .unwrap_or("has an unreadable XRMR header")
                ),
            );
            report.invalid_markers_skipped += 1;
            continue;
        }
        let Some(xprm) = unique_marker_geometry(&sources, form_id, &view, b"XPRM")
            .filter(|data| data.len() == 32)
        else {
            incomplete.insert(cell);
            report.non_box_rooms_skipped += 1;
            continue;
        };
        if u32_at(xprm, 28) != Some(SHAPE_BOX) {
            incomplete.insert(cell);
            report.non_box_rooms_skipped += 1;
            continue;
        }
        let Some((position, rotation)) = unique_marker_geometry(&sources, form_id, &view, b"DATA")
            .filter(|data| data.len() == 24)
            .and_then(transform_of)
        else {
            incomplete.insert(cell);
            report.invalid_markers_skipped += 1;
            continue;
        };
        let half = [
            f32_at(xprm, 0).unwrap_or(0.0),
            f32_at(xprm, 4).unwrap_or(0.0),
            f32_at(xprm, 8).unwrap_or(0.0),
        ];
        if !half
            .iter()
            .chain(&position)
            .chain(&rotation)
            .all(|v| v.is_finite())
            || half.iter().any(|value| *value < 0.0)
        {
            report.invalid_markers_skipped += 1;
            incomplete.insert(cell);
            continue;
        }
        let optional_form_id = |present: bool, tag: &[u8; 4]| {
            present
                .then(|| view.find(tag))
                .flatten()
                .filter(|data| data.len() == 4)
                .and_then(|data| u32_at(data, 0))
                .filter(|id| *id != 0)
        };
        tx.execute(
            "INSERT OR REPLACE INTO room_bounds(ref_id, cell_id, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, half_x, half_y, half_z, lighting_template_id, image_space_id, flags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                form_id, cell, position[0], position[1], position[2], rotation[0], rotation[1],
                rotation[2], half[0], half[1], half[2], optional_form_id(layout.lighting, b"LNAM"),
                optional_form_id(layout.image_space, b"INAM"), layout.flags
            ],
        )?;
        for (index, linked) in links
            .skip(layout.links.start)
            .take(if matching_occurrences {
                layout.links.len()
            } else {
                0
            })
            .enumerate()
        {
            // Remapping can clear a failed link to zero; that does not prove no join existed.
            if linked != 0 {
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO room_links(room_ref, linked_ref, link_index) VALUES (?1, ?2, ?3)",
                    params![form_id, linked, index],
                )?;
                if inserted == 0 {
                    // The reverse scan must see the last authored occurrence of this target.
                    tx.execute(
                        "UPDATE room_links SET link_index=?3 WHERE room_ref=?1 AND linked_ref=?2",
                        params![form_id, linked, index],
                    )?;
                }
                report.links += inserted as u64;
            } else {
                incomplete.insert(cell);
            }
        }
        report.rooms += 1;
        rooms_by_cell.entry(cell).or_default().push(Room {
            ref_id: form_id,
            shape: RoomBox::new(position, rotation, half),
        });
    }

    for (plugin, (summary, anomaly)) in link_anomalies {
        let RoomLinkAnomalies {
            count,
            first_marker,
            first_cell,
        } = summary;
        eprintln!(
            "warning: {plugin} has {count} room-link anomalies (first: room marker {first_marker:08X} in cell {first_cell:08X} {anomaly})"
        );
        report
            .room_link_anomalies
            .insert(plugin.to_owned(), summary);
    }

    for &(&form_id, record) in &ordered {
        if &record.record_type != b"REFR" || base_of(record) != PORTAL_MARKER_BASE {
            continue;
        }
        let cell = record.cell_form_id.unwrap_or(0);
        let view = SubrecordView::new(&record.subrecords);
        if view.find(b"XPRM").is_none() {
            incomplete.insert(cell);
            report.portals_without_size_skipped += 1;
            continue;
        }
        let Some(xprm) = unique_marker_geometry(&sources, form_id, &view, b"XPRM")
            .filter(|data| data.len() == 32 && u32_at(data, 28) == Some(SHAPE_PORTAL))
        else {
            incomplete.insert(cell);
            report.invalid_markers_skipped += 1;
            continue;
        };
        let Some((position, rotation)) = unique_marker_geometry(&sources, form_id, &view, b"DATA")
            .filter(|data| data.len() == 24)
            .and_then(transform_of)
        else {
            incomplete.insert(cell);
            report.invalid_markers_skipped += 1;
            continue;
        };
        let half_x = f32_at(xprm, 0).unwrap_or(0.0);
        let half_z = f32_at(xprm, 8).unwrap_or(0.0);
        if ![half_x, half_z]
            .iter()
            .chain(&position)
            .chain(&rotation)
            .all(|v| v.is_finite())
            || half_x <= 0.0
            || half_z <= 0.0
        {
            report.invalid_markers_skipped += 1;
            incomplete.insert(cell);
            continue;
        }
        // `XPOD` is an array of (origin, destination) pairs of 8 bytes (xEdit 4.1.5f defs).
        // One row per pair; a portal with no complete pair gets one row with NULL sides.
        let mut endpoints = record.subrecords.iter().filter(|(tag, _)| tag == b"XPOD");
        let xpod = endpoints
            .next()
            .map_or(&[][..], |(_, data)| data.as_slice());
        if endpoints.next().is_some() {
            incomplete.insert(cell);
            report.invalid_markers_skipped += 1;
            continue;
        }
        let pairs = xpod.len() / 8;
        if pairs > 1 {
            report.portals_with_extra_pairs += 1;
        }
        let truncated = !xpod.len().is_multiple_of(8);
        if truncated {
            report.portals_with_trailing_xpod_bytes += 1;
            incomplete.insert(cell);
        }
        if pairs == 0 {
            incomplete.insert(cell);
        }
        let side = |pair: usize, offset: usize, incomplete: &mut HashSet<u32>| {
            let id = u32_at(xpod, pair * 8 + offset).filter(|&id| pair < pairs && id != 0)?;
            if rooms_by_cell
                .get(&cell)
                .is_some_and(|rooms| rooms.iter().any(|room| room.ref_id == id))
            {
                Some(id)
            } else {
                incomplete.insert(cell);
                None
            }
        };
        for pair in 0..pairs.max(1) {
            let room_a = side(pair, 0, &mut incomplete);
            let room_b = side(pair, 4, &mut incomplete);
            let pair_index = i64::try_from(pair)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let proven = match refresh {
                Some(refresh) if !refresh.changed.contains(&form_id) => refresh
                    .previous_outside
                    .get(&(form_id, pair_index))
                    .copied()
                    .unwrap_or([false; 2]),
                _ => [true; 2],
            };
            let outside = [0, 4].map(|offset| {
                proven[offset / 4] && !truncated && u32_at(xpod, pair * 8 + offset) == Some(0)
            });
            if refresh.is_some()
                && ((room_a.is_none() && !outside[0]) || (room_b.is_none() && !outside[1]))
            {
                incomplete.insert(cell);
            }
            tx.execute(
                "INSERT OR REPLACE INTO room_portals(ref_id, pair_index, cell_id, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, half_x, half_z, room_a, room_b, unbounded_a, unbounded_b)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    form_id, pair_index, cell, position[0], position[1], position[2],
                    rotation[0], rotation[1], rotation[2], half_x, half_z, room_a, room_b,
                    outside[0], outside[1]
                ],
            )?;
            if room_a.is_none() || room_b.is_none() {
                report.portals_with_missing_side += 1;
            }
        }
        report.portals += 1;
    }

    // Marker validation above needs missing transforms to flag incomplete cells. The placement
    // exporter reports missing or short DATA once as reference_unusable_data.
    ordered.retain(|(_, record)| {
        SubrecordView::new(&record.subrecords)
            .find(b"DATA")
            .is_some_and(|data| data.len() >= 24)
    });

    // Assignment rules are independent of geometric membership and survive missing models.
    for &(&id, record) in &ordered {
        if refresh.is_some_and(|refresh| !refresh.changed.contains(&id)) {
            continue;
        }
        let base = base_of(record);
        if matches!(
            base,
            ROOM_MARKER_BASE | PORTAL_MARKER_BASE | OCCLUSION_MARKER_BASE
        ) {
            continue;
        }
        let view = SubrecordView::new(&record.subrecords);
        let mut targets = view.find_all(b"XMBR");
        let authored_target = targets.next();
        let target = authored_target
            // silent-ok: A malformed authored XMBR selects the unknown assignment below.
            .and_then(|data| <[u8; 4]>::try_from(data).ok())
            .map(u32::from_le_bytes)
            .filter(|&id| id != 0 && targets.next().is_none());
        let assignment = if target.is_some() {
            "target"
        } else if authored_target.is_some() {
            "unknown"
        } else if record.flags & shared::records::MULTIBOUND_REFERENCE_FLAG != 0 {
            "none"
        } else if view.find(b"XPRM").is_some()
            || matches!(&sources, Some(RoomLinkSources::Typed(decoded))
                if decoded.records.get(&id).is_some_and(|source| source.rejected_fields.contains(b"XPRM")))
        {
            // Native occlusion primitives can substitute their shape's translation for NiBound.
            "unknown"
        } else {
            "auto"
        };
        tx.execute(
            "INSERT INTO room_reference_rules(reference_id,cell_id,assignment,target_ref) VALUES (?1,?2,?3,?4)",
            params![id,record.cell_form_id,assignment,target],
        )?;
    }
    // Legacy membership remains available to older readers.
    {
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO reference_rooms(reference_id, room_ref, contained) VALUES (?1, ?2, 1)",
        )?;
        for &(&form_id, record) in &ordered {
            let base = base_of(record);
            if base == ROOM_MARKER_BASE
                || base == PORTAL_MARKER_BASE
                || base == OCCLUSION_MARKER_BASE
                || record.flags & shared::records::MULTIBOUND_REFERENCE_FLAG != 0
            {
                continue;
            }
            let Some(rooms) = record
                .cell_form_id
                .and_then(|cell| rooms_by_cell.get(&cell))
            else {
                continue;
            };
            let Some((position, _)) = SubrecordView::new(&record.subrecords)
                .find(b"DATA")
                .and_then(transform_of)
            else {
                continue;
            };
            match write_pivot_rows(&mut insert, rooms, form_id, position)? {
                0 => report.objects_in_no_room += 1,
                1 => {}
                _ => report.objects_in_several_rooms += 1,
            }
        }
    }
    let mut dangling_links = tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM room_links l JOIN room_bounds source ON source.ref_id=l.room_ref \
         WHERE source.cell_id=?1 AND NOT EXISTS(SELECT 1 FROM room_bounds target \
         WHERE target.ref_id=l.linked_ref AND target.cell_id=?1))",
    )?;
    let cells: BTreeSet<_> = rooms_by_cell
        .keys()
        .copied()
        .chain(occluders.cells)
        .chain(incomplete.intersection(interior_cells).copied())
        .collect();
    for cell in cells {
        let dangling: bool = dangling_links.query_row([cell], |row| row.get(0))?;
        tx.execute(
            "INSERT INTO room_graph_cells VALUES (?1,?2)",
            params![cell, !incomplete.contains(&cell) && !dangling],
        )?;
    }
    Ok(report)
}

fn write_pivot_rows(
    insert: &mut rusqlite::Statement<'_>,
    rooms: &[Room],
    id: u32,
    position: [f32; 3],
) -> Result<usize> {
    let pivot = position.map(f64::from);
    let mut containing_rooms = 0;
    for room in rooms.iter().filter(|room| room.shape.contains(pivot)) {
        insert.execute(params![id, room.ref_id])?;
        containing_rooms += 1;
    }
    Ok(containing_rooms)
}

// ---------------------------------------------------------------------------------------------
// Integration stage
// ---------------------------------------------------------------------------------------------

pub(super) fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// Adds `contained = 0` membership rows: objects whose pivot is outside a room box but whose
/// world bounds overlap it. Needs `statics.bounds_*`, which exist only after
/// `finalize_world_database` has read the converted models. Safe to run again: it replaces
/// every `contained = 0` row. Returns the number of rows written.
pub fn add_overlap_rows(conn: &mut Connection) -> Result<u64> {
    let tx = conn.transaction()?;
    let written = refresh_overlap_rows(&tx)?;
    tx.commit()?;
    Ok(written)
}

/// Refresh the published overlap memberships from the current bounds within a transaction.
pub(crate) fn refresh_overlap_rows(tx: &Transaction<'_>) -> Result<u64> {
    if !table_exists(tx, "room_bounds")? || !table_exists(tx, "reference_rooms")? {
        return Ok(0);
    }
    write_overlap_rows(tx, None)
}

fn stored_room_boxes(tx: &Transaction<'_>, cell: Option<u32>) -> Result<BTreeMap<u32, Vec<Room>>> {
    let mut rooms_by_cell: BTreeMap<u32, Vec<Room>> = BTreeMap::new();
    {
        let mut statement = tx.prepare(
            "SELECT ref_id, cell_id, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, half_x, half_y, half_z FROM room_bounds WHERE ?1 IS NULL OR cell_id=?1 ORDER BY ref_id",
        )?;
        let rows = statement.query_map([cell], |row| {
            let f = |index: usize| row.get::<_, f32>(index);
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                [f(2)?, f(3)?, f(4)?],
                [f(5)?, f(6)?, f(7)?],
                [f(8)?, f(9)?, f(10)?],
            ))
        })?;
        for row in rows {
            let (ref_id, cell, position, rotation, half) = row?;
            rooms_by_cell.entry(cell).or_default().push(Room {
                ref_id,
                shape: RoomBox::new(position, rotation, half),
            });
        }
    }
    Ok(rooms_by_cell)
}

fn write_overlap_rows(tx: &Transaction<'_>, cell: Option<u32>) -> Result<u64> {
    // A reused older database may lack the bounds columns; with no rooms there is nothing to do.
    let has_rooms: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_bounds WHERE ?1 IS NULL OR cell_id=?1)",
        [cell],
        |row| row.get(0),
    )?;
    let has_bounds: bool = tx.query_row(
        "SELECT count(*) = 2 FROM pragma_table_info('statics') WHERE name IN ('bounds_valid', 'bounds_min_x')",
        [],
        |row| row.get(0),
    )?;
    let has_header_flags: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('references') WHERE name='header_flags')",
        [],
        |row| row.get(0),
    )?;
    if has_header_flags {
        // Repair geometric memberships left by previous conversions as well as new output.
        let flag = shared::records::MULTIBOUND_REFERENCE_FLAG;
        if let Some(cell) = cell {
            tx.prepare_cached(
                "DELETE FROM reference_rooms WHERE reference_id IN (
                    SELECT id FROM \"references\" WHERE cell_id=?2 AND (header_flags & ?1) != 0)",
            )?
            .execute(params![flag, cell])?;
        } else {
            tx.execute(
                "DELETE FROM reference_rooms WHERE reference_id IN (
                    SELECT id FROM \"references\" WHERE (header_flags & ?1) != 0)",
                [flag],
            )?;
        }
    }
    if !has_rooms || !has_bounds {
        return Ok(0);
    }
    let rooms_by_cell = stored_room_boxes(tx, cell)?;
    if cell.is_none() {
        tx.execute("DELETE FROM reference_rooms WHERE contained = 0", [])?;
    }
    let mut written = 0u64;
    {
        let header_flags = if has_header_flags {
            "r.header_flags"
        } else {
            "0"
        };
        let mut select = tx.prepare(&format!(
            "SELECT r.id, r.cell_id, r.pos_x, r.pos_y, r.pos_z, r.rot_x, r.rot_y, r.rot_z, r.scale,
                    s.bounds_min_x, s.bounds_min_y, s.bounds_min_z, s.bounds_max_x, s.bounds_max_y, s.bounds_max_z
             FROM \"references\" r JOIN statics s ON s.id = r.base_form_id
             WHERE s.bounds_valid = 1 AND r.base_form_id NOT IN (?1, ?2)
               AND r.cell_id IN (SELECT cell_id FROM room_bounds WHERE ?4 IS NULL OR cell_id=?4)
               AND ({header_flags} & ?3) = 0
             ORDER BY r.id",
        ))?;
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO reference_rooms(reference_id, room_ref, contained) VALUES (?1, ?2, 0)",
        )?;
        let mut rows = select.query(params![
            ROOM_MARKER_BASE,
            PORTAL_MARKER_BASE,
            shared::records::MULTIBOUND_REFERENCE_FLAG,
            cell,
        ])?;
        while let Some(row) = rows.next()? {
            let f = |index: usize| row.get::<_, f32>(index);
            let id: u32 = row.get(0)?;
            let cell: u32 = row.get(1)?;
            let corners = placed_bounds_corners(
                [f(9)?, f(10)?, f(11)?],
                [f(12)?, f(13)?, f(14)?],
                [f(2)?, f(3)?, f(4)?],
                [f(5)?, f(6)?, f(7)?],
                f(8)?,
            );
            let Some(rooms) = rooms_by_cell.get(&cell) else {
                continue;
            };
            for room in rooms
                .iter()
                .filter(|room| room.shape.overlaps_points(&corners))
            {
                // The pivot rows (contained = 1) win: the key is (reference, room).
                written += insert.execute(params![id, room.ref_id])? as u64;
            }
        }
    }
    if cell.is_none() {
        tx.execute(
            "INSERT OR REPLACE INTO room_graph_stats(name, value) VALUES ('overlap_rows', ?1)",
            [written as i64],
        )?;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_room_links_keep_the_last_authored_master_candidate() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::esm::exporter::create_tables(&conn).unwrap();
        let mut records = HashMap::new();
        for (id, master, links) in [
            (1u32, true, vec![]),
            (2, true, vec![]),
            (3, false, vec![1u32, 2, 1]),
        ] {
            let mut shape = vec![0; 32];
            for offset in [0, 4, 8] {
                shape[offset..offset + 4].copy_from_slice(&100f32.to_le_bytes());
            }
            shape[28..32].copy_from_slice(&1u32.to_le_bytes());
            let mut fields = vec![
                (b"NAME".to_vec(), ROOM_MARKER_BASE.to_le_bytes().to_vec()),
                (b"DATA".to_vec(), vec![0; 24]),
                (b"XPRM".to_vec(), shape),
                (
                    b"XRMR".to_vec(),
                    vec![u8::try_from(links.len()).unwrap(), 0, u8::from(master), 0],
                ),
            ];
            fields.extend(
                links
                    .into_iter()
                    .map(|id| (b"XLRM".to_vec(), id.to_le_bytes().to_vec())),
            );
            records.insert(
                id,
                RawRecord {
                    form_id: id,
                    record_type: *b"REFR",
                    flags: 0,
                    subrecords: fields,
                    cell_form_id: Some(7),
                    worldspace_form_id: None,
                    load_order: 0,
                },
            );
        }
        let tx = conn.transaction().unwrap();
        let report = export_room_graph(&tx, &records, &HashSet::from([7])).unwrap();
        tx.commit().unwrap();
        let links = conn
            .prepare("SELECT linked_ref FROM room_links WHERE room_ref=3 ORDER BY link_index")
            .unwrap()
            .query_map([], |r| r.get::<_, u32>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            links,
            vec![2, 1],
            "the last A in [A,B,A] must remain the first reverse-scan candidate"
        );
        assert_eq!(report.links, 2, "the counter describes stored rows");
        use shared::room_assignment::{JoinedRoom, joined_masters};
        let rooms = [
            JoinedRoom {
                ref_id: 1,
                master: true,
                links: vec![],
            },
            JoinedRoom {
                ref_id: 2,
                master: true,
                links: vec![],
            },
            JoinedRoom {
                ref_id: 3,
                master: false,
                links,
            },
        ];
        assert_eq!(joined_masters(&rooms)[2], Some(0));
    }

    #[test]
    fn authored_multibound_overrides_survive_overlap_integration_of_an_existing_database() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::esm::exporter::create_tables(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO statics(id,model_path,flags,bounds_valid,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z)
             VALUES(500,'wall.nif',0,1,-50,-10,-10,50,10,10);
             INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z)
             VALUES(1,7,0,0,0,0,0,0,100,100,100);
             INSERT INTO \"references\"(id,cell_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,header_flags)
             VALUES(100,7,500,0,0,0,0,0,0,0,2147483648),
                   (101,7,500,0,140,0,0,0,0,0,2147483648),
                   (102,7,500,0,140,0,0,0,0,0,0);
             INSERT INTO reference_rooms VALUES(100,1,1),(101,1,0);"
        ).unwrap();
        for _ in 0..2 {
            add_overlap_rows(&mut conn).unwrap();
            let rows = conn
                .prepare("SELECT reference_id,contained FROM reference_rooms ORDER BY reference_id")
                .unwrap()
                .query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, i64>(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();
            assert_eq!(
                rows,
                vec![(102, 0)],
                "stale authored memberships are removed and integration cannot recreate them"
            );
        }
    }

    fn rotate_runtime(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
        // v' = v + 2w(u x v) + 2 u x (u x v)
        let u = [q[0], q[1], q[2]];
        let cross = |a: [f32; 3], b: [f32; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let uv = cross(u, v);
        let uuv = cross(u, uv);
        [0, 1, 2].map(|i| v[i] + 2.0 * (q[3] * uv[i] + uuv[i]))
    }

    /// The Creation-space matrix is the same rotation the engine applies after the basis change.
    #[test]
    fn creation_rotation_matches_the_runtime_quaternion() {
        for rot in [
            [0.0, 0.0, 1.2],
            [0.4, 0.0, 0.0],
            [0.0, -0.7, 0.0],
            [0.5, -0.3, 2.0],
        ] {
            let quaternion = shared::coordinates::creation_euler_to_runtime_quaternion(rot);
            let matrix = creation_rotation(rot);
            for creation in [[1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.3, -2.0, 5.0]] {
                let expected = rotate_runtime(
                    quaternion,
                    shared::coordinates::creation_to_runtime_vector(creation),
                );
                let ours = shared::coordinates::creation_to_runtime_vector(
                    apply(&matrix, creation.map(f64::from)).map(|value| value as f32),
                );
                for axis in 0..3 {
                    assert!(
                        (expected[axis] - ours[axis]).abs() < 1.0e-4,
                        "{rot:?} {creation:?}: {expected:?} != {ours:?}"
                    );
                }
            }
        }
    }

    /// Bounds are glTF-axis, Creation-unit numbers: a model 100 long along Creation X, 20 along
    /// Creation Y and 10 along Creation Z has runtime bounds x 100, y 10, z 20, and placing it
    /// unrotated at the origin gives the same extents back in Creation axes, in the same units.
    #[test]
    fn model_bounds_convert_from_gltf_axes_without_a_unit_scale() {
        let corners = placed_bounds_corners(
            [-50.0, -5.0, -10.0],
            [50.0, 5.0, 10.0],
            [1000.0, 2000.0, 3000.0],
            [0.0; 3],
            1.0,
        );
        let min = |axis: usize| {
            corners
                .iter()
                .map(|c| c[axis])
                .fold(f64::INFINITY, f64::min)
        };
        let max = |axis: usize| {
            corners
                .iter()
                .map(|c| c[axis])
                .fold(f64::NEG_INFINITY, f64::max)
        };
        // Creation x = runtime x, creation y = -runtime z, creation z = runtime y.
        assert_eq!((min(0), max(0)), (950.0, 1050.0));
        assert_eq!((min(1), max(1)), (1990.0, 2010.0));
        assert_eq!((min(2), max(2)), (2995.0, 3005.0));
        // Scale multiplies the extents about the pivot.
        let scaled = placed_bounds_corners(
            [-50.0, -5.0, -10.0],
            [50.0, 5.0, 10.0],
            [0.0; 3],
            [0.0; 3],
            2.0,
        );
        assert_eq!(
            scaled
                .iter()
                .map(|c| c[0])
                .fold(f64::NEG_INFINITY, f64::max),
            100.0
        );
    }
}
