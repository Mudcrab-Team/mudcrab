# Implementation validation

The initial local results below describe commit `6a29ba6274cc87d6b8aac5348a14413cc16d6ae8`, observed on 2026-11-06 on upstream base `ee686282126e369af0b68a8282db80dc7e10dd6a`. The [receipt](validation/receipt.json) hashes the checked source files and retained compressed logs. The original before measurement remains pinned to `1dcca42fc398baaf626e588694f9e08084117f11`. Hosted CI and the PR review are separate gates; this record does not claim they have finished.

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

The original evidence audit checked bounded RTTI, vtable, CALL and `.pdata` observations. Their [published structural summary](../light-radius-construction.txt) now omits copied instruction listings and executable byte strings; the publication mapping identifies that derivative. The published extraction command was rerun against the pinned executable. Rebuilt recorder DLLs are not claimed to be byte-identical to captured DLLs; capture provenance uses the archived source bytes and original receipts.


## Review corrections

CodeRabbit identified two reproducible gaps after the initial implementation was published. Evidence-only changes now select both CI validators. Capture failures now retain and hash any trace, or explicitly report that an unsuccessful capture never created one. New campaigns archive acquisition source bytes before starting and require identical sources on resume. The README now directs verification to the new corpus.

The [follow-up receipt](validation/review-fixes-receipt.json) hashes the corrected tools and the new checks. The [CI selection regression](validation/review-ci-before.log.gz) failed for all eight evidence/source cases before the fix and [passes afterward](validation/review-ci-after.log.gz), while an unrelated documentation path still skips these jobs. The [capture regression](validation/review-capture-before.log.gz) reproduced missing failure fields in three cases. The final [157-test Python run](validation/review-python-all.log.gz) passes, including failed-then-resumed complete campaigns, altered failed traces, archive drift and a falsely successful missing-trace record. It stubs native process execution and replays existing real trace bytes through the capture CLI and verifier; it is not a new native measurement.

The current-build [engine script-option audit](validation/review-config-audit.log.gz) also passes. The original native corpus, captured acquisition source archives, frozen evaluator and production replay sources are unchanged. The original receipt remains the historical source snapshot for the first implementation; the follow-up receipt identifies the corrected evidence tools.


## Failed-attempt accounting

A second hosted review showed that the recorder can fail its file/Steam checks before it creates `failure.json`, and that input analysis or proof metadata can raise errors other than `ValueError`. The premise that every failed attempt reached the existing handler was false. The [regression before correction](validation/attempt-accounting-before.log.gz) reproduced four omitted failure records and two cases where removing a failure marker still let the verifier accept the campaign.

The wrapper now records a generic recorder failure when the child exits without a failure file. Proof loading, analysis and public-proof extraction share an error boundary that retains trace bytes, records the exception type, and re-raises. The verifier independently requires exactly one zero-exit, nonfailed attempt per accepted run in declared order. An unclassified attempt cannot be hidden by a later successful retry.

The final [158-test Python run](validation/attempt-accounting-after.log.gz) passes. Its seven failure/resume scenarios include invoking the real recorder against a session whose game files are absent, exercising the pre-`try` failure without starting Skyrim; finite float32 factors whose product overflows; and missing proof fields before and after analysis. The [receipt](validation/attempt-accounting-receipt.json) identifies the corrected sources. Both historical evidence rechecks pass. Production code, original acquisition archives and native measurements remain unchanged.


## Publication and review corrections, 2026-11-10

The [review response](../review-2026-11-10.md) records the new findings, corrections and focused verification. Earlier receipts and counts above remain historical. The [publication mapping](../publication.json) binds their original hashes to sanitized public files, including nested archives and logs. It does not turn a sanitized derivative into the original acquisition bytes. The numerical traces, frozen model and evaluator are unchanged.

Both production source snapshots are now archived. The portable recheck validates historical evidence against those archives; CI separately executes the current production transition against pinned recordings. Fresh capture proofs qualify achieved camera cadence and retain failed controls. The ordinary Rust suite also reads one committed native trace.


## New-head CI corrections, 2026-11-10

The [post-CI record](validation/post-ci-2026-11-10/README.md) covers the newly published SQLite advisory, the v3 campaign profile check, and the script-option audit registration. Final local verification passes 3,305 workspace tests, 178 Python tests, both software-Vulkan checks, workspace Clippy, formatting, and all 12 production replays. The prior review receipt remains the historical snapshot before these corrections. No native Skyrim recording was added.
