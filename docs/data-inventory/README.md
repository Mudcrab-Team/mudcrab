# World-data inventory (#298)

The inventory keeps declaration, decoding, preservation, query dependency, mapping and
behavioral evidence separate. No state implies the next. `source_*` means an explicit
source annotation, not a test result. `unknown` never means unused. A declared table is
not evidence that any row was published.

The report and the JSON are **generated, not committed**. CI and `inventory.py` write
`inventory.json` and `report.md` into this folder, and `.gitignore` keeps them out of commits.
They hold small light/water/movement traces, a source-only view and fingerprints of reader
source files. CI uploads them as the `world-data-inventory-source` artifact. The Rust `test`
job uploads the full emitted-schema report as the `world-data-inventory` artifact. The schema
status of the source-only view is `not_extracted`.
[validation.md](validation.md) is a dated log of earlier local runs. Its fingerprints are
history, not current values. [fixture-report.md](fixture-report.md) demonstrates the same
table/column report using disposable synthetic SQLite fixtures.

## What is committed, and what is generated

The gate keeps one purpose: a change that adds or changes a database table, a column or a
record reader needs a reviewed disposition. Only the reviewed, stable facts are committed:

| File | Content | Changes when |
|---|---|---|
| `tools/world_data_inventory/dispositions.json` | Human dispositions for blob default, traced fields, legacy shapes and limits | A person classifies a field, a legacy shape or a limit |
| `tools/world_data_inventory/tables/<table>.json` | One table: classification, owner and reason | A person classifies that table |
| `tools/world_data_inventory/stores/<store>.json` | One external store: owner, reason, states, anchors and its own review-provenance `reviewed_at_commit` | A person adds or edits that store |
| `tools/world_data_inventory/reviewed-surface.json` | One line per reviewed record reader (call-site key and hash of its SQL) and one line per reviewed `table.column` | A reader or a column is added, changed or removed |

The key of a reader holds the file path, the method, a hash of the call expression and an
occurrence number. It holds no line number and no hash of the whole file. The SQL hash
catches a changed constant behind an unchanged call.

Each table has its own file. Each external store has its own file and its own reviewed
commit. A pull request that adds or changes a table or a store edits only that file. Two pull
requests that add different tables or stores therefore never conflict on a shared line.

The per-store `reviewed_at_commit` records the last human review of that store. It is
provenance only. The gate checks the store anchors against the checked-out tree and does not
check that the commit exists or is reachable. The report shows the commit under test from
`git rev-parse HEAD` as "Checked at". `dispositions.json` has no stored top-level commit.

These are generated, never committed, and gitignored: `docs/data-inventory/inventory.json`,
`docs/data-inventory/report.md` and the old `tools/world_data_inventory/reviewed-sources.json`.
They hold fingerprints of the converter, shared and engine source files. A source edit that
adds no table, column or reader changes **no committed file**. The tool refuses to run if
git tracks one of the generated files.

## Migrating a branch that has the old committed files

Main no longer holds the three generated files. A branch that edited one of them gets a
modify/delete conflict, or an old text conflict, on the next merge. The fix is the same for
every branch. Delete the three files and commit:

```sh
git rm -qf --ignore-unmatch -- docs/data-inventory/inventory.json docs/data-inventory/report.md tools/world_data_inventory/reviewed-sources.json && git commit --no-edit
```

During a rebase, run the `git rm` part, then `git rebase --continue`. Do not take main's
copy and do not re-add the files. Then run `python3 -B tools/world_data_inventory/inventory.py --check`.
If your branch adds, changes or removes a record reader, the check lists it. Review the
dispositions, run `inventory.py --review-sources` and commit `reviewed-surface.json`.

## Migrating a branch that adds a store

Main moved the `stores` block out of `dispositions.json`. A branch that adds a store there
gets a conflict on the next merge. Move the entry to its own file:

1. Create `tools/world_data_inventory/stores/<filename>.json`. Choose a lowercase hyphenated
   filename, for example `conversion-manifest.json`. The `store` value is the old key, which
   may already end in `.json`; do not repeat the suffix. Use the entry's `owner`, `reason`,
   `states` and `evidence`, add `"format_version": 1`, `"store": "<old key>"` and a
   `reviewed_at_commit` that records your review of that store.
2. Delete the entry and the now-empty `stores` block from `dispositions.json`.
3. Run `python3 -B tools/world_data_inventory/inventory.py --check`.

Only lowercase `.json` files are loaded. Any other regular file in `stores/` fails the check
instead of disappearing from the report. The tool refuses a `stores` block in
`dispositions.json` and names the directory in its error message.

A branch that changes the old top-level `reviewed_at_commit` in `dispositions.json` gets one
conflict after this change. Resolve it by deleting that line and taking main's version. The
line no longer exists, and the report takes the commit under test from `git rev-parse HEAD`.

## Migrating a branch that adds a table

Main moved the `tables` block out of `dispositions.json`. A branch that adds or edits a table
there gets a conflict on the next merge. Move the entry to its own file:

1. Create `tools/world_data_inventory/tables/<table>.json`. The `table` value is the old key.
   Use the entry's `classification`, `owner` and `reason`, and add `"format_version": 1`.
2. Delete the entry and the now-empty `tables` block from `dispositions.json`.
3. Run `python3 -B tools/world_data_inventory/inventory.py --check`.

Only `*.json` files are loaded. Any other regular file in `tables/` fails the check instead
of disappearing from the report. The tool refuses a `tables` block in `dispositions.json`
and names the directory in its error message.

## Generate and check

