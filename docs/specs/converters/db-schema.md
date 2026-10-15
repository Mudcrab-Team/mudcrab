# Mudcrab SQLite 3 Database Schema (`skyrim_world.db`)

This specification details the canonical DDL schema, tables, indices, and column constraints for `skyrim_world.db`, as implemented in [`crates/converter/src/esm/exporter.rs`](../../../crates/converter/src/esm/exporter.rs).

---

## 1. Schema Overview

`skyrim_world.db` is built by `crates/converter` by parsing master files (`Skyrim.esm`) and plugin files (`.esp`/`.esl`). With `PipelineConfig.plugins_file` (CLI `--plugins-file`), the converter reads Windows-1252 selection, keeps the last duplicate entry's position and checkbox, and matches installed filenames case-insensitively. Installed Skyrim, Update, Dawnguard, HearthFires and Dragonborn masters are always active in that fixed order, followed by installed entries of `Skyrim.ccc`. `skyrim_ccc_file` (`--skyrim-ccc-file`) overrides the default file beside Data. Only checked player entries load, including mod masters; official and Creation entries keep their implicit positions. An explicit `skyrim_ini_file` (`--skyrim-ini-file`) supplies General `sTestFile1` through `sTestFile10`: listed plugins retain their listed positions even if unchecked, while unlisted entries append by ascending file modification time (filename breaks ties). No ambient user profile is read. Missing entries and optional CCC/INI read errors warn and continue. Plugins with missing, disabled or unusable masters and their transitive dependents are omitted with bounded notices; an empty usable selection gives a clear error.

MO2 profiles reuse the shared selection rules and the converter's header/missing-master closure recovery over winning overlay paths. Installed official masters and game-folder CCC entries remain implicitly active; other selected plugins follow `loadorder.txt`. Missing ordered entries append in last-occurrence `plugins.txt` order, with a warning. Both profile lists use Windows-1252 and retain only the last duplicate. Missing files and unusable dependency closures skip with profile-context notices, preserving usable neighbors. Classic Skyrim still activates bare profile entries. Plugin filename and TES4 master keys use the shared Unicode `normalize_plugin_name` function (`to_lowercase`), including MO2's direct plugin overlay keys. Record signatures, EditorIDs and other asset keys retain their existing rules. MO2 preserves its existing final order validation rather than applying player-list priority normalization.

The existing explicit-order normalization gives ESM-flagged plugins and `.esm`/`.esl` files priority, preserving listed order within each category except that regular dependencies are moved ahead of master files that need them. Cyclic plugins and all their dependents are omitted before normalization. The established final order admits at most 254 full and 4,096 light plugins; overflow and its transitive dependents are omitted without backfilling later entries or changing survivor order. One bounded warning summarizes each recovery cause; the conversion report's `skipped_plugins` retains every original filename removed by selection recovery, once each in pre-recovery selection order, including attempted ambiguous filenames, unusable headers and missing-master closures. Unchecked plugins are not skips. `load_order` records surviving original filenames in the same order as the published `plugins` table. Both vectors default to empty when absent from older reports. A plugin the normalized order still places before one of its masters is omitted with its dependents rather than moved. Surviving paths feed slot assignment, archive priority, database merging and terrain caches. An ESL-flagged `.esp` takes a light slot but stays among regular plugins.

Player and MO2 selection use a shared fallback when `Skyrim.ccc` is missing or unreadable: the installed subset of `ccBGSSSE001-Fish.esm`, `ccQDRSSE001-SurvivalMode.esl`, `ccBGSSSE037-Curios.esl`, `ccBGSSSE025-AdvDSGS.esm` and `_ResourcePack.esl`, in that order after the five official masters. A readable CCC, even empty, stays authoritative. `selection_warnings` records the fallback's CCC path and read failure separately from conversion failures; CLI/progress/launcher display it as a warning without making usable output incomplete. The optional vector defaults to empty for older reports.

Archive ownership and load-order priority share the same exact-or-space/dash/underscore stem matcher. The longest installed owner must survive recovery before a companion can contribute inputs, including repair and metadata rebuilds. Active `_` and `-` companions are admitted in MO2; unowned archives remain excluded there and admitted outside MO2. Equal active stems keep their first slot. Direct MO2 plugin-name ambiguity in the winning layer is recoverable; higher-layer unique replacements retain normal overlay behavior.

Without `plugins_file` or an MO2 profile, only plugins directly in Data are selected and dependencies precede dependents. Among ready plugins, ESM-flagged plugins and `.esm`/`.esl` files take priority, followed by the five official files' conventional order and Unicode-normalized filename order. Unusable headers and missing-master closures are omitted with bounded notices, as in player selection, and this path shares cycle and slot-overflow recovery. A nonempty plugin selection reduced to no usable plugins fails clearly, while asset-only inputs retain their existing behavior. Automatic discovery cannot infer override priorities between unrelated mods; nested backup/optional plugins are ignored while nested assets remain discoverable.

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
| `XPOD` | REFR | the rooms a portal marker joins: an array of (origin, destination) pairs; every u32 a FormID (xEdit 4.1.5f defs) |
| `XLRM` | REFR | linked room of a room marker |
| `LNAM`, `INAM` | REFR | room marker lighting template, image space (other record types keep their own handling) |

