# World-data inventory

Schema status: **not_extracted**. Reviewed source: `64108a1c38b0a854d4f82d7031ea50b57c336a5a`.

Prepare dependencies are not executed rows, mapped rows, semantic use, population, or retail parity.

## Denominators

- rusqlite_candidates: 61
- emitted_schema_columns: unknown
- manually_traced_fields: 14
- named_external_stores: 3

## Traced examples

| Field | Decoded | Preserved | Projected | Query-dependent | Mapped | Semantic use | Role / reason |
|---|---|---|---|---|---|---|---|
| `cells.data` | unknown | source_preserved | source_projected | unknown | unknown | unknown | provenance: Opaque archive/payload disposition. Preservation does not establish interpretation of every subrecord. Current image-space reader decodes selected records.data; no blanket unused claim. |
| `emittance.day_b` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | result: Day color maps through ReferenceEmittance to lit_by; other time slots are not implied. |
| `emittance.day_g` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | result: Day color maps through ReferenceEmittance to lit_by; other time slots are not implied. |
| `emittance.day_r` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | result: Day color maps through ReferenceEmittance to lit_by; other time slots are not implied. |
| `inhouse_source_records.payload` | unknown | source_preserved | source_projected | unknown | unknown | unknown | provenance: Opaque archive/payload disposition. Preservation does not establish interpretation of every subrecord. Current image-space reader decodes selected records.data; no blanket unused claim. |
| `lights.falloff` | source_decoded | source_preserved | source_projected | unknown | unknown | unknown | result: Published LIGH falloff; no mapping in the audited ReferenceQuery light suffix. Other paths remain unknown. Active PR #488 adds a consumer; it is not part of this reviewed head. |
| `lights.radius` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | result: Source path sets PointLight.range and intensity through radius_of; overrides and caps remain separate policy. No native parity claim. |
| `movement_types.forward_run` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | result: Selected NPC_Default_MT speed feeds directional target_speed. Selection and sprint multiplier remain #264 research gaps. |
| `race_movement_links.run_movt_id` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | validation: Read to reject unsupported non-null race selection; not proof that the linked movement profile controls speed. Active PR #246 owns selection changes. |
| `race_movement_links.walk_movt_id` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | validation: Read to reject unsupported non-null race selection; not proof that the linked movement profile controls speed. Active PR #246 owns selection changes. |
| `records.data` | unknown | source_preserved | source_projected | unknown | unknown | unknown | provenance: Opaque archive/payload disposition. Preservation does not establish interpretation of every subrecord. Current image-space reader decodes selected records.data; no blanket unused claim. |
| `references.data` | unknown | source_preserved | source_projected | unknown | unknown | unknown | provenance: Opaque archive/payload disposition. Preservation does not establish interpretation of every subrecord. Current image-space reader decodes selected records.data; no blanket unused claim. |
| `waters.deep_color` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | filter, result: Value maps to WaterColors.deep and StandardMaterial.base_color. Opacity remains an approximation; native parity unproven. |
| `waters.shallow_color` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | filter, result: Non-NULL predicate gates eligibility; value maps to WaterColors.shallow. No shallow-value material sink found in audited setup. Active PR #299 adds shallow color behavior; it is not part of this reviewed head. |

## External stores

- `cell_cache.rkyv`: Versioned rkyv store supplies terrain heights, normals, colors, layers, water height/type through CellCache. SQLite absence does not mean terrain is unused. Source path only; native draw evidence pending.
- `converted glTF/KTX2 assets`: External model/texture paths are assets, not SQL field payloads. This slice names the store; no asset completeness or shader-use claim.
- `merged-terrain JSON/KTX2/rkyv`: terrain/land-textures.json, KTX2 texture arrays and terrain/<worldspace>/chunk_<x>_<y>.rkyv form an alternative terrain path with per-cell fallback. Mapping is not GPU residency or draw confirmation.

## Evidence anchors

