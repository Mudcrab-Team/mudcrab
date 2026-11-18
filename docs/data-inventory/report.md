# World-data inventory

Schema status: **not_extracted**. Reviewed source: `64108a1c38b0a854d4f82d7031ea50b57c336a5a`.

Prepare dependencies are not executed rows, mapped rows, semantic use, population, or retail parity.

## Denominators

- rusqlite_candidates: 91
- emitted_schema_columns: unknown
- manually_traced_fields: 18
- named_external_stores: 3

## Traced examples

| Field | Decoded | Preserved | Projected | Query-dependent | Mapped | Semantic use | Role / reason |
|---|---|---|---|---|---|---|---|
| `cells.data` | unknown | source_preserved | source_projected | unknown | unknown | unknown | provenance: Opaque archive/payload disposition. Preservation does not establish interpretation of every subrecord. Current image-space reader decodes selected records.data; no blanket unused claim. |
| `emittance.day_b` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | result: Day color maps through ReferenceEmittance to lit_by; other time slots are not implied. |
| `emittance.day_g` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | result: Day color maps through ReferenceEmittance to lit_by; other time slots are not implied. |
| `emittance.day_r` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | result: Day color maps through ReferenceEmittance to lit_by; other time slots are not implied. |
| `inhouse_source_records.payload` | unknown | source_preserved | source_projected | unknown | unknown | unknown | provenance: Opaque archive/payload disposition. Preservation does not establish interpretation of every subrecord. Current image-space reader decodes selected records.data; no blanket unused claim. |
| `lights.fade_status` | source_decoded | source_preserved | source_projected | unknown | unknown | source_sink | result: Status of the LIGH FNAM fade: 0 absent, 1 valid, 2 malformed or negative. evaluate_light treats status 2 as unit fade (#488). The SELECT mapping is not verified: the column is optional and added by a format! fragment. |
| `lights.falloff` | source_decoded | source_preserved | source_projected | unknown | unknown | unknown | result: Published LIGH falloff. Since #488 it reaches LightRow.falloff and evaluate_light reads it only for lights whose flags carry a shadow mode (0x1C00); the renderer does not apply it (counted as unsupported_falloff). No mapping in the audited ReferenceQuery light suffix; other paths remain unknown. |
| `lights.radius` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | result: Source path: evaluate_light adds the reference's signed XRDS offset to this radius (#488) and sets PointLight.range through backend_range; the 0.1 fallback, the 8192 cap and the intensity normalization remain separate backend policy. No native parity claim. |
| `movement_types.forward_run` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | result: Selected NPC_Default_MT speed feeds directional target_speed. Selection and sprint multiplier remain #264 research gaps. |
| `race_movement_links.run_movt_id` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | validation: Read to reject unsupported non-null race selection; not proof that the linked movement profile controls speed. Active PR #246 owns selection changes. |
| `race_movement_links.walk_movt_id` | source_decoded | source_preserved | source_projected | source_reference | source_mapping | source_sink | validation: Read to reject unsupported non-null race selection; not proof that the linked movement profile controls speed. Active PR #246 owns selection changes. |
| `records.data` | unknown | source_preserved | source_projected | unknown | unknown | unknown | provenance: Opaque archive/payload disposition. Preservation does not establish interpretation of every subrecord. Current image-space reader decodes selected records.data; no blanket unused claim. |
| `reference_light_data.data_size` | source_decoded | source_preserved | source_projected | unknown | unknown | source_sink | result: Byte size of the XLIG subrecord. evaluate_light accepts only 16 or 20 bytes and counts any other size as malformed reference data (#488). |
| `reference_light_data.fade_offset` | source_decoded | source_preserved | source_projected | unknown | unknown | source_sink | result: Second float of the REFR XLIG subrecord (xEdit name; not traced to a retail row beyond RF-LIGHT-GPU-007). evaluate_light adds it to the base fade (#488). A 16 or 20 byte XLIG is the only valid size. |
| `references.data` | unknown | source_preserved | source_projected | unknown | unknown | unknown | provenance: Opaque archive/payload disposition. Preservation does not establish interpretation of every subrecord. Current image-space reader decodes selected records.data; no blanket unused claim. |
| `references.radius_status` | source_decoded | source_preserved | source_projected | unknown | unknown | source_sink | result: Status of the reference XRDS radius offset: 0 absent, 1 valid, 2 malformed or not finite. evaluate_light rejects the radius when the status is 2 (#488). The SELECT mapping is not verified: the column is optional and added by a format! fragment. |
| `waters.deep_color` | source_decoded | source_preserved | source_projected | source_reference | unknown | source_sink | filter, result: Value maps to WaterColors.deep and then, through render::water_deep_color, to the water material base colour. Opacity remains an approximation; native parity unproven. The SELECT mapping is no longer verified: the water-colors query is built with format! for its optional opacity and data columns, and the lexical scan resolves only plain literals and constants. |
| `waters.shallow_color` | source_decoded | source_preserved | source_projected | source_reference | unknown | source_sink | filter, result: Non-NULL predicate gates eligibility; value maps to WaterColors.shallow. Since #299 the water material setup passes it to WaterExtension::with_body and the underwater pass reads it. The SELECT mapping is no longer verified: the water-colors query is built with format! for its optional opacity and data columns, and the lexical scan resolves only plain literals and constants. |

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
- `lights.fade_status`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/lights.rs](../../crates/engine/src/lights.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `lights.falloff`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `lights.radius`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/lights.rs](../../crates/engine/src/lights.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `movement_types.forward_run`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/physics.rs](../../crates/engine/src/physics.rs). Exact anchors in inventory.json.
- `race_movement_links.run_movt_id`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/physics.rs](../../crates/engine/src/physics.rs). Exact anchors in inventory.json.
- `race_movement_links.walk_movt_id`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/physics.rs](../../crates/engine/src/physics.rs). Exact anchors in inventory.json.
- `records.data`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/world/imagespace.rs](../../crates/engine/src/world/imagespace.rs). Exact anchors in inventory.json.
- `reference_light_data.data_size`: [crates/converter/src/esm/reference_links.rs](../../crates/converter/src/esm/reference_links.rs), [crates/engine/src/lights.rs](../../crates/engine/src/lights.rs). Exact anchors in inventory.json.
- `reference_light_data.fade_offset`: [crates/converter/src/esm/reference_links.rs](../../crates/converter/src/esm/reference_links.rs), [crates/engine/src/lights.rs](../../crates/engine/src/lights.rs). Exact anchors in inventory.json.
- `references.data`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/world/imagespace.rs](../../crates/engine/src/world/imagespace.rs). Exact anchors in inventory.json.
- `references.radius_status`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/lights.rs](../../crates/engine/src/lights.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `waters.deep_color`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/streaming.rs](../../crates/engine/src/streaming.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.
- `waters.shallow_color`: [crates/converter/src/esm/exporter.rs](../../crates/converter/src/esm/exporter.rs), [crates/engine/src/streaming.rs](../../crates/engine/src/streaming.rs), [crates/engine/src/world/database.rs](../../crates/engine/src/world/database.rs). Exact anchors in inventory.json.

## Retained legacy dispositions

- `room_portals pre-pair_index`: Retained legacy migration; old primary key is not a current emitted shape.
- `door_links pre-destination_name`: Optional legacy column migration; missing door table uses NULL query suffix.
- `terrain unresolved_texture_ids JSON`: Owned legacy snapshot replaced by scalar diagnostic rows, not a second runtime store.

## Query extraction

| Candidate | Status |
|---|---|
| `crates/engine/src/app.rs:987` execute | literal |
| `crates/engine/src/app.rs:988` execute | literal |
| `crates/engine/src/app.rs:992` execute_batch | unknown |
| `crates/engine/src/app.rs:1007` execute | literal |
| `crates/engine/src/app.rs:1012` prepare | literal |
| `crates/engine/src/app.rs:1016` execute | unknown |
| `crates/engine/src/app.rs:1021` execute | literal |
| `crates/engine/src/app.rs:1025` execute | literal |
| `crates/engine/src/app.rs:2631` query_row | unknown |
| `crates/engine/src/app.rs:2666` query_row | unknown |
| `crates/engine/src/bin/world-inspect.rs:323` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:347` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:498` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:519` query_row | unknown |
| `crates/engine/src/bin/world-inspect.rs:564` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:573` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:591` query_row | literal |
| `crates/engine/src/bin/world-inspect.rs:599` prepare | literal |
| `crates/engine/src/bin/world-inspect.rs:683` prepare | unknown |
| `crates/engine/src/bin/world-inspect.rs:718` prepare | unknown |
| `crates/engine/src/bug_report/snapshot.rs:697` query_row | literal |
| `crates/engine/src/bug_report/snapshot.rs:706` query_row | literal |
| `crates/engine/src/cell_commands.rs:108` query_row | literal |
| `crates/engine/src/cell_commands.rs:136` query_row | unknown |
| `crates/engine/src/object_materials.rs:69` query_row | literal |
| `crates/engine/src/object_materials.rs:79` prepare | literal |
| `crates/engine/src/object_materials.rs:97` prepare | literal |
| `crates/engine/src/object_materials.rs:111` query_row | unknown |
| `crates/engine/src/physics/carry.rs:223` prepare | literal |
| `crates/engine/src/physics.rs:115` query_row | literal |
| `crates/engine/src/physics.rs:123` query_row | literal |
| `crates/engine/src/physics.rs:126` query_row | literal |
| `crates/engine/src/physics.rs:137` query_row | literal |
| `crates/engine/src/physics.rs:150` query_row | literal |
| `crates/engine/src/room_culling.rs:655` prepare_cached | literal |
| `crates/engine/src/room_culling.rs:657` query_row | unknown |
| `crates/engine/src/room_culling.rs:670` prepare_cached | literal |
| `crates/engine/src/room_culling.rs:692` prepare_cached | literal |
| `crates/engine/src/room_culling.rs:715` prepare_cached | literal |
| `crates/engine/src/room_culling.rs:754` prepare_cached | literal |
| `crates/engine/src/route.rs:350` query_row | unknown |
| `crates/engine/src/streaming/collision_placement_probe.rs:52` query_row | literal |
| `crates/engine/src/streaming/lod.rs:935` prepare | unknown |
| `crates/engine/src/world/actor_catalog.rs:176` prepare | literal |
| `crates/engine/src/world/actor_catalog.rs:200` query_row | literal |
| `crates/engine/src/world/actor_catalog.rs:220` prepare | unknown |
| `crates/engine/src/world/actor_catalog.rs:221` prepare | unknown |
| `crates/engine/src/world/actor_catalog.rs:280` prepare | literal |
| `crates/engine/src/world/database.rs:559` prepare | literal |
| `crates/engine/src/world/database.rs:574` prepare | literal |
| `crates/engine/src/world/database.rs:588` prepare | literal |
| `crates/engine/src/world/database.rs:606` prepare | literal |
| `crates/engine/src/world/database.rs:607` query_row | unknown |
| `crates/engine/src/world/database.rs:623` prepare | unknown |
| `crates/engine/src/world/database.rs:672` query_row | literal |
| `crates/engine/src/world/database.rs:678` query_row | literal |
| `crates/engine/src/world/database.rs:981` query_row | literal |
| `crates/engine/src/world/database.rs:1127` query_row | literal |
| `crates/engine/src/world/database.rs:1134` query_row | literal |
| `crates/engine/src/world/database.rs:1146` prepare_cached | literal |
| `crates/engine/src/world/database.rs:1183` query_row | literal |
| `crates/engine/src/world/database.rs:1206` query_row | literal |
| `crates/engine/src/world/database.rs:1230` query_row | literal |
| `crates/engine/src/world/database.rs:1238` query_row | literal |
| `crates/engine/src/world/database.rs:1248` prepare_cached | literal |
| `crates/engine/src/world/database.rs:1290` query_row | literal |
| `crates/engine/src/world/database.rs:1487` prepare_cached | literal |
| `crates/engine/src/world/database.rs:1488` query_row | unknown |
| `crates/engine/src/world/database.rs:1496` prepare_cached | literal |
| `crates/engine/src/world/database.rs:1497` query_row | unknown |
| `crates/engine/src/world/database.rs:1521` prepare_cached | literal |
| `crates/engine/src/world/database.rs:1547` prepare_cached | literal |
| `crates/engine/src/world/database.rs:1550` query_row | unknown |
| `crates/engine/src/world/database.rs:1557` prepare_cached | literal |
| `crates/engine/src/world/database.rs:1561` query_row | unknown |
| `crates/engine/src/world/database.rs:1634` query_row | literal |
| `crates/engine/src/world/database.rs:1657` query_row | literal |
| `crates/engine/src/world/database.rs:1668` query_row | literal |
| `crates/engine/src/world/database.rs:1680` query_row | literal |
| `crates/engine/src/world/database.rs:1716` query_row | literal |
| `crates/engine/src/world/database.rs:1733` prepare_cached | unknown |
| `crates/engine/src/world/database.rs:1743` prepare_cached | unknown |
| `crates/engine/src/world/database.rs:1754` prepare_cached | unknown |
| `crates/engine/src/world/database.rs:1755` query_row | unknown |
| `crates/engine/src/world/environment.rs:606` prepare | literal |
| `crates/engine/src/world/environment.rs:645` prepare | literal |
| `crates/engine/src/world/environment.rs:739` prepare | literal |
| `crates/engine/src/world/environment.rs:890` prepare | unknown |
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
