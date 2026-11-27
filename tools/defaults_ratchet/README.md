# Retail Defaults Ratchet (`tools/defaults_ratchet/`)

This ratchet blocks new unlabelled invented defaults on retail Skyrim data and world-database boundaries in Mudcrab.

It implements the recommendation from the 2026-11-27 invented defaults audit (`defaults-audit-20261127` in shared evidence storage).

<!-- TODO(lead): link tracking issues for unfiled clusters from defaults-audit-20261127 (CL-04, CL-05 door hitbox, CL-06 rooms/occluders, CL-07 ATXT/white terrain, CL-08 streaming water fallback, CL-09 lod_object_material, CL-10 VMAD NULL coercion, CL-13 unwired GMST/INI keys). -->

## Rules

1. **Scope (`config.json`)**
   - `crates`: lists `converter`, `engine`, `retail-rules`, `shared`, and `project-wormhole-nif`.
   - `db_write_modules`: lists converter database writers and engine SQLite world-database boundary readers.
   - `retail_data_modules`: lists retail record, NIF, material, terrain, grass, weather, lighting, physics, and world modules.
   - Test files (`tests.rs`, `*_tests.rs`, `tests/`, `benches/`, `examples/`) and `#[cfg(test)]` blocks are out of scope.

2. **SQLite boundary rule (`db_write_modules`)**
   - `sql-not-null-default`: `NOT NULL DEFAULT <non-null>` in SQL DDL or migration strings.
   - `sql-coalesce-literal`: `COALESCE(..., <literal>)` in SQL queries, excluding aggregate summary expressions such as `COALESCE(MAX(id), 0)`.
   - `fallback-struct-write` / `fallback-db-params`: struct literals on `*Row` / `*Record` (excluding `RawRecord`), structs with `"unresolved"` state, or structs and `params![...]` in `None =>` / `else` / `Err` / fallback functions that write `0`, `0.0`, `""`, `"male"`, or `"female"` into non-counter fields.

3. **Retail-data module rule (`retail_data_modules`)**
   - `unwrap-or-literal`: `.unwrap_or(<literal|CONST>)` and `.unwrap_or_else(|| <literal|CONST>)`.
   - `map-or-literal`: `.map_or(<literal|CONST>, ...)`.
   - `fallback-constant`: fallback returns or uses of `SKYRIM_CLEAR_*`, `DEFAULT_*`, or `FALLBACK_*` constants, excluding `use` imports, `const`/`static` declarations, and `==`/`!=` comparisons.

4. **Per-crate baseline ratchet (`baseline.json`)**
   - Tracks the full-tree finding count per crate.
   - The count may stay flat or go down. Raising a count or raising `baseline.json` above `--base` fails CI.

## Exemption labels

When missing retail data really has a verified retail default or a tracked temporary stand-in, label the site on the same line, in the contiguous `//` comment block above it, or on the owning statement's first line / comment block:

- **Verified retail default** (cite `SPEC-N`, `RF-*`, `xEdit`, `nif.xml`, or `CommonLibSSE-NG`):
  ```rust
  // retail-default: xEdit LIGH FNAM Fade default 1.0 (SPEC-1 section 2.6)
  let fade = light.fade.unwrap_or(1.0);
  ```
- **Tracked stand-in** (uses the `scripts/check_retail_claims.py` marker format, with an open issue and at least four words or `no retail data`):
  ```rust
  // STAND-IN(#946): use SkyrimClear day lighting until dynamic weather lands
  let sunlight = weather.unwrap_or(SkyPalette::SKYRIM_CLEAR_DAY);
  ```

Do not use `// retail-default:` with no reference, and do not use a malformed `STAND-IN` marker.

## Commands

```sh
python3 -B tools/defaults_ratchet/defaults_ratchet.py --check
python3 -B tools/defaults_ratchet/defaults_ratchet.py --check --base origin/main
python3 -B tools/defaults_ratchet/defaults_ratchet.py --audit-tree
python3 -B tools/defaults_ratchet/defaults_ratchet.py --update-baseline
python3 -B -m unittest discover -s tools/defaults_ratchet -p 'test_*.py' -v
```
