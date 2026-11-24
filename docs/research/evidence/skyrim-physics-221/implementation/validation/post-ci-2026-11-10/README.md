# Post-CI corrections, 2026-11-10

The [security job on 78459dc](https://github.com/Mudcrab-Team/mudcrab/actions/runs/42783786950/job/129719952299) failed on RUSTSEC-2026-0459. The same audit also fails on upstream main 8e9a875. Both lockfiles contain libsqlite3-sys 0.30.1. The advisory database used for this comparison is d9cb059f3217db87aeccd190b81eaffe8cc29cb9.

The [upstream advisory](https://github.com/RustSec/advisory-db/blob/main/crates/libsqlite3-sys/RUSTSEC-2026-0459.md) identifies bundled SQLite before 3.54.0 as affected and libsqlite3-sys 0.39 as patched. A crafted database and sibling WAL file can expose stale process memory when opened. This follow-up updates rusqlite from 0.32.1 to 0.41.1 and libsqlite3-sys to 0.39.1. It adds no advisory suppression.

The compatibility changes preserve the existing database calls. The new fallible_uint feature restores checked u64 and usize SQL conversions previously available without a feature flag. The cache-size pragma takes the literal main schema name. The snapshot helper consumes column_names as an iterator, preserving the same owned column names.

The lockfile also updates hashlink, uuid and thiserror, and adds jiff and the newer release's WebAssembly dependencies. Rusqlite requires uuid 1.28 or later; its default WebAssembly dependency reaches rsqlite-vfs, which requires thiserror 2.0.20 or later. The existing bundled-full feature selection is retained. No physics transition, numeric observation or frozen model changes.

The prior review receipt remains a snapshot of 78459dc. The new receipt identifies this dependency follow-up separately.

## Capture profile preflight

The [new CodeRabbit finding](https://github.com/Mudcrab-Team/mudcrab/pull/544#discussion_r4522803850) identified a v4 session accepted by the v3 campaign's executable and save checks. The failing regression reached a stubbed recorder instead of rejecting the session. The corrected preflight rejects the incompatible profile before creating private or public output. A paired control confirms that the historical v3 session still reaches the recorder. The real CLI also exits 2 with the profile error and creates neither output directory. No native Skyrim launch is involved in these checks.

The aggregate docstring-coverage recommendation remains the [previously documented follow-up](https://github.com/Mudcrab-Team/mudcrab/pull/544#issuecomment-6688046196).

## Script-option audit

The first full workspace run passed 3,304 tests and failed the script-option audit on the new publication helper's `--sanitize` option. The audit now registers that helper's four non-engine options only for its own path, and recognizes the capture regression's native-tool arguments. Its existing tests still reject those options in an engine launcher. The corrected full run passes all 3,305 tests with 57 skipped.

## Reproduction

```sh
cargo audit --ignore RUSTSEC-2026-0194 --ignore RUSTSEC-2026-0195
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo nextest run --locked --workspace --lib --bins --tests --examples --no-fail-fast --no-tests=warn
python3 -B -m unittest discover -s scripts -p 'test_*.py'
python3 -B scripts/check_skyrim_jump_runtime.py --output /tmp/jump-production-fresh --require-heldout-pass
python3 -B docs/research/evidence/skyrim-physics-221/implementation/recheck.py
python3 -B scripts/publish_skyrim_evidence.py
```

## Verification

The [receipt](receipt.json) records the final sources and artifact hashes. The [workspace run](workspace.log.gz) passes 3,305 tests with 57 skipped. [Python tests](python.log.gz) pass 178 cases. [Workspace Clippy](clippy.log.gz), formatting, and the [two software-Vulkan tests](gpu.log.gz) pass. The GPU tests use the same private Mesa 26.2.6 fallback driver documented in the original validation record.

The [fresh production replay](production-result.json) passes 12/12 with unchanged maximum RMSE 0.11414554895231227 and pointwise error 0.22509765625. The [source receipt](production-source.json) identifies the replay inputs and production sources. The [historical evidence recheck](historical-recheck.json) also passes. No new native Skyrim recording is claimed.

The [main audit](audit-main.json) fails on RUSTSEC-2026-0459, while the [updated audit](audit-updated.json) reports no unsuppressed vulnerability using the same advisory database. Existing advisory exclusions and warnings are unchanged. The [initial workspace failure](workspace-before.log.gz), [profile regression before correction](profile-before.log.gz), and [real CLI rejection](profile-cli.log.gz) preserve the reproduced failures and their boundaries.
