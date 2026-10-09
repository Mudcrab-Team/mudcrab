# Mudcrab SQLite 3 Database Schema (`skyrim_world.db`)

This specification details the canonical DDL schema, tables, indices, and column constraints for `skyrim_world.db`, as implemented in [`crates/converter/src/esm/exporter.rs`](../../../crates/converter/src/esm/exporter.rs).

---

## 1. Schema Overview

`skyrim_world.db` is built by `crates/converter` by parsing master files (`Skyrim.esm`) and plugin files (`.esp`/`.esl`). When `PipelineConfig.plugins_file` is supplied, ESM-flagged plugins and `.esm`/`.esl` files take priority, keeping the listed order within each category except that regular dependencies are moved ahead of the master files that need them. The resulting order is validated before assigning full/light slots, ordering archive (BSA/BA2) priority, and merging database records and terrain caches. Unrelated regular plugins retain the user's order. This is not a general dependency sort: any inversions remaining after normalization, including a master file listed before another master it depends on, are rejected. The CLI and launcher currently use automatic discovery: only plugins directly in Data are selected, with dependencies ordered before dependents. Among available plugins, ESM-flagged plugins and `.esm`/`.esl` files take priority, followed by the five official files' conventional order and case-insensitive filename order. The ESL header flag alone assigns a light slot; an ESL-flagged `.esp` stays among regular plugins. Missing masters and dependency cycles fail with diagnostics. This deterministic fallback cannot infer a user's intended override order between unrelated mods; nested backup/optional plugins are ignored while nested assets remain discoverable.

The database stamps its version in `schema_info`; the current version is **7**
(`shared::WORLD_DATABASE_SCHEMA_VERSION`). Schema 4 added lights and
`references.radius_override`. Schema 5 adds grass data in #152;
the combined producer exports grass and LOD tables. Schema 6 adds LOD origins, chunk metadata,
its spatial index, and a build identity. The engine, `world-inspect` and
launcher accept world schemas **3 through 7**, using
`shared::supports_runtime_world_database_schema`. Complete converter packages
support schemas **15 through 24**. Legacy worlds render full detail without
LOD; an advertised LOD package requires the current database contract.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      `skyrim_world.db` Implemented Schema                   │
│  ┌──────────────────────────┬──────────────────────┬─────────────────────┐  │
│  │   `plugins`              │   `records`          │ `worldspaces`       │  │
│  │   (Active Plugin Order)  │   (Raw FormID Data)  │ (Worldspace EDIDs)  │  │
│  ├──────────────────────────┼──────────────────────┼─────────────────────┤  │
│  │   `cells`                │   `references`       │ `refs_rtree`        │  │
│  │   (Cell Grid & Names)    │   (3D World Placements)│ (3D Spatial R-Tree) │  │
│  ├──────────────────────────┼──────────────────────┼─────────────────────┤  │
│  │   `land`                 │   `lod`              │ `scripts`           │  │
│  │   (Terrain Heightmaps)   │  (Unused LOD Table)  │ (Papyrus Bytecode)  │  │
│  ├──────────────────────────┴──────────────────────┴─────────────────────┤  │
│  │   `formid_map` & `conversion_cache`                                   │  │
│  │   (32-bit to 64-bit ID Bridge & Cache Hashes)                        │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Table Definitions

### 1. Active Plugin Registry (`plugins`)

Stores loaded `.esm`/`.esp`/`.esl` plugin file metadata, load order priority, and checksums.

```sql
CREATE TABLE IF NOT EXISTS plugins (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    priority INTEGER NOT NULL,
    checksum BLOB NOT NULL
);
```

---

### 2. Primary Record Database (`records`)

Stores unparsed raw subrecord byte payloads indexed by 32-bit Skyrim `FormID` and 4-character record type codes.

```sql
CREATE TABLE IF NOT EXISTS records (
    id INTEGER PRIMARY KEY,
    form_id INTEGER NOT NULL,
    record_type TEXT NOT NULL,          -- 'CELL', 'REFR', 'NPC_', 'WEAP', 'ARMOR', 'SPEL', etc.
    data BLOB NOT NULL                  -- Serialized subrecords payload
);

CREATE INDEX IF NOT EXISTS idx_records_formid ON records(form_id);
CREATE INDEX IF NOT EXISTS idx_records_type ON records(record_type);
```

