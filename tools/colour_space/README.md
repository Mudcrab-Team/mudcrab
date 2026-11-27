# Colour-Space Guard (`tools/colour_space/`)

This tool blocks new unmarked sRGB conversions of retail Skyrim record colours and ratchets existing sites down toward zero.

<!-- TODO(lead): Link the tracking and bug issue numbers (C-01, C-02, C-03, and the colour-space tracking issue) once filed. -->
TODO (lead): Link the colour-space issue numbers here once they exist.

## Rule (SPEC-1)

Under the `skyrim` visual profile (`SPEC-1 §1`, [docs/specs/engine/near-lighting.md](../../docs/specs/engine/near-lighting.md)), retail record RGB bytes (`WATR`, `WTHR`, `LIGH`, `CELL`, `LGTM`, `REGN`) are used directly as `byte / 255.0` without sRGB-to-linear decoding.

Decode or store colours as sRGB only when you cite a concrete reason:

- `// colour-space: SPEC-1 §2.4` (or another spec / retail-facts reference)
- `// colour-space: mudcrab-only because <reason>` (include a reason word such as `because` or `since` so `scripts/check_retail_claims.py` accepts `mudcrab-only`)
- `// colour-space: ui`

Place the marker on the flagged line itself or on the line immediately above it.

## Scope and Patterns

`scope.json` lists the in-scope files and globs so the scope is visible and easy to extend:

- `crates/engine/src/emittance.rs`
- `crates/engine/src/underwater*.rs`
- `crates/engine/src/weather_sky.rs`
- `crates/engine/src/retail_lighting.rs`
- `crates/engine/src/sky.rs`
- `crates/engine/src/render.rs`
- `crates/engine/src/world/environment/interior.rs`
- `crates/engine/src/lights.rs`
- `crates/engine/src/grass_render.rs`
- `crates/engine/src/shaders/*.wgsl`

On added lines in scope (`--base <rev>`), the check flags:

- Rust: `Color::srgb(`, `Color::srgb_u8(`, `Color::srgba(`, `.to_linear()`, `LinearRgba::from(`, and GPU texture formats ending in `Srgb` (`TextureFormat::*Srgb` and `*UnormSrgb`).
- WGSL: `srgb_to_linear(` and `sky_srgb_to_linear(`.

## Heuristic Choices (Low False Positives)

- Comments (`//`, `///`, `/* ... */`), string literals, raw strings, and char literals are masked before pattern matching, while `// colour-space:` markers are read only from real `//` comments.
- Rust `#[cfg(test)]` modules/items, `#[test]` functions, and `*_tests.rs` / `tests.rs` files are excluded.
- WGSL function declarations (`fn srgb_to_linear(` and `fn sky_srgb_to_linear(`) are skipped; only call sites are flagged.
- Texture-format matching targets `TextureFormat::*Srgb` and `*UnormSrgb` so non-texture enums such as `SceneGradeMode::SnapshotSrgb` or `LandTextureUsage::DiffuseSrgb` are not flagged.
- Lines moved or reindented within a diff (matching removed line text after stripping whitespace) are ignored in `--base` mode.

## Ratchet (`baseline.json`)

`baseline.json` records the full-tree unmarked hit count in scope (`unmarked_total` and per-file `counts`). CI fails if the count rises above `baseline.json` or if a PR raises `baseline.json` above the base commit. When fixing a bug or marking a `mudcrab-only` site lowers the count, update `baseline.json` with `--update-baseline`.

## Commands

```sh
python3 -B tools/colour_space/colour_space.py --base origin/main --ratchet
python3 -B tools/colour_space/colour_space.py --audit
python3 -B tools/colour_space/colour_space.py --update-baseline
python3 -B -m unittest discover -s tools/colour_space -p 'test_*.py' -v
```
