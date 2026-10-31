# World-data inventory (#298)

The inventory keeps declaration, decoding, preservation, query dependency, mapping and
behavioral evidence separate. No state implies the next. `source_*` means an explicit
source annotation, not a test result. `unknown` never means unused. A declared table is
not evidence that any row was published.

The checked-in [report](report.md) and [JSON](inventory.json) contain the reviewed source
inventory and small light/water/movement traces. Their schema status is `not_extracted`
because this committed view is source-only. The Rust extraction test passed locally;
[validation.md](validation.md) records the retained report and identities. Applicable CI
runs upload the full emitted-schema report as the `world-data-inventory` artifact.
[fixture-report.md](fixture-report.md) demonstrates the same table/column report using
disposable synthetic SQLite fixtures.

## Generate and check

Python 3.9+ and its standard-library SQLite module suffice. R-tree support is required
for the emitted schema and fixture controls.

```sh
python3 -B -m unittest discover -s tools/world_data_inventory -p 'test_*.py' -v
python3 -B tools/world_data_inventory/inventory.py --check
```

The scripts CI job runs both commands when inventory, converter/shared, engine source,
or workflow files change. The Rust test job runs
`world::database::inventory_tests::world_data_inventory_extraction` in its normal suite
and retains its extraction, JSON inventory and Markdown report as `world-data-inventory`.

For local executable extraction, read the storage guide and discover the shared paths
first. Use the installed wrapper when this checkout lacks `scripts/workspace.py`:

```sh
inventory_workspace="${MUDCRAB_WORKSPACE_ROOT:-$(git rev-parse --path-format=absolute --git-common-dir)/../.mudcrab-local}"
python3 "$inventory_workspace/bin/workspace.py" paths
# Optional: set WORLD_DATA_INVENTORY_OUTPUT to a task-owned directory under shared evidence/.
# Without it, extraction and reports are validated in a disposable directory.
python3 "$inventory_workspace/bin/workspace.py" cargo --isolated issue298 test --locked -p engine --lib world::database::inventory_tests::world_data_inventory_extraction -- --exact
```

The test executes `create_tables`, a small synthetic legacy publication, and a small
synthetic in-house record-bundle publication. It captures `sqlite_master`, real
`pragma_table_info`, schema/producer/cache versions and reader identities. The bundle
path also creates provenance/script/terrain tables absent from `create_tables`; neither
path converts meshes/textures nor launches an engine. This is not a full install campaign.
No row counts are used as population claims.

The test composes exterior, interior, parent and exterior-cell lookup SQL through the
same production builder as `load_cell`. It prepares all 64 independent selector masks
(records, lights, radius override, doors, emittance, enable state), six partial-enable
rejections, two single-prerequisite emittance shapes and an incomplete-door shape.
The current legacy and in-house publications also prepare their own query variants.
These are synthetic compatibility shapes derived by dropping current tables/columns,
not a claim to reconstruct every historical package. Other retained legacy migrations
have explicit manual dispositions.

The generator replays emitted DDL into cache-disabled in-memory databases, checks the
replayed columns against emitted metadata, and uses SQLite's authorizer while compiling
`EXPLAIN`. The original query is never stepped. Count-only empty-column reads remain
table dependencies. Python and bundled rusqlite SQLite versions are retained in the
extracted report identity; incompatible replay/preparation fails visibly.

## Maintain classifications

[dispositions.json](../../tools/world_data_inventory/dispositions.json) follows
[dispositions.schema.json](../../tools/world_data_inventory/dispositions.schema.json).
Selected fields carry independent states, roles, owner, reason, exact source anchors,
mapping ordinals/expressions and sink/test references. Existing test references identify
relevant evidence; they do not assert that those tests ran in this task. All other table
fields have explicit deferred/provenance policies; BLOBs have an opaque disposition.
Named external stores remain separate from SQL accounting.