---

### 3. Worldspace Registry (`worldspaces`)

Stores worldspace hierarchy and parent world relations (e.g. Tamriel `0x0000003C`, Solstheim).

```sql
CREATE TABLE IF NOT EXISTS worldspaces (
    id INTEGER PRIMARY KEY,             -- WorldSpace FormID
    editor_id TEXT NOT NULL,            -- EDID string (e.g. 'Tamriel')
    parent_world INTEGER,               -- Parent WorldSpace FormID (if child worldspace)
    flags INTEGER NOT NULL,
    lod_origin_x INTEGER,               -- Validated sidecar/explicit origin; NULL if unresolved
    lod_origin_y INTEGER
);
```

---

### 4. Cell Registry (`cells`)

Stores exterior cell grid coordinates and interior cell names.

```sql
CREATE TABLE IF NOT EXISTS cells (
    id INTEGER PRIMARY KEY,             -- CELL FormID
    worldspace_id INTEGER NOT NULL,     -- Parent WorldSpace FormID
    grid_x INTEGER,                     -- Exterior cell Grid X (NULL if interior)
    grid_y INTEGER,                     -- Exterior cell Grid Y (NULL if interior)
    interior_name TEXT,                 -- Interior cell name (NULL if exterior)
    flags INTEGER NOT NULL,
    data BLOB                           -- Optional cell binary payload
);
```

---

### 5. Placed World References (`references`)

Stores 3D positions, rotations, scales, and cell parentage for all placed world objects (`REFR`, `ACHR`, `ACRE`, `PGRE`, `PMIS`).

```sql
CREATE TABLE IF NOT EXISTS references (
    id INTEGER PRIMARY KEY,
    cell_id INTEGER NOT NULL,           -- Parent CELL FormID
    form_id INTEGER NOT NULL,           -- Base Object FormID
    pos_x REAL NOT NULL,                -- 3D X Coordinate
    pos_y REAL NOT NULL,                -- 3D Y Coordinate
    pos_z REAL NOT NULL,                -- 3D Z Coordinate
    rot_x REAL NOT NULL,                -- Rotation X (Radians)
    rot_y REAL NOT NULL,                -- Rotation Y (Radians)
    rot_z REAL NOT NULL,                -- Rotation Z (Radians)
    scale REAL NOT NULL DEFAULT 1.0,    -- Scale multiplier
    radius_override REAL,               -- XRDS radius in Creation units (NULL when the REFR has none)
    header_flags INTEGER NOT NULL DEFAULT 0, -- Winning record header flags, uninterpreted
    enable_parent_id INTEGER,           -- Remapped XESP parent FormID; NULL when absent
    enable_parent_flags INTEGER,        -- XESP flags; NULL when absent
    data BLOB                           -- Subrecords payload
);

-- Index for O(1) interior cell reference loading
CREATE INDEX IF NOT EXISTS idx_references_cell_id ON references(cell_id);
```

