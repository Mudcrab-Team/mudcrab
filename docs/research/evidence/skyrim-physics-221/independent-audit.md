# Independent audit of #221 evidence

Goal. Establish native gravity, launch, apex, and timestep semantics using #520 observations and fresh held-out evidence, with production physics unchanged.

Reviewed by gpt-6.1-sol. This is an independent same-family review. Other-family providers were disabled. No child review was launched. This pass changed no repository file, launched no game, fitted no model, and did not score held-out models. The sole written artifact is this report.

## Material interpretation flag

The requested frame caps have not produced different observed camera cadences. At inspection, the manifest contained three training and four held-out runs. Direct qualification and median consecutive camera QPC differences gave these values.

| Run | Achieved camera Hz | Median camera dt / wall dt |
| --- | --- | --- |
| train-60hz-1x-1 | 60.89 | 0.9980 |
| train-60hz-1x-2 | 60.88 | 0.9976 |
| train-60hz-1x-3 | 60.87 | 0.9983 |
| heldout-30hz-1x-1 | 60.90 | 0.9983 |
| heldout-120hz-1x-1 | 60.90 | 0.9986 |
| heldout-60hz-0.5x-1 | 60.89 | 0.4988 |
| heldout-30hz-1x-2 | 60.89 | 0.9981 |

These are measured facts from the completed traces, not forecasts. The half-time treatment works. The requested 30 and 120 wall-cadence treatments do not separate in these observations. A completed campaign may support transfer across controller dt changes caused by timescale. It cannot claim validation across observed 30 and 120 camera Hz from these cap labels. Do not replace, relabel, or selectively discard these held-out runs to conceal this result. A new cadence experiment would need a separate declared campaign.

Evidence. `scripts/evaluate_skyrim_jump.py:117`, `tools/skyrim-recorder/reference/jump-v3/manifest.json`, and the trace files named in that manifest. Measurement was `1/statistics.median(b['qpc']-a['qpc'])` over qualified sample pairs. No evaluator score was computed.

## Checks that passed

The frozen seal recomputed to `c1026fb434bb72d0bb4b255888fd340dd202590e0f9a73d7a19b6a6e0607d575`. The evaluator source matches its frozen source digest. Both train and held-out source receipts match every currently recorded source hash. Every completed run qualifies under the unchanged evaluator and matches the frozen executable, recorder DLL, source save, input sender, and Proton launcher contract.

All 13 evaluator unit tests passed independently with `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p test_evaluate_skyrim_jump.py -v`. The tests exercise hash/sequence/footer rejection, a missing jump, sealed coefficients, frozen source, training separation, prohibition on held-out fitting, clock disagreement, the production baseline's first displacement, and a discrete recurrence checked against a separately written closed form. This proves those checked behaviors, not all possible trace semantics.

The native predictor boundary accepts only frozen coefficients and timestep values. `native_forecast` maintains its own height, maximum height, and velocity. It does not read future observed height or velocity. `evaluate` constructs its step array solely from recorded controller dt and the launch boundary. Direct callback diagnostics separately use observed velocities and heights and are labeled as diagnostics. The latter are local state-update checks, not independent position forecasts.

The production comparison uses actual defaults from `crates/engine/src/physics.rs:34`, `:90`, `:30`, `:475`, and `:1682`. These establish gravity 900, launch 340, fixed 1/60, and a grounded launch step that skips gravity. The evaluator's displacement `n*v*h - g*h*h*n*(n-1)/2` matches that free-flight update order. It is an idealized comparison with a flat clipped floor, not a replay of Rapier contacts or the complete production character controller. `git diff --stat` was empty and production files were absent from `git status --short`.

## Limits that the final report must preserve

1. Native forward recurrence is conditional on the observed controller timestep schedule and launch boundary. It validates an integration rule under that schedule. It does not predict the scheduler or establish callback ownership from camera cadence alone. Its one-update-per-camera qualification is checked directly on active airborne callback samples.
2. The airborne multiplier is a height-history gate. It includes the equality case on the nested first launch update. Calling it merely a falling-velocity multiplier would misstate the native code. Static evidence at `/tmp/mudcrab-221-static.md` separates verified field consumers from unresolved provenance and position integration questions.
3. `peak_error_gate` is the maximum absolute pointwise height error gate, not the separately reported sampled-apex error. `scripts/evaluate_skyrim_jump.py:294` applies RMSE <=1, maximum point error <=3, window coverage, and the callback-count predicate for native recurrence. Name both metrics accurately. These are declared engineering tolerances, not confidence intervals or a proof of engine equivalence.
4. The QPC forecast is deliberately an unscaled wall-clock hypothesis at half timescale. Only its scoring window length is scaled. Its expected failure there tests that specific hypothesis and is not a general comparison against a clock model that already knows timescale.
5. The sampled apex is not the continuous apex. Launch phase has one camera-boundary uncertainty. Landing is approximated by clipping to the preinput floor. The fixed 1.15 game-second window prevents post-landing dilution from varying arbitrarily, while pointwise metrics remain sample-weighted.
6. Three repeats per declared condition, one actor, one save, one courtyard, one executable and one instrumentation build do not identify arbitrary contact, equipment, world, or actor behavior. There are no confidence intervals or independent platforms.
7. Capture receipts bind source hashes and frozen-file bytes. Score validates run proof fields as values copied into the manifest, rather than independently reopening every private proof or binding each proof to a public signature. This is adequate for the inspected local chain with retained native proofs, not a malicious-producer security guarantee. The evaluator also accepts missing proof fields if a synthetic training corpus omits all of them. The real campaign has nonempty pinned provenance and rejects deviations from it.
8. The known `Path.with_suffix('.log')` collision affects fractional-timescale stdout log names. Native trace, proof and screenshots have distinct directory paths. This report did not re-prove preservation of each private log. Preserve the original source receipt and exact capture source if fixing the path after capture. Do not change the sealed evaluator to fix reporting or capture wrappers.

## Trail review

Read the five existing decision rows through the freeze checkpoint. The freeze row's claim of 13 passing tests is independently reproducible. The design correctly labels camera dt as a hypothesis and the recorder pointer pilot as necessary. The design's planned cadence discrimination needs a new append-only result row documenting the failed manipulation. The existing rows are not evidence that the intended cadence separation happened. Parent was notified immediately when the measured mismatch was found.

No final held-out model verdict is issued here. Capture was still underway when this audit read the manifest. Parent must include every completed attempt and all nine planned held-out runs or explicitly report incompleteness and rejection reasons.

Attention.

reviewed by gpt-6.1-sol

The cadence manipulation failed in inspected completed runs. Same-family review cannot substitute for the unavailable different-family review. The native predictor is independent of future motion observations but conditional on observed timesteps. No source, seal, leakage, or completed-run qualification blocker was found.