For blob remapping, placed references are REFR, ACHR, ACRE, PGRE, PMIS, PHZD,
PARW, PBAR, PBEA, PCON and PFLA. This does not expand the record types exported
to the `references` table; the six newly covered types retain their blobs in
`records` without adding `references` rows.
Fields not covered may still hold plugin-local FormIDs. In particular, do not
consume `WRLD.RNAM` large-reference lists, `XLOC` lock keys, `XPWR` water
reflections as resolved IDs. Other
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

### 15. Interior Room Graph (`room_bounds`, `room_portals`, `room_links`, `reference_rooms`, `room_graph_stats`)

Skyrim marks the rooms of an interior with `RoomMarker` references (base `0x1F`) and the
doorways between them with `PortalMarker` references (base `0x20`). The converter decodes both
and records which room boxes each placed object belongs to. The tables hold facts only: which
objects to draw is the engine's policy (design: `docs/research/interior-room-graph-converter.md`,
upstream issue #188). Only interior cells and only box rooms (`XPRM` shape 1) are written;
exterior markers are skipped silently, and rooms of other shapes, and rooms or portals with a non-finite size or transform, are skipped and counted in the conversion report.
All FormIDs are in load-order numbering, like `references.id`.
The two marker base forms are the ones `Skyrim.esm` defines (always load-order slot 0, so the numbers do not change with the load order). A plugin that places rooms with a marker base of its own is not recognised and produces no rows; the conversion report does not count that case.

```sql
CREATE TABLE IF NOT EXISTS room_bounds (       -- one row per interior RoomMarker with a box XPRM
    ref_id INTEGER PRIMARY KEY,                -- references.id of the marker
    cell_id INTEGER NOT NULL,
    pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,   -- Creation units
    rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,   -- radians, as references
    half_x REAL NOT NULL, half_y REAL NOT NULL, half_z REAL NOT NULL, -- XPRM[0..12]
    lighting_template_id INTEGER,              -- LNAM, NULL when absent
    image_space_id INTEGER,                    -- INAM, NULL when absent
    flags INTEGER NOT NULL DEFAULT 0           -- XRMR flags byte | (XRMR u16 << 8), kept raw
);
CREATE INDEX IF NOT EXISTS idx_room_bounds_cell ON room_bounds(cell_id);

CREATE TABLE IF NOT EXISTS room_portals (      -- one row per XPOD pair of an interior PortalMarker with an XPRM
    ref_id INTEGER NOT NULL,                   -- references.id of the marker; NOT unique, see pair_index
    pair_index INTEGER NOT NULL DEFAULT 0,     -- position of the (origin, destination) pair in XPOD
    cell_id INTEGER NOT NULL,
    pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
    rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
    half_x REAL NOT NULL, half_z REAL NOT NULL, -- doorway half-width and half-height
    room_a INTEGER,                            -- the pair's origin; NULL when 0, absent or not a room of this cell
    room_b INTEGER,                            -- the pair's destination; same
    PRIMARY KEY (ref_id, pair_index)
);
CREATE INDEX IF NOT EXISTS idx_room_portals_cell ON room_portals(cell_id);

CREATE TABLE IF NOT EXISTS room_links (        -- XLRM: rooms seen together without a portal
    room_ref INTEGER NOT NULL,
    linked_ref INTEGER NOT NULL,
    PRIMARY KEY (room_ref, linked_ref)
);

CREATE TABLE IF NOT EXISTS reference_rooms (   -- room membership, interiors with rooms only
    reference_id INTEGER NOT NULL,             -- references.id
    room_ref INTEGER NOT NULL,                 -- room_bounds.ref_id
    contained INTEGER NOT NULL,                -- 1: pivot inside the box; 0: only the bounds overlap it
    PRIMARY KEY (reference_id, room_ref)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_reference_rooms_room ON reference_rooms(room_ref);

CREATE TABLE IF NOT EXISTS room_graph_stats (  -- conversion report counters, one row per name
    name TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
```

Columns and rules:

- **`room_bounds`**: the box is centred on the marker's position and oriented by its rotation
  (the same Euler convention as any reference, `shared::coordinates`). `half_*` are the `XPRM`
  half-extents in the room's own frame. `lighting_template_id` (`LNAM`) and `image_space_id`
  (`INAM`) are decoded for the lighting work in #129-#135; nothing in the engine uses them yet.