The reference flag and enable-parent columns preserve winning raw-record
metadata for later object LOD (#106). Current terrain LOD does not consume
them. `header_flags` comes from the record header; `enable_parent_id` and
`enable_parent_flags` are the two little-endian words of the eight-byte XESP
subrecord, with the parent FormID resolved through plugin load order. The raw
subrecord payload remains in `data`. An invalid parent link is published as
`enable_parent_id = 0` (no parent) with its flags kept; a malformed `XESP` is
dropped, so both columns are NULL. See the remapped-field table below.

---

### 6. Hybrid Spatial Indexing (`refs_rtree` & Interior `cell_id` Index)

To prevent `float32` single-precision accuracy loss at large exterior coordinates (e.g. Tamriel bounds $\pm 200,000$) and avoid coordinate collisions between interior local origins $(0,0,0)$ and exterior global space, Mudcrab uses a **Two-Tier Hybrid Spatial Strategy**:

1. **Exterior Worldspace R-Tree (`refs_rtree`):** Coordinates inside the R-Tree virtual table are stored normalized relative to cell centers (values constrained between $-2048.0$ and $+2048.0$), keeping numbers small to guarantee high single-precision float accuracy.
2. **Interior Cell Direct Lookup (`idx_references_cell_id`):** Interior dungeons and houses do not use R-Trees. All interior references are loaded directly by `cell_id` for instant $O(1)$ lookup upon entering interior doors.

```sql
-- R-Tree virtual table for Exterior 3D bounding box spatial queries
CREATE VIRTUAL TABLE IF NOT EXISTS refs_rtree USING rtree(
    id,                                 -- Matches internal reference ID
    minX, maxX,                         -- Local cell X offset (-2048.0 to +2048.0)
    minY, maxY,                         -- Local cell Y offset (-2048.0 to +2048.0)
    minZ, maxZ,                         -- World Z Height units
    +cell_id,                           -- Exterior CELL FormID
    +worldspace_id                      -- Parent WorldSpace FormID (e.g. 0x0000003C for Tamriel)
);
```

---

### 7. Terrain Heightmaps (`land`)

Stores 33x33 terrain heightmap data, vertex textures (`vtex`), and vertex colors (`vclr`) extracted from `LAND` records.

```sql
CREATE TABLE IF NOT EXISTS land (
    cell_id INTEGER PRIMARY KEY,        -- Parent CELL FormID
    heightmap BLOB NOT NULL,            -- 33x33 float/byte heightmap buffer
    vtext BLOB,                         -- Land texture layers
    vclr BLOB                           -- Land vertex colors
);
```

---

### 8. Level of Detail (`lod_chunks`, `lod_chunks_spatial`, `lod_build`)

Schema 5 stores spatial metadata and file-backed GLB payloads, not geometry
blobs. Source-cell names are serialized text used for full-detail handoff.
Bounds are world-space Creation units; `payload_path` is relative to the asset
root and `content_hash` is the payload's SHA-256. The single build row matches
`lod-manifest.json`.

```sql
CREATE TABLE IF NOT EXISTS lod_chunks (
    worldspace_id INTEGER NOT NULL, tier INTEGER NOT NULL,
    anchor_x INTEGER NOT NULL, anchor_y INTEGER NOT NULL,
    payload_path TEXT NOT NULL, content_hash TEXT NOT NULL,
    bounds_min_x REAL NOT NULL, bounds_min_y REAL NOT NULL, bounds_min_z REAL NOT NULL,
    bounds_max_x REAL NOT NULL, bounds_max_y REAL NOT NULL, bounds_max_z REAL NOT NULL,
    source_cells TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (worldspace_id, tier, anchor_x, anchor_y)
);
CREATE VIRTUAL TABLE IF NOT EXISTS lod_chunks_spatial USING rtree(
    id, minX, maxX, minY, maxY, +worldspace_id, +tier, +anchor_x, +anchor_y
);
CREATE TABLE IF NOT EXISTS lod_build (
    id INTEGER PRIMARY KEY CHECK (id = 1), build_identity TEXT NOT NULL
);
```

The unused pre-schema-6 `lod` blob placeholder is retained. Terrain LOD
uses `lod_chunks` for indexing and external GLB files for payloads. See
[ADR-0010](../../adr/0010-lod-chunk-payload-format.md).

---

### 9. Compiled Scripts (`scripts`)

Stores compiled Papyrus script bytecode and property bindings.

```sql
CREATE TABLE IF NOT EXISTS scripts (
    form_id INTEGER PRIMARY KEY,        -- Script FormID
    script_name TEXT NOT NULL,          -- Script EDID name
    bytecode BLOB NOT NULL,             -- Papyrus PEX binary bytecode
    properties BLOB                     -- Script properties table
);
```

---

### 10. FormID Translation Map (`formid_map`)

Maps each resolved 32-bit FormID to its stable owning plugin and plugin-local
ID. `plugin_name` is the lowercase owning filename, such as `skyrim.esm`;
`internal_id` is the low 24 bits for full plugins or the low 12 bits for light
plugins. Full and light slots are assigned independently. The pair
`(plugin_name, internal_id)` survives changes to the load-order slots.
`plugin_name` is the lowercase filename; join to `plugins` with
`lower(plugins.name) = formid_map.plugin_name`.

Ownership differs from override provenance: `records.load_order` identifies
the winning plugin's priority. Game settings override by case-insensitive
EditorID and retain the first definition's identity. A deletion with no EDID
resolves through any previously encountered non-null FormID alias; an unknown
header-only deletion is skipped with a warning. Later restorations keep the
original identity. Ambiguous aliases and live settings without an EDID are
errors, rather than silently replacing or dropping another record.

Known FormID fields inside the published subrecord blobs (`records.data`,
`cells.data`, `references.data`) are rewritten into the same load-order numbering
as the record keys, so a consumer can look them up directly. That covers the
single-FormID fields in the converter's `is_form_id_subrecord`, the
LAND/GRAS/LTEX payloads, the primary VMAD script-property references, and these
reference and cell fields, validated against their expected sizes:

| Field | Records | FormIDs |
| --- | --- | --- |
| `XTEL` | placed references | door destination reference (bytes 0-3 of 32) |
| `XESP` | placed references | enable parent (bytes 0-3 of 8; flags and unused bytes preserved) |
| `XLKR` | placed references | keyword and linked reference (8 bytes), or the linked reference alone (legacy 4 bytes) |
| `XNDP` | placed references | navmesh (bytes 0-3 of 8) |
| `XEMI` | placed references | emitted light or region |
| `XAPR` | placed references | activate-parent reference (one 8-byte subrecord per parent) |
| `XLRT` | placed references | location reference types (array) |
| `XHOR` | ACHR | horse reference |
| `LTMP` | CELL, WRLD | lighting template |
| `XCIM`, `XCMO`, `XCAS` | CELL | image space, music type, acoustic space |
| `XCCM` | CELL | region the cell takes its sky and weather from |
| `XCLR` | CELL | regions (array) |

For blob remapping, placed references are REFR, ACHR, ACRE, PGRE, PMIS, PHZD,
PARW, PBAR, PBEA, PCON and PFLA. This does not expand the record types exported
to the `references` table; the six newly covered types retain their blobs in
`records` without adding `references` rows.
Fields not covered may still hold plugin-local FormIDs. In particular, do not
consume `WRLD.RNAM` large-reference lists, `XLOC` lock keys, `XPWR` water
reflections, or `XPOD`/`XLRM` room and portal links as resolved IDs. Other
unconverted fields include `XLIB`, `XMBR`, `XATR`, `XTNM`, `PDTO`, `CELL.XILL`
and `WRLD.ZNAM`. Check the converter's field-specific handling before using
these payloads; this list is not exhaustive.

The fields in this table validate master indices and light-plugin local IDs.
An out-of-range index or light-plugin local ID wider than 12 bits is an invalid
optional link: the converter sets that FormID to zero and reports a warning,
aggregated per source plugin with the count and first record/field diagnostic.
Other records and valid links continue to convert. Malformed field lengths cause
the entire subrecord to be dropped and counted in the same per-plugin warning,
since its FormID offsets cannot be decoded safely. An array with an incomplete
entry is dropped in full; valid repeated subrecords are retained. Existing
validation of record headers and required fields is unchanged. Zero FormIDs
remain zero. Other bytes in retained subrecords (such as teleport coordinates,
enable flags, navmesh triangles and activation delays) are preserved. Unlisted fields remain opaque,
not an assurance that all FormIDs in arbitrary Skyrim or mod subrecords have
been resolved.

Existing packs must be converted again to obtain these corrected links. This
does not change the schema-4 table or blob layout, so the database and asset
cache versions are unchanged. Every pipeline run, including resume, rebuilds
the database from source plugins while reusing unaffected assets. Old packs
are still accepted by the runtime: schema 4 alone does not certify these links
were remapped. A future consumer of these fields must account for old packs.
`export_to_db` without a load order refreshes already-resolved records (for
example movement annotations), preserving established `formid_map` ownership;
it neither resolves plugin-local IDs nor upgrades an old pack.

```sql
CREATE TABLE IF NOT EXISTS formid_map (
    form_id INTEGER PRIMARY KEY,       -- Resolved 32-bit Skyrim FormID
    plugin_name TEXT NOT NULL,          -- Owning plugin filename
    internal_id INTEGER NOT NULL,       -- Plugin-local ID (24 or 12 bits)
    record_type TEXT NOT NULL           -- Record type ('REFR', 'NPC_', etc.)
);
```

---

### 11. Asset Conversion Cache (`conversion_cache`)

Stores plugin file path hashes and timestamps to bypass re-converting unchanged files.

```sql
CREATE TABLE IF NOT EXISTS conversion_cache (
    plugin_path TEXT PRIMARY KEY,
    file_hash BLOB NOT NULL,
    last_converted INTEGER NOT NULL
);
```

---

### 12. Point Light Sources (`lights`)

One row per `LIGH` base record: radius, colour, flags, falloff exponent and the
optional `FNAM` fade, which is what the runtime places a point light from. The
radius is a `DATA` `u32` in Creation units widened to a float, and the row is
written whether or not the record has a `MODL`: an invisible light still lights
the space, and most `LIGH` records in `Skyrim.esm` are invisible. A record whose
`DATA` is missing or holds fewer than the 20 bytes these columns need gets no
row rather than invented values, and a `LIGH` without a `MODL` gets no `statics`
row either - there would be no mesh to draw.

A reference that places a light usually carries its own `XRDS` radius, stored in
`references.radius_override` (10,810 of the 12,148 `LIGH` references in
`Skyrim.esm`), which overrides the base record's radius for that placement.

```sql
CREATE TABLE IF NOT EXISTS lights (
    id INTEGER PRIMARY KEY,        -- LIGH FormID
    editor_id TEXT,
    radius REAL NOT NULL,          -- Creation units (DATA u32)
    color_r INTEGER NOT NULL, color_g INTEGER NOT NULL, color_b INTEGER NOT NULL,
    flags INTEGER NOT NULL,        -- DATA flags (dynamic, can carry, negative, flicker, off by default, ...)
    falloff REAL NOT NULL,
    fade REAL                      -- FNAM, if present
);
```

---

### 13. Water Definitions (`waters`)

Stores one row per `WATR` record: Skyrim's per-water colours and reflectivity, decoded from the
record's `DNAM` subrecord (offsets in `docs/research/water.md` section 1.2), plus the raw
subrecords for anything not broken out into a column.

```sql
CREATE TABLE IF NOT EXISTS waters (
    id INTEGER PRIMARY KEY,             -- WATR FormID
    editor_id TEXT,
    opacity INTEGER,                    -- ANAM, 0-100
    flags INTEGER NOT NULL,             -- record header flags
    shallow_color INTEGER,              -- DNAM+40: packed 0x00BBGGRR
    deep_color INTEGER,                 -- DNAM+44: packed 0x00BBGGRR
    reflection_color INTEGER,           -- DNAM+48: packed 0x00BBGGRR
    fresnel REAL,                       -- DNAM+24: Fresnel Amount (Schlick F0)
    reflectivity REAL,                  -- DNAM+20: Reflectivity Amount
    flow_normal_path TEXT,              -- NAM5, canonicalised (SSE flowmap waters only)
    data BLOB NOT NULL                  -- Serialized subrecords payload
);
```

`shallow_color`/`deep_color`/`reflection_color`/`fresnel`/`reflectivity` are additive columns: a
`skyrim_world.db` built before they existed has a `waters` table without them, and
`AssetCatalog::water_colors` (`crates/engine/src/world/database.rs`) returns `None` for every
water against such a database rather than failing to open it. The engine falls back to Skyrim's
DefaultWater values (`render::DEFAULT_WATER_FRESNEL` / `render::DEFAULT_WATER_REFLECTIVITY`, and
`streaming.rs`'s deep-colour constant) until a reconversion populates them. Adding them did **not**
bump `shared::WORLD_DATABASE_SCHEMA_VERSION`: nothing that already reads `waters` depends on their
presence, and the fallback exists specifically so a reconversion is not required.

---

### 14. Grass Definitions and Landscape Associations (`grass_types`, `landscape_texture_grasses`)

`grass_types` projects each effective `GRAS` record, including its canonical converted
model path (`MODL` through the existing mesh asset-path mapping). Grass models use the
same NIF-to-GLB converter as other meshes. `flags` below contains the `DATA` grass
flags rather than record header flags. `load_order` identifies the winning
override; `formid_map` retains owning-plugin identity independently.

The layout follows [xEdit's TES5 GRAS and LTEX definitions](https://github.com/TES5Edit/TES5Edit/blob/dev-4.1.5/Core/wbDefinitionsTES5.pas)
and [CommonLibSSE-NG's TESGrass definition](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/main/include/RE/T/TESGrass.h).
`DATA` is 32 bytes; its padding and unprojected fields stay in `records.data`.
The converter preserves authored values without clamping to vanilla ranges. A field
whose bytes are missing, or a non-finite float, becomes NULL. Missing, empty or unsafe
model paths also become NULL; the original subrecords remain available for diagnostics.
Path normalization does not verify that the referenced asset exists. Even vanilla
records can name an absent mesh, so a non-NULL `model_path` is not an availability
guarantee. Grass consumers must tolerate missing models, skip the unavailable
model with a useful diagnostic, and continue processing other grass types.

```sql
CREATE TABLE IF NOT EXISTS grass_types (
    id INTEGER PRIMARY KEY,            -- resolved GRAS FormID
    editor_id TEXT,
    model_path TEXT,                    -- canonical meshes/...glb from MODL
    density INTEGER,                    -- DATA+0: u8
    min_slope INTEGER,                  -- DATA+1: u8, degrees
    max_slope INTEGER,                  -- DATA+2: u8, degrees
    units_from_water INTEGER,           -- DATA+4: u16, distance from water level
    water_comparison INTEGER,           -- DATA+8: u32, enum below
    position_range REAL,                -- DATA+12: f32
    height_range REAL,                  -- DATA+16: f32
    color_range REAL,                   -- DATA+20: f32
    wave_period REAL,                   -- DATA+24: f32
    flags INTEGER,                     -- DATA+28: u8
    load_order INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS landscape_texture_grasses (
    ltex_id INTEGER NOT NULL,           -- resolved LTEX FormID
    gras_id INTEGER NOT NULL,           -- resolved GNAM GRAS FormID
    PRIMARY KEY (ltex_id, gras_id)
);
```

`water_comparison` values 0 through 7 mean Above At Least, Above At Most,
Below At Least, Below At Most, Either At Least, Either At Most,
Either At Most Above and Either At Most Below, respectively. `flags` bits 0,
1 and 2 mean Vertex Lighting, Uniform Scaling and Fit to Slope. These definitions
establish the authored contract, not Skyrim's exact placement equations.

Each non-null repeated `LTEX.GNAM` yields an association; duplicate pairs collapse.
The winning LTEX list replaces the earlier list in full. There is no foreign key on
the grass target: a deleted or unresolved grass can still be named by a texture,
and consumers resolve it against `grass_types`. Both projections refresh atomically
from a complete effective load-order export, removing stale grass and associations
on deletions or later overrides. Deleted GRAS records therefore do not reappear as
grass definitions. The subset export used by movement annotation preserves unrelated
grass rows; if it includes an LTEX, only that texture's association list is replaced.
Movement annotation continues to accept schema 4 through the current schema and
preserves the existing database version; it does not perform a full reconversion.

---

### 15. NPC Base Records (`npcs`)

One row per winning `NPC_` record. `full_name` is the record's `FULL` name. A
plugin with the TES4 `Localized` flag (0x80), which every official master sets,
stores `FULL` as a 4-byte ID into `Strings/<plugin>_<language>.STRINGS` instead
of as text; the converter resolves it through the table of the plugin that
supplied the winning record, so a localized override uses its own table and a
plain plugin's override stays text. Tables are read from loose files first, then
from the plugin's extracted archives, matching names without regard to case.
`--language` (`PipelineConfig.language`, default `english`) selects the table,
as Skyrim's `sLanguage` does. A string ID of 0, a missing table or a missing
string stores `NULL` - never the ID's bytes read as text - and the conversion
prints one warning per affected plugin. A string that is not valid UTF-8 is read
as Windows-1252.

```sql
CREATE TABLE IF NOT EXISTS npcs (
    id INTEGER PRIMARY KEY,        -- NPC_ FormID
    editor_id TEXT,
    full_name TEXT,                -- FULL, resolved through the string table when localized
    race_id INTEGER,               -- RNAM
    class_id INTEGER,              -- CNAM
    flags INTEGER NOT NULL         -- record header flags
);
```
