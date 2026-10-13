# LOD conversion measurements

Use a release converter for complete conversion measurements. The ignored Rust
tests provide matched extraction and atlas diagnostics; the `quick` profile is
useful for those diagnostics but does not establish release performance.

`plan_lod_validation.py` prints six sequential runs with separate evidence
directories: CPU LOD cold/warm metadata rebuilds, GPU LOD cold/warm metadata
rebuilds, and full GPU fresh/warm conversions. Metadata rebuilds verify and
retain ordinary assets from the source pack. They require its ordinary texture
encoder and quality settings to match. The initial CPU source must lack current
LOD cache proof for a zero-hit cold measurement.

```sh
python3 scripts/lod-performance/plan_lod_validation.py \
  --converter target/release/converter \
  --data /path/to/skyrim/Data \
  --source-pack /path/to/modern_assets \
  --root target/lod-validation \
  --guard-pid 0 --stage-summary > target/lod-validation-plan.json

python3 scripts/lod-performance/run_validation_plan.py \
  --plan target/lod-validation-plan.json
```

Set `--guard-pid` to an existing conversion's PID to prevent overlap. Pass `0`
explicitly when no conversion needs guarding. Keep the validation root separate
from the source pack and other jobs' staging/output directories. Execute each
generated run, ordinary validation and LOD audit in order. The runner stops on
failure, records binary and helper hashes, checks that outputs are disjoint from
the source pack and game data, and verifies the protected manifest after each
leg. It preserves the fresh audit before the warm full conversion replaces
that isolated output. `--dry-run` validates the plan without starting commands.
To resume, choose a new status path and pass `--resume-after <audited-label>`
with `--previous-status <old-status.json>`. The runner verifies the recorded
status chain and completed prefix. A changed binary additionally requires
`--allow-new-binary`; both hashes remain in the receipt.

`run_conversion_observed.py` is macOS-specific. It records `/usr/bin/time -l`,
child CPU and block-I/O counters, stage events, competing process names and
`iostat`, while `caffeinate -i` prevents idle sleep. Optional GPU statistics are
global driver counters with an unknown averaging window. They do not identify
one process's GPU usage; report phase timings and actual GPU/fallback counts
alongside them. Use an environment with native GPU access to test the GPU path.
Compare recorded UTC start/end spans with monotonic elapsed times. Idle-sleep
prevention does not establish that a run had no clock or suspend gap; retain and
disclose any difference before treating a timing as experienced wall time.

`audit_lod_pack.py` checks every chunk's payload hash, cell membership, geometry,
spatial rows and embedded three-mip sRGB UASTC atlas. It compares cold/warm
outputs exactly or compares geometry and coverage across an encoder change.
It supplements the converter's ordinary `check --full`. `--allow-unproven` is
only for an intentional fallback run; healthy GPU runs require full reusable
input proof and zero fallback chunks.
Missing authored LOD origins are accepted only when the source reference has
the same worldspace origins, LAND identities and plugin checksums. Metadata
archive omissions must match source-package rules and the unchanged archive
hash. Additional omissions fail the audit; full extraction permits none.

The ignored Rust checks are:

- `archive::batched::tests::real_entry_extraction_timing`: set
  `EXTRACTION_BENCH_SOURCE` to `Skyrim - Misc.bsa`, and optionally
  `EXTRACTION_BENCH_DIR` and `EXTRACTION_BENCH_REPORT`. It rewrites the first
  2,048 real entries into an uncompressed fixture outside the timed extraction
  intervals, checks identical inventories and validates corruption recovery.
- `lod::albedo::tests::gpu_atlas_quality_preserves_mips_gutters_padding_and_reusable_slots`:
  checks CPU/GPU quality, alpha, authored mips, gutters and repeated batches.
- `texture_gpu::tests::prepared_gpu_batches_serialize_and_return_all_slots`:
  checks concurrent calls, deterministic results and slot return.
- `lod::albedo::tests::gpu_real_pack_atlas_benchmark`: set `LOD_BENCH_PACK`,
  `LOD_BENCH_VFS` and `LOD_BENCH_JSON` (under `target`). Optional
  `LOD_BENCH_CPU_JOBS`, `LOD_BENCH_GPU_QUALITY`, `LOD_BENCH_GPU_BATCH_MB` and
  `LOD_BENCH_BLOCK=tamriel|ocean` select the fixed workload. CPU and GPU encode
  identical prepared mips; the report separates preparation, encoding, GLB
  assembly and quality checks.
- Integration test
  `gpu_atlases_have_separate_reuse_proof_and_preserve_the_terrain_contract`
  in `fixture_lod_pipeline`: checks encoder/quality transitions and warm reuse.

Run one exact check with `cargo test -p converter --profile quick --lib
<test-name> -- --ignored --exact --nocapture`, or run its compiled test binary
under `/usr/bin/time -l`. Use `--test fixture_lod_pipeline` for the integration
check. Avoid overlapping benchmarks with builds, renderers or another converter.
