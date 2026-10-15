//! Interior room graph: room boxes, portals, room links and room membership.
//!
//! Skyrim marks the rooms of an interior with `RoomMarker` references and the doorways between
//! them with `PortalMarker` references. This module decodes both into the optional tables of
//! `docs/specs/converters/db-schema.md` §15 and records which room boxes contain each placed
//! object. The tables hold facts only; which objects to draw is the engine's policy.
//!
//! Design and byte layouts: `docs/research/interior-room-graph-converter.md`.

use crate::esm::{extractors::SubrecordView, records::RawRecord};
use rusqlite::{Connection, OptionalExtension, Result, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// `RoomMarker` base form in `Skyrim.esm`, which is always load-order slot 0.
pub const ROOM_MARKER_BASE: u32 = 0x1F;
/// `PortalMarker` base form in `Skyrim.esm`.
pub const PORTAL_MARKER_BASE: u32 = 0x20;
/// `XPRM` shape value of a box room.
const SHAPE_BOX: u32 = 1;

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
             PRIMARY KEY (ref_id, pair_index)
         );
         CREATE INDEX IF NOT EXISTS idx_room_portals_cell ON room_portals(cell_id);
         CREATE TABLE IF NOT EXISTS room_links (
             room_ref INTEGER NOT NULL, linked_ref INTEGER NOT NULL,
             PRIMARY KEY (room_ref, linked_ref)
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
"#;

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
    /// Room or portal markers with a non-finite size, position or rotation; they get no row.
    pub invalid_markers_skipped: u64,
    /// Objects in a cell with rooms whose pivot is in no room box.
    pub objects_in_no_room: u64,
    /// Objects in a cell with rooms whose pivot is in more than one room box.
    pub objects_in_several_rooms: u64,
    /// `reference_rooms` rows with `contained = 0`, added after model bounds are known.
    pub overlap_rows: u64,
}

const STAT_NAMES: [&str; 12] = [
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
];

impl RoomGraphReport {
    fn values(&self) -> [u64; 12] {
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
                _ => {}
            }
        }
        Ok(report)
    }
}

