# ESM to SQLite + rkyv Transformation Specification

This document describes how Skyrim master (`.esm`) and plugin (`.esp` / `.esl`) records are projected into the indexed SQLite database (`skyrim_world.db`) and the zero-copy `rkyv` terrain cache.

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
localized-name resolution in the selected `--language`.

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
  1. `skyrim_world.db` (SQLite database with R-Tree spatial indexing).
  2. `cell_cache.rkyv` (Zero-copy binary memory-mapped terrain and cell data).
- **Goal:**
  1. Use spatial and cell indexes to select placed-reference rows by worldspace and cell coordinates.
  2. Index records by 32-bit `FormID` for lookup.
  3. Support priority-weighted plugin load orders (`plugins.txt` overrides).

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
2. **Interior Cell Index (`idx_references_cell`):** Interior cells do not use the R-Tree. The exporter creates an ordinary SQLite index on `references(cell_id)`, which the engine uses to select reference rows for an interior cell. This is an illustrative query shape; it does not fetch complete objects or establish a lookup-time guarantee.

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

-- Ordinary SQLite index used by interior reference lookup
CREATE INDEX IF NOT EXISTS idx_references_cell ON "references"(cell_id);
```

#### Exterior reference query shape:

```sql
-- Select reference rows in one 4096-unit exterior grid cell.
-- Bind the worldspace ID and grid-derived lower/upper X and Y bounds.
SELECT r.id, r.cell_id, r.base_form_id, r.pos_x, r.pos_y, r.pos_z
FROM exterior_spatial x
JOIN "references" r ON r.id = x.id
WHERE x.worldspace_id = ?1
  AND x.minX >= ?2 AND x.minX < ?3
  AND x.minY >= ?4 AND x.minY < ?5;
```

The engine uses half-open bounds `[grid * 4096, (grid + 1) * 4096)` for each axis and filters by
worldspace. The R-Tree stores point bounds (`minX = maxX`, and likewise for Y and Z), so the engine
tests the point's minimum coordinates. Its full projection selects explicit reference columns and
joins the base model plus optional door, emittance and enable-state data; the query above only
illustrates spatial candidate selection. Cell-local X/Y offsets remain in `references.local_x` and
`references.local_y`.

#### Interior reference query shape:

```sql
-- Select reference rows for one interior CELL FormID.
SELECT r.id, r.cell_id, r.base_form_id, r.pos_x, r.pos_y, r.pos_z
FROM "references" r
WHERE r.cell_id = ?1;
```

The engine may join the same base-model and optional reference data described above. This example
selects reference rows; it does not represent full object loading or promise a lookup latency.

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

The runtime's `CellCache::open` in `crates/engine/src/world/cache.rs` maps the file,
validates the archive, checks `CELL_CACHE_VERSION`, and builds its cell index, returning errors
on failure. This helper illustrates the archive-validation step and returns its result;
version checking remains a separate step before using the cache.

```rust
use rkyv::access;
use shared::ArchivedCellCache;

pub fn load_cell_cache<'a>(
    mmap: &'a memmap2::Mmap,
) -> Result<&'a ArchivedCellCache, rkyv::rancor::Error> {
    // Validated access: the archive is checked before it is read in place.
    access::<ArchivedCellCache, rkyv::rancor::Error>(&mmap[..])
}
```

---

## 5. Implemented Export Path

The record-reader entry points in `crates/converter/src/esm/mod.rs` and `esm/inhouse.rs` prepare
the database schema and pass winning records to the exporter. The transaction in
`crates/converter/src/esm/exporter.rs` writes the canonical `records` archive and its runtime
projections together.

For placed references, `insert_reference_with_canonical_data` writes a `references` row with
the owning cell, worldspace, base object, position, rotation, scale, flags and canonical data.
Exterior rows also retain cell-local X/Y offsets. It then removes any previous spatial entry
for that reference, including when a subset update moves it into an interior. For an exterior
reference, it inserts the global position as equal minimum and maximum bounds, with cell and
worldspace auxiliary values:

```sql
DELETE FROM exterior_spatial WHERE id = ?1;

-- Executed only for an exterior reference.
-- Bind: reference ID, global X, global Y, global Z, cell ID, worldspace ID.
INSERT OR REPLACE INTO exterior_spatial
    (id, minX, maxX, minY, maxY, minZ, maxZ, cell_id, worldspace_id)
VALUES (?1, ?2, ?2, ?3, ?3, ?4, ?4, ?5, ?6);
```

Interior references remain in `references` and use its cell index. The exporter writes the
canonical record and runtime reference rows in addition to these spatial statements; the
R-Tree entries supply candidates for the queries in section 3.
