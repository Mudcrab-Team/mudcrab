# Mudcrab #221 implementation evidence audit

Reviewed by gpt-6.1-sol (xhigh), 2026-11-06. This is a fresh, same-family evidence/trail review. Other model-family providers were unavailable. It does not satisfy an independent different-family review and is not the sol-loop production-code review.

Scope: implementation report, player movement record map, implementation decision log, saved baseline/current artifacts, v4 campaign/report/receipts/archive/traces, and the prepare/score/report tooling. I read the show-me-your-work skill and T3 runtime adapter. I did not edit repository files, run Cargo, launch the game, commit, push, comment externally, or spawn agents. All audit outputs are under /tmp.

Verdict: no blocker in the core jump evidence. Before/after numbers, input isolation, acquisition provenance and chart are reproducible. One small source-linkage correction was identified in the light paragraph; the parent accepted it and is adding the missing identity/caller bytes. Full repository verification and PR readiness are outside this verdict and were unfinished at audit time.

## Rechecks

Run from the repository root:

```sh
python3 -B /tmp/mudcrab-221-evidence-recheck.py
```

The audit-owned script is retained at that path. Its complete successful output is `/tmp/mudcrab-221-evidence-recheck.json`. It imports the committed evidence tools, asserts their saved results against recomputation, checks source/archive/input/prediction hashes, independently derives the headless metrics using native float32 subtraction, resolves local Markdown links, and regenerates the chart to `/tmp/mudcrab-221-audit-comparison.html`. No Rust code is executed by this recheck.

| Check | Result |
| --- | --- |
| Baseline/current/prepared input bytes | Identical SHA-256 `27d4ca7949c8aabe82a3f387a2993a98aaeb6746f8b4a5d0e55a52e6958ed4fd` |
| Original corpus | 15 unique trace hashes: 3 training and 12 held-out |
| Baseline rescoring | 0/12; maximum RMSE `15.181785630790774`; maximum pointwise error `36.202089310625` |
| Current rescoring | 12/12; maximum RMSE `0.11414554895231227`; maximum pointwise error `0.22509765625` |
| Gates | Same frozen 1.15 game-second window, RMSE ≤ 1 and maximum error ≤ 3 |
| Baseline source archive | Every entry matches its source receipt; archived physics.rs differs from baseline revision only by the test-module declaration |
| Current source receipt | Every listed file matches its current bytes; source, receipt, input, prediction, corpus, training-source and acquisition-archive hashes all resolve |
| Baseline raw headless metrics | First rise `5.666667938232422`; apex `67.08328247070312`; apex tick 23; landing tick 47 |
| Current raw headless metrics | First rise `5.126792907714844`; apex `71.47846221923828`; apex tick 27; landing tick 51 |
| V4 numeric result | Exactly reproduces saved result: 4 accepted, 6 attempts, 2 retained failures; every launch-prediction error is zero |
| V4 capture archive | SHA-256 `1a412594a03829d11d761bb484a94bc81a6df915c79f3de27aad7ba801a5efbd`; all accepted receipt source hashes match archived bytes |
| Interactive chart | Byte-identical regenerated HTML, SHA-256 `7ee092f33030892fe95bcbc9fd1e7d3a897a5632f52f60cd6ad27f2876665615` |
| Local Markdown links | All 28 local links in the four scoped reports resolve |

The input schema contains only `schema`, a single `effective_jump_height_creation` profile value, and per-run ID, zero initial floor, and recorded controller-dt sequence. The three original training launches supply the height (`78.2799969460657` before Rust float32 conversion). The strict Rust structs reject extra fields. The bridge evolves position/support/history itself and invokes production `integrate_walk`. Observed future heights, velocities, apex, support and maximum are used only outside Rust for qualification/scoring. I found no held-out fitting or threshold change. Reusing the original research corpus is regression validation, which both report and chart state; it is not a newly blinded experiment.

The current headless export explicitly reports synthetic unit-actor height 76. Its raw sample-derived values match the summaries. The native actor uses about 78.28. The implementation report explicitly prevents interpreting the headless apex as an equal-actor native comparison. The v3 frozen seal, evaluator hash, acquisition source receipts, original training declarations and no-overlap checks pass through `prepare()`. The historical v3 report now clearly frames its old production comparisons as historical evidence.

