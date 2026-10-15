# Interior room graph: converter design (#188, converter half)

Status: implemented in the converter (2026-10-06); this note is the design it follows. The engine
half (culling) is a later PR and is only sketched here where the table shape depends on it.

## Summary

The converter decodes Skyrim's room markers and portal markers into four small optional tables
in `skyrim_world.db`, and assigns every placed object in a roomed interior to the rooms whose box
contains its pivot. The tables only record facts. Which objects to draw is the engine's policy:
objects in no room or in several rooms stay drawn. A database without the tables behaves as today,
so this needs **no world DB schema bump** (the `waters` precedent, `db-schema.md` §13).

## What the data holds

All layouts below were checked against `Skyrim.esm` bytes (a scan of every REFR; numbers are
Skyrim.esm only) and against public documentation: the UESP REFR page
(<https://en.uesp.net/wiki/Skyrim_Mod:Mod_File_Format/REFR>) and its talk page, and xEdit issue
TES5Edit/TES5Edit#919 for the subrecord grouping. No disassembly was used.

| Base | FormID | Placed refs | Notes |
|---|---|---|---|
| `RoomMarker` | `0x1F` | 3,561 | a box around a room |
| `PortalMarker` | `0x20` | 2,612 | a doorway rectangle between two rooms |

Subrecords on a marker reference, besides the usual `NAME` and `DATA` (position, rotation):

| Subrecord | Size | Layout | On |
|---|---|---|---|
| `XPRM` | 32 | `float[3]` half-extents, `float[3]` colour/255, `float` unknown (0.2 room, 0.25 portal), `u32` shape (1 box, 2 sphere, 3 portal box) | every room, 2,611 portals |
| `XMBO` | 12 | `float[3]`, equal to `XPRM[0..12]` on every room (the Creation Kit rejects a mismatch) | every room |
| `XPOD` | 8 per pair | array of (origin, destination) `u32` FormID pairs (xEdit 4.1.5f defs, `wbArray` of a two-FormID struct): the rooms on either side; the masters only have one pair | 2,592 portals (121 with the second id 0, 20 without `XPOD`) |
| `XRMR` | 4 | `u8` count of `XLRM` that follow, `u8` flags, `u16` unknown (0 or 1) | 1,991 rooms |
| `LNAM` | 4 | `u32` FormID: lighting template for the room | follows `XRMR` when flag `0x80` is set |
| `INAM` | 4 | `u32` FormID: image space for the room | follows `XRMR` when flag `0x40` is set |
| `XLRM` | 4 | `u32` FormID of another room in the same cell | 1,965 links |

Checked against bytes:

- The `XRMR` count equals the number of `XLRM` on every one of the 1,991 rooms.
- The flags match the public names exactly: `0x80` with `LNAM` on 537 + 81 rooms, `0x40` with
  `INAM` on 5 + 81, never one without the other. So rooms can carry their own lighting template
  and image space (618 and 86 rooms). That is lighting data, worth passing to the lighting plan
  (#129-#135); the converter can keep it at no cost.
- The `u16` is 1 on 667 rooms and 0 on 1,324; no public source names it. Kept raw, not used.
- `XLRM` links: all 1,965 point to a room in the same cell, 1,950 are mirrored by a link back, and
  only 25 join rooms that already share a portal. So a link joins rooms that have **no portal
  between them** but must be seen together (the Creation Kit's "linked rooms"). The engine should
  treat a link as an open edge: if one room is visible, so is the other, with no narrowing.
- Room membership is not stored on objects. The Creation Kit wiki gives the rule as the bound
  "in which its pivot point is contained" (ck.uesp.net, Room Bounds and Portal Basics; seen only
  through a search snippet, as the page refused automated fetching). An earlier byte scan of
  Skyrim.esm measured that rule: in Ansilvund01 and KilkreathRuins03, 90.6% and 99.8% of
  references fall in at least one room, and 13.2% and 1.2% in more than one.
- `XPRM` holds half-extents. A portal's `XPRM.y` is always 1.0 (a thickness placeholder), and its
  doorway is the marker's local X-Z rectangle (normal along local +Y), from the same
  earlier byte scan.

Coverage: 246 of 590 interiors in Skyrim.esm have room markers, 94 have 16 or more. In the
all-plugins conversion, 4,246 room markers and 3,098 portals sit in interiors; 159 rooms and 133
portals sit in exteriors, which stay out of scope (#106 owns exterior culling).

## Proposed tables

Written by the ESM export stage from the merged, winning records, so a mod that moves, replaces or
deletes a marker is already applied. All FormIDs are in load-order numbering, like `references.id`.

```sql
-- §15. Interior room graph (optional; absent tables mean "no room data")
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

CREATE TABLE IF NOT EXISTS room_portals (      -- one row per XPOD pair of an interior PortalMarker
    ref_id INTEGER NOT NULL,                   -- not unique: one row per XPOD pair
    pair_index INTEGER NOT NULL DEFAULT 0,
    cell_id INTEGER NOT NULL,
    pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
    rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
    half_x REAL NOT NULL, half_z REAL NOT NULL, -- doorway half-width and half-height
    room_a INTEGER,                            -- the pair's origin; NULL when 0, absent or not a room_bounds row of the same cell
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
    contained INTEGER NOT NULL,                -- 1: pivot inside the box; 0: pivot outside, bounds overlap it
    PRIMARY KEY (reference_id, room_ref)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_reference_rooms_room ON reference_rooms(room_ref);

CREATE TABLE IF NOT EXISTS room_graph_stats (  -- conversion report counters
    name TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
```

Why this shape:

- **Facts, not policy.** `reference_rooms` has a `contained = 1` row per room containing the
  object's pivot, and a `contained = 0` row per further room its placed bounds overlap. No
  `contained = 1` row means "in no room", several mean "in several" (the pivot-only counters
  count these). The engine decides what to draw; per
  #188, both cases stay drawn. Nothing is lost if the engine later uses "visible if any of its
  rooms is visible" instead.
- **Small.** About 3.4k + 2.5k + 2k rows for the graph, and roughly 300k membership rows (about
  290k objects in roomed interiors, a few percent in two rooms), a few MB.
- **No raw blobs.** The marker's raw subrecords are already in `references.data`.
- **Only box rooms.** Every room marker in Skyrim.esm is a box. A room with another shape gets no
  `room_bounds` row and is counted in the conversion report, so its objects show as "in no room".

## Converter work

1. **FormID remap.** `XPOD`, `XLRM`, `LNAM` and `INAM` on references are not in the REFR FormID
   layouts today (`crates/converter/src/esm/mod.rs`, `form_id_layout`), so they keep plugin-local
   numbering. That is invisible for Skyrim.esm (slot 0) and wrong for any mod's markers. Add them
   to the layout table, with a test beside `every_covered_reference_and_cell_field_is_remapped_at_its_offsets`
   (`crates/converter/tests/plugin_references.rs`). `XPOD` is an array of pairs, so it is an
   `Array { stride: 4 }` layout and a mod's portal with several pairs is remapped in full.
2. **Graph tables.** In `export_records` (`crates/converter/src/esm/exporter.rs`), after the
   reference loop, read the markers from the parsed subrecords (not from the stored blob, which is
   rkyv's layout). Rebuild the tables each export, as the grass tables are.
3. **Membership pass.** Per interior cell with at least one room: transform each reference's
   position into each room's frame (inverse of the same Euler order the engine uses) and test
   `|local| <= half`. At about 2,200 objects and 23 rooms per large cell, this is about 50k tests,
   well under a second for the whole game. Marker references themselves are skipped.
4. **Validation counters** in the conversion report: rooms, portals, links, portals with a missing
   or dangling side, objects in no room, objects in several rooms, non-box rooms skipped. These are
   what the engine half's metrics compare against.
5. **Spec:** a new §15 in `docs/specs/converters/db-schema.md`, stating the fallback rules below.

Tests: the in-memory exporter tests (`exporter.rs`, helpers `record`, `reference`, `base`) for the
membership pass (inside, outside, two rooms, rotated room); `plugin_references.rs` for the remap;
`dummy-content`'s interior fixture for a full pipeline run.

## Fallbacks (conservative)

| Case | Converter writes | Engine (the engine half, a later PR) |
|---|---|---|
| Interior without room markers | no rows | draw everything, as today |
| Old database, tables absent | n/a | feature off, no error |
| Portal side 0, missing or not a room | `room_a`/`room_b` NULL | ignore that edge, or fall back to the rooms whose box contains the portal's centre |
| Object in no room / several | no row / several rows | always drawn |
| Camera outside every room | n/a | draw everything |
| Non-box room shape | skipped, counted | its objects are in no room, so drawn |
| Exterior markers | not written | out of scope (#106) |

## Mods and the #185 contract

The tables are derived data, rebuilt from the winning records on every conversion, so a mod that
edits a cell's markers or objects changes them with no manual step. At runtime, an object that is
enabled, moved or spawned later has no stored membership: the engine either recomputes it (cheap,
the same point test) or treats it as "in no room" and draws it. Either keeps per-object identity,
which is what #185 asks for.

## Schema

No bump. The engine opens any database from version 3 to 7 and already probes for optional tables
and columns before reading them (`has_lights`, `has_lod_table` in `crates/engine/src/world/database.rs`);
the room tables follow that pattern. A bump becomes necessary only if some consumer is allowed to
require them, and would then go in the same change that raises the engine's maximum.

## Open questions

1. The pivot rule's source is a search snippet; the page itself should be read before the engine
   half relies on it. The measured coverage supports it either way.
2. (Answered: the `contained = 0` overlap rows, written after integration from `statics.bounds_*`.)
   Large objects whose pivot sits in one room but whose mesh reaches into another can vanish too
   early. Skyrim has the same behaviour. A guard (always draw objects whose bounds are larger than
   their room) is possible later, using `statics.bounds_*`, which exist after integration.
3. The `XRMR` `u16` and the unknown `XPRM` float stay unexplained; both are kept raw or ignored.
4. Rooms hide their lights when culled. That matches Skyrim, but it is a visible behaviour change
   and belongs to the engine half's image-comparison test.
