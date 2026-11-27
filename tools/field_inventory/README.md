# Skyrim Record Field Inventory Guard

This tool enforces a permanent disposition and code-anchor guard over every known Skyrim SE ESM record field and NIF block field in `catalog.json`.

## Files

- `catalog.json`: Known Skyrim SE ESM and NIF fields (`id`, `signature_or_block`, `subrecord`, `field`, `offset`, `type`, `meaning`, `source`, `kind`, `occurrences`, plus optional `size_bytes` and `conditional`), citing upstream references by project and version only.
- `dispositions.json`: Covers every catalog ID through:
  - `dispositions`: Individual entries for `respected`, `partial`, `dropped`, `not-applicable`, `record-not-read`, and `covered-by-flag-audit` fields with `owner`, `reason`, repo-relative `{path, anchor}` evidence pairs, and `issue`, `spec`, or written `reason` for `dropped` / `partial` fields.
  - `system_not_implemented_by_signature`: `system-not-implemented` fields grouped by `signature_or_block` (one entry per signature or block naming `missing_system`, `owner`, `reason`, at least one `{path, anchor}` evidence pair, and explicit `field_ids`) to keep the committed JSON file compact while still proving every catalog ID is covered.
- `baseline.json`: Ratchet ceiling for `dropped + partial` fields. It may stay flat or go down, never up unless a reclassification raises it with an explicit `raise_reason` naming an issue. CI checks `baseline.json` against the base revision via `--base-ref` and fails any unapproved increase.
- `inventory.py`: Validates ID completeness across `dispositions` and `system_not_implemented_by_signature`, allowed dispositions, evidence anchors, `dropped`/`partial` documentation, the `dropped + partial` ratchet, and base-revision ceiling increases (sharing common helpers with `tools/flag_inventory` via `tools/inventory_common.py`).
- `test_inventory.py`: Unit tests covering every failure case.

## Allowed dispositions

- `respected`
- `partial`
- `dropped`
- `system-not-implemented`
- `format-not-read`
- `record-not-read`
- `not-applicable`
- `covered-by-flag-audit`

## Commands

Run the CI guard check:

```sh
python3 -B tools/field_inventory/inventory.py check
python3 -B tools/field_inventory/inventory.py check --base-ref origin/main
```

Run the unit tests:

```sh
python3 -B -m unittest discover -s tools/field_inventory -p 'test_*.py' -v
```

When fixing a `dropped` or `partial` field, update its entry in `dispositions.json` and lower `baseline.json` with:

```sh
python3 -B tools/field_inventory/inventory.py update-baseline
```

`update-baseline` may only keep or lower the ceiling; it rejects raises.
