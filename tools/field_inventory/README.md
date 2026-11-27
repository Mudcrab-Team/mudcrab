# Skyrim Record Field Inventory Guard

This tool enforces a permanent disposition and code-anchor guard over every known Skyrim SE ESM record field and NIF block field in `catalog.json`.

## Files

- `catalog.json`: Known Skyrim SE ESM and NIF fields (`id`, `signature_or_block`, `subrecord`, `field`, `offset`, `type`, `meaning`, `source`, `kind`, `occurrences`, plus optional `size_bytes` and `conditional`), citing upstream references by project and version only.
- `dispositions.json`: Covers every catalog ID through:
  - `dispositions`: Individual entries for `respected`, `partial`, `dropped`, `not-applicable`, `record-not-read`, and `covered-by-flag-audit` fields with `owner`, `reason`, repo-relative `{path, anchor}` evidence pairs, and `issue`, `spec`, or written `reason` for `dropped` / `partial` fields.
  - `system_not_implemented_by_signature`: `system-not-implemented` fields grouped by `signature_or_block` (one entry per signature or block naming `missing_system`, `owner`, `reason`, representative `{path, anchor}` evidence pairs, and explicit `field_ids`) to keep the committed JSON file compact while still proving every catalog ID is covered.
- `baseline.json`: Ratchet ceiling for `dropped + partial` fields. It may stay flat or go down, never up.
- `inventory.py`: Validates ID completeness across `dispositions` and `system_not_implemented_by_signature`, allowed dispositions, evidence anchors, `dropped`/`partial` documentation, and the `dropped + partial` ratchet (sharing common helpers with `tools/flag_inventory` via `tools/inventory_common.py`).
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
```

Run the unit tests:

```sh
python3 -B -m unittest discover -s tools/field_inventory -p 'test_*.py' -v
```

When fixing a `dropped` or `partial` field, update its entry in `dispositions.json` and lower `baseline.json` with:

```sh
python3 -B tools/field_inventory/inventory.py update-baseline
```