The v4 accepted runs use measured player CALL `0x6752ba` and ten archived source hashes each. Their memory controls preserve NPC/race fields while separately changing setting and reference scale. The repeat restores baseline. Both failed traces retain verified raw hashes and failure reasons: readiness interruption, then the missing player setter path. Their incomplete Python pre-capture receipts are explicitly disclosed and neither is accepted input evidence. Source receipts are checked against `capture-source.zip`, not mutable current acquisition tools.

## Trail and transcript check

I used only the scoped T3 thread:

<t3-thread://v2/0fda66cb-6d5a-49c9-8939-da3e4c009f6c/8c19d7fd-676a-4db0-90d7-9162bb5472b8>

Run ordinal 3 is the implementation run. Message positions 336–389 establish the user's `$poteto-mode $full-stop implement issue #221` request, open/unmerged PR boundary, baseline revision and resolved-height plan. Positions 419–573 corroborate live-input-first sequencing, the readiness failure, missing player setter path, baseline source/results, controls, repeat and 155 Python checks. Positions 590–676 corroborate package provenance, CI integration, 102 focused physics tests, 12/12 replay, synthetic-profile distinction and eight final jump checks. Position 717 accurately reports the Python-version-specific schema failure and CI-Python recheck rather than claiming every local test passed. Activity positions 478–482 show the old source/build before production edits; positions 490–494 corroborate full-stop loading and baseline commands. These reads were scoped to the named thread, not unrelated history.

All seven current implementation decision rows map to real decisions/actions and supporting artifacts or logs. The design candidates named by row 3 exist under `/tmp/mudcrab-221-implementation-design/{a,b,c}/candidate.md`; the committed design preserves the selected shape and same-family limitation. The failed acquisition pivots are retained in the manifest and referenced by rows 5–6. I found no invented successful run, silently removed failed native attempt, or undocumented change of scoring gate.

I read the supplied tool-output logs. They corroborate baseline replay/headless success, intentional first-step failure before implementation, first-step success afterward, 102 physics tests (3 ignored), eight final jump tests (2 ignored), 12/12 current replay, current headless success and 155 Python tests. The final v3/v4 build logs identify emitted DLLs; the final input static output equals its saved static artifact. This audit did not independently rebuild DLLs or rerun executable disassembly. Temporary test logs are local evidence; the parent still needs the planned committed final validation record for a remote PR reader.

## Specific should-fix

`docs/research/skyrim-physics-221-implementation.md:70` names `TESObjectLIGH` vtable `0x18140f0`, slot `0x4a`, function `0x274090` and constructor `0x274390`. At the first audit read, its linked `light-radius-construction.txt` contained only the command and bytes for `0x27468e..0x2746fd`; those identity/caller addresses occur nowhere in that artifact or the scoped static tooling. The radius read/helper/store portion is supported. Add the pinned-binary RTTI/vtable/CALL excerpt, or omit the unsupported identity/caller clause. Parent acknowledged this and is appending those bytes. This is a narrow source-linkage issue, not evidence that the light observation is false.

## Limits and attention

reviewed by gpt-6.1-sol

- Same-family review only; no different-family provider was available.
- The report correctly retains substantive unknowns: serialized collision scale, MOVT units/modifiers/selection, rigid-body acceleration, native scheduling/high cadence, full light composition/photometry, general actors, inheritance/save mutations and swimming. A 12/12 character replay does not resolve all of #221.
- The full workspace nextest run was not green at this audit snapshot. Parent subsequently reported fixture field-order losses, intended snapshot/hash drift and CLI audit gaps and is correcting them. Clippy and final PR checks had no completed success evidence available to this audit. Do not represent focused proof as complete full-stop readiness.
- Before handoff, retain the final validation commands/results and rerun affected checks after the ongoing corrections. Refresh replay receipts if any listed source changes. Keep the PR open, unmerged, and without automatic merge, as the transcript requires.