/// Deletes every row of the room tables. Called at the start of a full export.
pub fn clear(conn: &Connection) -> Result<()> {
    for table in [
        "room_bounds",
        "room_portals",
        "room_links",
        "reference_rooms",
        "room_graph_stats",
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

fn transform_of(view: &SubrecordView<'_>) -> ([f32; 3], [f32; 3]) {
    let data = view.get_f32_slice(b"DATA").unwrap_or_default();
    let at = |index: usize| data.get(index).copied().unwrap_or(0.0);
    ([at(0), at(1), at(2)], [at(3), at(4), at(5)])
}

fn is_placed(record_type: &[u8; 4]) -> bool {
    matches!(record_type, b"REFR" | b"ACHR" | b"ACRE" | b"PGRE" | b"PMIS")
}

/// Rebuilds the room tables from the merged records. `interior_cells` are the CELL records
/// with no worldspace and no grid position; markers elsewhere are ignored.
///
/// Reads parsed subrecords only, never the stored blob. Writes `contained = 1` membership rows
/// (the object's pivot is inside the box); `add_overlap_rows` adds the rest after integration.
pub fn export_room_graph(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    interior_cells: &HashSet<u32>,
) -> Result<RoomGraphReport> {
    clear(tx)?;
    let mut report = RoomGraphReport::default();
    let mut ordered: Vec<(&u32, &RawRecord)> = master
        .iter()
        .filter(|(_, record)| {
            is_placed(&record.record_type)
                && record
                    .cell_form_id
                    .is_some_and(|cell| interior_cells.contains(&cell))
        })
        .collect();
    ordered.sort_unstable_by_key(|(form_id, _)| **form_id);

    let base_of = |record: &RawRecord| {
        SubrecordView::new(&record.subrecords)
            .get_form_id(b"NAME")
            .unwrap_or(0)
    };

    // Rooms first: portals and links refer to them.
    let mut rooms_by_cell: BTreeMap<u32, Vec<Room>> = BTreeMap::new();
    for &(&form_id, record) in &ordered {
        if &record.record_type != b"REFR" || base_of(record) != ROOM_MARKER_BASE {
            continue;
        }
        let cell = record.cell_form_id.unwrap_or(0);
        let view = SubrecordView::new(&record.subrecords);
        let Some(xprm) = view.find(b"XPRM").filter(|data| data.len() >= 32) else {
            report.non_box_rooms_skipped += 1;
            continue;
        };
        if u32_at(xprm, 28) != Some(SHAPE_BOX) {
            report.non_box_rooms_skipped += 1;
            continue;
        }
        let (position, rotation) = transform_of(&view);
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
        {
            report.invalid_markers_skipped += 1;
            continue;
        }
        let flags = view
            .find(b"XRMR")
            .filter(|data| data.len() >= 4)
            .map(|data| {
                i64::from(data[1]) | (i64::from(u16::from_le_bytes([data[2], data[3]])) << 8)
            })
            .unwrap_or(0);
        let optional_form_id = |tag: &[u8; 4]| {
            view.find(tag)
                .filter(|data| data.len() == 4)
                .and_then(|data| u32_at(data, 0))
                .filter(|id| *id != 0)
        };
        tx.execute(
            "INSERT OR REPLACE INTO room_bounds(ref_id, cell_id, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, half_x, half_y, half_z, lighting_template_id, image_space_id, flags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                form_id, cell, position[0], position[1], position[2], rotation[0], rotation[1],
                rotation[2], half[0], half[1], half[2], optional_form_id(b"LNAM"),
                optional_form_id(b"INAM"), flags
            ],
        )?;
        for (_, link) in record.subrecords.iter().filter(|(tag, _)| tag == b"XLRM") {
            // A zero id is "no room": the plugin remap clears an invalid link to 0.
            if let Some(linked) = u32_at(link, 0)
                .filter(|_| link.len() == 4)
                .filter(|id| *id != 0)
            {
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO room_links(room_ref, linked_ref) VALUES (?1, ?2)",
                    params![form_id, linked],
                )?;
                report.links += inserted as u64;
            }
        }
        report.rooms += 1;
        rooms_by_cell.entry(cell).or_default().push(Room {
            ref_id: form_id,
            shape: RoomBox::new(position, rotation, half),
        });
    }

    for &(&form_id, record) in &ordered {
        if &record.record_type != b"REFR" || base_of(record) != PORTAL_MARKER_BASE {
            continue;
        }
        let cell = record.cell_form_id.unwrap_or(0);
        let view = SubrecordView::new(&record.subrecords);
        let Some(xprm) = view.find(b"XPRM").filter(|data| data.len() >= 12) else {
            report.portals_without_size_skipped += 1;
            continue;
        };
        let (position, rotation) = transform_of(&view);
        let half_x = f32_at(xprm, 0).unwrap_or(0.0);
        let half_z = f32_at(xprm, 8).unwrap_or(0.0);
        if ![half_x, half_z]
            .iter()
            .chain(&position)
            .chain(&rotation)
            .all(|v| v.is_finite())
        {
            report.invalid_markers_skipped += 1;
            continue;
        }
        // `XPOD` is an array of (origin, destination) pairs of 8 bytes (xEdit 4.1.5f defs).
        // One row per pair; a portal with no complete pair gets one row with NULL sides.
        let xpod = view.find(b"XPOD").unwrap_or(&[]);
        let pairs = xpod.len() / 8;
        if pairs > 1 {
            report.portals_with_extra_pairs += 1;
        }
        if !xpod.len().is_multiple_of(8) {
            report.portals_with_trailing_xpod_bytes += 1;
        }
        let side = |pair: usize, offset: usize| {
            u32_at(xpod, pair * 8 + offset)
                .filter(|_| pair < pairs)
                .filter(|id| {
                    *id != 0
                        && rooms_by_cell
                            .get(&cell)
                            .is_some_and(|rooms| rooms.iter().any(|room| room.ref_id == *id))
                })
        };
        for pair in 0..pairs.max(1) {
            let (room_a, room_b) = (side(pair, 0), side(pair, 4));
            tx.execute(
                "INSERT OR REPLACE INTO room_portals(ref_id, pair_index, cell_id, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, half_x, half_z, room_a, room_b)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    form_id, pair as i64, cell, position[0], position[1], position[2],
                    rotation[0], rotation[1], rotation[2], half_x, half_z, room_a, room_b
                ],
            )?;
            if room_a.is_none() || room_b.is_none() {
                report.portals_with_missing_side += 1;
            }
        }
        report.portals += 1;
    }

    // Membership: every placed object of a cell with rooms except the markers themselves.
    {
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO reference_rooms(reference_id, room_ref, contained) VALUES (?1, ?2, 1)",
        )?;
        for &(&form_id, record) in &ordered {
            let base = base_of(record);
            if base == ROOM_MARKER_BASE || base == PORTAL_MARKER_BASE {
                continue;
            }
            let Some(rooms) = record
                .cell_form_id
                .and_then(|cell| rooms_by_cell.get(&cell))
            else {
                continue;
            };
            let (position, _) = transform_of(&SubrecordView::new(&record.subrecords));
            let pivot = position.map(f64::from);
            let mut inside = 0u32;
            for room in rooms.iter().filter(|room| room.shape.contains(pivot)) {
                insert.execute(params![form_id, room.ref_id])?;
                inside += 1;
            }
            match inside {
                0 => report.objects_in_no_room += 1,
                1 => {}
                _ => report.objects_in_several_rooms += 1,
            }
        }
    }
    report.write(tx)?;
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// Integration stage
// ---------------------------------------------------------------------------------------------

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
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
    if !table_exists(conn, "room_bounds")? || !table_exists(conn, "reference_rooms")? {
        return Ok(0);
    }
    // A reused older database may lack the bounds columns; with no rooms there is nothing to do.
    let has_rooms: bool =
        conn.query_row("SELECT EXISTS(SELECT 1 FROM room_bounds)", [], |row| {
            row.get(0)
        })?;
    let has_bounds: bool = conn.query_row(
        "SELECT count(*) = 2 FROM pragma_table_info('statics') WHERE name IN ('bounds_valid', 'bounds_min_x')",
        [],
        |row| row.get(0),
    )?;
    if !has_rooms || !has_bounds {
        return Ok(0);
    }
    let mut rooms_by_cell: BTreeMap<u32, Vec<Room>> = BTreeMap::new();
    {
        let mut statement = conn.prepare(
            "SELECT ref_id, cell_id, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, half_x, half_y, half_z FROM room_bounds ORDER BY ref_id",
        )?;
        let rows = statement.query_map([], |row| {
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
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM reference_rooms WHERE contained = 0", [])?;
    let mut written = 0u64;
    {
        let mut select = tx.prepare(
            "SELECT r.id, r.cell_id, r.pos_x, r.pos_y, r.pos_z, r.rot_x, r.rot_y, r.rot_z, r.scale,
                    s.bounds_min_x, s.bounds_min_y, s.bounds_min_z, s.bounds_max_x, s.bounds_max_y, s.bounds_max_z
             FROM \"references\" r JOIN statics s ON s.id = r.base_form_id
             WHERE s.bounds_valid = 1 AND r.base_form_id NOT IN (?1, ?2)
               AND r.cell_id IN (SELECT cell_id FROM room_bounds)
             ORDER BY r.id",
        )?;
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO reference_rooms(reference_id, room_ref, contained) VALUES (?1, ?2, 0)",
        )?;
        let mut rows = select.query(params![ROOM_MARKER_BASE, PORTAL_MARKER_BASE])?;
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
    tx.execute(
        "INSERT OR REPLACE INTO room_graph_stats(name, value) VALUES ('overlap_rows', ?1)",
        [written as i64],
    )?;
    tx.commit()?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

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
