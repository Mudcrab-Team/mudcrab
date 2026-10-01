# ESM to libSQL (Turso Engine) + rkyv Transformation Specification

This document details the technical specification for parsing Skyrim Master (`.esm`) and Plugin (`.esp` / `.esl`) binary databases into an indexed **libSQL database (`skyrim_world.db`)** (Turso's open-source SQLite fork) paired with a **zero-copy `rkyv` hot storage cache**.

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
│   │   table: records         │         │   virtual table: refr_spatial   │  │
│   │   - form_id (PRIMARY KEY)│         │   using rtree(id, minX, maxX,   │  │
│   │   - record_type (4CHAR)  │         │               minY, maxY,       │  │
│   │   - load_order (INTEGER) │         │               minZ, maxZ)       │  │
│   │   - data_blob (BLOB)     │         └─────────────────────────────────┘  │
│   └──────────────────────────┘                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Database Schema DDL (`skyrim_world.db`)

### A. Primary Record Table

```sql
CREATE TABLE IF NOT EXISTS records (
    form_id INTEGER PRIMARY KEY,
    record_type TEXT NOT NULL,         -- 'CELL', 'REFR', 'NPC_', 'WEAP', 'ARMOR', etc.
    editor_id TEXT,                    -- e.g. 'WhiterunPlaza'
    load_order INTEGER NOT NULL,       -- Priority weight from plugins.txt (0 = Skyrim.esm, 1 = Update.esm, etc.)
    payload BLOB NOT NULL              -- Serialized record parameters
);

CREATE INDEX IF NOT EXISTS idx_records_type ON records(record_type);
CREATE INDEX IF NOT EXISTS idx_records_editor_id ON records(editor_id);
```

### B. World Cell & Cell References Table

```sql
CREATE TABLE IF NOT EXISTS cells (
    cell_id INTEGER PRIMARY KEY,        -- CELL FormID
    grid_x INTEGER,                     -- Exterior cell Grid X (NULL if interior)
    grid_y INTEGER,                     -- Exterior cell Grid Y (NULL if interior)
    worldspace_id INTEGER,              -- Parent WorldSpace FormID (e.g. 0x0000003C for Tamriel)
    is_interior BOOLEAN NOT NULL DEFAULT 0,
    name TEXT
);
```

### C. Hybrid Spatial Indexing Architecture (Exterior vs. Interior)

To prevent `float32` precision loss at large exterior coordinates (e.g. Tamriel bounds $\pm 200,000$) and avoid coordinate collisions between interior local origins $(0,0,0)$ and exterior global space, OpenSkyrim uses a **Two-Tier Hybrid Spatial Strategy**:

1. **Exterior Worldspace R-Tree (`exterior_spatial`):** Coordinates inside the R-Tree are stored normalized relative to the center of each $4096 \times 4096$ Skyrim cell origin, constraining bounding box values between $-2048.0$ and $+2048.0$. This guarantees high single-precision float accuracy.
2. **Interior Cell Direct Lookup (`cell_id` Hash Index):** Interior dungeons and houses do not use R-Trees. All interior `REFR` objects are indexed directly by `cell_id` for $O(1)$ fast lookup when entering interior doors.

```sql
-- R-Tree virtual table for Exterior 3D bounding box spatial queries
CREATE VIRTUAL TABLE IF NOT EXISTS exterior_spatial USING rtree(
    id,            -- Matches REFR FormID
    minX, maxX,    -- Local cell X offset (-2048.0 to +2048.0)
    minY, maxY,    -- Local cell Y offset (-2048.0 to +2048.0)
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

```rust
use rkyv::{Archive, Serialize, Deserialize};

#[derive(Archive, Serialize, Deserialize, Debug)]
#[rkyv(archived = ArchivedLandData)]
pub struct LandData {
    pub cell_x: i32,
    pub cell_y: i32,
    pub heightmap: [f32; 1089],  // 33x33 heightmap grid
    pub normals: [[f8; 3]; 1089], // Compressed vertex normals
    pub texture_layers: Vec<u16>,
}
```

### Reading Zero-Copy at Runtime:

```rust
use rkyv::access;
use memmap2::MmapOptions;

pub fn load_cell_heightmap<'a>(mmap: &'a memmap2::Mmap, offset: usize) -> &'a ArchivedLandData {
    // 0 nanoseconds parsing CPU time!
    // Directly casts bytes from virtual memory straight into Rust reference
    unsafe { access::<ArchivedLandData, rkyv::rancor::Error>(&mmap[offset..]).unwrap() }
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
            "INSERT OR REPLACE INTO records (form_id, record_type, load_order, payload) VALUES (?1, 'REFR', ?2, ?3)",
            params![form_id, load_order, payload],
        )?;

        // 2. Insert 3D bounding box into Spatial R-Tree Index
        let bbox = 64.0; // bounding radius around object
        tx.execute(
            "INSERT OR REPLACE INTO refr_spatial (id, minX, maxX, minY, maxY, minZ, maxZ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![form_id, x - bbox, x + bbox, y - bbox, y + bbox, z - bbox, z + bbox],
        )?;

        tx.commit()
    }
}
```
