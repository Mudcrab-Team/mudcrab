# World-data inventory

Schema status: **emitted**. Reviewed source: `disposable-fixture`.

Prepare dependencies are not executed rows, mapped rows, semantic use, population, or retail parity.

## Denominators

- rusqlite_candidates: 3
- emitted_schema_columns: 11
- manually_traced_fields: 0
- named_external_stores: 0

## Traced examples

| Field | Decoded | Preserved | Projected | Query-dependent | Mapped | Semantic use | Role / reason |
|---|---|---|---|---|---|---|---|

## External stores


## Evidence anchors


## Retained legacy dispositions


## Emitted schema: disposable-fixture

| Table.column | Type | Disposition | Prepare dependencies | Population |
|---|---|---|---|---|
| `future_gameplay.id` | INTEGER | deferred | 0 | not_observed |
| `future_gameplay.opaque` | BLOB | opaque | 0 | not_observed |
| `lights.id` | INTEGER | deferred | 1 | not_observed |
| `lights.radius` | REAL | deferred | 1 | not_observed |
| `lights.falloff` | REAL | deferred | 0 | not_observed |
| `lights.*` (count/table scope) | | | 1 | not_observed |
| `placements.id` | INTEGER | deferred | 0 | not_observed |
| `placements.light_id` | INTEGER | deferred | 1 | not_observed |
| `placements.enabled` | INTEGER | deferred | 1 | not_observed |
| `spatial.id` | INT | deferred | 0 | not_observed |
| `spatial.min_x` | REAL | deferred | 0 | not_observed |
| `spatial.max_x` | REAL | deferred | 0 | not_observed |

## Query extraction

| Candidate | Status |
|---|---|
| `crates/engine/src/disposable_fixture.rs:2` prepare | disposable-fixture: prepared |
| `crates/engine/src/disposable_fixture.rs:3` prepare | disposable-fixture: prepared |
| `crates/engine/src/disposable_fixture.rs:4` prepare | disposable-fixture: unknown |

## Limits

- Synthetic fixture only.
