# Local validation — 2026-10-31

> Historical log. The source fingerprints below are past values. Since the CI-generated inventory change, no committed file holds a source fingerprint; see the README.

Source: `64108a1c38b0a854d4f82d7031ea50b57c336a5a` plus this worktree's local changes.
Owner: Taylor; task `issue298`, worktree `t3-d3471fd5`. Pre-existing untracked `AGENTS.md`
was not changed.

- **16 distinct Python controls passed**, on Python 3.9 and Python 3.11. Commands:
  `python3 -B -m unittest discover -s tools/world_data_inventory -p 'test_*.py' -v`
  and the same command with `python3.11`.
- Source/report drift checks passed with both interpreters:
  `python3 -B tools/world_data_inventory/inventory.py --check`.
- Focused managed isolated Rust extraction passed: **one test**, covering 64 selector
  combinations, six rejected partial-enable shapes, three optional-table controls and
  the two current publication shapes. The first cold run finished in 8m 57s; the test
  itself took 3.01s. The retention run also passed: 1m 07s build, 3.13s test.
- `cargo fmt --all -- --check`, `git diff --check`, workflow YAML parsing and local
  Markdown link checks passed. No broad workspace suite or Clippy run was performed.

The first managed preflight rejected the default 8 GiB estimate with 59.49 GiB of the
60 GiB build budget occupied (249.19 GiB filesystem free). A later preflight admitted
the cold build. This task changed no budget and deleted no artifacts. The retention
run uses the same isolated directory and a 1 GiB warm-run estimate.

## Retained evidence

Task-owned local evidence is retained under shared storage at
`evidence/issue298-d3471fd5-20261031`: `report.md`, `inventory.json`, `extraction.json`,
`command.txt`, `cargo-test.log`, `source.patch` and `source-manifest.json`.
These are local receipts, not public download links. Applicable GitHub Actions runs
upload equivalent emitted schema/query/report files as the `world-data-inventory`
artifact. The committed [fixture report](fixture-report.md) is a portable example.

Reviewed source fingerprint: `85f08fa38331643f02d794149682bdb7fc30df407e25d85a274c5757fdc4f331`.
The retained test binary is
`builds/isolated-0c394012ed21e713-issue298-64108a1c38b0/target/debug/deps/engine-7cd893477fe1ae9b`
under shared storage, SHA-256
`566ab699d60d27e2a9361a9ec55ca35c6ab8a9519e6db6cfa87e5b44ddd20498`.
Taylor owns its retention for this extraction evidence; no binary copy was made.

Storage `settle` completed and identified this isolated build as a retirement candidate;
it remains retained. `prune` dry run returned no candidates. No deletion was applied.

Nonfatal host warnings concerned `rust-objcopy`'s missing `libLLVM.dylib`, ISPC object
platform metadata and future incompatibility in `proc-macro-error2`. They were not
repaired as part of this tooling slice.

Prepare dependencies, source sink annotations and existing test references do not prove
executed engine rows, draw readiness, native physics/rendering behavior or Skyrim parity.
These results precede publication; GitHub CI results remain separate from local evidence.

## Refresh after merging main — 2026-11-18

This branch merged `main` (`cd7f77f5`). The converter, shared and engine sources had moved
since the first review, so the source guard fired. The refresh changed only classification data:

- New emitted tables `base_texture_swaps` and `race_animation_sources` got the same
  `deferred` disposition as the other declared tables. No field-level trace was added.
- The `waters.deep_color` sink anchor moved with #299 and `waters.shallow_color` gained
  its new sink. Their `SELECT` mapping is no longer verified: the water-colors query is now
  built with `format!`, and the lexical scan resolves only plain literals and constants.
  Both fields keep their sink and test references, and their `mapped` state is `unknown`.
- Reviewed source fingerprint after the refresh: `5c00c868405471c27a55469f2f7c4b385c9f537315ccf86ee079e409263a2da1`.
- Passed locally: the 16 Python controls, `inventory.py --check`, the Rust extraction test
  (`world::database::inventory_tests`, one test) and `config::tests` (103 tests).
- The refresh did not trace the 29 new query candidates or the other new tables from main
  one by one. They remain `unknown` or `deferred`.

## Review fixes — 2026-11-18

- The tool now reads and writes every file as UTF-8 with LF newlines. A source file under `crates/converter/src/records/` holds a byte that the Windows cp1252 codec rejects, so a host default read failed there.
- Plain `tests.rs` files are no longer scanned. A test-only query left the committed inventory (90 candidates became 88).
- The CI `test` job also runs when only the inventory files change (`check_world_data`), so the extraction-backed mapping check runs on a baseline refresh.
- Passed locally: 18 Python tests, `inventory.py --check`, the Rust extraction test and `config::tests` (104 tests together). The `.github/` edit made the flag audit mandatory.

## Refresh with #488 — 2026-11-18

#488 changes the light consumer, so the reviewed source baseline was refreshed on that branch after merging `main` (with #496):

- The `lights.radius` sink and test anchors now name `backend_range` and the additive-offset test. `lights.falloff` records that the engine reads it only for shadow-mode lights and does not apply it.
- New dispositions: table `reference_light_data` (`deferred`), fields `lights.fade_status`, `references.radius_status`, `reference_light_data.fade_offset` and `reference_light_data.data_size`, each with a source sink and a test. Their `SELECT` mapping is `unknown` (optional columns added by `format!` fragments).
- Reviewed source fingerprint: `4df4d35be6bd713ff9b116c26f5261aaf2ada8699d8a3ec97036aae9c637ad3a`.
- Passed locally: 18 Python tests, `inventory.py --check`, the Rust extraction test and `config::tests` (104 tests together, on the merge head before the reader edit; the extraction test is re-run below).
- Main moved (#783 texture audit, refreshed on `main` by #790). The baseline here was regenerated after merging it, with no disposition change. The fingerprint above is the current one.

