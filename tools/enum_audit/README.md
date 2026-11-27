# Retail Enum Audit (`tools/enum_audit/`)

This tool enforces permanent checks against silent catch-alls on retail Skyrim enum values in Mudcrab.

## Components

1. **Decision site registry (`sites.json`)**
   Records the audited retail enum decision sites with:
   - `file`: repo-relative POSIX path.
   - `anchor`: unique substring inside `file` (never a line number).
   - `enum_id`: catalog ID from the enum audit or `"unnamed"` for raw binary format tags.
   - `status`:
     - `exhaustive`: exhaustive enum match, explicit error rejection on unknown values, or explicit predicate on an already-validated enum (`52` sites).
     - `catch-all-allowed`: intentional fallback with required `reason` and `reference` (`14` sites).
     - `catch-all-bug`: unfixed bug or unvalidated silent catch-all with `issue` (`#1054`, `#1060`, `#1061`, or `""` when unfiled) and `reason` (`13` sites).

2. **Anchor and metadata check (`enum_audit.py --check`)**
   Fails if any anchor disappears from its file, moves to a different file without updating `sites.json`, matches more than once, or uses a bare line number.

3. **Bug-count ratchet (`baseline.json` and `--base`)**
   Fails if the count of `catch-all-bug` sites rises above `baseline.json` (`13`) or above the `--base` commit's `sites.json`. The count may only go down.

4. **Added-line catch-all heuristic (`enum_audit.py --check --base <rev>`)**
   Scans added non-test Rust lines (excluding `tests.rs`, `*_tests.rs`, `#[cfg(test)]` blocks, comments, strings, and moved identical lines) in:
   - `crates/converter/src/esm/`
   - `crates/converter/src/records/`
   - `crates/converter/src/collision.rs` (and `collision*/`)
   - `crates/converter/src/animation/`
   - `crates/converter/src/mesh.rs` and `crates/converter/src/mesh/`
   - `crates/shared/src/`
   - `vendor/project-wormhole-nif/src/`

   The heuristic flags four patterns on added lines:
   - `match-catch-all`: `_ =>` or lowercase binding arm inside a `match` expression that has at least one integer or integer-range arm, when the catch-all arm does not return an explicit error (`Err`, `bail!`, `ensure!`, `unreachable!`, `panic!`, `nom::Err`, `ok_or`).
   - `integer-clamp`: `.min(N)` or `.clamp(lo, hi)` with integer arguments (excluding `.len().min(...)`).
   - `small-bitmask`: `& mask` for masks in `2..=0x7F`, excluding `!= 0` / `== 0` bitflag tests and lines referencing `flags` or `form_id`.
   - `unwrap-or-fallback`: `.unwrap_or("unknown"|"unused"|"default")`, or `.unwrap_or(N)` / `.unwrap_or(Enum::Variant)` excluding non-enum fields (`form_id`, `offset`, `stride`, `size`, `len`, `count`, `capacity`, `depth`, `width`, `height`, `mip`, `index`, `from_utf8`, `as_bytes`, `u64`, `usize`, `f32`, `f64`, `Path::`, `Vec3::`).

   An added line is exempt when the line itself or the line immediately above it contains:
   ```rust
   // retail-enum: <id> <reason>
   ```
   where both `<id>` and `<reason>` are non-empty.

   On the current tree (`--audit-tree`), the heuristic reports `27` hits across `123` scoped production `.rs` files: `14` retail enum / packed bit-extraction hits and `13` false positives (`0.11` per file).

## Why `#![warn(clippy::wildcard_enum_match_arm)]` is scoped

Running `cargo clippy --workspace --all-targets --all-features -- -W clippy::wildcard_enum_match_arm` reports:
- `retail-rules`: `0` warnings
- `mo2`: `1` warning
- `dummy-content`: `2` warnings
- `shared`: `3` warnings (non-retail types: `serde_json::Value`, `IdentityError`, `Resolution`)
- `launcher`: `15` warnings
- `vendor/project-wormhole-nif`: `30` warnings (and crate has `#![allow(clippy::all)]`)
- `converter`: `79` warnings
- `engine`: `132` warnings

CI's `tools/cast_ratchet/cast_ratchet.py` gate fails on any Clippy warning outside its four cast lints. Adding `#![warn(clippy::wildcard_enum_match_arm)]` to `converter`, `engine`, `launcher`, `shared`, `dummy-content`, `mo2`, or `vendor/project-wormhole-nif` would emit warnings on existing non-retail matches and break CI without touching unrelated code. It is enabled only in `crates/retail-rules/src/lib.rs` (`0` warnings) and left out of the other crates until retail enum types are migrated and non-retail matches are annotated.

## Usage

```sh
python3 -B tools/enum_audit/enum_audit.py --check
python3 -B tools/enum_audit/enum_audit.py --check --base origin/main
python3 -B tools/enum_audit/enum_audit.py --audit-tree
python3 -B -m unittest discover -s tools/enum_audit -p 'test_*.py' -v
```
