# ESM to libSQL (Turso Engine) + rkyv Transformation Specification

This document details the technical specification for parsing Skyrim Master (`.esm`) and Plugin (`.esp` / `.esl`) binary databases into an indexed **libSQL database (`skyrim_world.db`)** (Turso's open-source SQLite fork) paired with a **zero-copy `rkyv` hot storage cache**.

The default in-house frontend uses MudCrab's authored record schema and generic
Rust decoder. `--record-reader legacy` selects the hand-written reader, and
`--record-reader inhouse` selects the default explicitly. The in-house decoder
owns typed-field validation and FormID resolution through the existing
load-order mapping. Its canonical decoded fields drive the existing SQLite and
rkyv projections, retaining the existing rkyv `records.data` encoding. The original
decompressed payload is separately preserved in the auxiliary
`inhouse_source_records` table; its embedded FormIDs remain file-relative. Unknown types are
opaque source records, not a claim of typed schema coverage. See
[the pipeline specification](pipeline.md) for producer identity, diagnostics and
English localized-name resolution.

In-house output also retains localized occurrence IDs and lookup status in
`inhouse_localized_fields`, winning LAND physical order in
`inhouse_terrain_source_order`, and normalized source texture assignments in
`inhouse_terrain_layers`. `cell_cache_preserved.rkyv` keeps original texture
identities and all active normalized layers, without the runtime capacity
limit. The runtime cache retains its existing one base plus five overlays per
quadrant, dropping the lowest summed-opacity overlays (highest slot on ties)
with a bounded per-cell warning. Its retained layers use explicit placeholders
for unusable targets without dropping their blend weights. These auxiliary
contracts preserve source information beyond the nullable runtime projections.

---

## 1. Overview & Objectives

- **Input:** Skyrim master records (`Skyrim.esm`, `Update.esm`, `Dawnguard.esm`, `.esp` mod files).
- **Output:**
  1. `skyrim_world.db` (libSQL / Turso database with R-Tree spatial indexing & async local-first sync).
  2. `cell_cache.rkyv` (Zero-copy binary memory-mapped blobs for instant terrain heightmaps and dense cell references).
- **Goal:**
  1. Provide sub-millisecond 3D spatial queries for placed game objects (`REFR`) based on player camera coordinates.
  2. Maintain `O(1)` primary key resolution for 32-bit `FormID` records.
  3. Support priority-weighted plugin load orders (`plugins.txt` overrides).
  4. Enable optional **Embedded Replication / Cloud Sync** via Turso for cross-device save games and multiplayer world sync!

---

## 2. ESM Record Structure to Database Schema Mapping

Skyrim `.esm` files consist of 24-byte record headers (`TES4`, `CELL`, `LAND`, `REFR`, `NPC_`, `WEAP`, `ARMOR`).

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                            Skyrim .esm Records                              │
│  ┌──────────────┐     ┌──────────────┐     ┌──────────────┐     ┌─────────┐ │
│  │ CELL Record  │ ──► │ REFR Record  │ ──► │ LAND Record  │ ──► │ NPC_ Rec│ │
│  │ (Grid X, Y)  │     │ (3D Pos/Rot) │     │ (Heightmap)  │     │ (Stats) │ │
│  └──────────────┘     └──────────────┘     └──────────────┘     └─────────┘ │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
                                       ▼ (Rust nom parser & Converter)
┌─────────────────────────────────────────────────────────────────────────────┐
│                            SQLite 3 Database Schema                         │
│                                                                             │
│   ┌──────────────────────────┐         ┌─────────────────────────────────┐  │
│   │   table: records         │         │ virtual table: exterior_spatial │  │
│   │   - form_id (PRIMARY KEY)│         │   using rtree(id, minX, maxX,   │  │
│   │   - record_type (4CHAR)  │         │               minY, maxY,       │  │
│   │   - load_order (INTEGER) │         │               minZ, maxZ)       │  │
│   │   - data (BLOB)          │         └─────────────────────────────────┘  │
│   └──────────────────────────┘                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Database Schema DDL (`skyrim_world.db`)

The authoritative, complete DDL is in [`db-schema.md`](db-schema.md); the two core tables are
repeated here as implemented in `crates/converter/src/esm/exporter.rs`.

### A. Primary Record Table

```sql
CREATE TABLE IF NOT EXISTS records (
    form_id INTEGER PRIMARY KEY,       -- winning record, load-order numbering
    record_type TEXT NOT NULL,         -- 'CELL', 'REFR', 'NPC_', 'WEAP', 'ARMO', etc.
    cell_id INTEGER,                   -- parent CELL of a placed reference or LAND
    worldspace_id INTEGER,             -- parent WRLD of an exterior CELL or reference
    load_order INTEGER NOT NULL,       -- plugins.id of the winning plugin (0 = Skyrim.esm, ...)
    data BLOB NOT NULL                 -- rkyv ArchivedRecordData (the subrecord payloads)
);

CREATE INDEX IF NOT EXISTS idx_records_type ON records(record_type);
```

There is no `editor_id` column; a record's Editor ID is its `EDID` subrecord in `data`.

### B. World Cell & Cell References Table

```sql
CREATE TABLE IF NOT EXISTS cells (
    id INTEGER PRIMARY KEY,             -- CELL FormID
    worldspace_id INTEGER,              -- Parent WorldSpace FormID; NULL for an interior
    grid_x INTEGER,                     -- Exterior cell Grid X (NULL if interior)
    grid_y INTEGER,                     -- Exterior cell Grid Y (NULL if interior)
    interior_name TEXT,                 -- The cell's EDID, not its display name
    flags INTEGER NOT NULL,             -- record header flags
    data BLOB,                          -- rkyv ArchivedRecordData
    full_name TEXT                      -- Resolved FULL display name
);
```

Placed references have their own `references` table (positions, rotations, base object); see
`db-schema.md` §5.

### C. Hybrid Spatial Indexing Architecture (Exterior vs. Interior)

Exterior references use a spatial index with a worldspace filter; interior references use direct cell lookup:

1. **Exterior Worldspace R-Tree (`exterior_spatial`):** The exporter inserts each reference's global X/Y/Z position as a point (equal minimum and maximum bounds). SQLite stores these bounds as single-precision values; the exporter applies no coordinate magnitude limit. `cell_id` and `worldspace_id` are auxiliary fields rather than spatial dimensions. Separate reference columns retain cell-local X/Y offsets.
2. **Interior Cell Direct Lookup (`cell_id` Hash Index):** Interior dungeons and houses do not use R-Trees. All interior `REFR` objects are indexed directly by `cell_id` for $O(1)$ fast lookup when entering interior doors.

```sql
-- R-Tree virtual table for Exterior 3D bounding box spatial queries
CREATE VIRTUAL TABLE IF NOT EXISTS exterior_spatial USING rtree(
    id,            -- Matches REFR FormID
    minX, maxX,    -- Global reference X position
    minY, maxY,    -- Global reference Y position
    minZ, maxZ,    -- World Z Height units
    +cell_id,      -- Exterior CELL FormID
    +worldspace_id -- Parent WorldSpace FormID (e.g. 0x0000003C for Tamriel)
);

-- Index for Interior Cell O(1) object loading
CREATE INDEX IF NOT EXISTS idx_records_cell_id ON records(cell_id) WHERE cell_id IS NOT NULL;
```

#### Executing a Sub-Millisecond Exterior Camera Query:

```sql
-- Fetch static 3D objects near player camera in Tamriel (WorldSpace 0x0000003C)
SELECT r.* FROM records r
JOIN exterior_spatial s ON r.form_id = s.id
WHERE s.worldspace_id = 0x0000003C
  AND s.minX >= -1024.0 AND s.maxX <= 1024.0
  AND s.minY >= -1024.0 AND s.maxY <= 1024.0
ORDER BY r.load_order DESC;
```

#### Executing an Instant Interior Cell Query:

```sql
-- Fetch all 3D objects inside Whiterun Breezehome (CELL 0x00013629)
SELECT * FROM records
WHERE cell_id = 0x00013629
ORDER BY load_order DESC;
```

---

## 4. `rkyv` Zero-Copy Hot Storage (`cell_cache.rkyv`)

For raw terrain heightmaps (`LAND` records) and ultra-dense exterior grid geometry, parsing SQL rows introduces minor memory copies. We use `rkyv` binary buffers mapped with `mmap`.

### Rust Struct Definition:

The implemented record is `shared::CachedLand` (`crates/shared/src/lib.rs`), inside a versioned
`shared::CellCache` (`CELL_CACHE_VERSION` 3):

```rust
#[derive(Debug, Clone, Archive, Serialize, Deserialize, PartialEq)]
#[rkyv(bytecheck())]
pub struct CachedLand {
    pub cell_id: u32,
    pub width: u16,
    pub height: u16,
    /// Row-major, absolute Creation Engine height units.
    pub heights: Vec<f32>,
    /// Packed signed XYZ normals, three bytes per vertex.
    pub normals: Vec<i8>,
    /// Packed RGB colors, three bytes per vertex.
    pub vertex_colors: Vec<u8>,
    pub layers: Vec<TerrainLayer>,
    pub water_height: Option<f32>,
    pub water_type_form_id: Option<u32>,
}
```

### Reading Zero-Copy at Runtime:

```rust
use rkyv::access;
use memmap2::MmapOptions;

pub fn load_cell_cache<'a>(mmap: &'a memmap2::Mmap) -> &'a ArchivedCellCache {
    // Validated access: the archive is checked before it is read in place.
    access::<ArchivedCellCache, rkyv::rancor::Error>(&mmap[..]).unwrap()
}
```

---

## 5. Rust Implementation (`rusqlite` + `nom` + `rkyv` Crates)

```rust
use rusqlite::{Connection, params};
use memmap2::Mmap;

pub struct EsmToSqliteConverter {
    db: Connection,
}

impl EsmToSqliteConverter {
    pub fn new(db_path: &str) -> Result<Self, rusqlite::Error> {
        let db = Connection::open(db_path)?;
        // Enable WAL mode for high-concurrency multi-threaded reads
        db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        Ok(Self { db })
    }

    pub fn insert_refr_object(&mut self, form_id: u32, x: f32, y: f32, z: f32, load_order: u32, payload: &[u8]) -> Result<(), rusqlite::Error> {
        let tx = self.db.transaction()?;

        // 1. Insert record into primary table
        tx.execute(
            "INSERT OR REPLACE INTO records (form_id, record_type, load_order, data) VALUES (?1, 'REFR', ?2, ?3)",
            params![form_id, load_order, payload],
        )?;

        // 2. Insert 3D bounding box into Spatial R-Tree Index
        let bbox = 64.0; // bounding radius around object
        tx.execute(
            "INSERT OR REPLACE INTO exterior_spatial (id, minX, maxX, minY, maxY, minZ, maxZ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![form_id, x - bbox, x + bbox, y - bbox, y + bbox, z - bbox, z + bbox],
        )?;

        tx.commit()
    }
}
```
