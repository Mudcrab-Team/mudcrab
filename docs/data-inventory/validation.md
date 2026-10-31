# Local validation — 2026-10-31

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
