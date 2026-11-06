# Implementation validation

These local results were observed on 2026-11-06, on upstream base `ee686282126e369af0b68a8282db80dc7e10dd6a`. The [receipt](validation/receipt.json) hashes the checked source files and retained compressed logs. The original before measurement remains pinned to `1dcca42fc398baaf626e588694f9e08084117f11`. Hosted CI and the PR review are separate gates; this record does not claim they have finished.

The machine runs CachyOS Linux with Rust 1.98.1, Python 3.14.9 and Python 3.11. Rust commands used `RUSTFLAGS='-C debuginfo=0'`, `CARGO_BUILD_JOBS=4` and a task-private `CARGO_TARGET_DIR`. The local linker was lld. No Cargo dependency or linker configuration was changed.

| Check | Observed result | Retained log |
| --- | --- | --- |
| `cargo nextest run --workspace --lib --bins --tests --examples --no-fail-fast --no-tests=warn` | 3,303 passed, 57 skipped | [nextest](validation/nextest.log.gz) |
| `cargo fmt --all -- --check` | Exit 0 | [format](validation/format.log.gz) |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Exit 0 | [clippy](validation/clippy.log.gz) |
| `cargo test --workspace --doc` | Exit 0; zero doctests across six crates | [doctests](validation/doctests.log.gz) |
| `python3 -B -m unittest discover -s scripts -p 'test_*.py' -v` | 155 passed | [capture helpers](validation/python-capture.log.gz) |
| `python3.11 -B -m unittest discover -s scripts/tests -p 'test_*.py' -v` | 206 run, 5 skipped, no failures | [script checks](validation/python-scripts.log.gz) |
| `python3.11 -B -m unittest discover -s tools/schema_validation -p 'test_*.py' -v` | 92 passed | [schema checks](validation/schema-python311.log.gz) |
| `python3 -B -m unittest discover -s tools/record_comparison -p 'test_*.py' -v` | 13 passed | [record comparison](validation/record-comparison.log.gz) |
| `python3 -B -m unittest discover -s scripts/lod-performance -p test_helpers.py -v` | 20 passed | [LOD helpers](validation/lod-helpers.log.gz) |
| `cargo audit --ignore RUSTSEC-2026-0194 --ignore RUSTSEC-2026-0195` | Exit 0, seven allowed dependency warnings | [dependency audit](validation/dependency-audit.log.gz) |
| Production held-out replay | 12/12; worst RMSE 0.11414555, pointwise error 0.22509766 | [replay](validation/production-replay.log.gz) |
| Focused real Bevy/Rapier jump suite | Eight passed, two exports ignored | [jump tests](validation/jump-tests.log.gz) |
| Native recorder rebuilds with `/W4 /WX` | Both v3 and v4 succeed | [v3](validation/native-v3-build.log.gz), [v4](validation/native-v4-build.log.gz) |

The two ignored CI GPU tests also pass. The initial CI-equivalent nextest command failed before test work because this host had no software Vulkan adapter; [that failure](validation/gpu-no-adapter.log.gz) is retained. A signature-verified Mesa 26.2.6 driver was extracted into the task cache. Setting `VK_DRIVER_FILES` to its `lvp_icd.json`, `LD_LIBRARY_PATH` to its private library directory, `WGPU_BACKEND=vulkan` and `LP_NUM_THREADS=2` allowed the already-built converter test binary to run each test with `--ignored --exact --nocapture`:

- `texture_gpu::tests::v210_software_gpu_encodes_and_decodes_color_alpha_normals_and_mips`: [passed](validation/gpu-texture.log.gz).
- `lod::albedo::tests::gpu_authored_atlas_software_vulkan_preserves_mips_gutters_padding_and_slots`: [passed](validation/gpu-atlas.log.gz).

This changed no system installation or repository graphics behavior. The receipt identifies the driver bytes. CI supplies its own Mesa package.

The first workspace run exposed an out-of-order synthetic NPC field, intended fixture hash/snapshot drift, and script-option audit gaps. Reordering `ACBS` before `RNAM`, requiring both record readers to preserve all jump fields, and correcting the fixture expectations produced the final green run. The added base `fJumpHeightMin` record now owns the identity while later plugins still supply the winning value. No scoring tolerance or frozen native artifact changed during these corrections.

The schema suite initially exposed a Python 3.14 deep-JSON-parser expectation difference in an unchanged test. It passes on the CI-pinned Python 3.11. Dependency warnings and the `proc-macro-error2` future-incompatibility warning are recorded, not presented as newly resolved issues. PowerShell checksum checks, pinned object-LOD builder tests and release performance budgets are left to their hosted CI jobs; they were not run locally.

## Recheck the evidence

From the repository root:

```sh
python3 -B docs/research/evidence/skyrim-physics-221/implementation/recheck.py
python3 -B scripts/verify_skyrim_jump_inputs.py --output /tmp/jump-inputs.json
python3 -B scripts/check_skyrim_jump_runtime.py \
  --output /tmp/jump-production-fresh --require-heldout-pass
```

The [portable audit recheck](recheck.py) retains the evidence reviewer's verification logic with only the repository-root lookup and output destinations made portable. It verifies saved hashes, independent rescoring, input isolation, raw headless metrics, native input controls, chart regeneration and local report links. Its [recorded output](validation/evidence-recheck.json) is retained. It does not execute Rust; the last command does.

The evidence audit's light source-linkage finding is resolved by the bounded RTTI, vtable, CALL and `.pdata` excerpt in [light-radius-construction.txt](../light-radius-construction.txt). The published extraction command was rerun against the pinned executable. Rebuilt recorder DLLs are not claimed to be byte-identical to captured DLLs; capture provenance uses the archived source bytes and original receipts.
