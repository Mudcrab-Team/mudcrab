# Skyrim Flag Inventory Guard

This tool enforces a permanent disposition and code-anchor guard over every known Skyrim SE flag in `catalog.json`.

## Files

- `catalog.json`: Known Skyrim SE flags (`id`, `container`, `bit`, `name`, `meaning`, `source`, `group`), citing upstream references by project and version only.
- `dispositions.json`: One entry per catalog ID with `disposition`, `owner`, `reason`, repo-relative `{path, anchor}` evidence pairs, and `issue` for `dropped` / `partial` flags.
- `baseline.json`: Ratchet ceiling for `dropped + partial` flags. It may stay flat or go down, never up.
- `inventory.py`: Validates ID completeness, allowed dispositions, evidence anchors, `dropped`/`partial` documentation, and the `dropped + partial` ratchet.
- `test_inventory.py`: Unit tests covering every failure case.

## Allowed dispositions

- `respected`
- `partial`
- `dropped`
- `system-not-implemented`
- `format-not-read`
- `record-not-read`
- `not-applicable`

## Commands

Run the CI guard check:

```sh
python3 -B tools/flag_inventory/inventory.py check
```

Run the unit tests:

```sh
python3 -B -m unittest discover -s tools/flag_inventory -p 'test_*.py' -v
```

When fixing a `dropped` or `partial` flag, update its entry in `dispositions.json` and lower `baseline.json` with:

```sh
python3 -B tools/flag_inventory/inventory.py update-baseline
```