The source guard is intentionally conservative. It hashes converter/shared producers,
candidate SQL readers and annotated evidence, including new files. Changes require
review even when only an unrelated function changed. This prevents a source-only CI
check from silently accepting new columns without executing Rust. The Rust test also
rejects unknown emitted tables, missing annotated fields, replay mismatches, broken
anchors and selected mapping/dependency drift. It does not demand a reader for every
future gameplay table.

After reviewing changed projections and updating dispositions/anchors:

```sh
python3 -B tools/world_data_inventory/inventory.py --review-sources
python3 -B tools/world_data_inventory/inventory.py --check
```

Review the fingerprint and report diffs. Do not use regeneration to substitute for
classification review. An emitted extraction can also be analyzed with
`inventory.py --extraction /path/to/extraction.json --output /path/to/report`.
Its source commit and reviewed-source hash identify the evidence; it is not transferable
to another checkout just because the world schema version matches.

## Evidence limits and next instrumentation

The lexical scan resolves plain Rust string literals and same-file string constants at
known dot-call methods. It skips `#[cfg(test)]` items and test files. It does not resolve
receiver types, arbitrary macros, ORM SQL, caller parameters, generic method syntax or
every conditional-compilation form. Unresolved candidates remain `unknown`; unenumerated
dynamic paths are also unknown. SQLite resolves aliases, joins and predicates for known
SQL; manual roles distinguish lookup/filter/validation/result/provenance. Selected
SELECT-list checks validate positional expressions and prepared dependencies, not every
row-mapping closure. Schema column denominators count declarations per extracted shape.

The current traces preserve important distinctions:

- Light falloff is published but absent from the audited light suffix. Day emittance
  reaches a material multiplier; other time slots are not implied.
- Shallow water color gates row eligibility and maps into a DTO. The audited material
  setup uses deep color; no downstream shallow-value sink is established here.
- Race links reject unsupported selection. Selected directional MOVT speeds affect the
  movement target; this does not validate selection rules or sprint semantics.
- Terrain uses `cell_cache.rkyv`, merged JSON/KTX2/rkyv and fallback paths outside SQLite.
- Current `world/imagespace.rs` reads selected `records.data` archives. The older audit's
  blanket claim that the engine never reads BLOBs is not retained.

Optional runtime instrumentation should assign stable query IDs/callsites, use statement
tracing on **every** connection (startup/catalog/worker/tooling), and retain executions,
errors, resets/reuse, returned rows and successful mapping counts separately. Cached
statement execution need not produce another authorizer event. Neither tracing nor DTO
access certifies a shader, draw readiness or gameplay effect.

Semantic claims need explicit sinks and field-intervention tests that change one stored
input and observe the actual consumer. CPU fixtures can establish their own scope;
render/physics and Skyrim parity claims require candidate-specific native evidence.
No native qualification or exhaustive consumption percentage is provided here.

## Coordination

Checked live on 2026-10-31 against local source
`64108a1c38b0a854d4f82d7031ea50b57c336a5a`. No duplicate inventory implementation appeared
in the open PR list. #269 owns publication gaps, #264 authored resolution/provenance and
#162 Phase 1 sign-off. #266 schema evidence tools and #280 reader-diff are merged and
remain separate tools; this adds no record decoder.

#270/#271/#276/#277/#278/#279 and #236 are merged. #225 rooms and #282 grass remain open.
Active #488 (`094f4674b2dfeaca66c57df853cf83a9461a0f76`) changes local-light consumption;
#299 (`904a2d0cf33738754dc7a0b82ff388849c4adec2`) changes shallow-water consumption;
#246 (`0524aab7a3875651ec57d7f03bfdfa058ef3371f`) owns movement/default selection;
#480 (`d38f461b036a25458c548124b8c0b7f08c0f9070`) adds interior lighting consumers.
Their proposed behavior is not attributed to this source head. Landing them requires
updating this inventory, not duplicating their feature work.