Python 3.9+ and its standard-library SQLite module suffice. R-tree support is required
for the emitted schema and fixture controls.

```sh
python3 -B -m unittest discover -s tools/world_data_inventory -p 'test_*.py' -v
python3 -B tools/world_data_inventory/inventory.py --check
```

The scripts CI job runs both commands when inventory, converter/shared, engine source,
or workflow files change. `--check` always enforces the committed `reviewed-surface.json`
against the tree, then regenerates `inventory.json` and `report.md`. A missing generated
file is created and a stale one is overwritten. The Rust test job runs
`world::database::inventory_tests::world_data_inventory_extraction` in its normal suite.
That test also checks every emitted `table.column` against `reviewed-surface.json`, and
uploads its extraction, JSON inventory and Markdown report as `world-data-inventory`, also
after a failure.

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
synthetic in-house record-bundle publication. It also captures the legacy database after
an empty subset export, which creates `reference_link_subset_sources`. It records
`sqlite_master`, `pragma_table_info`, schema/producer/cache versions and reader identities.
The bundle creates provenance, script and terrain tables absent from `create_tables`.
The test runs without mesh or texture conversion or an engine launch. No row counts are
used as population claims.

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
Each file under [tables/](../../tools/world_data_inventory/tables) follows
[table.schema.json](../../tools/world_data_inventory/table.schema.json).
Each file under [stores/](../../tools/world_data_inventory/stores) follows
[store.schema.json](../../tools/world_data_inventory/store.schema.json).
Selected fields carry independent states, roles, owner, reason, exact source anchors,
mapping ordinals/expressions and sink/test references. Existing test references identify
relevant evidence; they do not assert that those tests ran in this task. All other table
fields have explicit deferred/provenance policies; BLOBs have an opaque disposition.
Tables and named external stores remain separate from SQL accounting. Each table and store
has its own file, and each store has its own `reviewed_at_commit`, so a change touches one
file and one review record. That commit is provenance for the last human review, not a
reachability check.

The committed gate is deliberately narrow, so it stays stable. The scripts job checks every
record reader that the lexical scan finds: a new reader, a reader whose SQL changed, or a
removed reader fails until `reviewed-surface.json` lists it. The Rust test checks every
emitted column the same way, and rejects unknown emitted tables, missing annotated fields,
replay mismatches, broken anchors and selected mapping/dependency drift. It does not demand
a reader for every future gameplay table. A new column in an existing table fails in the Rust
`test` job, not in the scripts job, because only the extraction sees the columns.

The fingerprint of the converter, shared and engine sources is still computed, but only as a
derived value. It binds an extraction to one checkout and appears in the generated files.
Unlike the earlier committed `reviewed-sources.json`, it is not compared with a committed copy.

**What the gate no longer covers.** Taylor accepted this trade-off to stop the conflicts.
The maintainers should know its size:

- 24 of the 88 reviewed readers have a `null` SQL hash. Their SQL is a variable, a `format!`
  result, or a constant from another file. Only the text of the call expression is reviewed.
  For such a reader, the tool also hashes the same-file string constants that the call names
  or interpolates (`{COLS}`). A constant in another file, or a variable that is built
  elsewhere, is not covered. An edit to it changes no committed file.
- The composed-query extraction covers the 75 `ReferenceQuery` variants only. It does not
  cover the other `null` readers.
- An edit to converter or shared projection code (the `decoded`, `preserved` and `projected`
  states) no longer gates anything. A column change that it causes is still caught in the Rust
  `test` job.
- An edit to an `evidence` or `sink` file that makes a disposition false no longer gates
  anything while its anchor text exists. `validate_refs` checks only that the anchor string
  is in the file.
- A new column dependency of an existing column through a `null` reader is not gated. The
  generated report records `prepare_dependencies`, but no committed baseline compares them.

After reviewing a new or changed reader and updating dispositions/anchors:

```sh
python3 -B tools/world_data_inventory/inventory.py --review-sources
python3 -B tools/world_data_inventory/inventory.py --check
```

For a new table or column, classify the table in `tables/<table>.json` first. Then take an
extraction: run the Rust extraction test with `WORLD_DATA_INVENTORY_OUTPUT` set to a
task-owned folder (it keeps `extraction.json` even when the check fails), or download
`extraction.json` from the failed CI run's `world-data-inventory` artifact. Then run:

```sh
python3 -B tools/world_data_inventory/inventory.py --review-sources --extraction /path/to/extraction.json
```

Review the diff of `reviewed-surface.json`, the table file and `dispositions.json`. Do not
use regeneration to substitute for classification review. An emitted extraction can also be
analyzed with `inventory.py --extraction /path/to/extraction.json --output /path/to/report`.
Its source commit and source fingerprint identify the evidence; it is not transferable to
another checkout just because the world schema version matches.

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

- Light falloff is published. Since #488 the engine reads it only for lights with a shadow
  mode and does not apply it. Reference light fade and radius have status columns. Day emittance
  reaches a material multiplier; other time slots are not implied.
- Shallow water color gates row eligibility and maps into a DTO. Since #299 the water
  material setup passes it to the water shader, as it does the deep color.
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
#488 (this refresh) changes local-light consumption and adds the `reference_light_data` table;
#299 (merged) changes shallow-water consumption;
#246 (`0524aab7a3875651ec57d7f03bfdfa058ef3371f`) owns movement/default selection;
#480 (`d38f461b036a25458c548124b8c0b7f08c0f9070`) adds interior lighting consumers.
Their proposed behavior is not attributed to this source head. Landing them requires
updating this inventory, not duplicating their feature work.
