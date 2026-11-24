# Mudcrab SQLite 3 Database Schema (`skyrim_world.db`)

This specification details the canonical DDL schema, tables, indices, and column constraints for `skyrim_world.db`, as implemented in [`crates/converter/src/esm/exporter.rs`](../../../crates/converter/src/esm/exporter.rs).

The in-house reader is the default; `--record-reader legacy` selects the
hand-written frontend. Both record frontends use the same runtime column contract and TXST slot
interpretation: diffuse TX00, normal TX01, mask TX02, glow
TX03, height TX04, environment TX05, detail TX06, and specular TX07. The existing
`detail_path` column carries TX06 multilayer data; TX03 is the source's
glow/detail-map slot and TX07 its backlight-mask/specular slot. These role names
retain the runtime approximation and do not imply complete shader support.
ARMO model projection uses MOD2 with MOD4 as the female world-model fallback
for both frontends. MOD3 is an ARMA model slot, not an ARMO world-model path.

---

## Optional PC control files

The converter publishes `interface/controls/pc/controlmap.txt`,
`keyboard_english.txt`, and `gamepad.txt` as optional copied files beside the
world database. Their presence adds no database tables or required capability;
older packs without these paths remain readable. This preserves the original
text contract for future input/menu consumers without a SQL-specific parser.

These three paths are additive inputs, extracted separately from each enabled
archive on every conversion. Only archive headers and the selected control
payloads are read by this extra pass; unrelated payloads are not decompressed
or repacked. Existing `converter-inputs-v4` and `runtime-english-v4` inventories
retain their original coverage and remain reusable, without a second archive
pack generation. Mesh, texture, database and script schema/cache versions stay
the same.
Archives overlay in the existing plugin load order, last provider wins, then
loose Data files (or the effective MO2 loose winner) override archives. Paths
match case-insensitively and publish in lowercase.
Each control payload is extracted independently. A failed payload emits one
warning for that path and leaves its earlier archive provider intact (or absent),
while readable files from the same archive still win normally. An unreadable
archive retains one archive-level warning rather than repeating it per path.

Rows receive structural validation, preserving binding expressions as opaque
text for the input layer. Controlmap rows have seven tab-separated fields
(event, keyboard, mouse, gamepad, three 0/1 remap flags), optionally followed by
hexadecimal event flags. Consecutive tabs provide alignment; spaces inside
names and aliases remain meaningful. Keyboard rows contain a key name and hex
hardware code, optionally a decimal Flash code; gamepad rows contain a name
and hex hardware code. Alias, alternative and chord binding expressions are
retained; this pass does not resolve or semantically validate them.

Original bytes, comments (`//`), blank context separators and line endings are
preserved, including legacy Windows-1252 names and an optional leading UTF-8
BOM. A malformed physical line is removed entirely, including its newline, so
it cannot introduce a new context. One warning per affected file summarizes
the skipped-line count and first original 1-based line number, making the report
and manifest incomplete while valid neighboring lines and files still publish.
LF, CRLF and CR-only files retain their original endings.
If removing a malformed row would join a retained CR to a later blank LF row,
the CR is completed to CRLF so the blank context separator remains distinct.
Control extraction runs independently of ordinary archive payload failures;
readable controls still publish with the asset failure warning. Undiscovered
loose prefixes mask affected control sources as well as ordinary VFS inputs,
so an unknown loose override cannot expose an older archive binding.
An unreadable winning file warns and is omitted. Output I/O failures remain
errors. An empty or absent optional file is permitted.

`interface/controls/pc/provenance.json` is an object keyed by published logical
path. Each entry has `provider` (`archive:<relative archive path>` or
`loose:<logical path>`), `source_sha256` (original bytes), `output_sha256`
(sanitized bytes), and `skipped_lines` (original 1-based line numbers). Loose
provider paths describe the effective logical winner, including MO2, rather
than machine-specific installation paths. The source hash identifies its exact
content. The sidecar is absent when no control files publish.

A metadata rebuild (`--reuse-assets <source-pack>` into a new output) can add or
refresh these files: it extracts only the three paths from the existing matched
plugin archive selection and applies loose overrides, retaining verified
GLB/KTX2/Luau bytes. Existing metadata restrictions still apply (complete
compatible source pack, matching plugins, disjoint new output, no MO2 mode).
An ingestion-only extraction does not publish runtime files. Normal conversion
also refreshes the files while reusing unrelated compatible assets; a full
mesh/texture reconversion is unnecessary for this addition.

## 1. Schema Overview

`skyrim_world.db` is built by `crates/converter` by parsing master files (`Skyrim.esm`) and plugin files (`.esp`/`.esl`). With `PipelineConfig.plugins_file` (CLI `--plugins-file`), the converter reads Windows-1252 selection, keeps the last duplicate entry's position and checkbox, and matches installed filenames case-insensitively. Installed Skyrim, Update, Dawnguard, HearthFires and Dragonborn masters are always active in that fixed order, followed by installed entries of readable `Skyrim.ccc`. `skyrim_ccc_file` (`--skyrim-ccc-file`) overrides the default file beside Data. Only checked player entries load, including mod masters; official and Creation entries keep their implicit positions. An explicit `skyrim_ini_file` (`--skyrim-ini-file`) supplies General `sTestFile1` through `sTestFile10`: listed plugins retain their listed positions even if unchecked, while unlisted entries append by ascending file modification time (filename breaks ties). No ambient user profile is read. Missing entries and optional CCC/INI read errors warn and continue. Plugins with missing, disabled or unusable masters and their transitive dependents are omitted with bounded notices; an empty usable selection gives a clear error.

MO2 profiles reuse the shared selection rules and the converter's header/missing-master closure recovery over winning overlay paths. Installed official masters and game-folder CCC entries remain implicitly active; other selected plugins follow `loadorder.txt`. Missing ordered entries append in last-occurrence `plugins.txt` order, with a warning. Both profile lists use Windows-1252 and retain only the last duplicate. Missing files and unusable dependency closures skip with profile-context notices, preserving usable neighbors. Classic Skyrim still activates bare profile entries. Plugin-name keys use the shared Unicode `normalize_plugin_name` function (`to_lowercase`) across player lists, MO2 lists, merged Data lookup, master matching and identity maps. Windows-1252 `CAFÉ.esp` and `café.esp` therefore match; Unicode normalization and full Unicode case folding are not implemented. Record signatures, EditorIDs and other asset keys retain their existing rules. MO2 preserves its existing final order validation rather than applying player-list priority normalization.

The existing explicit-order normalization gives ESM-flagged plugins and `.esm`/`.esl` files priority, preserving listed order within each category except that regular dependencies are moved ahead of master files that need them. Cyclic plugins and all their dependents are omitted before normalization. The established final order admits at most 254 full and 4,096 light plugins; overflow and its transitive dependents are omitted without backfilling later entries or changing survivor order. One bounded warning summarizes each recovery cause; the conversion report's `skipped_plugins` retains every original filename removed by selection recovery, once each in pre-recovery selection order, including attempted ambiguous filenames, unusable headers and missing-master closures. Unchecked plugins are not skips. `load_order` records surviving original filenames in the same order as the published `plugins` table. Both vectors default to empty when absent from older reports. A plugin the normalized order still places before one of its masters is omitted with its dependents rather than moved. Surviving paths feed slot assignment, archive priority, database merging and terrain caches. An ESL-flagged `.esp` takes a light slot but stays among regular plugins.

Player and MO2 selection use a shared fallback when `Skyrim.ccc` is missing or unreadable: the installed subset of `ccBGSSSE001-Fish.esm`, `ccQDRSSE001-SurvivalMode.esl`, `ccBGSSSE037-Curios.esl`, `ccBGSSSE025-AdvDSGS.esm` and `_ResourcePack.esl`, in that order after the five official masters. A readable CCC, even empty, stays authoritative. `selection_warnings` records the fallback's CCC path and read failure separately from conversion failures; CLI/progress/launcher display it as a warning without making usable output incomplete. The optional vector defaults to empty for older reports.

Archive ownership and load-order priority share the same exact-or-space/dash/underscore stem matcher. The longest installed owner must survive recovery before a companion can contribute inputs, including repair and metadata rebuilds. Active `_` and `-` companions are admitted in MO2; unowned archives remain excluded there and admitted outside MO2. Equal active stems keep their first slot. Direct MO2 plugin-name ambiguity in the winning layer is recoverable; higher-layer unique replacements retain normal overlay behavior.

Without `plugins_file` or an MO2 profile, only plugins directly in Data are selected and dependencies precede dependents. Among ready plugins, ESM-flagged plugins and `.esm`/`.esl` files take priority, followed by the five official files' conventional order and Unicode-normalized filename order. Unusable headers and missing-master closures are omitted with bounded notices, as in player selection, and this path shares cycle and slot-overflow recovery. A nonempty plugin selection reduced to no usable plugins fails clearly, while asset-only inputs retain their existing behavior. Automatic discovery cannot infer override priorities between unrelated mods; nested backup/optional plugins are ignored while nested assets remain discoverable.

The database stamps its version in `schema_info`; the current version is **9**
(`shared::WORLD_DATABASE_SCHEMA_VERSION`). Schema 4 added lights and
`references.radius_override`. Schema 5 adds grass data in #152;
the combined producer exports grass and LOD tables. Schema 6 adds LOD origins, chunk metadata,
its spatial index, and a build identity. Schema 9 combines the corrected texture-set slot
projection: `mask_path` comes from TX02, `glow_path` from TX03, `height_path`
from TX04 and `environment_path` from TX05. TX00 and TX01 provide diffuse and
normal paths. Both frontends project TX06 into `detail_path` and TX07 into
`specular_path`, as documented above. Schema 9 also
projects only XESP byte 4 into `enable_parent_flags`. Separate candidate fixes
used producer 25/world 8 for different contracts; the combined output first shipped
as 26/9, and producer 27 keeps world schema 9.
The engine, `world-inspect` and launcher accept world schemas **3 through 9**, using
`shared::supports_runtime_world_database_schema`. Complete converter packages
support schemas **15 through 28**. Legacy worlds render full detail without
LOD; an advertised LOD package requires the current database contract.

Converter producer 26 also corrects the texture encodings chosen from those
columns. Producer 27 adds sphere, multi-sphere and cylinder collision shapes
and more physical layers to the GLB collision data; the database stays at
schema 9. Producer 28 changes the KTX2 mip layout and alignment; the database
still uses schema 9. Producer-27 KTX2 files need regeneration, while
producer-27 GLBs remain reusable because their bytes are unchanged. At producer
28, GLB producers earlier than 27 are incompatible. Normal conversion rebuilds the
database, cell cache and derived LOD outputs, and regenerates assets whose
producer contract or source/configuration/output proof is incompatible.
Unchanged scripts and archive ingestion remain reusable when their own source,
dependency, configuration and output-byte proofs match.
Metadata-only rebuilds preserve the original retained producer/configuration
and mesh and texture bytes. Run normal conversion to repair those retained
encodings and collision data.

The legacy TX06/TX07 alignment changes the SQL role assignment while retaining
the current texture-byte encoding: multilayer/detail and specular/backlight are
both color uses in `TextureEncoding::from_semantics`. Exchanging those roles
leaves the selected encoding unchanged, including normal/data precedence for
textures with several uses. ARMO model-path alignment also changes the SQL
projection; mesh conversion discovers NIF inputs independently of that column.
These projection changes require rebuilding metadata. Any subsequent change to
converted mesh or texture bytes must invalidate the affected asset cache key;
metadata retention preserves the producer identity of the retained bytes.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      `skyrim_world.db` Implemented Schema                   │
│  ┌──────────────────────────┬──────────────────────┬─────────────────────┐  │
│  │   `plugins`              │   `records`          │ `worldspaces`       │  │
│  │   (Active Plugin Order)  │   (Raw FormID Data)  │ (Worldspace EDIDs)  │  │
│  ├──────────────────────────┼──────────────────────┼─────────────────────┤  │
│  │   `cells`                │   `references`       │ `exterior_spatial`  │  │
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
    id INTEGER PRIMARY KEY,             -- load-order index; records.load_order refers to it
    name TEXT NOT NULL UNIQUE,          -- plugin filename as installed
    priority INTEGER NOT NULL,          -- same as id
    checksum BLOB NOT NULL              -- SHA-256 of the plugin file
);
```

---

### 2. Primary Record Database (`records`)

Stores subrecord byte payloads indexed by 32-bit Skyrim `FormID` and 4-character
record type codes, encoded as rkyv `ArchivedRecordData`. Both frontends resolve
known embedded links to load-order numbering. The in-house frontend projects
validated decoded fields into this existing encoding. Its auxiliary experimental
`inhouse_source_records(form_id, source_form_id, load_order, record_type, flags,
payload)` table holds original
decompressed source bytes, including file-relative IDs and unused framing. Those
source bytes are provenance, not decoded runtime fields. `load_order` names the
winning source plugin through `plugins.id`. Required movement and terrain
semantics are validated before winner selection; rejected candidates leave an
earlier usable winner intact and are counted in per-plugin diagnostics. The pack's
`record-reader.json` identifies the frontend and schema.

When a later plugin deletes a previously defined base of a compatible type, both readers omit
placements that depended on it and count them in the conversion notices. Each notice includes
a sample placement and base FormID. This preserves record-data completeness. A placement that
names an already deleted, never-defined, or incompatible base still counts as a recovery warning,
subject to the engine's existing hardcoded low-FormID fallback.

```sql
CREATE TABLE IF NOT EXISTS records (
    form_id INTEGER PRIMARY KEY,        -- winning record, load-order numbering
    record_type TEXT NOT NULL,          -- 'CELL', 'REFR', 'NPC_', 'WEAP', 'ARMO', 'SPEL', etc.
    cell_id INTEGER,                    -- parent CELL of a placed reference or LAND; NULL otherwise
    worldspace_id INTEGER,              -- parent WRLD of an exterior CELL or reference; NULL otherwise
    load_order INTEGER NOT NULL,        -- plugins.id of the plugin that supplied the winning record
    data BLOB NOT NULL                  -- rkyv ArchivedRecordData: the subrecord payloads
);

