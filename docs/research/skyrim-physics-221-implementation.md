# Native character jump implementation for issue 221

This change implements the character gravity, jump-height impulse, first-step subtraction, and resolved-height history established by the [native study](skyrim-physics-221.md). The production Rust transition passes all 12 original held-out trajectories under the unchanged tolerance. The old production transition passes none. The implementation also replaces the apex diagnostic with measured height above takeoff and loads the supported player's jump inputs from effective package records.

The observations concern Skyrim SE 1.7.104.0, executable SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`. The before-change revision is `1dcca42fc398baaf626e588694f9e08084117f11`. All native addresses below are RVAs from preferred image base `0x140000000`. The frozen evaluator, model and numerical traces retain their original bytes. Acquisition sources and receipts have explicit publication derivatives that remove personal paths and executable byte strings; the [publication record](evidence/skyrim-physics-221/publication.json) maps their original and published hashes. Statements about the original 900/340 character model in that study describe this baseline revision.

## Observed input semantics

The [separate v4 campaign](../../tools/skyrim-recorder/reference/jump-inputs-v4/implementation-inputs.md) records the live setting, raw reference scale, NPC and race factors, exact setter argument, stored controller height, and launch impulse. Four fresh-process runs qualify. Two failed baseline attempts remain in the manifest with their numeric traces and stated limitations.

| Native condition | Live setting | Raw reference scale | Exact setter argument, Creation units | Launch speed, Havok units/s |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 76 | 100 | 78.27999877929688 | 4.684390068054199 |
| Setting changed to 64 | 64 | 100 | 65.91999816894531 | 4.2986907958984375 |
| Reference scale changed to 1.25 | 76 | 125 | 97.84999084472656 | 5.237307071685791 |
| Fresh baseline repeat | 76 | 100 | 78.27999877929688 | 4.684390068054199 |

NPC 7 has height one and sex index zero. Its race is `00013746`, with both height fields equal to float32 `1.0299999713897705`. Those factors stay fixed across the controls. Each intervention's setting and reference changes are verified in memory. The repeat restores the baseline without carrying intervention state across processes.

The reconstructed multiplication preserves native float32 order. Reference scale is unsigned 16-bit scale multiplied by native float32 0.01. The NPC factor is selected race height multiplied by NPC height. Reference factor multiplies that product, then the loaded `fJumpHeightMin` multiplies the resulting scale. Setter `0x10645e0` converts the argument to Havok length. Player CALL `0x6752ba` supplies every accepted setter observation. The earlier inspected CALL `0x7cf269` reaches the same setter through another path and did not capture these player jumps.

The exact input product, stored height, and launch predictions agree in all four runs. No trajectory fit supplies those values. The [static verifier](../../scripts/inspect_skyrim_jump_inputs.py) checks hashes of ranges in the operator-owned pinned binary and their recorded field meanings. The [numeric verifier](../../scripts/verify_skyrim_jump_inputs.py) recomputes the products, controls, trace hashes, and archived acquisition source hashes.

## Production semantics and supported inputs

[VerticalMotion](../../crates/engine/src/physics/vertical.rs) owns velocity and flight history. Velocity remains in native Havok units until the displacement boundary. Native float32 gravity is `9.8100004196167` downward. The two length conversion values are `69.99124908447266` and `0.014287499710917473`.

Launch derives velocity from the absolute effective height and gravity. It initializes the prior maximum to the current resolved body height and applies gravity immediately. A strict height increase selects multiplier one and advances the maximum. Equal or lower resolved height selects float32 1.35. First-launch equality therefore receives the stronger subtraction. Collision-resolved height from the preceding tick selects this branch. A post-Writeback diagnostic never changes the gravity maximum.

The apex diagnostic records maximum resolved height above the jump origin, including clipped motion, and retains the completed apex after landing. Motion resets clear this history. Console and settings pauses preserve it. The existing custom stair sweeps remain in place.

[MovementTuning startup](../../crates/engine/src/physics.rs) reads effective `fJumpHeightMin` by editor ID, canonical Player NPC 7 `ACBS` and `NAM6`, and canonical RACE `DATA` selected through normalized `RNAM`. Checked archive decoding and field validation reject malformed supported records. Nonzero active template inheritance is unsupported. Finite signed or zero settings are accepted because the native impulse uses absolute height. Actor height factors must be finite and positive.

Plugin attribution uses `plugins.priority`, the exported load order. Plugin row IDs are a separate identity. Tests use shifted plugin row IDs, a later GMST override with a different FormID, asymmetric race heights, nonunit NPC height, and an RNAM whose raw plugin slot differs from its normalized slot. Generated fixture records now carry complete jump inputs.

The current player representation uses reference scale one and own-record actor fields. Save-specific race, sex, height, runtime scale changes, and swimming are not implemented by this profile. Table-less legacy packages and asset-free fixtures explicitly use a synthetic unit actor with setting 76. A modern package with missing or unsupported fields fails with the specific input diagnostic. A missing schema table suggests reconversion. Multiple distinct GMST FormIDs sharing `fJumpHeightMin` remain unsupported and require checking plugin settings; reconversion cannot resolve that ambiguity.

These are distinct implementation policies. Bevy and Rapier retain the fixed 60 Hz schedule, while WALK reads `Time<Fixed>` for its actual delta. Rigid-body gravity remains 900 Creation units/s². The WALK gravity transition also applies when falling from a ledge, not only after a jump. Its equal-or-lower-height branch applies about 926.93 Creation units/s² instead of the previous 900. NOCLIP movement and rigid-body gravity are unaffected. Falling off support and ceiling/contact behavior remain engine policies tested through Rapier, not new measurements of native collision semantics.

## Before and after evidence

The [production replay](../../scripts/check_skyrim_jump_runtime.py) invokes the actual Rust `integrate_walk` transition. Rust receives one height derived only from the original three training recordings and each run's recorded controller timestep sequence. It evolves its own height, maximum, velocity, and idealized floor contact. Observed future position, velocity, support, maximum, and apex never enter the prediction.

| Production replay | Original held-out passes | Worst RMSE | Worst pointwise error |
| --- | ---: | ---: | ---: |
| Before, original 900/340 | 0/12 | 15.18178563 | 36.20208931 |
| After, native height and history semantics | 12/12 | 0.11414555 | 0.22509766 |

Errors are Creation units over the same 1.15 game-second window. Gates remain RMSE ≤ 1 and maximum pointwise error ≤ 3. Both versions use native recorded timesteps for this comparison. The old fixed-60-Hz candidate in the original research is a separate experiment.

Of the 12 held-out recordings, six repeat the roughly 61 Hz training cadence despite requesting 30 or 120 Hz. Three use half game speed at roughly 61 camera Hz, and three corrected-control captures achieve roughly 30 camera Hz. The six failed cadence controls remain repeatability evidence. They do not validate native behavior at 120 Hz. Historical run IDs retain the requested frame caps; displays identify the achieved cadence separately. The [interactive comparison](evidence/skyrim-physics-221/implementation/comparison.html) displays the recorded controller step and all 12 trajectories. Reusing this original held-out corpus is implementation regression validation. It is not a newly blinded experiment, and no held-out fit or threshold adjustment occurred.

The [before-change evidence](evidence/skyrim-physics-221/implementation/baseline/README.md) includes exact source bytes, predictions, receipts, and the first-step assertion failure. That assertion observed `5.666667` before the change against native expectation `5.2069583`; it now passes in the ordinary test suite. The [implementation result](evidence/skyrim-physics-221/implementation/current/result.json), [source receipt](evidence/skyrim-physics-221/implementation/current/source.json) and [source snapshot](evidence/skyrim-physics-221/implementation/current/production-source.zip) identify the implementation measured on 2026-11-06. Both saved before/after results are historical snapshots, validated against their archived source. CI separately runs the current production code on the pinned recordings, so a later source edit does not rewrite historical evidence.

Real Bevy/Rapier tests sample raw body transforms and controller outputs after Writeback. They cover launch, apex, repeated landing, a ceiling plateau, a ledge fall to a lower landing, a new jump origin, pause/resume, motion resets, and actual fixed-clock delta use. Separate tests cover signed and zero height and strict resolved-height history. The existing stair and dynamic-body tests pass.

A nonignored Rust test decompresses a committed, SHA-256-pinned 30 Hz native recording and compares production output with its observed heights. Only the training-derived height and recorded timesteps enter the prediction. The full 12-run comparison remains a separate CI gate.

The saved headless exports exercise the asset-free fixture. Its new profile is explicitly synthetic height 76, unlike the native actor's roughly 78.28. [Before](evidence/skyrim-physics-221/implementation/baseline/headless.json), first rise is `5.66666794`, apex rise is `67.08328247`, and landing occurs at tick 47. [After](evidence/skyrim-physics-221/implementation/current/headless.json), first rise is `5.12679291`, apex rise is `71.47846222`, and landing occurs at tick 51. Those measurements prove the production fixture's behavior; they are not an equal-actor native apex comparison.

## Issue coverage and unresolved questions

| Issue item | Established result and current boundary |
| --- | --- |
| Havok scale | The character path uses float32 69.99124908447266 and 0.014287499710917473. Serialized NIF collision geometry scaling remains unresolved. The converter retains 70 and this change requires no collision reconversion. |
| MOVT SPED | Directional fields and package provenance remain supported. Native speed units, actor-value modifiers, actor scale ordering, animation coupling, and selection paths remain unresolved. Numerical MOVT behavior is unchanged. |
| Gravity | Character world gravity, controller multiplier, and height-history branch are observed. Character production behavior is corrected. Native rigid-body acceleration remains unresolved, so its separate 900 policy remains. |
| Launch and apex | The baseline loaded factors, exact setter/store, height-derived impulse, first subtraction, and cadence-dependent apex are established for the recorded ground jumps. General actors, save mutations, inherited traits, and swimming remain unsupported. |
| Time base | The recordings establish consumed controller dt, velocity-before-position integration, and a one-camera-boundary lag. Native dt production, quantization, catch-up, clamping, and achieved 120 Hz behavior remain unresolved. The project keeps fixed 60 Hz. |
| Light distances | The inspected light constructor selects a radius and stores it without a Havok conversion. Full reference radius composition and downstream photometry remain unresolved. Light numbers and metre policy are unchanged. |

The [bounded light observation](evidence/skyrim-physics-221/light-radius-construction.txt) lists recorded structural facts and commands for inspecting an operator-owned executable. Copied instruction listings and executable byte strings have been removed from the published tree. `TESObjectLIGH` vtable `0x18140f0`, slot `0x4a`, identifies `0x274090`, which calls constructor `0x274390`. At `0x2746a2`, the constructor loads base radius from `+0xe4`. An optional reference helper at `0x2ffd70` supplies another selected value. Stores at `0x2746e6` and `0x2746f3` write that value to `+0x128`, `+0x12c`, and `+0x130`. This local boundary does not prove every later light transform or attenuation equation.

The issue remains referenced rather than automatically closed because the table retains substantive unknowns. The [implementation decisions](evidence/skyrim-physics-221/implementation-decisions.tsv) record the design and proof boundaries.

## Reproduce the implementation checks

Use the repository's pinned Rust toolchain and Python 3.11 or newer. Run from the repository root and choose fresh output paths.

```sh
python3 -B scripts/verify_skyrim_jump_inputs.py --output /tmp/jump-inputs.json
python3 -B scripts/check_skyrim_jump_runtime.py \
  --output /tmp/jump-production --require-heldout-pass
cargo test -p engine --lib physics::jump_replay_tests
MUDCRAB_JUMP_HEADLESS_OUTPUT=/tmp/jump-headless.json \
  cargo test -p engine --lib physics::jump_replay_tests::export_headless_jump \
  -- --ignored --exact --nocapture
python3 -B scripts/report_skyrim_jump_runtime.py --output /tmp/jump-comparison.html
```

The input verifier and production held-out replay are also CI checks. The [native recorder guide](../../tools/skyrim-recorder/README.md) gives fresh-capture commands and the operator-owned executable/save requirements. No game executable, save, or game asset is included in this evidence package.

The [validation record](evidence/skyrim-physics-221/implementation/validation.md) retains exact local commands, results, source hashes, compressed logs and environment limitations. Its portable recheck verifies the saved before/after evidence independently of executing Rust again.