- **`room_portals`**: a portal's doorway is the marker's local X-Z rectangle (normal along local
  +Y); `half_x`/`half_z` are its half-width and half-height. A portal marker without `XPRM`
  has no size and gets no row. `XPOD` is an array of 8-byte (origin, destination) FormID pairs
  (xEdit 4.1.5f defs, `Core/wbDefinitionsTES5.pas`: `wbArray(XPOD, 'Portal Data',
  wbStruct('References', [wbFormIDCk('Origin', [REFR, NULL]), wbFormIDCk('Destination',
  [REFR, NULL])]))`), so a portal gets **one row per pair**, numbered by `pair_index` from 0
  in stored order; the geometry columns repeat on every row. A portal whose `XPOD` is absent,
  empty or shorter than one pair gets one row with `pair_index` 0 and both sides NULL. Bytes
  after the last whole pair are ignored. Every `XPOD` in the official masters is one pair, so
  vanilla output has `pair_index` 0 only. **Readers must not assume `ref_id` is unique**: join
  or group on `(ref_id, pair_index)`, or on `ref_id` and expect several rows. The key changed
  from `ref_id` alone, and `create_tables` migrates an older-shaped `room_portals` (no
  `pair_index`) in place, inside one savepoint: every row is kept with `pair_index` 0 until the
  next full export rebuilds the table.
  In the plugin remap `XPOD` is an array of FormIDs: every four bytes are remapped, and a
  length that is not a multiple of four is dropped with a warning, like any other FormID array.
- **`room_links`**: one row per `XLRM` as stored on the room, with no mirror rows. A link joins
  rooms that may have no portal between them but must be seen together: if one is visible, so is
  the other. The target is stored as written, even if it is not a `room_bounds` row.
- **`reference_rooms`**: written for every placed object (`REFR`, `ACHR`, `ACRE`, `PGRE`,
  `PMIS`) in an interior cell that has at least one room, except the room and portal markers
  themselves. `contained = 1` rows come from the ESM export: the object's pivot (its position)
  lies inside the box, edges included. `contained = 0` rows are added after model bounds are
  known (the integration stage, once `statics.bounds_*` come from the converted GLBs): the
  pivot is outside, but the object's bounds, placed with the reference's scale, rotation and
  position, overlap the box. `statics.bounds_*` are glTF-axis (Y-up) numbers in Creation units,
  with no metre scale. The test is conservative (the placed bounds' corners, taken into the room
  frame, as an axis-aligned box against the half-extents), so it can report an overlap that does
  not exist, which only makes the engine draw more. A `contained = 1` row is never also written
  as `contained = 0`.
  Overlap rows need a base with bounds in `statics`, which holds every base type with a world
  model (`STAT`, `MSTT`, `FURN`, `TREE`, `FLOR`, `CONT`, `DOOR`, `ACTI`, placed items and armour,
  and `LIGH` with a model). Placed references of other base types get pivot rows only: actors
  (`NPC_`, which move and stay ordinary entities), lights without a model (their reach is a
  lighting question, not geometry), decals (`TXST`), and bases with no geometry (`IDLM`, `SOUN`,
  `ASPC`). In the all-plugins conversion these are 22,251 of 364,262 pivot rows.
- **`room_graph_stats`**: the counters the conversion report shows in `integration-report.json`
  under `room_graph`: `rooms`, `portals` (portal markers written, not rows), `links`,
  `portals_with_missing_side` (`room_portals` rows with a NULL side, so a portal with several
  pairs can add several), `portals_with_extra_pairs` (markers whose `XPOD` holds more than one
  pair), `portals_with_trailing_xpod_bytes` (markers whose `XPOD` length is not a multiple of 8),
  `non_box_rooms_skipped`, `portals_without_size_skipped`, `invalid_markers_skipped` (rooms and portals with a non-finite size or transform), `objects_in_no_room` and
  `objects_in_several_rooms` (both by pivot), and `overlap_rows`.

The tables are derived data: a full load-order export clears and rebuilds them from the winning
records on every conversion, like the grass tables, and the subset export used by movement
annotation leaves them alone. `create_tables` adds them to an older reused database.

**These tables are optional and did not bump `shared::WORLD_DATABASE_SCHEMA_VERSION`.** No engine code reads
them yet; a reader must probe for them first, as the engine does for `lights` and the additive
`waters` columns, so a database without them behaves as before.

| Case | Converter writes | Engine |
|---|---|---|
| Interior without room markers | no rows | draw everything, as today |
| Old database, tables absent | n/a | feature off, no error |
| Portal side 0, missing or not a room | `room_a`/`room_b` NULL | ignore that edge |
| Object in no room / several | no `contained = 1` row / several | always drawn |
| Camera outside every room | n/a | draw everything |
| Non-box room shape | skipped, counted | its objects are in no room, so drawn |
| Exterior markers | not written | out of scope (#106) |