CREATE INDEX IF NOT EXISTS idx_records_type ON records(record_type);
CREATE INDEX IF NOT EXISTS idx_records_cell_id ON records(cell_id) WHERE cell_id IS NOT NULL;
```

---

### Draft: optional in-house override history (`record_overrides`)

The prototype in-house producer adds a tooling table containing each physically
framed source record, including singletons, deleted versions and rejected
candidates. Runtime readers continue to query the existing winning-record
tables. No losing payload or decoded field set is retained for history during
conversion. Singletons are explicit: this makes complete physical-header audits
and lazy version lookup uniform without a second implicit representation.

`record_overrides` has a physical primary key `(plugin_id, source_record_offset)`
and a `(form_id, chain_position)` lookup index. Columns are canonical `form_id`,
zero-based `chain_position`, `plugin_id` naming `plugins.id`, native
`source_form_id`, `deleted`, `record_type`, `flags`, `source_record_offset`,
`source_record_size`, `status`, `reason`, native enclosing cell/world/topic IDs
and topic-group offset. The preservation extension also stores proven
`original_owner`, unmasked `full_local`, `source_identity_state`, exclusive
`source_group_end`, `frame_complete`, and exact four-byte `record_signature`
BLOB. A source lookup index covers `(original_owner,full_local,plugin_id,
source_record_offset)`. The record signature does not create a separate source
identity namespace; changing kinds at one owner/local remains a type conflict.
`source_record_offset` points at the 24-byte native
header; size is the declared on-disk payload length, including the compressed
size word when present and excluding the header. Unmappable identities are NULL
and remain identifiable by plugin, offset and native FormID. Positions follow
load order, then physical source offset; duplicate physical IDs stay distinct.

`accepted` versions participate in the existing winner/deletion policy;
`skipped` versions retain a reason and do not displace usable predecessors.
The last accepted version is the source winner unless it deletes the record.
The existing torn-override rule skips an incomplete payload that would replace
an earlier complete version; its physical history row retains that reason.
GMST history uses the reader's canonical EditorID aliases, including an already
decoded EditorID when a later mandatory field rejects that version. Broken
EditorID framing retains the raw numeric alias; history does not guess an ID.
Each TES4 row uses a NULL canonical identity
and status `header`, and stores source path and the authored schema hash.
`inhouse_plugin_masters(plugin_id,master_index,master_name)` stores original MAST
order as scalar rows with contiguous zero-based indices; a header with no child
rows has no masters. New snapshots have no `plugin_masters` JSON column.
The history loader still accepts that column in retained packs when the child
table is absent. These small metadata rows allow replay to reconstruct
original full/light mapping without reading another missing or changed plugin.
They are excluded from ordinary record-chain counts. Their deterministic positions
in the NULL-identity group are header ordering, not a semantic record chain.

`inhouse-records history <pack> <hex FormID> [--data DIR] [--json]` opens the
database read-only and verifies each source against `plugins.checksum` before
decoding the requested offset with the producing schema. Changed, missing,
oversized or undecodable versions remain local diagnostics; other versions
continue. The text view aligns authored field names and occurrences and marks
differences. This is decoded source provenance, not a merge or patch format;
partial decoding and unsupported semantic roles retain their existing limits.
Retained 18-column history layouts remain queryable. If any of the three
physical replay safety columns (group end, frame completeness or exact record
signature) is absent or NULL, that version is reported as undecodable with its
stored provenance; replay never invents missing bounds or reads unchecked bytes.
Older ASCII-folded owner spellings are accepted only through an alias derived
from the authenticated stored plugin basename and its original master list.

These optional tooling tables do not change the runtime database
contract, accepted version range or asset encodings. World and converter asset
schema constants therefore remain unchanged. The existing committed history
contribution uses in-house adapter 5; subsequent adapter identities cover the
preservation extension and required-header master-index compatibility. The
independent `diagnostics_version` in the reader identity covers complete scalar
diagnostics and the normalization of stored tooling lists. Older
in-house provenance output is invalidated through the existing reader/configuration
identity. Runtime schema constants and reusable asset encodings remain unchanged,
and older texture or mesh bytes are not relabeled. The broad configuration hash
includes the reader identity for metadata provenance. Normal conversion can
reuse compatible assets through separately recorded per-kind settings and source
proofs. Adapter-11 metadata recognizes the exact adapter-10 combined settings
proof for independently eligible asset producers, without changing retained
bytes or their recorded producer. The metadata-only path accepts the current
reader identity and that explicit adapter-10 retention contract with matching
schema/add-on bytes and complete reader fields. Older in-house packs without
`diagnostics_version` remain refused rather than relabeled with the new tooling
contract; unknown adapter versions cannot authorize retained settings.
Header rows bind the exact authored
schema; a different schema is reported as undecodable rather than silently
interpreting an old pack with new definitions. Deterministic sorted insertion
and an explicit physical primary key make ordered history contents reproducible.

`inhouse-records history-source <pack> <owner basename> <hex full local>
[--data DIR]` also reaches nullable runtime identities. Proven source ownership
comes only from the original supplier's MAST list: lower high-byte indices name
masters, the equal index names self, and a generic greater index remains
`master_index_out_of_range` with no invented self owner. The existing GMST
greater-index convention is separately labelled `gmst_editor_id_convention`;
its physical rows and decoded EDID/runtime aliases do not establish native
owner proof. Source words of zero remain a separate null state, while nonzero
self/master words with a local component of zero retain that exact source key.
Missing-master load-order validation remains a producer limitation: this
extension does not admit or scan a wholly rejected plugin sequence.

`history-physical <pack> <decimal plugin id> <decimal source offset> [--data DIR]`
reaches every stored header, including unproven owner words. Source/physical
access requires the caller to authenticate the complete database against a
trusted completed-producer receipt/pin and preserve that integrity through
the call, retaining before/after bundle guards. Read-only SQL is not integrity
validation. Original-file checksums prove read bytes, not edited stored MAST,
slot, group-bound or candidate/winner context. These APIs do not independently
verify arbitrary edited databases; their context authority is the authenticated
producing bundle. Callers must consume/drop streamed versions to keep one-body
memory, and must reject partial delivery without a successful complete summary.
Both new commands
emit one version JSONL row at a time followed by a summary. The Rust streaming
callback exposes exact original 24-byte header and on-disk stored payload
(including compression prefix) with full original-file checksum guards before
and after reading through the same handle. It does not inflate, normalize,
repair flags, mask locals, or manufacture typed/runtime decoding. Complete
stored frames have a 64 MiB body access ceiling. A frame crossing its original
containing-group end yields header-only material even when later outer siblings
would satisfy its nominal size. Changed or unavailable originals remain local
access failures. Source queries have an explicit 4096-version capacity; callback
errors and exceeded query capacity return partial/error rather than claiming
complete access. The caller must retain the emitted partial report. Files are
not embedded or copied during conversion, so immutable originals are required
for future body access, and each queried version entails whole-original
checksum passes. The conversion retains header structs plus a sorted-reference
vector; this is not a disk-bounded all-candidate conversion design.

The report distinguishes the latest physical source candidate, latest
reader-admitted candidate, current accepted-source winner, runtime presence,
and surviving earlier runtime fallback. Deletions remain admitted history but
do not become a live source/runtime winner. Wide light locals remain proven
owner/full-local source keys with nullable runtime IDs and exact raw access;
they never spill or mask into an adjacent light slot. Generic required header
indices beyond the original MAST list use the existing supplier fallback for
runtime mapping and increment a separate bounded compatibility diagnostic.
Original ownership stays unresolved, and the full 24-bit local component is
retained in source history. Optional links keep their existing clear-or-reject
policy. Width and null-runtime checks still reject unusable candidates locally,
with their header/reason retained and valid neighbors continuing.
Full conversion atomically recreates only the owned
tooling table within the provenance transaction, so its older optional schema
does not silently survive with missing columns. There are no new sidecar files,
asset conversions or runtime schema changes. Normal asset-cache identity
invalidation remains an existing global adapter coupling for full conversion,
despite unchanged reusable asset encodings.

### 3. Worldspace Registry (`worldspaces`)

Stores worldspace hierarchy and parent world relations (e.g. Tamriel `0x0000003C`, Solstheim).

```sql
CREATE TABLE IF NOT EXISTS worldspaces (
    id INTEGER PRIMARY KEY,             -- WorldSpace FormID
    editor_id TEXT NOT NULL,            -- EDID string (e.g. 'Tamriel')
    parent_world INTEGER,               -- Parent WorldSpace FormID (if child worldspace)
    flags INTEGER NOT NULL,
    lod_origin_x INTEGER,               -- Validated sidecar/explicit origin; NULL if unresolved
    lod_origin_y INTEGER,
    full_name TEXT                      -- Display name (FULL, e.g. 'Skyrim'); see "Display names"
);
```

A `WRLD` without `EDID` gets no `worldspaces` row and no worldspace environment row. The converter
does not invent an editor id for it. The export reports it as `worldspace_missing_editor_id`.

---

### 4. Cell Registry (`cells`)

Stores exterior cell grid coordinates and each cell's Editor ID.

```sql
CREATE TABLE IF NOT EXISTS cells (
    id INTEGER PRIMARY KEY,             -- CELL FormID
    worldspace_id INTEGER,              -- Parent WorldSpace FormID; NULL for an interior
    grid_x INTEGER,                     -- Exterior cell Grid X (NULL if interior)
    grid_y INTEGER,                     -- Exterior cell Grid Y (NULL if interior)
    interior_name TEXT,                 -- The cell's EDID (Editor ID) when it has one, exterior or
                                        -- interior; not the display name
    flags INTEGER NOT NULL,
    data BLOB,                          -- Optional cell binary payload
    full_name TEXT                      -- Display name (FULL); see "Display names" below
);
```

#### Display names (`full_name`)

`worldspaces.full_name`, `cells.full_name` and `statics.full_name` hold the record's `FULL`, the
name the game shows (door prompts, the crosshair label, container menus, the map). `npcs.full_name`
follows the same rules. `cells.interior_name` is still the cell's Editor ID (`EDID`), not its
name; it keeps its column name for compatibility.

`FULL` is a localized string. In a plugin with the TES4 `Localized` flag (0x80) it is a four-byte
ID into `Strings/<plugin>_<language>.STRINGS`, looked up in the configured language's table for the
plugin that supplied the winning record: a loose `Data/Strings` file first, then the archives.
English text uses Windows-1252; other languages use UTF-8 with Windows-1252 fallback. Plain plugin
text uses Windows-1252. Both record readers publish the same UTF-8 text:

- The in-house reader resolves every field its schema types as `lstring`.
- The legacy reader (`--record-reader legacy`) resolves the subrecords the in-house schema types
  as `lstring` in every occurrence for that record type, using the same table lookup
  (`crates/converter/src/esm/legacy_text.rs`). That covers `FULL` on every record type, and
  `DESC`, `RNAM` and others where the subrecord is unambiguous. This changed the legacy
  projection; version 4 introduced language/encoding. Version 5 also publishes every schema-declared
  projectile/hazard placement and its base model (#498). Version 6 adds the STAT MODS alternate-texture
  table (`base_texture_swaps`). The in-house adapter is version 14.
  Version-3 retained proofs are accepted only for their historical English setting; all older
  metadata rebuilds use the configured language. Retained assets keep their independent proof.

A name whose ID is 0 is the existing null sentinel and is NULL without a loss warning. A nonzero ID
missing from the table, or a table that cannot be read, leaves NULL and adds one bounded warning
per plugin with a count and first example. The canonical `records.data` payloads hold the same
resolved text for both readers.

The columns are additive and optional. They did not bump `shared::WORLD_DATABASE_SCHEMA_VERSION`:
`create_tables` adds them to an older reused table, and a reader must probe for them.

`cells.flags` and `worldspaces.flags` are the record-header flags. The `DATA` flags (interior,
has water, show sky, ...) are in `cell_environment.data_flags` and
`worldspace_environment.data_flags` below.

#### Cell and Worldspace Environment (`cell_environment`, `cell_regions`, `cell_lighting`, `worldspace_environment`)

Authored lighting, image space, climate, music, ownership and flags (#269 gap 1). Before these
tables existed the data was only in `cells.data` / `records.data`, and the engine hard-coded
`SkyrimClear` lighting for every space (#264). Layouts follow xEdit 4.1.5f
(`Core/wbDefinitionsTES5.pas`, `CELL` and `WRLD`).

```sql
CREATE TABLE IF NOT EXISTS cell_environment (  -- one row per CELL
    cell_id INTEGER PRIMARY KEY,
    data_flags INTEGER,          -- DATA (u8 in old plugins, u16 in SSE): 0x1 interior, 0x2 has water,
                                 -- 0x4 can travel from here, 0x8 no LOD water, 0x20 public area,
                                 -- 0x40 hand changed, 0x80 show sky, 0x100 use sky lighting
    image_space_id INTEGER,      -- XCIM -> IMGS
    acoustic_space_id INTEGER,   -- XCAS -> ASPC
    music_id INTEGER,            -- XCMO -> MUSC
    sky_region_id INTEGER,       -- XCCM -> REGN the cell takes its sky and weather from
    location_id INTEGER,         -- XLCN -> LCTN
    encounter_zone_id INTEGER,   -- XEZN -> ECZN
    owner_id INTEGER,            -- XOWN -> FACT, NPC_ or ACHR
    owner_rank INTEGER,          -- XRNK, the faction rank that owns it
    water_id INTEGER,            -- XCWT -> WATR
    water_height REAL            -- XCLW; NULL when absent or not finite
);
CREATE TABLE IF NOT EXISTS cell_regions (      -- XCLR, one row per region
    cell_id INTEGER NOT NULL,
    region_id INTEGER NOT NULL,
    PRIMARY KEY (cell_id, region_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS cell_lighting (     -- one row per CELL with an XCLL or an LTMP
    cell_id INTEGER PRIMARY KEY,
    lighting_template_id INTEGER,              -- LTMP -> LGTM
    xcll_size INTEGER,                         -- 92, or 72 for the short form; NULL without XCLL
    ambient_r INTEGER, ambient_g INTEGER, ambient_b INTEGER,             -- XCLL[0..3]
    directional_r INTEGER, directional_g INTEGER, directional_b INTEGER, -- XCLL[4..7]
    fog_near_r INTEGER, fog_near_g INTEGER, fog_near_b INTEGER,          -- XCLL[8..11]
    fog_near REAL, fog_far REAL,                                         -- XCLL[12], [16]
    directional_rotation_xy INTEGER, directional_rotation_z INTEGER,     -- XCLL[20], [24], degrees
    directional_fade REAL, fog_clip_distance REAL, fog_power REAL,       -- XCLL[28], [32], [36]
    dalc_x_pos_r INTEGER, dalc_x_pos_g INTEGER, dalc_x_pos_b INTEGER,    -- XCLL[40..72]: directional
    dalc_x_neg_r INTEGER, dalc_x_neg_g INTEGER, dalc_x_neg_b INTEGER,    -- ambient X+, X-, Y+, Y-,
    dalc_y_pos_r INTEGER, dalc_y_pos_g INTEGER, dalc_y_pos_b INTEGER,    -- Z+, Z-, specular, scale
    dalc_y_neg_r INTEGER, dalc_y_neg_g INTEGER, dalc_y_neg_b INTEGER,
    dalc_z_pos_r INTEGER, dalc_z_pos_g INTEGER, dalc_z_pos_b INTEGER,
    dalc_z_neg_r INTEGER, dalc_z_neg_g INTEGER, dalc_z_neg_b INTEGER,
    dalc_specular_r INTEGER, dalc_specular_g INTEGER, dalc_specular_b INTEGER,
    dalc_scale REAL,
    fog_far_r INTEGER, fog_far_g INTEGER, fog_far_b INTEGER,             -- XCLL[72..75]
    fog_max REAL, light_fade_begin REAL, light_fade_end REAL,            -- XCLL[76], [80], [84]
    inherit_flags INTEGER                                                -- XCLL[88]
);
CREATE TABLE IF NOT EXISTS worldspace_environment ( -- one row per WRLD
    worldspace_id INTEGER PRIMARY KEY,
    data_flags INTEGER,          -- DATA: 0x1 small world, 0x2 can't fast travel, 0x8 no LOD water,
                                 -- 0x10 no landscape, 0x20 no sky, 0x40 fixed dimensions, 0x80 no grass
    parent_flags INTEGER,        -- PNAM byte 0: 0x1 use land, 0x2 LOD, 0x4 map, 0x8 water,
                                 -- 0x10 climate, 0x40 sky cell data from the parent (WNAM)
    climate_id INTEGER,          -- CNAM -> CLMT
    water_id INTEGER,            -- NAM2 -> WATR
    lod_water_id INTEGER,        -- NAM3 -> WATR
    lod_water_height REAL,       -- NAM4
    default_land_height REAL,    -- DNAM[0]
    default_water_height REAL,   -- DNAM[4]
    lighting_template_id INTEGER, -- LTMP -> LGTM (interior lighting)
    music_id INTEGER,            -- ZNAM -> MUSC
    location_id INTEGER,         -- XLCN -> LCTN
    encounter_zone_id INTEGER,   -- XEZN -> ECZN
    bounds_min_x REAL, bounds_min_y REAL, bounds_max_x REAL, bounds_max_y REAL, -- NAM0, NAM9
    lod_multiplier REAL,         -- NAMA
    center_cell_x INTEGER, center_cell_y INTEGER -- WCTR, fixed-dimensions centre cell
);
```

Rules:

- Colours are 8-bit sRGB; each one's fourth byte is unused and not stored. The `dalc_*` columns
  are the cell's directional ambient lighting colours (what `LGTM` and `WTHR` keep in a `DALC`).
- `XCLL` is 92 bytes in Skyrim SE. xEdit accepts it from 72 bytes, ending after the directional
  ambient block; the fields past the end are then NULL. An `XCLL` under 72 bytes is ignored.
  Floats that are not finite are NULL. `inherit_flags` says which fields the lighting template
  supplies instead: 0x1 ambient, 0x2 directional, 0x4 fog colour, 0x8 fog near, 0x10 fog far,
  0x20 directional rotation, 0x40 directional fade, 0x80 clip distance, 0x100 fog power,
  0x200 fog max, 0x400 light fade distances. The template is in `lighting_templates` below.
- Every link column holds a load-order FormID, and only when the winning record it names exists
  and has the type listed; otherwise it is NULL (a `cell_regions` row is skipped). The in-house
  reader already clears links that do not resolve and the legacy reader keeps them, so this check
  is what makes both readers publish the same rows.
- A typed field whose length the in-house schema rejects (for example a `DATA` that is neither 1
  nor 2 bytes, or an `XCLW` that is not 4) is NULL for both readers: the in-house reader leaves it
  out of its runtime projection, so the projection applies the same sizes to legacy records.
- The tables are derived data: a full load-order export recreates them from the winning records.
  The movement-annotation subset export leaves them alone, and diagnostic archives leave them
  empty (they are deferred runtime views, like the room tables).

#### Weather, Climate, Image Space and Lighting Templates (`weathers`, `weather_colors`, `weather_directional_ambient`, `climates`, `climate_weathers`, `region_weathers`, `image_spaces`, `lighting_templates`)

The records Skyrim's sky and interior lighting come from (#269 gap 2). Together with the tables
above, this is the data the engine needs to replace its hard-coded `SkyrimClear` day (#264).

```sql
CREATE TABLE IF NOT EXISTS weathers (          -- one row per WTHR
    id INTEGER PRIMARY KEY, editor_id TEXT,
    fog_day_near REAL, fog_day_far REAL, fog_night_near REAL, fog_night_far REAL,   -- FNAM (32 bytes)
    fog_day_power REAL, fog_night_power REAL, fog_day_max REAL, fog_night_max REAL,
    wind_speed INTEGER, transition_delta INTEGER, sun_glare INTEGER, sun_damage INTEGER, -- DATA (19 bytes),
    precipitation_begin INTEGER, precipitation_end INTEGER,                           -- raw u8 values
    lightning_begin INTEGER, lightning_end INTEGER, lightning_frequency INTEGER,
    classification_flags INTEGER,              -- DATA[11]: 0x1 pleasant, 0x2 cloudy, 0x4 rainy, 0x8 snow, ...
    lightning_r INTEGER, lightning_g INTEGER, lightning_b INTEGER,
    visual_effect_begin INTEGER, visual_effect_end INTEGER,
    wind_direction INTEGER, wind_direction_range INTEGER,
    disabled_cloud_layers INTEGER,             -- NAM1 bits
    precipitation_id INTEGER,                  -- MNAM -> SPGD
    visual_effect_id INTEGER,                  -- NNAM -> RFCT
    sky_static_id INTEGER,                     -- TNAM -> STAT
    sun_glare_lens_id INTEGER,                 -- GNAM -> LENS
    image_space_sunrise_id INTEGER, image_space_day_id INTEGER,              -- IMSP -> IMGS
    image_space_sunset_id INTEGER, image_space_night_id INTEGER,
    volumetric_lighting_sunrise_id INTEGER, volumetric_lighting_day_id INTEGER, -- HNAM -> VOLI
    volumetric_lighting_sunset_id INTEGER, volumetric_lighting_night_id INTEGER
);
CREATE TABLE IF NOT EXISTS weather_colors (    -- NAM0: one row per colour type and time of day
    weather_id INTEGER NOT NULL,
    color_type TEXT NOT NULL,                  -- sky_upper, fog_near, unknown, ambient, sunlight, sun,
                                               -- stars, sky_lower, horizon, effect_lighting,
                                               -- cloud_lod_diffuse, cloud_lod_ambient, fog_far,
                                               -- sky_statics, water_multiplier, sun_glare, moon_glare
    time_of_day TEXT NOT NULL,                 -- sunrise, day, sunset, night
    r INTEGER NOT NULL, g INTEGER NOT NULL, b INTEGER NOT NULL,
    PRIMARY KEY (weather_id, color_type, time_of_day)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS weather_directional_ambient ( -- the four DALC subrecords, in that order
    weather_id INTEGER NOT NULL, time_of_day TEXT NOT NULL,
    x_pos_r INTEGER, x_pos_g INTEGER, x_pos_b INTEGER, ...,   -- x_pos, x_neg, y_pos, y_neg, z_pos,
    specular_r INTEGER, specular_g INTEGER, specular_b INTEGER, -- z_neg, then specular and scale
    scale REAL,
    PRIMARY KEY (weather_id, time_of_day)
);
CREATE TABLE IF NOT EXISTS climates (          -- one row per CLMT
    id INTEGER PRIMARY KEY, editor_id TEXT, sun_texture TEXT, sun_glare_texture TEXT, -- FNAM, GNAM
    sunrise_begin INTEGER, sunrise_end INTEGER,   -- TNAM (6 bytes), in units of 10 minutes
    sunset_begin INTEGER, sunset_end INTEGER,
    volatility INTEGER, moon_phase INTEGER        -- TNAM[4], TNAM[5] (moons and phase length bits)
);
CREATE TABLE IF NOT EXISTS climate_weathers (  -- CLMT WLST, in file order
    climate_id INTEGER NOT NULL, position INTEGER NOT NULL,
    weather_id INTEGER,                        -- -> WTHR
    chance INTEGER NOT NULL,
    chance_global_id INTEGER,                  -- -> GLOB
    PRIMARY KEY (climate_id, position)
);
CREATE TABLE IF NOT EXISTS region_weathers (   -- REGN RDWT, in file order across the record
    region_id INTEGER NOT NULL, position INTEGER NOT NULL,
    weather_id INTEGER, chance INTEGER NOT NULL, chance_global_id INTEGER,
    PRIMARY KEY (region_id, position)
);
CREATE TABLE IF NOT EXISTS image_spaces (      -- one row per IMGS
    id INTEGER PRIMARY KEY, editor_id TEXT,
    eye_adaptation_speed REAL, bloom_blur_radius REAL, bloom_threshold REAL, bloom_scale REAL, -- HNAM
    receive_bloom_threshold REAL, white REAL, sunlight_scale REAL, sky_scale REAL,              -- (36 bytes)
    eye_adaptation_strength REAL,
    saturation REAL, brightness REAL, contrast REAL,       -- CNAM
    tint_amount REAL, tint_r REAL, tint_g REAL, tint_b REAL, -- TNAM, colour as 0..1 floats
    dof_strength REAL, dof_distance REAL, dof_range REAL,  -- DNAM (12 or 16 bytes)
    dof_sky_blur_radius INTEGER                            -- DNAM[14], raw u16 code
);
CREATE TABLE IF NOT EXISTS lighting_templates ( -- one row per LGTM
    id INTEGER PRIMARY KEY, editor_id TEXT,
    data_size INTEGER,                          -- 64, 68, 72, 76, 80, 88 or 92; NULL otherwise
    ...,                                        -- DATA: the same columns as cell_lighting, from
                                                -- ambient_r to light_fade_end (DATA[88..92] unknown)
    subrecord_dalc_x_pos_r INTEGER, ...,        -- the separate DALC subrecord, laid out like
    subrecord_dalc_scale REAL                   -- weather_directional_ambient
);
```

Rules beyond the ones above:

- `weather_colors` has a row for each colour type `NAM0` holds: 13 to 17 types of 16 bytes (sunrise,
  day, sunset, night; each RGB and an unused byte). A `NAM0` of another length gives no rows.
- A `DALC` is 24, 28 or 32 bytes (the specular colour and the scale are optional); missing fields
  are NULL.
- Links follow the same existence and type check as above. `CLMT` `WLST` and `WTHR` `IMSP`, `HNAM`,
  `MNAM`, `NNAM`, `TNAM`, `GNAM` and `SNAM` are now remapped by the legacy reader too, as the
  in-house schema already did (see the remapped-field table).

**These tables are optional and did not bump `shared::WORLD_DATABASE_SCHEMA_VERSION`.**
`create_tables` adds them to an older reused database. The engine reads them into
`world::environment::EnvironmentCatalog` (cell lighting, worldspace environment, lighting
templates, weathers with their colours and directional ambient, image spaces, climate weather
lists). A table that is missing or lacks a column the reader needs loads as empty, with one log
line. The clock sun reads climate timing from it, and interiors in sky modes 1 and 2 take their
key light, ambient and fog from `cell_lighting`, `lighting_templates`, `image_spaces` and
`room_bounds` (`docs/specs/engine/interior-environment.md`).

| Case | Converter writes | Engine |
|---|---|---|
| Old database, tables absent | n/a | empty catalog, one log line, built-in lighting |
| Table of another shape | n/a | empty catalog for that table, one log line |
| Cell without XCLL or LTMP | `cell_environment` row only | no authored lighting for it |
| Link to a missing or wrong-type record | NULL | n/a |

---

Missing or unsupported-size `LGTM.DATA` already leaves the DATA lighting columns NULL.
The `lighting_template_unusable_data` omission now reports this loss (section 19);
`editor_id`, identity and an independently valid `DALC` remain available. Accepted sizes
come from the documented xEdit layouts, not guessed retail defaults.

### 5. Placed World References (`references`)

Stores 3D positions, rotations, scales, and cell parentage for placed world objects (`REFR`,
`ACHR`, `ACRE`, `PGRE`, `PMIS`, `PHZD`, `PARW`, `PBAR`, `PBEA`, `PCON`, `PFLA`).

```sql
CREATE TABLE IF NOT EXISTS "references" (
    id INTEGER PRIMARY KEY,
    cell_id INTEGER NOT NULL,           -- Parent CELL FormID
    worldspace_id INTEGER,              -- Parent WorldSpace FormID; NULL in an interior
    base_form_id INTEGER NOT NULL,      -- Base Object FormID (NAME)
    is_exterior INTEGER NOT NULL,       -- 1 when the parent cell has a worldspace or a grid
    pos_x REAL NOT NULL,                -- 3D X Coordinate
    pos_y REAL NOT NULL,                -- 3D Y Coordinate
    pos_z REAL NOT NULL,                -- 3D Z Coordinate
    local_x REAL,                       -- Exterior only: pos_x within its 4096-unit grid cell
    local_y REAL,                       -- (from the position, not the owning CELL); NULL inside
    rot_x REAL NOT NULL,                -- Rotation X (Radians)
    rot_y REAL NOT NULL,                -- Rotation Y (Radians)
    rot_z REAL NOT NULL,                -- Rotation Z (Radians)
    scale REAL NOT NULL DEFAULT 1.0,    -- Scale multiplier
    radius_override REAL,               -- signed XRDS radius offset in Creation units (NULL when absent/invalid)
    header_flags INTEGER NOT NULL DEFAULT 0, -- Winning record header flags, stored raw
    enable_parent_id INTEGER,           -- Remapped XESP parent FormID; NULL when absent
    enable_parent_flags INTEGER,        -- XESP byte 4 (0 through 255); NULL when absent
    emittance_id INTEGER,               -- Remapped XEMI light or region FormID; NULL when absent
    data BLOB                           -- Subrecords payload
);

-- Ordinary SQLite index used for interior cell reference lookup
CREATE INDEX IF NOT EXISTS idx_references_cell ON "references"(cell_id);
```

A placed record needs the leading 24 bytes of `DATA` for position and rotation, as
[xEdit xedit-4.1.5f's placed-record definition](https://github.com/TES5Edit/TES5Edit/blob/xedit-4.1.5f/Core/wbDefinitionsTES5.pas#L5320-L5323)
uses `wbDATAPosRot`. Missing or short `DATA` keeps the canonical `records` row but
omits the placement, spatial row and derived reference links, reporting
`reference_unusable_data` (section 19). The required transform columns are `NOT NULL`;
the converter does not invent an origin or partial rotation. A subset update with
unusable `DATA` also removes any prior placement and spatial row, owned link/detail rows,
door transitions involving it, and related room rows. Room graph publication excludes those
unusable placements before projecting markers or memberships; it cannot recreate an origin
from their canonical bytes. Incoming authored links may still name the retained canonical record.

The same omission applies to the other required placement fields (#811, section 19). A
placed record with no four-byte `NAME`, with an `XSCL` that is present but shorter than four
bytes, or with no parent cell gets no placement row and no spatial row. The converter stores
no base form 0, no scale 1.0 and no cell 0 for it. An absent `XSCL` is different: it is an
authored scale of 1.0 and stays so. Each omission is reported as `reference_missing_base`,
`reference_short_scale` or `reference_missing_cell`, with the form ID and the field.

The typed record reader can retain a first definition without `NAME` as a target for
package and faction links. Runtime plugin export requires exactly one nonzero base `NAME`.
It omits the record, its `references` and `exterior_spatial` rows, and resolves incoming
typed links as it would for an absent target.

The reference flag and enable-parent columns preserve winning raw-record
metadata for initial spawn filtering (#165) and later object LOD (#106).
The engine resolves parent chains before spawning. With no parent, header bit
`0x800` marks an initially disabled reference; the documented rule and
runtime limits are in [reference enable-state evidence](../../research/reference-enable-state-20261005.md).
Current terrain LOD does not consume them. `header_flags` comes from the record header. `enable_parent_id` is
XESP's little-endian parent FormID (bytes 0-3), resolved through plugin load order;
`enable_parent_flags` is byte 4 alone in both readers. Bit 0 inverts the parent's
state and bit 1 enables pop-in. The three unused bytes (5-7) remain unchanged
in the canonical subrecords serialized in `data`. Older legacy projections could
include padding in the normalized flags value; reconversion corrects that
projection. An invalid parent link is published as
`enable_parent_id = 0` (no parent) with its flags kept; a malformed `XESP` is
dropped, so both columns are NULL. See the remapped-field table below.

`emittance_id` is the reference's `XEMI`: the light or region whose colour the
`External_Emittance` surfaces of its model take (§16). A cleared link is stored as NULL.

#### Base object models (`statics`)

`references.base_form_id` joins `statics.id`. The runtime spawns a reference only when its base
has a `statics` row whose `model_path` it can resolve, so a base type missing from this table
never renders.

```sql
CREATE TABLE IF NOT EXISTS statics (
    id INTEGER PRIMARY KEY,                    -- base record FormID, load-order numbering
    editor_id TEXT,                            -- EDID
    model_path TEXT,                           -- MODL (ARMO: MOD2, then MOD4); NULL when absent
    flags INTEGER NOT NULL,                    -- record header flags
    full_name TEXT,                            -- FULL, resolved localized text when available
    bounds_min_x REAL NOT NULL DEFAULT -64, bounds_min_y REAL NOT NULL DEFAULT -64,
    bounds_min_z REAL NOT NULL DEFAULT -64, bounds_max_x REAL NOT NULL DEFAULT 64,
    bounds_max_y REAL NOT NULL DEFAULT 64, bounds_max_z REAL NOT NULL DEFAULT 64,
    bounds_valid INTEGER NOT NULL DEFAULT 0    -- 1 once integration measured the converted GLB
);
```

Rows come from every placeable base type with a world model: `STAT`, `MSTT`, `FURN`, `TREE`,
`FLOR`, `CONT`, `DOOR`, `ACTI`, `TACT`, `WEAP`, `MISC`, `BOOK`, `AMMO`, `ALCH`, `INGR`, `SLGM`,
`KEYM`, `SCRL`, `ARMO`, `APPA`, `ADDN`, `ARTO`, `PROJ`, `HAZD`, and `LIGH` when it has a model. `TACT`
(talking activators), `APPA` (apparatus), `ADDN` (addon nodes) and `ARTO` (art objects) were
added for #269; before that their references had no row and did not render. The engine still
skips models under `effects/`, `sky/` or `markers/`, with one drawable activator exception for
`effects/fxcobwebroundwall01.nif`
(`converted_model_path` in `crates/engine/src/streaming.rs`), which covers many `ADDN` and `ARTO`
models; that is render policy, not data the table lacks. Marker filenames alone are not excluded:
the base record's `Is Marker` flag controls editor-marker admission. They are additive
rows, so no `shared::WORLD_DATABASE_SCHEMA_VERSION` bump: an older database keeps loading and
simply lacks them until reconverted. Mesh conversion discovers NIF inputs independently of this
table, so their models were already converted. Both record readers produce the same rows.

#### Reference Links and Details (`reference_linked_refs`, `reference_activate_parents`, `reference_details`)

What a placed reference is linked to, what activates it, who owns it, how it is locked and
whether it is a map marker (#269 gap 3). Patrols, idle markers, traps, levers, pressure plates,
stealing and locked doors need these; before they were only in `references.data`. Written for
every exported placed reference (the eleven source types listed above). Layouts follow xEdit
4.1.5f (`Core/wbDefinitionsTES5.pas`, `REFR`).

```sql
CREATE TABLE IF NOT EXISTS reference_linked_refs (       -- XLKR, one row per subrecord in file order
    reference_id INTEGER NOT NULL,                       -- references.id
    position INTEGER NOT NULL,
    keyword_id INTEGER,                                  -- -> KYWD; NULL for the 4-byte form
    linked_ref_id INTEGER,                               -- -> a placed reference
    PRIMARY KEY (reference_id, position)
);
CREATE TABLE IF NOT EXISTS reference_activate_parents (  -- XAPR, one row per subrecord
    reference_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    parent_ref_id INTEGER,                               -- -> a placed reference
    delay REAL,                                          -- seconds; NULL if not finite
    PRIMARY KEY (reference_id, position)
);
CREATE TABLE IF NOT EXISTS reference_details (           -- one row per reference with any of these
    reference_id INTEGER PRIMARY KEY,
    owner_id INTEGER,                    -- XOWN -> FACT, NPC_ or ACHR
    owner_rank INTEGER,                  -- XRNK, the faction rank that owns it
    activate_parent_flags INTEGER,       -- XAPD: 0x1 parent activate only
    lock_level INTEGER,                  -- XLOC[0]: 1 novice, 25 apprentice, 50 adept, 75 expert,
                                         -- 100 master, 255 requires key
    lock_key_id INTEGER,                 -- XLOC[4..8] -> KEYM, remapped here (see below)
    lock_flags INTEGER,                  -- XLOC[8]: 0x4 leveled lock
    open_by_default INTEGER NOT NULL DEFAULT 0, -- ONAM present (doors)
    map_marker INTEGER NOT NULL DEFAULT 0,      -- XMRK present
    map_marker_flags INTEGER,            -- FNAM: 0x1 visible, 0x2 can travel to, 0x4 "show all" hidden
    map_marker_type INTEGER,             -- TNAM[0], the marker icon enum
    location_id INTEGER,                 -- XLCN -> LCTN (persistent location)
    location_ref_type_id INTEGER,        -- XLRL -> LCRT or LCTN
    encounter_zone_id INTEGER,           -- XEZN -> ECZN
    attached_ref_id INTEGER              -- XATR -> a placed reference
);
```

Rules:

- These detail rows are projected from every winning placed source type listed in §5.
  Target types are field-specific and are not the same set as source types:
  `XLKR.keyword_id` accepts `KYWD`, `PLYR`, `ACHR`, `REFR`, `PGRE`, `PHZD`, `PMIS`, `PARW`,
  `PBAR`, `PBEA`, `PCON` and `PFLA`; `XLKR.linked_ref_id` and `XAPR.parent_ref_id` accept
  `PLYR`, `ACHR`, `REFR`, `PGRE`, `PHZD`, `PMIS`, `PARW`, `PBAR`, `PBEA`, `PCON` and `PFLA`.
  `XATR.attached_ref_id` accepts `REFR`, `PGRE`, `PHZD`, `PMIS`, `PARW`, `PBAR`, `PBEA`, `PCON`
  and `PFLA` (not `ACHR` or `ACRE`). `owner_id` accepts `FACT`, `ACHR` or `NPC_`; `lock_key_id`
  accepts `KEYM`; `XLCN` accepts `LCTN`; `XLRL` accepts `LCRT` or `LCTN`; and `XEZN` accepts
  `ECZN`. A zero ID resolves to NULL. A nonzero hardcoded ID below `0x800` is retained if no
  plugin record exists and no accepted deletion removed it; a present winner must be non-deleted and have an allowed type. Other
  targets resolve to NULL. Valid-length `XLKR` and `XAPR` rows are kept in source order with
  contiguous positions among accepted-length entries, even when a target resolves to NULL. This
  makes legacy projection match the in-house reader's invalid-link clearing behavior.
- Subset exports refresh incoming `XLKR`, `XAPR`, `XATR` and `XOWN` links when a target is
  added, deleted, restored or changes record type. Full runtime exports index authored
  target IDs, source IDs and slot positions in `reference_authored_targets`, including
  sources with no detail row. The first restoration on an older pack builds the index in
  its edit transaction. Later edits reread changed sources and look up incoming slots by
  target ID. Unrelated columns, including owner rank, retain their values.
  `reference_authored_changes(reference_id INTEGER PRIMARY KEY)` queues source IDs for
  reindexing. Placement updates add IDs explicitly or through the trigger on updates to
  `references.data`. The refresh clears the queue in the same transaction.
  Placement layers also refresh affected links in the same transaction.
  For an unedited in-house source, the converter uses the original plugin payload and master
  list to recover authored IDs that full conversion cleared. A slot stays NULL if surviving
  occurrences cannot be matched unambiguously. `reference_link_subset_sources` records
  which source a subset replaced and retains recoverable link fields for typed subsets.
  Typed subset edits also resolve these recovered fields against targets already present in the pack.
  A later placement-layer edit removes that entry and supplies the effective source.
  The provenance table has columns `form_id INTEGER PRIMARY KEY` and `data BLOB`. A subset or
  placement edit upgrades older FormID-only tables in the edit transaction. Existing rows keep
  their source markers and have NULL link data. A full export drops this edit history.
- The exporter stores the FormID of each deleted non-placement record it receives in
  `subset_nonplacement_deletions(form_id INTEGER PRIMARY KEY)`. A later subset uses the table
  to recognize deleted targets whose canonical records still exist. A live subset replacement
  clears the FormID's deletion entry. Full plugin exports also store accepted deletions that
  have no surviving winner, including placements absent from the canonical archive.
- A field whose length the in-house schema rejects is skipped for both readers: `XLKR` must be 4
  or 8 bytes, `XAPR` 8, `XAPD` 1, `XOWN`/`XRNK`/`XLCN`/`XLRL`/`XEZN`/`XATR` 4, `FNAM` 1 and `TNAM`
  2.
- `XLOC` is untyped bytes in the in-house schema, so neither reader remaps its key in
  `references.data`. The projection remaps the key through the supplying plugin's master list
  (`records::source_identity::header_runtime_id`) and applies the normal `KEYM` target check,
  including retention of a nonzero absent hardcoded ID below `0x800`. A subset export without a
  load order leaves `lock_key_id` NULL.
- The map marker's name (`FULL`, a localized string) is not projected into these tables yet.
  Both readers retain its resolved text in the canonical record payload after #271.
- Derived data like the other runtime views: a full load-order export recreates the tables, and
  diagnostic archives leave them empty.

**These tables are optional and did not bump `shared::WORLD_DATABASE_SCHEMA_VERSION`.**
`create_tables` adds them to an older reused database. No engine code reads them yet; a reader
must probe for them first.


---

### 6. Hybrid Spatial Indexing (`exterior_spatial` & Interior `cell_id` Index)

Exterior references use a spatial index with a worldspace filter; interior references use direct cell lookup:

1. **Exterior Worldspace R-Tree (`exterior_spatial`):** Bounds are the reference's global X/Y/Z point position, with equal minimum and maximum values. The exporter does not constrain coordinate magnitudes. SQLite stores single-precision bounds; `cell_id` and `worldspace_id` are auxiliary columns, not spatial dimensions. Cell-local X/Y offsets remain separate columns in `references`.
2. **Interior Cell Index (`idx_references_cell`):** Interior cells do not use the R-Tree. An ordinary SQLite index on `references(cell_id)` supports the engine's interior reference selection; this description makes no latency or complexity claim.

```sql
-- R-Tree virtual table for Exterior 3D bounding box spatial queries
CREATE VIRTUAL TABLE IF NOT EXISTS exterior_spatial USING rtree(
    id,                                 -- Matches internal reference ID
    minX, maxX,                         -- Global reference X position
    minY, maxY,                         -- Global reference Y position
    minZ, maxZ,                         -- World Z Height units
    +cell_id,                           -- Exterior CELL FormID
    +worldspace_id                      -- Parent WorldSpace FormID (e.g. 0x0000003C for Tamriel)
);
```

---

### 7. Terrain Heightmaps (`land`)

Stores 33x33 terrain heightmap data, vertex textures (`vtex`), vertex colors (`vclr`) and
vertex normals extracted from the winning `LAND` record of each cell.

```sql
CREATE TABLE IF NOT EXISTS land (
    cell_id INTEGER PRIMARY KEY,        -- Parent CELL FormID
    heightmap BLOB NOT NULL,            -- VHGT: 33x33 height deltas
    vtex BLOB,                          -- VTEX: land texture FormIDs
    vclr BLOB,                          -- VCLR: vertex colors
    normals BLOB                        -- VNML: vertex normals
);
```

A `LAND` without a `VHGT` field (or with an empty one) gets no `land` row, and the terrain cell
cache and merged terrain mesh publish no cell for it. The converter stores no empty blob and no
zero heights (#811). The export reports it as `land_missing_heights`; the cell cache and the mesh
builder print a counted warning and skip the cell. Retail's behaviour for a height-less `LAND` is
unverified. `vtex`, `vclr` and `normals` are `NULL` when the record has no `VTEX`, `VCLR` or `VNML`
(earlier output stored an empty blob). Missing or zero `VNML` vectors are still rebuilt from the
authored heights in the cell cache: that derivation is not default fill, and the retail rule for a
zero vector is unverified (open item on #811).

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

#### Object LOD chunks (`lod_object_chunks`, `lod_object_chunks_spatial`)

Additive tables for retail object LOD, one row per retail `.bto` block of level 16, 8 or 4 (tier 16, 8 or 4). They have the columns,
primary key and R-tree of `lod_chunks` and `lod_chunks_spatial`, and are created only when the
first object chunk is published, so a pack without object LOD has no extra table and its database
is unchanged. `payload_path` is `lod/<world>/<tier>/objects_<x>_<y>.glb`, a file beside the terrain
chunk of the same key. The rows are checked against the same `lod_build` identity as terrain rows
and do not change it. `WORLD_DATABASE_SCHEMA_VERSION` is unchanged.

`lod-manifest.json` gains three optional fields, absent for a pack without object LOD:
`object_chunks` (row count), `objects_compiler_version` and `object_chunk_inputs` (payload path to
the SHA-256 of the compiler revision, key, LOD origin, `.bto` bytes and atlas bytes). Package
validation fails when a listed chunk is missing, has a different SHA-256, or is not a valid object
chunk GLB (see [lod-compiler.md](lod-compiler.md#object-lod-chunks)).

The additive table `lod_object_atlases(worldspace_id PRIMARY KEY, payload_path, content_hash)` lists
the shared atlas of a worldspace (`lod/<world>/objects_atlas.opensky-srgb.ktx2`, KTX2, SHA-256 of the file) that
its object chunks reference. A worldspace without a row has chunks that embed the atlas. The manifest
field `object_atlases` maps each atlas path to its SHA-256.

#### Large-reference membership (`large_references`)

This additive projection stores the defining plugin's filtered `WRLD.RNAM` membership.
The importer remaps each FormID in the supplying plugin's load order and accepts tuples whose
header and placement cell keys match. It omits and counts malformed or unresolved input.
The cell coordinates describe the placement bucket, including when the reference's parent
cell differs. See [reviewed retail facts, G01](https://github.com/Mudcrab-Team/skyrim_decomp/pull/821/files).

```sql
CREATE TABLE IF NOT EXISTS large_references (
    worldspace_id INTEGER NOT NULL,
    grid_x INTEGER NOT NULL,
    grid_y INTEGER NOT NULL,
    reference_id INTEGER NOT NULL,
    PRIMARY KEY (worldspace_id, grid_x, grid_y, reference_id)
);
```

The engine uses the first three columns to find exterior buckets. It checks that the winning
reference has a placement in the matching exterior worldspace and bucket.

`large_reference_omissions` records incomplete input. A row covers one bucket when both
coordinates are present, or the whole worldspace when both are NULL. The count covers rejected
RNAM subrecords, unresolved FormIDs, and members without a projected reference or worldspace.
The separate `confirmed_removed_count` records membership entries with a winning deleted REFR.
These entries have a known outcome and do not make their bucket incomplete.

```sql
CREATE TABLE IF NOT EXISTS large_reference_omissions (
    worldspace_id INTEGER NOT NULL,
    grid_x INTEGER,
    grid_y INTEGER,
    omitted_count INTEGER NOT NULL CHECK(omitted_count >= 0),
    confirmed_removed_count INTEGER NOT NULL CHECK(confirmed_removed_count >= 0),
    CHECK(omitted_count > 0 OR confirmed_removed_count > 0),
    CHECK(confirmed_removed_count = 0 OR grid_x IS NOT NULL),
    CHECK((grid_x IS NULL) = (grid_y IS NULL))
);
```

The engine requires both tables for promotion and preserves marked object LOD for incomplete
buckets. The schema version is unchanged.

---

### 9. Script attachments (`scripts`)

In-house output stores attached Papyrus script names and canonical VMAD bytes.
These are property bindings, not compiled PEX bytecode. Script status and
authored order remain in VMAD. The last exact-name attachment for a record keeps
the existing SQL overwrite behavior.

```sql
CREATE TABLE scripts (
    form_id INTEGER NOT NULL,
    script_name TEXT NOT NULL,
    vmad BLOB NOT NULL,
    PRIMARY KEY (form_id, script_name)
);
```

`script_properties` has `(form_id,script_name,property_index)` as its primary key,
with scalar property name, native `property_type`, status and `value_count`.
`script_property_values` adds a zero-based `value_index` to that key and stores
nullable `object_form_id`, `alias_id`, `string_value`, `integer_value`,
`float_bits` and `boolean_value` columns. Scalar properties have one value row;
arrays keep their authored element order. Native type zero and empty arrays
have no value rows and remain distinguishable by their property headers. Float
bits preserve all f32 representations without JSON or SQLite numeric coercion.

These tables replace `scripts.properties_json` for in-house exports inside the
existing record transaction. Complete exports refresh their owned script
snapshot; record archives leave all three runtime script tables empty. The
legacy reader's existing JSON projection remains separate. Runtime engine
consumers continue using the canonical record/VMAD data. Public partial exports
into a normalized database use scalar script rows without refreshing unrelated
attachments; fresh legacy databases retain the legacy JSON projection.

---

### 10. FormID Translation Map (`formid_map`)

Maps each resolved 32-bit FormID to its stable owning plugin and plugin-local
ID. `plugin_name` is the lowercase owning filename, such as `skyrim.esm`;
`internal_id` is the low 24 bits for full plugins or the low 12 bits for light
plugins. Full and light slots are assigned independently. The pair
`(plugin_name, internal_id)` survives changes to the load-order slots.
`plugin_name` uses the reader's shared Unicode lowercase filename key; join to
`plugins` by applying that same normalization to `plugins.name`. SQLite's
built-in `lower` alone does not normalize accented Windows-1252 case pairs.

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
| `XMBR` | placed references | multibound target (one 4-byte FormID) |
| `XATR` | placed references | attached reference (the in-house reader already remapped it; the legacy reader does since #269) |
| `SNAM`, `LNAM` | LIGH | sound and lens flare (the in-house reader already remapped them; the legacy reader does since #269) |
| `XAPR` | placed references | activate-parent reference (one 8-byte subrecord per parent) |
| `XLRT` | placed references | location reference types (array) |
| `XHOR` | ACHR | horse reference |
| `LTMP` | CELL, WRLD | lighting template |
| `XCIM`, `XCMO`, `XCAS` | CELL | image space, music type, acoustic space |
| `XCCM` | CELL | region the cell takes its sky and weather from |
| `XCLR` | CELL | regions (array) |
| `ZNAM` | WRLD | music (the in-house reader already remapped it; the legacy reader does since #269) |
| `RDWT` | REGN | weathers: (weather, chance, global) entries of 12 bytes; weather and global are FormIDs |
| `WLST` | CLMT | weathers, laid out like `RDWT` |
| `MNAM`, `NNAM`, `TNAM`, `GNAM` | WTHR | precipitation, visual effect, sky static, sun glare lens |
| `IMSP`, `HNAM` | WTHR | image spaces and volumetric lighting at sunrise, day, sunset, night (16 bytes) |
| `SNAM` | WTHR | sound descriptor (bytes 0-3 of 8; one subrecord per sound) |
| `XPOD` | REFR | the rooms a portal marker joins: an array of (origin, destination) pairs; every u32 a FormID (xEdit 4.1.5f defs) |
| `XLRM` | REFR | linked room of a room marker |
| `LNAM`, `INAM` | REFR | room marker lighting template, image space (other record types keep their own handling) |

For blob remapping, placed references are REFR, ACHR, ACRE, PGRE, PMIS, PHZD,
PARW, PBAR, PBEA, PCON and PFLA. This does not expand the record types exported
to the `references` table; the six newly covered types retain their blobs in
`records` without adding `references` rows.
For `XMBR`, the legacy reader remaps the four-byte word for every placed type listed above.
The in-house reader emits a runtime FormID when the record schema declares `XMBR` as a FormID.
With the built-in `ACRE` schema, `records.data` can retain opaque `XMBR` bytes,
including a source master index outside the plugin’s master list. The in-house source archive
(`inhouse_source_records.payload`) keeps the original plugin bytes.
Fields not covered may still hold plugin-local FormIDs. In particular, do not
consume `WRLD.RNAM` large-reference lists, `XLOC` lock keys (use `reference_details.lock_key_id`), `XPWR` water
reflections as resolved IDs. Other
unconverted fields include `XLIB`, `XTNM`, `PDTO` and
`CELL.XILL`. Check the converter's field-specific handling before using
these payloads; this list is not exhaustive.

When a reader remaps a field in this table, it validates master indices and light-plugin local IDs.
An out-of-range index or light-plugin local ID wider than 12 bits is an invalid
optional link: the converter sets that FormID to zero and reports a warning,
aggregated per source plugin with the count and first record/field diagnostic.
Other records and valid links continue to convert. The legacy reader warns about malformed
field lengths, including arrays with an incomplete entry. It normally drops the affected
occurrence and retains valid siblings. It keeps an empty payload for malformed placed `XMBR`,
`REFR.XLRM`, `REFR.XPOD`, `REFR.XORD`, and occluder `XESP` fields so room projection can
recognize an unusable authored value. An empty `XMBR` yields `unknown` assignment. An absent
`XMBR` can allow automatic assignment. Existing
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

A live sourced placement must match the record type already stored for its FormID.
A sourced placement deletion checks the owning plugin, local ID and record type
against the pack before removing the placement and its scripts. The owning
identity survives deletion. For a low FormID without a stored identity, a subset
records the deletion in `subset_placement_deletions(form_id INTEGER PRIMARY KEY)`.
Later subsets leave links to these deleted placements null. A live replacement
restores the link target, and a full conversion rebuilds the snapshot from its
source plugins.

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

A reference that places a light usually carries a signed `XRDS` radius offset, stored in
`references.radius_override` (10,810 of the 12,148 `LIGH` references in
`Skyrim.esm`). Add this signed offset to the base radius for that placement.

```sql
CREATE TABLE IF NOT EXISTS lights (
    id INTEGER PRIMARY KEY,        -- LIGH FormID
    editor_id TEXT,
    radius REAL NOT NULL,          -- Creation units (DATA u32)
    color_r INTEGER NOT NULL, color_g INTEGER NOT NULL, color_b INTEGER NOT NULL,
    flags INTEGER NOT NULL,        -- DATA flags (dynamic, can carry, negative, flicker, off by default, ...)
    falloff REAL NOT NULL,
    fade REAL,                     -- FNAM, if present
    time INTEGER,                  -- DATA[0] i32, the light's duration
    field_of_view REAL,            -- DATA[20], spotlight cone
    near_clip REAL,                -- DATA[24]
    flicker_period REAL,           -- DATA[28], raw (xEdit shows it multiplied by 0.01)
    flicker_intensity_amplitude REAL, -- DATA[32]
    flicker_movement_amplitude REAL,  -- DATA[36]
    value INTEGER,                 -- DATA[40] u32
    weight REAL,                   -- DATA[44]
    sound_id INTEGER,              -- SNAM -> SNDR
    lens_id INTEGER                -- LNAM -> LENS (lens flare)
);
```

The columns from `time` on were added for #269 gap 6. They come from the full 48-byte `DATA`
(xEdit 4.1.5f `wbDefinitionsTES5.pas`, `LIGH`); the in-house schema accepts only that length, so
a shorter `DATA` leaves them NULL for both readers, and floats that are not finite are NULL.
`sound_id` and `lens_id` keep a FormID only when the winning record exists with that type (the
in-house reader clears links that do not resolve and the legacy reader keeps them), and the legacy
reader now remaps `LIGH` `SNAM`/`LNAM` as the in-house schema already did. The columns are
additive and optional, with no `shared::WORLD_DATABASE_SCHEMA_VERSION` bump: `create_tables` adds
them to an older reused `lights` table. The engine reads radius, colour, flags, `fade`, `falloff` and `fade_status`.

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
DefaultWater values (`render::DEFAULT_WATER_FRESNEL`, `DEFAULT_WATER_REFLECTIVITY`,
`DEFAULT_WATER_DEEP_COLOR`, `DEFAULT_WATER_SHALLOW_COLOR` and `DEFAULT_WATER_OPACITY`) until a
reconversion populates them. Adding them did **not** bump
`shared::WORLD_DATABASE_SCHEMA_VERSION`: nothing that already reads `waters` depends on their
presence, and the fallback exists specifically so a reconversion is not required. `opacity` is
not one of those additions: it has been in the table since the table was first created.

The above-water fog (near and far distance, amount) has **no** column. The engine reads it per
water from the `DNAM` subrecord inside `data` (`DNAM+32`, `DNAM+36` and `DNAM+132`, three
little-endian `f32`s, see [water research](../../research/water.md)), so every existing database
yields each water's own fog without a reconversion. A water whose `data` does not decode, whose
`DNAM` is shorter than 136 bytes, or whose fog is not finite uses Skyrim's DefaultWater fog
(near -10, far 150, amount 0.7; `WaterFog::default()` in `crates/engine/src/world/database.rs`)
and keeps its colours.

---

### 14. Grass Definitions and Landscape Associations (`grass_types`, `landscape_texture_grasses`)

`grass_types` projects each effective `GRAS` record, including its canonical converted
model path (`MODL` through the existing mesh asset-path mapping). Grass models use the
same NIF-to-GLB converter as other meshes. `flags` below contains the `DATA` grass
flags rather than record header flags. `load_order` identifies the winning
override; `formid_map` retains owning-plugin identity independently.

The layout follows [xEdit's TES5 GRAS and LTEX definitions at xedit-4.1.5f](https://github.com/TES5Edit/TES5Edit/blob/xedit-4.1.5f/Core/wbDefinitionsTES5.pas#L10199-L10230)
and [CommonLibSSE-NG's TESGrass definition](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/main/include/RE/T/TESGrass.h).
`DATA` is 32 bytes; its padding and unprojected fields stay in `records.data`.
The converter preserves authored values without clamping to vanilla ranges. Missing or
short `DATA` records `grass_unusable_data` (section 19); each complete scalar at its fixed
offset is retained, while absent or incomplete scalar spans become NULL. For example,
three bytes preserve density and both slope values without inventing the remaining fields.
The in-house adapter recovers this unique, safely framed numeric prefix after typed-field
rejection; it does not mark the malformed typed field as decoded. The identity and model
still publish. A non-finite float becomes NULL. Missing, empty or unsafe
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
    ordinal INTEGER NOT NULL DEFAULT 0, -- first GNAM occurrence in the winning list
    PRIMARY KEY (ltex_id, gras_id)
);
```

`water_comparison` values 0 through 7 mean Above At Least, Above At Most,
Below At Least, Below At Most, Either At Least, Either At Most,
Either At Most Above and Either At Most Below, respectively. `flags` bits 0,
1 and 2 mean Vertex Lighting, Uniform Scaling and Fit to Slope. These definitions
establish the authored contract, not Skyrim's exact placement equations.

Each non-null repeated `LTEX.GNAM` yields an association; duplicate pairs collapse,
retaining the first occurrence's zero-based `ordinal`. Both record readers use this
projection. Consumers read each texture's species in `ordinal` order (with `gras_id`
as a deterministic tie fallback), preserving source order rather than sorting species
by FormID. Gaps after discarded or duplicate occurrences are allowed.
The winning LTEX list replaces the earlier list in full, including its order and an
empty replacement. `create_tables` adds `ordinal` to older association tables with a
negative sentinel, preserving unrelated rows during subset exports. The grass baker
recovers those lists, and lists in databases without the column, from winning raw
LTEX records without mutating the database. A complete export writes explicit ordinals.
There is no foreign key on the grass target: a deleted or unresolved grass can still be named by a texture,
and consumers resolve it against `grass_types`. Both projections refresh atomically
from a complete effective load-order export, removing stale grass and associations
on deletions or later overrides. Deleted GRAS records therefore do not reappear as
grass definitions. The subset export used by movement annotation preserves unrelated
grass rows; if it includes an LTEX, only that texture's association list is replaced.
Movement annotation continues to accept schema 4 through the current schema and
preserves the existing database version; it does not perform a full reconversion.

---

### Other Runtime and In-House Tables

These tables are written by `create_tables` / the export and had no DDL here before #269. Their
DDL below is copied from `crates/converter/src/esm/exporter.rs` and
`crates/converter/src/esm/inhouse*.rs`; `statics` is documented with `references` (§5) by #270.

```sql
CREATE TABLE IF NOT EXISTS npcs (               -- one row per NPC_
    id INTEGER PRIMARY KEY, editor_id TEXT,
    full_name TEXT,                             -- FULL
    race_id INTEGER,                            -- RNAM
    class_id INTEGER,                           -- CNAM
    flags INTEGER NOT NULL                      -- record header flags
);
CREATE INDEX IF NOT EXISTS idx_npcs_editor_id ON npcs(editor_id);

CREATE TABLE IF NOT EXISTS movement_types (     -- one row per MOVT
    id INTEGER PRIMARY KEY,
    editor_id TEXT NOT NULL,                    -- EDID
    left_walk REAL NOT NULL, left_run REAL NOT NULL,       -- SPED, Creation units per second;
    right_walk REAL NOT NULL, right_run REAL NOT NULL,     -- zero where the type cannot move
    forward_walk REAL NOT NULL, forward_run REAL NOT NULL,
    back_walk REAL NOT NULL, back_run REAL NOT NULL,
    load_order INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS race_movement_links (  -- one row per RACE
    id INTEGER PRIMARY KEY,
    walk_movt_id INTEGER,                       -- WKMV
    run_movt_id INTEGER,                        -- RNMV
    load_order INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS default_objects (    -- DOBJ DNAM, merged key by key over every override
    key TEXT PRIMARY KEY,                       -- use code, for example DMWL (walk) or DMRN (run)
    form_id INTEGER NOT NULL                    -- load-order FormID; a null link has no row
);
-- `default_objects` follows the game loader: a later `DOBJ` changes only the keys it lists, a null
-- link removes the key, and a zero key (Update.esm holds hundreds of padding pairs) is skipped.
-- Both record readers feed the key-merged `DOBJ` into the runtime projection, so `records.data`
-- for DOBJ 00000031 holds the merged `DNAM` under the winning plugin's `load_order`. It is a
-- derived record, not the winner's bytes. With the in-house reader, the winner's exact source
-- bytes stay in `inhouse_source_records`, and every version's header is in the source history. A package with no `default_objects` table was
-- converted before the table existed; the engine flags it and uses the earlier rule.
CREATE TABLE IF NOT EXISTS movement_game_settings ( -- fMoveCharWalkBase and fJumpHeightMin only
    id INTEGER PRIMARY KEY, editor_id TEXT NOT NULL,
    value REAL NOT NULL, load_order INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS texture_sets (       -- one row per TXST; slot roles in the preamble
    id INTEGER PRIMARY KEY, editor_id TEXT,
    diffuse_path TEXT,                          -- TX00
    normal_path TEXT,                           -- TX01
    glow_path TEXT,                             -- TX03
    height_path TEXT,                           -- TX04
    environment_path TEXT,                      -- TX05
    mask_path TEXT,                             -- TX02
    specular_path TEXT,                         -- TX07
    detail_path TEXT                            -- TX06
);
CREATE TABLE IF NOT EXISTS landscape_textures ( -- one row per LTEX
    id INTEGER PRIMARY KEY, editor_id TEXT,
    texture_set_id INTEGER,                     -- TNAM -> TXST
    material_type INTEGER,                      -- MNAM: the material type's FormID (MATT), not a number
    friction REAL,                              -- HNAM byte 0
    restitution REAL                            -- HNAM byte 1
);
```

The in-house reader also writes these auxiliary tables (diagnostic and provenance data; the
engine does not read them):

```sql
CREATE TABLE IF NOT EXISTS inhouse_source_records (     -- original decompressed source bytes
    form_id INTEGER PRIMARY KEY, source_form_id INTEGER NOT NULL, load_order INTEGER NOT NULL,
    record_type TEXT NOT NULL, flags INTEGER NOT NULL, payload BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS inhouse_localized_fields (   -- every localized string field it met
    form_id INTEGER NOT NULL, field_index INTEGER NOT NULL,
    signature TEXT NOT NULL, field_name TEXT NOT NULL,
    string_table TEXT NOT NULL,                         -- strings, dlstrings or ilstrings
    string_id INTEGER NOT NULL, load_order INTEGER NOT NULL,
    resolved_text TEXT,
    status TEXT NOT NULL CHECK(status IN ('resolved','null_id','missing')),
    PRIMARY KEY(form_id, field_index)
);
CREATE TABLE IF NOT EXISTS inhouse_terrain_source_order (
    form_id INTEGER PRIMARY KEY, source_record_offset INTEGER NOT NULL
);
CREATE TABLE inhouse_terrain_layers (                   -- recreated with every complete export
    form_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
    load_order INTEGER NOT NULL, layer_data BLOB NOT NULL
);
CREATE TABLE inhouse_unresolved_terrain_textures (      -- LAND layer textures that do not resolve
    form_id INTEGER NOT NULL, texture_form_id INTEGER NOT NULL,
    PRIMARY KEY(form_id, texture_form_id)
) WITHOUT ROWID;
```

---

### 15. Interior room graph and occlusion

Skyrim marks the rooms of an interior with `RoomMarker` references (base `0x1F`) and the
doorways between them with `PortalMarker` references (base `0x20`). The converter decodes both
and records which room boxes each placed object belongs to. The tables hold facts only: which
objects to draw is the engine's policy (design: `docs/research/interior-room-graph-converter.md`,
upstream issue #188). Only interior cells and only box rooms (`XPRM` shape 1) are written;
exterior markers are skipped silently, and rooms of other shapes, and rooms or portals with a non-finite size or transform, are skipped and counted in the conversion report.
All FormIDs are in load-order numbering, like `references.id`.
These marker base forms, and the occlusion `PlaneMarker` (`0x17`), are the ones `Skyrim.esm` defines (always load-order slot 0, so the numbers do not change with the load order). A plugin that places rooms with a marker base of its own is not recognised and produces no rows; the conversion report does not count that case.

```sql
CREATE TABLE IF NOT EXISTS room_bounds (       -- one row per interior RoomMarker with a box XPRM
    ref_id INTEGER PRIMARY KEY,                -- references.id of the marker
    cell_id INTEGER NOT NULL,
    pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,   -- Creation units
    rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,   -- radians, as references
    half_x REAL NOT NULL, half_y REAL NOT NULL, half_z REAL NOT NULL, -- XPRM[0..12]
    lighting_template_id INTEGER,              -- LNAM, NULL when absent
    image_space_id INTEGER,                    -- INAM, NULL when absent
    flags INTEGER NOT NULL DEFAULT 0           -- XRMR byte 1 | (second XRMR word << 8), kept raw
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
    unbounded_a INTEGER NOT NULL DEFAULT 0,     -- source XPOD explicitly names NULL on side A
    unbounded_b INTEGER NOT NULL DEFAULT 0,     -- source XPOD explicitly names NULL on side B
    PRIMARY KEY (ref_id, pair_index)
);
CREATE INDEX IF NOT EXISTS idx_room_portals_cell ON room_portals(cell_id);

CREATE TABLE IF NOT EXISTS room_occluders (
    ref_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
    shape INTEGER NOT NULL CHECK(shape IN (1,3)), -- XPRM box or plane
    pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
    rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
    half_x REAL NOT NULL, half_y REAL NOT NULL, half_z REAL NOT NULL,
    scale REAL NOT NULL,
    header_flags INTEGER NOT NULL,
    has_enable_parent INTEGER NOT NULL,
    has_links INTEGER NOT NULL,
    target_ref INTEGER
);
CREATE INDEX IF NOT EXISTS idx_room_occluders_cell ON room_occluders(cell_id);
CREATE TABLE IF NOT EXISTS room_occluder_links (
    ref_id INTEGER NOT NULL, link_index INTEGER NOT NULL,
    linked_ref INTEGER NOT NULL,
    single_target_verified INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (ref_id, link_index)
);

CREATE TABLE IF NOT EXISTS room_links (        -- ordered authored room-group links
    room_ref INTEGER NOT NULL,
    linked_ref INTEGER NOT NULL,
    link_index INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (room_ref, linked_ref)
);

CREATE TABLE IF NOT EXISTS reference_rooms (   -- room membership, interiors with rooms only
    reference_id INTEGER NOT NULL,             -- references.id
    room_ref INTEGER NOT NULL,                 -- room_bounds.ref_id
    contained INTEGER NOT NULL,                -- 1: pivot inside the box; 0: only the bounds overlap it
    PRIMARY KEY (reference_id, room_ref)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_reference_rooms_room ON reference_rooms(room_ref);

CREATE TABLE IF NOT EXISTS room_reference_rules (
    reference_id INTEGER PRIMARY KEY,
    cell_id INTEGER NOT NULL,
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
    cell_id INTEGER PRIMARY KEY,
    complete INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS room_graph_stats (  -- report counters and version markers
    name TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS room_link_anomalies (
    plugin TEXT PRIMARY KEY,
    count INTEGER NOT NULL,
    first_marker INTEGER NOT NULL,
    first_cell INTEGER NOT NULL
);
```

Columns and rules:

- **`room_occluders`** stores interior `PlaneMarker` placements, base form `0x17`, including
  cells with no room markers. Shape 1 is a box. Shape 3 is a local X-Z plane. `DATA` supplies
  the transform and `XPRM` supplies the unscaled half-extents. The native record loader consumes
  legacy `XOCP` without retaining its geometry. The converter skips deleted placements.
  Unusable geometry leaves no row and marks the cell incomplete. An absent `XSCL` gives scale 1. An authored scale must be one finite,
  positive float. A repeated, malformed, or non-positive scale skips the geometry and records
  an `occluder_unusable_geometry` omission. Resolved outgoing links are still stored.
  The runtime also skips invalid scale rows in older packs. For a valid
  scale it uses the authored extents at scale 1 or higher and shrinks them below 1. This bounds
  the shadow whether retail uses authored extents or scales them.
  The row retains disabled-state flags, enable-parent presence, and linked-plane presence for
  runtime policy. `target_ref` is NULL only when `XMBR` is absent. Zero preserves an unresolved
  or malformed explicit override. `has_links` remains true for malformed `XORD`, including after
  remapping.
  For a plane with one valid nonzero source target, the full converter also writes the
  corroborating `target` assignment in `room_reference_rules`. The runtime requires both
  facts to agree with the unique containing room before using a named plane.
- **`room_occluder_links`** preserves nonzero linked FormIDs from complete 16-byte `XORD`
  occurrences. Each occurrence holds four 32-bit FormIDs that can name a `REFR` or be null.
  [xEdit 4.1.5f's TES5 `REFR.XORD` definition](https://github.com/TES5Edit/TES5Edit/blob/xedit-4.1.5f/Core/wbDefinitionsTES5.pas#L12272-L12278)

  `link_index` is `occurrence * 4 + slot` using the original source position, including gaps for
  earlier malformed occurrences. The converter keeps each remapped nonzero FormID when it skips
  source geometry or the target is absent. The edge still suppresses the target if a later subset
  adds it. An unreadable occurrence sets the source cell's `complete` flag to false. In-house
  exports also clear that flag for unresolved FormIDs and targets without a live `REFR`.
  A full in-house export publishes `occlusion_version = 1` in `room_graph_stats`.
  It sets `occlusion_links_verified`
  to zero when a truncated marker payload or a malformed `XORD` can hide a cross-cell link.
  Full in-house exports and plugin placement imports also clear the flag when a discarded
  frame has unknown scope, a `REFR` fails before its links can be read, or a rejected
  placement has a possible `XORD` target and a `NAME` that can name a PlaneMarker.
  An unreadable `REFR` can introduce a PlaneMarker even if its predecessor used an ordinary base or
  the placement is new. A later usable replacement or deletion of the same FormID
  supersedes that evidence within the conversion or import.
  These markers let the loader keep unrelated occluders in a complete cell that contains an
  all-null `XORD`. Subset exports invalidate cells at the other end of cross-cell or unverified
  links. A verified same-cell edge can be replaced or removed while the engine reloads its
  endpoints and checks their shared owner.
  A plugin placement import also clears pack-wide link verification when remapping reports
  damage on an occlusion marker. It retains each outgoing link it can resolve.
  Occluder shadows require both markers because an unverified source in another cell can have a
  missing incoming link to a local occluder. Legacy exports omit both markers.
  A single complete source `XORD` with exactly one nonzero slot sets `single_target_verified`
  on its resolved edge. The pack editor checks the original Skyrim fields before remapping, or
  reads all four runtime FormID slots directly from native JSON. Existing rows default to zero. Isolated one-way and reciprocal pairs of
  eligible planes with a common owner may each cast independent shadows.
- **`room_bounds`**: the box is centred on the marker's position and oriented by its rotation
  (the same Euler convention as any reference, `shared::coordinates`). Exactly one 24-byte `DATA`
  supplies the transform. An invalid transform omits the room and marks its cell incomplete.
  A marker with no `XRMR` authors no flags, so `flags` is `0`. A marker whose `XRMR` header is
  present but unreadable (short, repeated or unframed) has unknown flags: it gets no row, its
  cell is incomplete and the export reports `room_marker_unreadable_flags` (#811).
  Exactly one 32-byte box `XPRM` supplies the `half_*` extents in the room’s own frame. `lighting_template_id` (`LNAM`) and `image_space_id`
  (`INAM`) are decoded for the lighting work in #129-#135; nothing in the engine uses them yet.
- A **`room_portals`** row describes a doorway in the marker's local X-Z plane, with its normal along +Y.
  The converter requires exactly one 24-byte `DATA` with finite position and rotation values and exactly
  one 32-byte shape-3 `XPRM` with finite, strictly positive X and Z half-extents (`half_x` and
  `half_z`). If either field is missing, duplicated, or invalid, the converter skips the portal
  and marks its cell incomplete. `XPOD` is an array of 8-byte (origin, destination) FormID pairs
  (xEdit 4.1.5f defs, `Core/wbDefinitionsTES5.pas`: `wbArray(XPOD, 'Portal Data',
  wbStruct('References', [wbFormIDCk('Origin', [REFR, NULL]), wbFormIDCk('Destination',
  [REFR, NULL])]))`), so a portal gets **one row per pair**, numbered by `pair_index` from 0
  in stored order; the geometry columns repeat on every row. A portal whose `XPOD` is absent,
  empty or shorter than one pair gets one row with `pair_index` 0 and both sides NULL.
  Missing, empty, truncated, or repeated `XPOD` fields mark the cell incomplete; conflicting
  occurrences are not exported. Both outside flags are zero for a truncated field, including on any
  retained leading pair. Every `XPOD` in the official masters is one pair, so
  vanilla output has `pair_index` 0 only. **Readers must not assume `ref_id` is unique**: join
  or group on `(ref_id, pair_index)`, or on `ref_id` and expect several rows. The key changed
  from `ref_id` alone, and `create_tables` migrates an older-shaped `room_portals` (no
  `pair_index`) in place, inside one savepoint: every row is kept with `pair_index` 0 until the
  next full export rebuilds the table.
  A NULL endpoint with its `unbounded_*` flag set names authored unbounded space. Without that
  flag it is unknown: a missing, malformed, unresolved or foreign-cell link does not establish
  outside connectivity. Adding these columns to an older database leaves both flags zero.
  In the plugin remap `XPOD` is an array of FormIDs: every four bytes are remapped. An
  invalid length or broken nonzero link leaves an empty legacy occurrence and a warning,
  so malformed siblings cannot disappear and establish false uniqueness or authored nulls.
  The typed reader restores flags only from a unique source field containing whole eight-byte
  pairs; its canonical cleared links cannot establish outside connectivity.
  Private comparison databases may add a nullable integer `retail_visit_index` to this table.
  It records the portal registration order from one retail load. For a complete visibility-v2
  cell the engine accepts the order only when there is one row per marker and the indices form
  a permutation from zero through the portal count minus one. Otherwise it uses FormID order.
  The ordinary converter has no load-specific order to export.
- **`room_links`**: one row per distinct `XLRM` target, retaining the last occurrence's
  zero-based `link_index`, with no synthesized mirror rows. Migration adds the column with zero defaults; only a fresh export
  publishes the authored-assignment version. A missing target or a target in another cell marks
  the source cell incomplete. The target is retained in either case.
  Both readers check the source order before publishing resolved links. One four-byte `XRMR`
  header precedes four-byte `LNAM` and `INAM` fields, in that order, when their flags are set.
  The lower 14 bits of the first `XRMR` word give the number of contiguous four-byte `XLRM`
  slots that follow. A misplaced field or count mismatch marks the cell incomplete. The export
  retains links from the valid prefix of this run, up to the declared count.
  In authored mode, `XRMR[2] != 0` marks a master (`(flags >> 8) & 255` in `room_bounds`).
  An unlinked marker is also a master. Slaves resolve linked masters, preferring direct masters
  before recursive links and traversing reverse source order. Explicit masters install missing
  reciprocal links during resolution. Distinct masters remain distinct, even within a connected
  component. Unresolvable cycles or construction-order ambiguity retain the whole-cell fallback.
  The room is the union of its primary and joined bounds; gaps are not filled by an enclosing box.
  Malformed or unresolved `XLRM` occurrences keep the cell incomplete in both readers,
  including fields rejected before typed canonical publication.
  The legacy reader also marks the cell incomplete when it skips a damaged placement with a
  known parent cell and no usable version supplies that placement.
  Legacy graphs continue treating links as open visibility edges.
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
  model (`STAT`, `MSTT`, `FURN`, `TREE`, `FLOR`, `CONT`, `DOOR`, `ACTI`, `TACT`, `APPA`, `ADDN`,
  `ARTO`, placed items and armour, and `LIGH` with a model; see §5). Placed references of other base types get pivot rows only: actors
  (`NPC_`, which move and stay ordinary entities), lights without a model (their reach is a
  lighting question, not geometry), decals (`TXST`), and bases with no geometry (`IDLM`, `SOUN`,
  `ASPC`). In the all-plugins conversion these were 22,251 of 364,262 pivot rows (measured before `TACT`, `APPA`, `ADDN` and `ARTO` joined `statics`).
- **`room_graph_stats`**: the counters the conversion report shows in `integration-report.json`
  under `room_graph`: `rooms`, `portals` (portal markers written, not rows), `links`,
  `portals_with_missing_side` (`room_portals` rows with a NULL side, so a portal with several
  pairs can add several), `portals_with_extra_pairs` (markers whose `XPOD` holds more than one
  pair), `portals_with_trailing_xpod_bytes` (markers whose `XPOD` length is not a multiple of 8),
  `non_box_rooms_skipped`, `portals_without_size_skipped`, `invalid_markers_skipped` (rooms and portals skipped for invalid geometry
  or ambiguous portal fields, including missing, truncated, duplicate or non-finite transforms,
  non-finite sizes, negative room half-extents, nonpositive portal X or Z half-extents, and repeated `XPOD` fields), `objects_in_no_room` and
  `objects_in_several_rooms` (both by pivot), `overlap_rows`, `occluders` (valid geometry rows),
  and `invalid_occluders_skipped` (unsupported or malformed occlusion primitives).
  `assignment_version = 1` enables authored master/slave resolution and reference ownership,
  together with the three assignment tables, `link_index`, and `visibility_version = 2`.
  If the pack declares an unsupported assignment version, the engine draws the full cell.
  `visibility_version = 2` additionally marks graphs whose outside endpoints have verified
  source provenance. The typed reader publishes this marker and the flags in the source-data
  transaction. Until that transaction commits, the runtime uses legacy fallback behavior.
- **`room_link_anomalies`** counts markers with invalid headers, misplaced fields, mismatched counts, or unresolved `XLRM` slots for each
  supplying plugin. The lowest affected marker FormID and its cell are stored with the count.
  Each plugin has one warning with that count and example. `integration-report.json` shows
  the counts under `room_graph.room_link_anomalies`.

Both plugin readers require a nonzero, resolved `NAME` base on live placements. A live override
needs one 24-byte `DATA` transform with six finite coordinates. A cell-owned predecessor also
requires a usable destination cell. Rejecting an override
preserves its predecessor and marks the affected interiors incomplete. If only the reference
ID resolves, the reader can still identify the predecessor’s cell. A later accepted override or
deletion clears damage tied to that ID. If only the destination cell resolves, the damage stays
with that cell. If a discarded frame tail could hide a placement header, an unseen override
could refer to a placement from another cell. All surviving interiors then use the whole-cell
fallback. A partial placement header that reaches the FormID marks the enclosing cell and the
surviving predecessor’s cell incomplete. An unresolvable FormID leaves all interiors incomplete.
A recognized placement tag with a missing or partial FormID also leaves all interiors incomplete,
because its predecessor could be in any cell. The same rule covers a one-, two-, or three-byte
prefix of any placed record type, including `REFR`, `ACHR`, and `PMIS`. When the available group kind bytes are consistent
with a cell-child group, the enclosing cell and the cell named by the label are incomplete.
A discarded `GRUP` prefix with an incomplete label leaves all interiors incomplete. A kind
that excludes cell-child groups affects only the enclosing cell when it is known.
Parsing resumes at the next trusted frame boundary.

The tables are derived data: a full load-order export clears and rebuilds them from the winning
records on every conversion, like the grass tables. Subsets without placed references, including
movement annotation, leave the graph alone. The low-level subset exporter reprojects edited
occluders. For ordinary placed references it clears membership and marks the previous and new
cells incomplete. The placement command described below rebuilds room ownership for its supported
edits in the same transaction.
A subset that changes or removes an existing base model also deletes that base's source sphere
and marks every referencing cell incomplete in the same transaction. An unchanged model retains
its sphere and cell completeness. A full room export restores completeness after rebuilding.
The certificate stores one completeness bit for the whole cell. A model-bounds refresh cannot
distinguish an outdated model from an unresolved room or portal in the same cell. Restoring
culling after a base-model change therefore requires a full room export.
`create_tables` adds these tables to an older reused database.

**These tables are optional and did not bump `shared::WORLD_DATABASE_SCHEMA_VERSION`.** The engine reads
them automatically and probes for them first, as it does for `lights` and the
additive `waters` columns, so a database without them behaves as before.
Unbounded traversal also requires both endpoint columns, `visibility_version = 2`, and a complete
`room_graph_cells` marker for the cell. A skipped unsupported room or invalid room/portal marker
makes that cell incomplete, so the runtime draws the whole cell. Negative room or portal extents are invalid; the loader also rejects them in older packs. Other complete cells retain
unbounded traversal. The engine keeps the full cell visible whenever its completeness row or the table itself is
missing. Write one row per assessed interior. Set `complete=1` only after validating its room
geometry, links and portal endpoints. Set `complete=0` for an interior with incomplete evidence.
In a versioned graph, the engine reads the cell's `references.id` values to distinguish known
placements from references belonging to other cells or unknown runtime objects.

Authored assignment metadata:

- **`room_reference_rules`** records ordinary placements independently of geometric overlap rows.
  For a `target` rule, `room_reference_rules.target_ref` is the nonzero runtime FormID projected from `XMBR`.
  A Multibound header flag with no `XMBR` is `none`. An ordinary placement is `auto`. Malformed/zero targets and unsupported primitive overrides
  are `unknown`. Repeated `XMBR` fields also produce `unknown` with no target, including identical
  repetitions. Both readers preserve malformed `XMBR` presence for every supported placed
  record type, including references carrying the Multibound flag; rejection cannot turn it
  into `auto` or `none`.
  Missing/foreign targets remain visible. An absent `XMBR` alone does not mean `NONE`.
- **`room_model_bounds`** stores source NIF spheres in runtime axes and Creation units, without
  metre scaling. Mesh conversion writes GLB root extras `mudcrabRoomSphere` version 1, and model
  integration rebuilds this table transactionally. It merges supported source child spheres in
  native order, ignoring the placed root's source transform. Models with disconnected exported geometry
  retain no assignment sphere. Supported fixed, self-contained skins
  also receive source bounds. External or unresolved rigs, physics-deformed skins, and unsupported
  trees have no row. Render AABBs are not substituted for missing native assignment bounds.
  The converter rejects a NIF that ends inside a declared block during parsing, before the skin
  pose check runs.
  Incremental Skyrim conversion and failed-asset repair project both bounds from the final
  staged GLB winner. Missing or unreadable winners clear the validity flag and prior sphere.
  After a mod update replaces or removes native GLBs in a private runtime copy, the pack maintainer
  or the mod installation script runs `room-model-bounds-refresh <assets-dir> <meshes/path.glb>...`
  with the engine closed. The launcher and engine leave this step to the caller. If the refresh
  fails, keep the pack offline until it succeeds or you restore the previous GLBs.
  List each changed lowercase runtime path. Native GLBs can supply a sphere in the same
  version 1 root extra. The command updates all matching base forms and geometric room overlaps
  in one SQLite transaction. Keep the original conversion pack for manifest verification and
  later Skyrim conversion or repair.
- **`room_graph_cells`** marks cells whose authored geometry was fully exported. A skipped
  unsupported room shape or invalid/missing marker size makes the cell incomplete. Incomplete
  cells retain full rendering in authored mode.

Initial attachment honors a valid named target directly, even when the object's center lies outside
the room bounds. Later reference-transform changes reclassify supported bounds. Automatic
assignment uses the sphere center, tests the union of each master's bounds, and classifies zero,
one, or multiple touched physical portals as ordinary-space, portal-shared, or common-node
ownership. Portal sharing uses the native sphere test, and each child mesh tests the resulting
volume with its current affine AABB. Ambiguous overlapping masters retain all possible owners.
The native runtime's choice depends on handle and hash order. `NONE`, unsupported transforms,
actors, and doors remain drawn. Activators (`ACTI`), including static webs, stay visible in legacy
graphs. An initial named target can cull an unskinned mesh without a source sphere. A skinned mesh with
a valid named target stays drawn if the model lacks a source sphere. A mesh also stays drawn if it
or one of its ancestors below the reference root carries `AnimatedBy`. Automatic attachment and
reassignment after movement keep meshes with unknown source bounds visible. Named-target
neighboring-room priority and specialized/forced native assignment paths are not fully reproduced.

The `switch-value-and-multibound-room-spheres` mesh contract includes the initially selected child of a switch with the active-child bound flag, empty value-node attachment points, and oriented boxes referenced by multibound nodes. The `initial-switch-scene` contract preserves the switch transform and links only the initially selected branch’s geometry into the exported scene. When the switch hides every mesh, the converter marks the GLB with `extras.mudcrabInactiveSwitch`. Pack repair and reuse accept the meshless scene. The static keeps `bounds_valid=0` and has no `room_model_bounds` row. The `skin-ancestor-room-spheres` contract withholds the bind-pose sphere when a transform controller moves a joint through a non-root ancestor. A missing or unsupported bound remains unknown. The [Gallows Rock comparison](../../research/skyrim-culling-rea-parity.md#gallows-rock-west-doorway) checks the resulting room assignments against retail.

Legacy `reference_rooms` rows remain for compatibility. Multibound-flagged references are omitted
from both pivot and overlap membership; integration removes stale rows. Legacy runtime exemptions
protect these references even before reconversion. Reconvert normally to obtain authored assignment:
the compiled converter digest invalidates old base-database cache bundles, and the
`native-room-assignment-spheres` and `complete-root-room-spheres` mesh-output contracts
regenerate source-sphere metadata. These
additive tables do not change the global world schema or add a feature flag.

#### Import interiors and room placements after conversion

Use `converter apply-room-placements` on a private, offline asset directory. The command takes
the pack's exclusive lock and requires world database schema 9 and `visibility_version = 2`.
It accepts either `assignment_version = 1` or an absent assignment marker. With version 1,
a valid native `target` assigns the reference to its named room. Without the marker, the import
succeeds and sets the Multibound exemption, so the reference stays drawn even when that room is
hidden. Reconvert the pack as described above to enable authored assignment.


```sh
converter apply-room-placements path/to/assets --plugin Decorations.esp
converter apply-room-placements path/to/assets --native decorations.json
```

`--plugin` appends an `.esp`, `.esm`, or `.esl` with interiors and placements to the original plugin order.
The pack must retain the in-house reader's plugin headers. Masters can come from the original
conversion or earlier placement imports. Existing FormID slots stay stable. The command reads
`REFR` adds, full overrides, moves, and deleted headers for existing interiors. The supported
bases are existing `STAT` and `DOOR` records and Skyrim's room, portal, and occlusion markers. A master-owned
`CELL` record can identify an existing interior for its children. The import uses only its FormID.

A plugin can declare a new interior with a plugin-owned `CELL`. Give it a nonzero local FormID
that fits the plugin's full or light slot, a null-terminated UTF-8 `EDID`, and `DATA` with only
the interior bit set (one or two bytes). Optional fields are an inline UTF-8 `FULL` in a
nonlocalized plugin and 72-byte or 92-byte `XCLL` lighting. The `CELL` must be outside worldspace
and cell-child groups. Its record header supports the compression flag. Its placements must use
unused FormIDs owned by the same plugin.

Supported `REFR` fields are `EDID`, `NAME`, `DATA`, `XSCL`, `XESP`, `XMBR`, and the marker fields
`XPRM`, `XRMR`, `XLRM`, `XPOD`, `XORD`, `LNAM`, and `INAM`. A room marker can carry one 12-byte `XMBO`.
On import its three extents must match the first 12 bytes of `XPRM`. A door can also carry a 32-byte
`XTEL` field. The importer skips the placement and preserves its previous door link if `XTEL`
occurs more than once, has the wrong width, contains a nonfinite arrival frame, or names a
nonzero source destination that cannot be resolved.
Static primitives, actor references, scripts, new base records, exterior changes, and other
`CELL` fields require a normal conversion.

The command warns when it skips a damaged record or discards the unread part of a group.
The parser resumes at a verified record or group boundary, and the command applies the usable
placements and interiors it reads. A plugin placement needs one finite 24-byte `DATA` transform.
If present, `XSCL` must occur once and contain a finite positive 32-bit float. The importer skips a
non-occluder placement if `XPRM` occurs more than once or exceeds 32 bytes. A damaged
placement leaves its destination and any retained
predecessor's cell incomplete. When a discarded tail could hide a placement header or the
command cannot identify the affected cell, all interiors use the whole-cell fallback. The
command also warns and skips placements that require a damaged new interior. Usable records
with unsupported content still roll back the import.

An occluder that passes placement admission can still have an unusable shape, such as a
malformed `XPRM`. The placement edit report counts projection omissions by category and samples
FormIDs and reasons. Resolved outgoing `XORD` slots still suppress their targets when the source
shape is omitted. A placement rejected earlier, for example for a nonfinite `DATA` transform,
produces a warning and leaves its affected cells incomplete. It contributes no projection
omission or new link slots. If it could hide outgoing PlaneMarker links, the importer clears
link verification. Valid neighboring placements still apply.

`--native` accepts a `cells` array for new interiors and an `edits` array for placements.
Provide at least one cell or edit. A cell declaration takes an `id`, an `editor_id`, and an optional
`full_name`. `put` adds or replaces a static placement or occluder, and `delete` removes a placement by
`id`. A native replacement preserves the existing `XESP` and initially-disabled flag. If the
existing enable state is unavailable or cannot be preserved, the import fails. Use numeric runtime
FormIDs from the target pack. For a new native interior or placement,
choose an unused ID from `0xFF000001` through `0xFFFFFFFF`. A new interior takes only fresh
placements. IDs used in earlier imports stay reserved for their original records even after a
placement is deleted. Position uses Creation units. Rotation uses radians and defaults to
`[0, 0, 0]`. Scale defaults to `1`. Ownership
defaults to `{"kind": "auto"}`. Use `{"kind": "none"}` for a Multibound override without a
target, or `{"kind": "target", "room": 2048}` for a named room.

`put_room` adds or replaces a box room marker. Its `half_extents` array gives finite, nonnegative
X, Y, and Z half-extents in Creation units. Optional `links` name joined rooms in authored order.
Set `master` to `true` or `false` to change the room's master flag. Omitting it preserves the
flag on an existing room. A new room defaults to `false`.
`put_portal` adds or replaces a portal marker with finite, positive X and Z `half_extents`. Its
`endpoints` array names two rooms in that cell. Use `0` for an explicit outside endpoint.

```json
{
  "cells": [
    {"id": 4278190081, "editor_id": "FreshInterior", "full_name": "Fresh Interior"}
  ],
  "edits": [
    {
      "op": "put_room", "id": 4278190082, "cell": 4278190081,
      "position": [0, -100, 0], "half_extents": [100, 100, 100]
    },
    {
      "op": "put_room", "id": 4278190083, "cell": 4278190081,
      "position": [0, 100, 0], "half_extents": [100, 100, 100]
    },
    {
      "op": "put_portal", "id": 4278190084, "cell": 4278190081,
      "position": [0, 0, 0], "half_extents": [30, 60],
      "endpoints": [4278190082, 4278190083]
    }
  ]
}
```

For a PlaneMarker, use base 23 and supply a shape such as
`{"kind":"plane","half_width":8,"half_height":8}` or
`{"kind":"box","half_extents":[8,12,16]}`. Optional `links` holds four
runtime FormIDs in physical XORD order. Use zero for an empty slot. Four zeros have
the same effect as omitting `links`. Skyrim placement plugins can supply the same
fields on a PlaneMarker. Occluder edits
require a pack with a source-verified occluder projection.

One transaction creates the runtime cell rows and updates placements, ownership rules, and the room
graph. Moves refresh both the source and destination cells. The command uses the model bounds
already in the pack. It reports how many cells were created and how many affected cells passed
certification. A fresh interior with complete input retains its certificate even when it has no
rooms. A later import can add its first room and portal. Incomplete graph evidence keeps the
whole-cell fallback. An existing cell must have a valid prior certificate to retain certification.
Unknown assignments and unverified portal endpoints retain their conservative behavior.
When an occluder first reaches a roomless interior with no
certificate, the exporter checks its stored Skyrim placements for earlier room, portal, or
occlusion markers before establishing a certificate.

The original conversion's canonical `records`, `formid_map`, source history, and reports remain
available for inspection. `room_placement_layers` retains the exact imported file and checksum.
`room_placement_edits` maps each created cell and edited placement to its source FormID and
records placement deletions.
A native `put` fails if its base FormID is in `subset_nonplacement_deletions`. The import
rolls back. A `delete` can still remove an existing placement that uses that base.
Reapplying the same source is a no-op. Plugins match existing imports by case-insensitive filename
and checksum. Different contents under an imported filename are rejected, including after a
successful import that skipped damaged records.
To replace an imported plugin, start with a fresh conversion of the same base pack and replay
the placement sources in their original order. Use the corrected plugin under the original name
at its original position. Keep the plugin's full or light classification and the local FormIDs
that later layers reference. To make a follow-up edit on the current pack, author a new patch
with a different filename. List the imported plugin as a master when overriding its forms.

The following table describes geometric membership. Whole-cell fallbacks and reference exemptions
take precedence over the per-object rules. Other renderer visibility checks still apply.

| Case | Converter writes | Engine room filter |
|---|---|---|
| Interior without room markers | no room rows | certified standalone occluders can hide fully shadowed meshes in the unbounded space |
| Old database, tables absent | n/a | feature off, no error |
| Source-verified outside portal side | NULL with an unbounded-side flag | traverse the explicit unbounded node |
| Missing, invalid, or foreign portal endpoint | NULL without verified outside provenance | retain the cell |
| Object in several rooms | several `contained = 1` rows | always retained by geometric membership |
| Object in no room, legacy graph | no `contained = 1` row | always retained |
| Object without geometric membership in a roomed cell | no `reference_rooms` rows | retain for ordinary view culling |
| Known object overlapping rooms in a complete versioned graph | overlap rows only | retained when a visible candidate volume passes its mesh-bound test; candidates come from unbounded space, overlap rooms, and portal sharing |
| Lookup point outside every room, geometric membership only | n/a | retain the cell |
| Non-box room shape in a new conversion | skipped, counted; cell marked incomplete | retain the cell |
| Exterior markers | not written | out of scope (#106) |

The geometric filter retains authored overrides including `NONE`, actors and their leveled lists,
doors, activators, loose inventory references, moving rigid bodies, and explicitly exempt roots
or meshes. References without valid stored model bounds also stay drawn because pivot-only rows
do not establish overlap coverage. Unknown references outside the graph are retained.
Missing or invalid per-mesh bounds and `NoFrustumCulling` skip aperture-bound testing; a candidate
space must still be reached. Portal-shared geometry can use either endpoint's visible volume.

The authored-assignment path uses these rules:

| Case | Engine |
|---|---|
| Absent room tables, or a roomless cell without a usable occluder | normal rendering |
| Certified roomless interior with a supported occluder | test mesh bounds against its unbounded-space shadow |
| Verified source NULL endpoint in a versioned graph | unbounded-space traversal |
| Unknown portal endpoint | whole-cell fallback |
| Incomplete authored cell or unresolved master | whole-cell fallback |
| Lookup point outside every authored room union, complete graph with source-verified endpoints | start in unbounded space and traverse its portals |
| Reference with no recoverable assignment | keep visible |
| Exterior markers | outside this feature's scope |

---

### 16. Reference Emittance Sources (`emittance`)

A NIF shader with `SLSF1_External_Emittance` (bit 29) takes its emission colour from outside the
model: from the light or region the placing reference names in `XEMI`
(`references.emittance_id`). Skyrim keeps building windows dark by day this way. The Sleeping Giant
Inn's reference in Riverwood (`0002D763`) names `FXLightRegionInvertWindowWhiterun` (`0008277B`),
whose one weather `FXWthrInvertWindowsWhiterun` (`0008277A`) has a black Effect Lighting colour by
day and an amber one at night (#242).

The complete load-order export rebuilds the table, writing one row per distinct `XEMI` target it can
resolve, as 8-bit sRGB at sunrise, day, sunset and night:

- `LIGH`: the `DATA` colour (bytes 8-10), the same at every time of day.
- `REGN`: the `RDWT` weather with the highest chance (the first on a tie), and that weather's
  `NAM0` Effect Lighting colour, the tenth colour type. `NAM0` holds 16 bytes per colour type
  (sunrise, day, sunset and night, each RGB and an unused byte); the official plugins store 13,
  14 or 17 types. A region changes weather with its climate, which the runtime does not model, so
  its likeliest weather stands in.

Effect Lighting rather than Sunlight: the inn's window shader has a white tint and a neutral grey
glow map, so the warm colour of Skyrim's lit windows at night can only come from the source. Both
colours are black by day for the window regions.

A target that is missing, deleted, not a `LIGH` or `REGN`, or whose weather has no Effect Lighting
colour gets no row and a warning; the runtime then keeps the model's own emission. The movement
annotator's subset export does not touch this table.

Measured on Skyrim SE with its official plugins: 9,655 references carry `XEMI`, naming 211
sources. 166 lights and 31 regions resolve; 14 regions list no weather at all (the largest,
`RiverwoodMountainsSnow` `0002E2CC`, is named by 479 references), and what Skyrim lends from such
a region is unverified, so their 651 references keep the model's emission.

```sql
CREATE TABLE IF NOT EXISTS emittance (
    id INTEGER PRIMARY KEY,        -- XEMI target: a LIGH or REGN FormID
    source_type TEXT NOT NULL,     -- 'LIGH' or 'REGN'
    weather_id INTEGER,            -- REGN: the WTHR its colours come from
    sunrise_r INTEGER NOT NULL, sunrise_g INTEGER NOT NULL, sunrise_b INTEGER NOT NULL,
    day_r INTEGER NOT NULL, day_g INTEGER NOT NULL, day_b INTEGER NOT NULL,
    sunset_r INTEGER NOT NULL, sunset_g INTEGER NOT NULL, sunset_b INTEGER NOT NULL,
    night_r INTEGER NOT NULL, night_g INTEGER NOT NULL, night_b INTEGER NOT NULL
);
```

The engine reads the day columns, its one time of day; see
[`color-pipeline.md`](../engine/color-pipeline.md#external-emittance-242).

**The table and `references.emittance_id` are optional and did not bump
`shared::WORLD_DATABASE_SCHEMA_VERSION`**, for the same reason as the room tables: they only add a
table and a column, and the engine probes for both. `create_tables` adds `emittance_id` to an older
reused `references` table. A database without them loads with every reference unlit by `XEMI`, so
`External_Emittance` surfaces keep the model's own emission.

---

### 17. Load Door Links (`door_links`)

A load door is a `REFR` of a `DOOR` base record that carries an `XTEL` subrecord. `XTEL` is 32
bytes in Skyrim SE: the destination door reference (FormID, bytes 0..4), the arrival position
(three `f32`, bytes 4..16), the arrival rotation (three `f32` radians, bytes 16..28) and a `u32`
flags word (bytes 28..32, `1` = no alarm). The arrival frame is the one the player is placed in
on this side's destination, not the position of the destination door. Both record readers keep
the 32 bytes unchanged in `records.data` and remap the destination FormID with the load order
(`form_id_layout` in `crates/converter/src/esm/mod.rs`; the in-house schema describes the
subrecord as `teleport` in `crates/converter/src/records/schema.json`). The `door_links` table is
projected from those canonical `records` and `references` rows, not from either reader, so the
legacy and in-house frontends publish identical rows.

```sql
CREATE TABLE IF NOT EXISTS door_links (        -- one row per load door that can be followed
    ref_id INTEGER PRIMARY KEY,                -- references.id of the door
    destination_ref_id INTEGER NOT NULL,       -- XTEL[0..4], after FormID remapping
    pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,   -- XTEL[4..16], Creation units
    rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,   -- XTEL[16..28], radians
    destination_cell_id INTEGER NOT NULL,      -- cells.id of the cell holding the destination door
    destination_worldspace_id INTEGER,         -- that cell's worldspace, NULL for an interior
    destination_name TEXT                      -- interior cell FULL (else EDID), or the worldspace EDID
);
```

A door is skipped, with no row, when it has no `XTEL`, when the `XTEL` is not exactly 32 bytes,
when the destination is `0`, when any arrival value is not finite, or when the destination door
is not a placed `DOOR` reference in a stored cell (an exterior cell needs its grid). The winning
record decides: an override that removes `XTEL` removes the link. The `XTEL` flags word is not
projected; it stays in `records.data`.

A full load-order export clears and rebuilds the table on every conversion, like the grass and
room tables. Placement edits refresh links to and from edited doors in the same transaction.
When a plugin placement layer edits a door, the projection reads its effective fields from
`references.data`. The movement-annotation subset export leaves the table alone. A diagnostic
archive defers every runtime view and leaves the table empty. `create_tables` adds the table to an older
reused database, and adds `destination_name` to a table that predates it.
`door-links-annotate <skyrim_world.db>` rebuilds it in one transaction from a converted
database's own winning records, with no plugin reparsed, so an existing package gains doors
without a full conversion. It takes the asset lock exclusively (close the engine first), accepts
schemas 4 through the current one, and leaves meshes, terrain, `schema_info`, reader identity and
the other tables unchanged. A corrupt `records` row fails the rebuild and keeps the old table.

**This table is optional and did not bump `shared::WORLD_DATABASE_SCHEMA_VERSION`**, for the same
reason as the room tables: it only adds a table, and the engine probes for it. The engine reads
it through a `LEFT JOIN` on `references`, only when the table exists with every column above.

| Case | Converter writes | Engine |
|---|---|---|
| Old database, table absent | n/a | logs once, doors stay scenery |
| Table of another shape (missing column) | n/a | warns, doors stay scenery |
| Door without `XTEL`, or with a malformed one | no row | scenery |
| Destination cell has no resolvable record | no row | scenery |
| Row with a NULL arrival value | n/a | not a load door |

---

### 18. Actor Model Catalog (`actor_models`, `actor_model_parts`)

Every non-deleted `NPC_` base in the merged record map gets one `actor_models` row describing its
race, sex, effective skin armor, body/head resolution and head path, plus one `actor_model_parts`
row per biped slot the effective skin resolves. The catalog is resolved once, after the record
loop, from the merged record map; the tables are projected from canonical records, not from either
reader's raw `records.data`, so the legacy and in-house frontends publish identical rows.

```sql
CREATE TABLE IF NOT EXISTS actor_models (
    id INTEGER PRIMARY KEY,          -- NPC_ FormID
    race_id INTEGER NOT NULL,
    sex TEXT NOT NULL,               -- 'male' | 'female'
    skin_armo_id INTEGER,            -- effective NPC WNAM override, else effective RACE/WNAM
    head_model TEXT,                 -- FaceGen or default head NIF path
    head_source TEXT,                -- 'facegen' | 'default'
    resolution TEXT NOT NULL,        -- 'body', 'body_head', 'unresolved'
    skin_tone_r REAL,                -- NPC_/QNAM, 0..1, encoded scale
    skin_tone_g REAL,
    skin_tone_b REAL,
    weight REAL                      -- NPC_/NAM7, 0..100
);
CREATE TABLE IF NOT EXISTS actor_model_parts (
    npc_id INTEGER NOT NULL,
    slot TEXT NOT NULL,              -- 'body' | 'hands' | 'feet'
    model_path TEXT NOT NULL,
    armature_race_id INTEGER,        -- ARMA/RNAM: the NPC's race or DefaultRace (25); NULL when RNAM is absent
    skin_texture_set_id INTEGER,     -- ARMA/NAM0 (male) or NAM1 (female), a TXST
    PRIMARY KEY (npc_id, slot)
);
```

**Traits.** When `NPC_/ACBS` `u16` at offset 18 has bit 0 set, `TPLT` is followed while each
target also has that bit set. The final Traits root supplies race (`RNAM`), sex (`ACBS` bit 0 at
offset 0), and skin override (`WNAM`). A Traits-enabled NPC's own valid `WNAM` is ignored. A
visited set detects cycles; there is no fixed depth limit. If the chain cannot resolve, the
original NPC still gets one unresolved row with `race_id = 0`, placeholder `sex = 'male'`, no
skin, parts, or head. Diagnostics identify missing, deleted, wrong-type, cyclic, or `LVLN`
targets. The detailed source and evidence boundaries are in
[`actor-catalog-inheritance-policy.md`](../../research/actor-catalog-inheritance-policy.md).

**Armature skin texture set.** The game draws a part's `skin_tint` shapes with the armature's skin
`TXST` for the NPC's sex (`NAM0` male, `NAM1` female), not with the part NIF's own texture set:
retail `femalehands_1.nif` has no `_s`, while `NakedHands` (`00000D6C`) gives female hands
`SkinHandFemale_1` (`0003EDE9`), whose `TX07` is `FemaleHands_1_S.dds`. `skin_texture_set_id`
stores that FormID (`NULL` when the armature has none for the sex; the other sex's set is not
used), and `armature_race_id` the armature's own `RNAM` (`NULL`, never `0`, for an armature with no
readable `RNAM` that matched through an additional race; `armature_missing_race` reports it), so `25` marks a part that came from the
shared `DefaultRace` armature. The converter composes the default humanoid body from the
`DefaultRace` sets: per sex and slot, the set most parts with `armature_race_id = 25` use, applied
in NIF slot order (`TX00`→0, `TX01`→1, `TX03`→2, `TX04`→3, `TX05`→4, `TX02`→5, `TX06`→6,
`TX07`→7; slots the set leaves empty keep the NIF's path) when the armature's model file is the
part being composed. Both columns arrived after the first catalog export without a schema bump;
`create_tables` adds them to an older reused `actor_model_parts`, and a database without them
composes the body from the NIFs' own textures.

**Skin tone.** `NPC_/QNAM` is three little-endian `f32` (red, green, blue), stored as written:
0..1 on the encoded scale (retail values are whole steps of 1/255; Alvor `00013475` is
198,176,168). The game passes it to the `skin_tint` shaders of the body, hands and feet as their
tint colour. A base without `QNAM`, or with a short or non-finite one, stores `NULL` in all three. Since
QNAM is not a confirmed Traits inheritance field, a Traits-enabled source NPC stores `NULL` even
when it or its resolved root carries QNAM; this conservative catalog policy avoids inventing root
inheritance.
The columns arrived after the first catalog export without a schema bump: `create_tables` adds
them to an older reused `actor_models`, and the engine probes for them, so a database without them
still loads and its actors stay untinted.

**Body weight.** `NPC_/NAM7` is one little-endian `f32` in 0..100: the weight the game blends
between the thin (`_0`) and heavy (`_1`) body meshes (0 is thinnest, 100 is heaviest). The column
is nullable; a base with no `NAM7`, or with a short, non-finite or out-of-range one, stores `NULL`
and the engine draws the heavy body. The weight comes only from the base's own record: `TPLT`
template inheritance is not resolved for it, matching `QNAM` and `RNAM`. The column arrived after
the first catalog export without a schema bump (`WORLD_DATABASE_SCHEMA_VERSION` stays 9):
`create_tables` adds it to an older reused `actor_models`, and the engine probes for it, so a
database without it still loads.

**Sex.** `NPC_/ACBS` bit 0 (`1` = female). A base with no `ACBS` defaults to male and records a
`missing_acbs` diagnostic.

Without Traits, the NPC supplies its own race and sex. A missing effective-root `ACBS` defaults
to male and records `missing_acbs`.

**Body resolver chain.** Effective Traits-root `NPC_/RNAM` -> the `RACE` record -> effective skin
armor -> every `ARMO/MODL` armature in authored order -> each `ARMA`:

1. Skin is the effective Traits-root NPC's `WNAM` override when it names a live `ARMO`; otherwise
   it falls back to the effective race's `WNAM`. An invalid explicit override records its
   diagnostic and uses that race fallback. This is a conservative catalog policy, not proof of
   Skyrim engine fallback behavior.
2. An armature is compatible when its primary `ARMA/RNAM` equals the effective race, or a
   resolvable additional-race `ARMA/MODL` names that race. These matches share one tier and keep
   authored `ARMO/MODL` order.
3. Compatible armatures are considered before armatures whose primary `RNAM` is `25`
   (`DefaultRace`), whatever order `ARMO/MODL` lists them in. If neither tier matches, no part is
   written and `no_matching_arma` is recorded (the resolver never picks the first `ARMA`).
4. `MOD2` is the male path. Female NPCs use `MOD3`, falling back to `MOD2` when `MOD3` is absent
   or empty.
5. The slot is the armature's biped bit: bit 2 = body, bit 3 = hands, bit 7 = feet (slot number
   minus 30). The resolver reads `ARMA/BOD2` when present and falls back to `ARMA/BODT`, which is
   the field retail Skyrim's `ARMA` records actually carry (the same `u32` slot word at offset 0;
   `ARMO` is the record with `BOD2`). Outside the creature whole-skin exception below, an armature with neither recognized bit is rejected loudly
   with an `unrecognized_slot` diagnostic; a slot is never inferred from the filename.
6. Paths are deduplicated case-insensitively, and a slot already filled keeps the higher-priority
   armature.

**Creature whole skins.** A non-FaceGen race with an authored project outside the shared
character project family may store its whole skin in biped slot 30 (`BODT`/`BOD2` bit 0).
When no ordinary body resolved, exactly one compatible head-slot model becomes the body.
An ordinary body wins regardless of armature order; multiple distinct candidates remain
unresolved with `ambiguous_creature_body`. Missing race flags or project metadata do not
establish this exception. This preserves humanoid and child body selection.

**Authored idle identity.** Every export creates the typed projection below with the other
runtime tables. Full runtime exports replace its rows in the record transaction; archive-only
record bundles keep the same empty table, like the other deferred runtime views. Metadata
rebuilds therefore invalidate retained idle bindings when their race project or skeleton
changes. The runtime accepts a catalog binding only when this table has a matching row. Older
databases need no migration to render actors; they have no authenticated generic idle library
until normal conversion produces one.

```sql
CREATE TABLE IF NOT EXISTS race_animation_sources (
    race_id INTEGER NOT NULL,
    sex TEXT NOT NULL,
    skeleton_nif TEXT,
    project_hkx TEXT,
    PRIMARY KEY (race_id, sex)
);
```

The converter follows project/character HKX metadata and publishes pose clips plus
`meshes/actor-idles/catalog.json`. Runtime lookup requires race, sex, full body path, project,
and race skeleton identity to agree. See [animal idle investigation](../../research/animal-idle-471.md)
for discovery, supported playback, provenance, and validation limits.

**Head.** The `RACE/DATA` `facegen_head` flag (bit 1 of the flags word at offset 32) selects the
FaceGen geometry `meshes/actors/character/facegendata/facegeom/<plugin>/<formid>.nif`, lowercased,
with `<plugin>` and plugin-local FormID from the effective Traits root's defining-owner
`formid_map` identity (`plugin_name`/`internal_id`). The local ID uses eight-digit lower-case hex
without the load-order slot prefix, so the runtime mod index never appears in the file name
(`head_source = 'facegen'`). Using the Traits root is a conservative catalog policy, not proof of
engine copy-body or FaceGen ownership behavior. Otherwise, when the
resolved body is humanoid (a part path contains `malebody` or `femalebody`), the race/sex default
head is used (`head_source = 'default'`):
`meshes/actors/character/character assets/` plus `malehead.nif`/`femalehead.nif`, with the
`maleheadargonian.nif`/`maleheadkhajiit.nif` variants when the race EditorID names Argonian or
Khajiit. A creature body never gets a humanoid default head.

**Path bases.** `head_model` is archive-root-relative: it starts with `meshes/` and uses forward
slashes. `actor_model_parts.model_path` preserves the authored `ARMA/MOD2` or `ARMA/MOD3`
string, including its case and separators. Retail part paths are relative to `meshes/` and
omit that prefix; consumers must normalize these authored paths before asset lookup.

This slice computes the head *path string* only; it does not compose a head NIF and does not treat
`HDPT/PNAM` as the head mesh. FaceGen presence is the race flag, not a per-NPC file probe: the ESM
export path has no asset index, so PR-6 validates the path before loading.

**Reason codes.** A base that fails to resolve records the reason it was rejected: `no_rnam`,
`no_race_record`, `race_no_wnam`, `no_armo_record`, `armo_no_modl`, `no_matching_arma`. Traits
resolution and skin overrides add nine codes: `missing_traits_template`, `deleted_traits_template`,
`wrong_type_traits_template`, `traits_template_cycle`, `leveled_traits_template`,
`zero_npc_skin_override`, `missing_npc_skin_override`, `deleted_npc_skin_override`, and
`wrong_type_npc_skin_override`. A creature base with more than one slot-30 whole-skin candidate
and no normal body records `ambiguous_creature_body`. A base that reaches head resolution but has no head path also
records `no_facegen_and_no_default`; early returns for unresolved Traits, missing race links, or
missing race records do not. The reasons are not mutually exclusive, and a report that counts
them counts a base once per reason.
`unrecognized_slot` and `missing_acbs` are recorded wherever they occur. Diagnostics are printed
by the exporter and returned by `converter::actor_models::resolve_actor_models`; they are not
stored in the database.

**Resolution.** `body_head` when at least one part and a head path resolve, `body` when parts
resolve without a head, and `unresolved` otherwise. A base with no effective-root `RNAM` is still
written with `race_id = 0` and `resolution = 'unresolved'`. Any unresolved Traits chain also
produces one row for the original NPC with `race_id = 0`, placeholder `sex = 'male'`, and no parts
or head, so it is recorded rather than dropped.

**Ownership.** A complete load-order export drops and rebuilds both tables in the record
transaction. A subset export, such as movement annotation, leaves them alone: a partial record map
cannot resolve an `NPC_` whose race or skin it did not receive, so it must not overwrite a complete
row with an unresolved one. A diagnostic archive (which defers every runtime view) leaves both
tables empty. `NPC_` bases are never written into `statics`.

**These tables are optional and do not bump `shared::WORLD_DATABASE_SCHEMA_VERSION`** (still 9):
they only add tables, and consumers must check that they exist before reading them. No mandatory
join is added to `REFERENCE_JOIN`; a database without them still opens and renders statics.
### 18a. Worn Outfit Catalog (`actor-outfits.json`)

A sidecar file beside the world database, not a table. It holds what each resolved `NPC_` base
wears by default: `NPC_/DOFT` -> `OTFT/INAM` -> `ARMO/MODL` -> `ARMA`, with each armature matched to
the base's race and sex by the same rule skin armor uses (exact or additional race first, then
`DefaultRace`; `MOD3` for female with a `MOD2` fallback) and named as a converted `meshes/**.glb`
scene. The type is `shared::actor_outfits::ActorOutfitCatalog`; `version` is 1.

Each row is `npc_id`, `outfit_id` and `parts`. A part is `armo_id`, `armature_id`, `biped_mask`
(`ARMA/BOD2` slots: body bit 2, hands bit 3, feet bit 7), `armature_race_id` and `scene`. A race
armature wins its slots over a `DefaultRace` armature of the same `ARMO`. A part whose scene is not
in the pack is skipped. `skipped` counts every item that did not become a part, by reason:
`leveled_list`, `not_armor`, `item_missing`, `armor_without_armature`, `no_compatible_armature`,
`armature_without_slots`, `armature_without_model`, `invalid_model_path`,
`converted_model_missing`.

Scope: the default outfit only. Leveled lists, inventory armor (`CNTO`) and the sleeping outfit
(`SOFT`) are not resolved. An actor with a Use Inventory template and no own `DOFT` takes the
template's outfit. The own `DOFT` wins when both exist (9 retail bases differ; not checked against
the game).

Producer: `converter derive-outfits <clone> --registry <packs.json> --clone-of <pack>` reads the
records the pack already holds and writes `actor-outfits.json` and `actor-outfits-receipt.json`
into an APFS clone of a registered pack. It reads no game data, needs no conversion, and refuses a
registered pack as target. The engine reads the file if present, validates it whole, and spawns
actors in their skin when it is absent or invalid.

### 19. Projection Omissions (`projection_omission_stats`, `projection_omission_samples`)

Runtime projections omit unusable rows or fields without inventing authored data. Each
omission is counted under a stable category so the conversion report can include it (#297).
Light, grass, placed-record and RACE animation projections in
`crates/converter/src/esm/exporter.rs`, lighting-template projection in `esm/environment.rs`,
and emittance resolution contribute categories. The category code is
`crates/converter/src/esm/projection_omissions.rs`.

```sql
CREATE TABLE IF NOT EXISTS projection_omission_stats (   -- one row per category with omissions
    category TEXT PRIMARY KEY,     -- stable name, see the table below
    severity TEXT NOT NULL,        -- 'data_loss', 'fallback' or 'omitted'
    count INTEGER NOT NULL         -- exact number of records left out
);
CREATE TABLE IF NOT EXISTS projection_omission_samples ( -- the lowest FormIDs, at most 8 per category
    category TEXT NOT NULL,
    form_id INTEGER NOT NULL,      -- the omitted record, or the XEMI target
    reason TEXT NOT NULL,          -- human-readable detail, e.g. the DATA length
    PRIMARY KEY (category, form_id)
) WITHOUT ROWID;
```

| Category | Record left out | Severity |
|---|---|---|
| `light_missing_data` | `LIGH` without `DATA`: no `lights` row | data loss |
| `light_short_data` | `LIGH` whose `DATA` is shorter than the 20 bytes the row needs: no `lights` row | data loss |
| `grass_unusable_data` | `GRAS` with missing or short `DATA`: unavailable scalar columns NULL, readable scalar prefix/model/identity retained | data loss |
| `reference_unusable_data` | Placed record with missing or short `DATA`: no placement or spatial row; canonical record retained | data loss |
| `reference_missing_base` | Placed record without a four-byte `NAME`: no placement or spatial row | data loss |
| `reference_short_scale` | Placed record whose `XSCL` is present but shorter than four bytes: no placement or spatial row | data loss |
| `reference_missing_cell` | Placed record with no parent cell: no placement or spatial row | data loss |
| `worldspace_missing_editor_id` | `WRLD` without `EDID`: no `worldspaces` row | omitted |
| `land_missing_heights` | `LAND` without `VHGT`: no `land` row | omitted |
| `land_unusable_heights` | `LAND` whose `VHGT` is present but empty: no `land` row | data loss |
| `armature_missing_race` | `ARMA` without a readable `RNAM`: `actor_model_parts.armature_race_id` is NULL | data loss |
| `room_marker_unreadable_flags` | Room marker whose `XRMR` header is present but short, repeated or unframed: no `room_bounds` row; its cell is incomplete | data loss |
| `occluder_unusable_geometry` | PlaneMarker with unusable geometry has no occluder row and leaves its cell incomplete | data loss |
| `occluder_unusable_links` | PlaneMarker with a malformed `XORD` or unresolved target leaves its cell incomplete | data loss |
| `lighting_template_unusable_data` | `LGTM` with missing or unsupported-size `DATA`: DATA lighting columns NULL, other fields retained | data loss |
| `race_unreadable_authoring` | `RACE` with unreadable animation model slots: no `race_animation_sources` row | data loss |
| `emittance_target_absent` | `XEMI` target not in the load order, or deleted: no `emittance` row | fallback |
| `emittance_light_unusable_data` | `XEMI` target `LIGH` without a usable `DATA`: no `emittance` row | fallback |
| `emittance_region_no_weather` | `XEMI` target `REGN` whose `RDWT` names no weather: no `emittance` row | fallback |
| `emittance_weather_no_lighting` | `XEMI` target `REGN` whose weather has no Effect Lighting colours: no `emittance` row | fallback |
| `emittance_target_wrong_type` | `XEMI` target that is neither a `LIGH` nor a `REGN`: no `emittance` row | fallback |

**Severity.** A missing light or placement row, incomplete grass or template DATA, or
unreadable RACE animation authoring loses all or part of a supported projection. Readable
GRAS scalars remain authored values. Only unavailable scalar spans become NULL. Each
unreadable RACE is counted once, regardless of how many NPCs use it. A
`data_loss` category adds one line to the conversion's `warnings` and makes the output
incomplete. A reference whose emittance target does not resolve keeps the model's own emission
(section 16), the documented fallback, so a `fallback` category adds one line to `notices` and the
output stays complete. A severity the reader does not know is read as `data_loss`, so a category
added by a newer converter cannot pass as success.

An `omitted` category is an intended omission (#811): the record is in the source, an optional or
required field is absent, and no value is invented. It adds one line to `notices`, adds a row to
the report's and manifest's `intended_omissions`, and leaves the output complete. Only an absent
field of an otherwise sound record qualifies. A field that is present but short or unreadable
(`reference_short_scale`, `room_marker_unreadable_flags`), a missing placement base or cell, and a
missing armature race stay `data_loss` until retail evidence shows they are authored absences (unverified).

Placement omission reasons describe the `DATA` available to the runtime projection.
An in-house typed-field rejection can make it unavailable there even though the original
short field remains in `inhouse_source_records.payload`. The low-level `insert_reference`
subset helper has no report sink: it warns on the terminal but does not update these
omission tables or an integration report. Full load-order exports persist the omission.

A full load-order export clears and rewrites both tables inside its transaction, so a re-export
replaces the counts instead of adding to them, and a repaired load order leaves the tables empty.
The movement-annotation subset export leaves them alone, and a diagnostic archive leaves them
empty. `finalize_world_database` reads them into `projection_omissions` in
`integration-report.json`, so a metadata rebuild or a reused database reports the same
omissions. A database without the tables reports none.

**These tables are optional and did not bump `shared::WORLD_DATABASE_SCHEMA_VERSION`**: they only
add tables, `create_tables` adds them to an older reused database, and the engine does not read
them.

### 20. Per-Base Texture Assignments (`base_texture_swaps`)

Optional STAT MODS (alternate texture) projection; world schema version is unchanged.

```sql
CREATE TABLE base_texture_swaps (
    base_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    shape_name TEXT NOT NULL,
    shape_index INTEGER NOT NULL,
    texture_set_id INTEGER NOT NULL,
    disposition TEXT NOT NULL,
    PRIMARY KEY (base_id, position)
);
```

IDs use the reader's resolved load-order numbering. Every MODS entry gets a row, in authored
order (`position`); no entry is dropped or gated. `shape_index` is the signed 3D Index: retail
keys entries by it, skips -1 and lets a later entry with the same index win (RF-TEX-001), and the
index counts geometry leaves in a pre-order depth-first walk of the model (RF-TEX-002).
`shape_name` is the 3D Name (with its `:N` suffix) as record data; the retail apply path does not
read it. `disposition` is `texture_set` (a live TXST; the runtime reads its slots from
`texture_sets`), `null_texture_set` (the target is FormID 0x28, the engine's NullTextureSet, which
hides the shape; RF-TEX-005) or `missing_texture_set` (deleted, wrong type or absent). Owner updates clear the base's
rows before inserting replacements; complete exports replace the table transactionally and
archive-only exports leave it empty. Subset exporters must supply referenced TXST records
alongside the bases whose appearance they update. All eight existing `texture_sets`
columns retain their consumers. See [per-record object appearance](../engine/object-materials.md)
for selection, instance isolation, retail coverage, diagnostics and the unimplemented MATO limits.

### 21. Authored local-light projections (#264/#364)

Optional additions preserve compatibility with schema 3–9 packages. `lights.fade_status` and
`references.radius_status` are nullable INTEGER columns: NULL means an older projection, 0 absent,
1 valid (including zero; signed XRDS is legal), 2 malformed/non-finite (negative FNAM unsupported).
For a valid LIGH reference the effective radius is base DATA radius plus raw XRDS, not replacement.
Invalid inputs retain their original bytes in the existing canonical record archive.
Repeated subrecords read the same way in the legacy and the in-house reader. A repeated `LIGH`
`FNAM`: the first occurrence decides, so a short one followed by a valid one gives status 2. A repeated
`REFR` `XRDS`: the first four-byte one wins, and a malformed one counts only when none is well-formed.

`reference_light_data(reference_id INTEGER PRIMARY KEY, data_size INTEGER NOT NULL, fov_offset REAL,
fade_offset REAL, end_distance_cap REAL, shadow_depth_bias REAL)` projects REFR XLIG without
evaluating it. Missing XLIG has no row. Only 16/20-byte layouts yield floats; non-finite values are
NULL. A complete export recreates the table; subset export removes/replaces rows for each exported
reference. No FormIDs occur within XLIG, so remapping is unnecessary.

See [point-lights.md](../engine/point-lights.md) for pinned layout sources, the bounded backend
transform, fallback dispositions and unresolved native equations. Reconversion into a new pack is
required for new columns; registered packs remain immutable.

## Optional terrain default metadata

`terrain_default_texture` is generated after texture conversion for every pack. Its singleton
row has `id = 0`, nullable `diffuse_path` and `normal_path` (canonical relative KTX2 paths),
and nullable `source_diffuse` (the canonical DDS path used by the LOD bake). A null diffuse
means white and has no normal or source. A null diffuse is written only for `--terrain-default
white` (a labelled test option) or when no cell needs the default. The engine probes for the table
so old databases keep their white default. A `skyrim-ini` default diffuse that cannot be used
fails the conversion; no generated white texture replaces it (#811). Invalid normal metadata
warns and disables only the normal, keeping a valid diffuse.
This additive metadata does not change LAND geometry, the cell-cache format, or texture
producer versions.