- `cells.data`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/world/imagespace.rs](../../crates/engine/src/world/imagespace.rs). Exact anchors in inventory.json.
- `emittance.day_b`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/emittance.rs](../../crates/engine/src/emittance.rs), [crates/engine/src/streaming.rs](../../crates/engine/src/streaming.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `emittance.day_g`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/emittance.rs](../../crates/engine/src/emittance.rs), [crates/engine/src/streaming.rs](../../crates/engine/src/streaming.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `emittance.day_r`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/emittance.rs](../../crates/engine/src/emittance.rs), [crates/engine/src/streaming.rs](../../crates/engine/src/streaming.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `inhouse_source_records.payload`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/world/imagespace.rs](../../crates/engine/src/world/imagespace.rs). Exact anchors in inventory.json.
- `lights.falloff`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `lights.radius`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/lights.rs](../../crates/engine/src/lights.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `movement_types.forward_run`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/physics.rs](../../crates/engine/src/physics.rs). Exact anchors in inventory.json.
- `race_movement_links.run_movt_id`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/physics.rs](../../crates/engine/src/physics.rs). Exact anchors in inventory.json.
- `race_movement_links.walk_movt_id`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/physics.rs](../../crates/engine/src/physics.rs). Exact anchors in inventory.json.
- `records.data`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/world/imagespace.rs](../../crates/engine/src/world/imagespace.rs). Exact anchors in inventory.json.
- `references.data`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/world/imagespace.rs](../../crates/engine/src/world/imagespace.rs). Exact anchors in inventory.json.
- `waters.deep_color`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/streaming.rs](../../crates/engine/src/streaming.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `waters.shallow_color`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.

## Retained legacy dispositions

- `room_portals pre-pair_index`: Retained legacy migration; old primary key is not a current emitted shape.
- `door_links pre-destination_name`: Optional legacy column migration; missing door table uses NULL query suffix.
- `terrain unresolved_texture_ids JSON`: Owned legacy snapshot replaced by scalar diagnostic rows, not a second runtime store.

## Query extraction

| Candidate | Status |
|---|---|
| `crates/engine/src/app.rs:810` execute | literal |
| `crates/engine/src/app.rs:811` execute | literal |
| `crates/engine/src/app.rs:815` execute_batch | unknown |
| `crates/engine/src/app.rs:830` execute | literal |
| `crates/engine/src/app.rs:835` prepare | literal |
| `crates/engine/src/app.rs:839` execute | unknown |
| `crates/engine/src/app.rs:844` execute | literal |
| `crates/engine/src/app.rs:848` execute | literal |
| `crates/engine/src/app.rs:2416` query_row | unknown |
| `crates/engine/src/app.rs:2451` query_row | unknown |
| `crates/engine/src/bin/world-inspect.rs:271` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:292` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:418` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:427` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:445` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:453` prepare | literal |
| `crates/engine/src/bin/world-inspect.rs:536` prepare | unknown |
| `crates/engine/src/bug_report/snapshot.rs:690` query_row | literal |
| `crates/engine/src/bug_report/snapshot.rs:699` query_row | literal |
| `crates/engine/src/cell_commands.rs:104` query_row | literal |
| `crates/engine/src/cell_commands.rs:132` query_row | unknown |
| `crates/engine/src/physics.rs:109` query_row | literal |
| `crates/engine/src/physics.rs:117` query_row | literal |
| `crates/engine/src/physics.rs:120` query_row | literal |
| `crates/engine/src/physics.rs:131` query_row | literal |
| `crates/engine/src/physics.rs:144` query_row | literal |
| `crates/engine/src/route.rs:350` query_row | unknown |
| `crates/engine/src/streaming/lod.rs:900` prepare | unknown |
| `crates/engine/src/world/actor_catalog.rs:140` query_row | literal |
| `crates/engine/src/world/actor_catalog.rs:160` prepare | unknown |
| `crates/engine/src/world/actor_catalog.rs:161` prepare | unknown |
| `crates/engine/src/world/actor_catalog.rs:220` prepare | literal |
| `crates/engine/src/world/database.rs:244` prepare | literal |
| `crates/engine/src/world/database.rs:259` prepare | literal |
| `crates/engine/src/world/database.rs:273` prepare | literal |
| `crates/engine/src/world/database.rs:288` prepare | literal |
| `crates/engine/src/world/database.rs:592` query_row | literal |
| `crates/engine/src/world/database.rs:599` query_row | literal |
| `crates/engine/src/world/database.rs:609` query_row | literal |
| `crates/engine/src/world/database.rs:636` query_row | literal |
| `crates/engine/src/world/database.rs:647` prepare_cached | literal |
| `crates/engine/src/world/database.rs:676` query_row | literal |
| `crates/engine/src/world/database.rs:870` prepare_cached | literal |
| `crates/engine/src/world/database.rs:871` query_row | unknown |
| `crates/engine/src/world/database.rs:879` prepare_cached | literal |
| `crates/engine/src/world/database.rs:880` query_row | unknown |
| `crates/engine/src/world/database.rs:904` prepare_cached | literal |
| `crates/engine/src/world/database.rs:930` prepare_cached | literal |
| `crates/engine/src/world/database.rs:933` query_row | unknown |
| `crates/engine/src/world/database.rs:940` prepare_cached | literal |
| `crates/engine/src/world/database.rs:944` query_row | unknown |
| `crates/engine/src/world/database.rs:1016` query_row | literal |
| `crates/engine/src/world/database.rs:1063` query_row | literal |
| `crates/engine/src/world/database.rs:1080` prepare_cached | unknown |
| `crates/engine/src/world/database.rs:1090` prepare_cached | unknown |
| `crates/engine/src/world/database.rs:1101` prepare_cached | unknown |
| `crates/engine/src/world/database.rs:1102` query_row | unknown |
| `crates/engine/src/world/environment.rs:486` prepare | unknown |
| `crates/engine/src/world/imagespace.rs:228` prepare | literal |
| `crates/engine/src/world/imagespace.rs:245` query_row | literal |
| `crates/engine/src/world/imagespace.rs:266` prepare | literal |

## Limits

- Committed inventory is a source-only view; requested emitted reports add actual schema/query dependencies. Column denominators sum shape-specific declarations, not unique fields or populated rows.
- No inferred table population. Prepare authorizer events are dependencies, including predicates/joins/count-only empty-column table reads; executed and mapped rows stay unknown.
- Lexical candidate scan covers dot-call prepare/prepare_cached/query_row/query_row_and_then/execute/execute_batch, plain strings and same-file string constants. It skips cfg(test) items and *_tests.rs. It is not receiver/type/macro/ORM analysis; other dynamic SQL remains unknown.
- Source fingerprints conservatively guard projection declarations and reader evidence. A changed file requires explicit review even for an unrelated change; no all-fields-consumed gate.
- Mapping annotations check selected SELECT-list ordinal/expression and prepared dependency, not every closure or runtime conversion. Source sinks and named existing tests do not mean those tests ran in this task.
- Python SQLite version, R-tree support and authorizer behavior may differ from bundled rusqlite. Rust prepares composed variants independently; cross-version failures remain visible.
- Schema-driven decoding and reader identities remain authoritative. No second record decoder, live engine instrumentation, conversion campaign or native semantic/parity evidence is provided.
