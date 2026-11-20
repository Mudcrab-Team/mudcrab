# Native Skyrim gravity, launch, apex and timestep evidence

This study investigates [issue #221](https://github.com/Mudcrab-Team/mudcrab/issues/221) with the native instrumentation developed for [#520](https://github.com/Mudcrab-Team/mudcrab/issues/520). It established the meaning of physics values before changing Mudcrab. Production physics and conversion constants were unchanged during this study. The subsequent [implementation report](skyrim-physics-221-implementation.md) records the live input controls, production correction and before/after validation. The baseline comparisons and frozen study below remain historical evidence.

The experiment uses original Skyrim SE 1.7.104.0 under Proton on 2026-11-06. The executable SHA-256 is `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`. Mudcrab's comparison revision is `1dcca42fc398baaf626e588694f9e08084117f11`. All addresses below are RVAs relative to preferred image base `0x140000000`.

The frozen native recurrence passes all 12 held-out runs from 15 unique fresh captures. Mudcrab's baseline `900 / 340` fixed-step recurrence passes none. The recordings support a launch-height-derived impulse, a height-history gravity multiplier and timestep-dependent apex. Actual 30 Hz and half-speed runs distinguish these semantics from an ascent parabola fitted at about 61 Hz.

## Observed facts and instruction-backed facts

The recorder observes player position, proxy position, controller velocity and controller dt separately from camera spring state. QPC measures observation time. The original native callbacks execute unchanged; sampling and file writes add overhead. The result concerns this instrumented runtime, save, actor and flat Whiterun courtyard.

The binary's air-state callback is RVA `0x10c7cc0`, reached through vtable slot RVA `0x1a92608`. It uses controller dt at `+0x88` and adds the world gravity vector times controller `+0x248` times dt to output velocity at `+0x90`. The multiplier at `+0x248` is dimensionless, not an acceleration. The observed world gravity Z is approximately `-9.81000042`; the observed controller multiplier is `1`.

The air-state callback compares centered controller height, converted to Creation units, with stored maximum height at `+0x240`. A strictly greater height advances that maximum and uses multiplier `1`. Equal or lower height selects the float32 setting `1.35000002384` at RVA `0x20d2558`. This is a height-history test. It also selects `1.35` during the first launch update, when current and stored height are equal. A simple rule that applies extra gravity only when velocity is negative misses that first update.

The Jumping callback is RVA `0x10c8030`, reached through vtable slot RVA `0x1a92680`. It derives an impulse of magnitude `sqrt(2 * abs(scaled world gravity) * abs(controller jump height))` opposite gravity and adds it to existing velocity. It then invokes a nested state callback. The recorded air entry inside launch isolates the impulse before that nested gravity subtraction. The outer launch's post-state already includes the subtraction.

The position conversion constant at RVA `0x17f423c` is float32 `69.99124908447266`. The opposite-direction constant at RVA `0x17fde5c` is float32 `0.014287499710917473`. The inspected movement path therefore does not literally use `70.0`. These observations do not justify changing every converter scale without checking its separate consumers and reconversion implications.

A jump-height setter at RVA `0x10645e0` converts its Creation-length input using the opposite-direction constant and stores controller `+0x23c`. The caller at `0x7cf220` reads `fJumpHeightMin` at RVA `0x20a8960`, multiplies by the reference-scale helper `0x2e6f30`, and conditionally applies `fJumpSwimmingMult`. The scale helper includes reference scale and, for an NPC, race and NPC height through helper `0x3c65c0`. The setting's file initializer is `64`, which is not an observation of its loaded gameplay value. The live setting value and individual scale inputs were not recorded.

The observed controller jump-height field is about `1.11842549` Havok length units. Its converted value is about `78.28` Creation units. It is an impulse-construction parameter; it is not the measured jump apex.

The native recorder verifies the player's controller type against vtable RVA `0x1a89578`. The process pointer is player `+0xf8`, followed by process `+8` and middle-process `+0x250`. The older header's player `+0xf0` process offset does not match this executable. Proxy velocity comes from controller `+0x340` then proxy `+0x60`. Raw proxy position and player position have different origins, so the comparison uses height changes.

These facts are reproduced by the [static verifier](../../scripts/inspect_skyrim_physics.py), its [recorded output](evidence/skyrim-physics-221/static.json), the [runtime profile](../../tools/skyrim-recorder/runtime.json), and the numeric traces linked below. The published checks contain range lengths and SHA-256 identities. They read executable bytes only from the operator-owned local game file.

## Inferred parameters and independent models

Three new runs at about 60.9 observed camera Hz calibrate seven fixed candidates. Old #520 jump data and two new instrumentation pilots were development evidence, not held-out validation. The frozen model uses native measurements from training only for base acceleration, initial velocity and the history multiplier. Independent position fits also estimate curvature and an effective launch coefficient under separate clocks.

The native discrete candidate's calibrated values are base acceleration `686.614182888` Creation units per game-second squared, pre-subtraction launch velocity `327.866312062` Creation units per game second, and history multiplier `1.35000002384`. The implied stronger acceleration is about `926.929163` Creation units per game-second squared. These are scoped to the recorded actor and settings. They are not universal replacement constants.

The native candidate starts at the recorded launch boundary with the frozen velocity, zero relative height and zero maximum height. At each observed controller timestep it chooses the multiplier from its own height history, subtracts gravity from its own velocity, and advances its own height with the updated velocity. It never consumes future observed height, velocity, maximum height, ground state or apex. It clips to an idealized floor at launch height. Native landing placement can differ from that floor.

The independent polynomial ascent fit estimates about `686.69` acceleration and `318.32` as the linear coefficient under summed camera time. That coefficient is not the directly observed launch impulse of `327.87`. Launch phase and discrete integration affect a position-fit coefficient. Reporting both as the same velocity would hide the semantics under investigation.

The other candidates are a single-acceleration parabola, a two-acceleration approximation switching at its own predicted apex, and Mudcrab's current `900 / 340` fixed-60-Hz recurrence. The first two are calibrated independently for QPC time and summed camera time. At half game speed the QPC candidate deliberately retains wall seconds; only its scoring window is lengthened. It tests whether wall seconds can stand in for game seconds.

This is conditional trajectory evaluation using native timestep schedules and a native launch boundary. It does not predict input-to-launch latency, generate native frame pacing, reproduce collision/contact resolution, or run Mudcrab's full Rapier controller.

## Frozen validation protocol

The [acquisition plan](../../tools/skyrim-recorder/fixtures/jump-v3.json) preassigns three repetitions per condition. Three requested 60 Hz runs train. Nine subsequent held-out runs alternate requested 30 Hz, 120 Hz, and 60 Hz with `sgtm 0.5`. Each starts a fresh process, reloads the same copied save, returns to the surveyed pose, settles, and receives a 500 ms W+Space chord. The source save, runtime, native DLL, input sender, Proton launcher, settings and source hashes accompany the corpus.

The [frozen artifact](../../tools/skyrim-recorder/reference/jump-v3/frozen.json) was written before any held-out capture. Its seal is `c1026fb434bb72d0bb4b255888fd340dd202590e0f9a73d7a19b6a6e0607d575`. The held-out capture receipt pins the complete frozen file's SHA-256 separately. No held-out parameter, clock or phase refit is allowed.

The scored window is 1.15 game seconds after the preceding camera boundary. That duration was fixed before validation so the two-wall-second post-release capture also covers the half-speed condition. The report gate requires full window coverage, height RMSE no greater than 1 unit and maximum absolute pointwise error no greater than 3 units. The discrete model additionally requires the observed one-controller-update-per-camera assumption during flight. Sampled apex error is reported separately and does not substitute for the pointwise error gate.

Observed apex means maximum sampled player Z minus the stationary baseline. It is a lower bound on a continuous apex, not a fitted maximum. Event timing is reported with neighboring sample intervals. Three repeats provide a range, not a population confidence interval. An invalid capture is a rejection; a valid trace on which a model fails is a model failure. Neither is silently omitted.

## Original held-out results

All 12 declared original captures succeeded on their first attempt. The three training runs and nine held-out runs remain in the corpus. No held-out trace was rejected or removed. The [condition summaries](../../tools/skyrim-recorder/reference/jump-v3/result/summary.json) and [interactive chart](../../tools/skyrim-recorder/reference/jump-v3/result/report.html) summarize the sealed evaluation. The reproduction commands regenerate per-run scores, curves and candidate comparisons in a fresh output directory; those redundant JSON exports are not committed.

The requested 30 and 120 Hz conditions both ran at about 60.9 camera Hz. They validate repeatability at that cadence, not transfer to those requested rates. The half-speed treatment worked. Its median camera-time to wall-time ratio was about 0.4988, while wall cadence stayed about 60.9 Hz.

Each table row summarizes three runs. Hz, controller dt, apex and RMSE are medians except the explicitly labeled apex range. Heights are Creation units. Error is prediction minus measured player height over the fixed window.

| Requested condition | Observed camera Hz | Controller dt, ms | Observed sampled apex range | Native evaluator RMSE | Mudcrab recurrence RMSE | Native passes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 60 Hz, 1x, training | 60.88 | 16.333 | 73.7581 to 73.7776 | 0.11125 | 11.439 | 3/3 |
| 30 Hz, 1x, held out | 60.90 | 16.333 | 73.7588 to 73.7773 | 0.11100 | 11.440 | 3/3 |
| 120 Hz, 1x, held out | 60.89 | 16.333 | 73.7583 to 73.7781 | 0.11125 | 11.446 | 3/3 |
| 60 Hz, 0.5x, held out | 60.89 | 8.167 | 76.0024 to 76.0115 | 0.02973 | 13.067 | 3/3 |

The native discrete candidate passes 9/9 held-out runs. Its largest pointwise height error is 0.22095 units. Airborne-only RMSE is below 0.00064 units in every held-out run. Most full-window error comes after landing, where the native contact places the player about 0.22 units above the original baseline at normal speed. The candidate clips to the original baseline. Tiny airborne residuals describe agreement in this controlled trace, not universal engine accuracy.

The single-acceleration candidate fails 9/9 held-out runs under both clocks. The camera-clock two-acceleration approximation passes all six normal-speed runs but fails all three half-speed runs, where median RMSE is about 2.30 units. It fitted a cadence-specific effective launch coefficient. The QPC two-acceleration candidate similarly passes the six normal-speed runs but has about 39.23 units median RMSE at half speed. Unscaled wall seconds cannot substitute for the observed game clock. Mudcrab's current recurrence fails 9/9 under both clocks.

## Cadence-control correction and follow-up

The independent audit identified the failed cadence manipulation before the original campaign finished. The runner used `DXVK_FRAME_RATE`, which DXVK 3 removed. The installed native log identifies DXVK `v3.1.1-97-ga78a467cadb22be`. The corrected control uses `DXVK_CONFIG="dxvk.maxFrameRate = 30"`, consistent with the [DXVK 3.0 release notes](https://github.com/doitsujin/dxvk/releases/tag/v3.0). The new log confirms that setting in the effective configuration.

The [separate follow-up plan](../../tools/skyrim-recorder/fixtures/jump-cadence-v3.json) declared three new 30 Hz held-out captures after the original nine completed. It requires an observed median camera rate between 27 and 33 Hz in every run before making a cadence claim. It reuses the identical frozen artifact and three original training traces. There is no parameter, evaluator, threshold or phase refit. The [follow-up acquisition source archive](evidence/skyrim-physics-221/cadence-source.zip) preserves the control correction. The original campaign archive preserves its ineffective control. Publication derivatives and original acquisition hashes are distinguished in the [publication record](evidence/skyrim-physics-221/publication.json).

All three follow-up captures succeeded on their first attempt, qualified, and achieved about 30.01 camera Hz. Their median controller dt is 33.333 ms. The [follow-up manifest](../../tools/skyrim-recorder/reference/jump-cadence-v3/manifest.json), [summary](../../tools/skyrim-recorder/reference/jump-cadence-v3/result/summary.json), and [interactive chart](../../tools/skyrim-recorder/reference/jump-cadence-v3/result/report.html) retain the observations and results. The same reproduction commands regenerate detailed JSON exports.

| Achieved condition | Observed sampled apex range | Native evaluator median RMSE | Native maximum pointwise error | Mudcrab median RMSE | Native passes |
| --- | ---: | ---: | ---: | ---: | ---: |
| 30.01 camera Hz, 1x | 69.1799 to 69.1985 | 0.11229 | 0.22510 | 6.082 | 3/3 |

The direct launch impulse remains 327.8663 units per game second. Its first post-gravity velocity is about 296.97. The lower sampled apex follows from the larger controller steps, without changing the launch parameter. Airborne-only RMSE remains below 0.00037 units. The fixed camera-clock two-acceleration approximation fails all three follow-up runs with median RMSE 4.508 units. Every other candidate also fails the declared gate in these runs.

Across the two campaigns there are 15 unique fresh recordings, comprising three training and 12 held-out runs. The unchanged native discrete candidate passes 12/12 held-out runs; the Mudcrab recurrence passes 0/12. The largest native pointwise error across held-out runs is 0.22510 units. These are conditional trajectory results for this fixture, not a full Mudcrab gameplay comparison. The new experiment establishes actual 30 Hz transfer. Native behavior at 120 Hz remains untested.

## Timestep semantics supported by the recordings

In every qualified run, controller dt at a camera boundary exactly equals the camera dt recorded at the preceding boundary. The maximum absolute lagged difference is zero in the recorded float32 values. The same-boundary values sometimes differ. Camera observation timestamps alone therefore do not identify which dt advanced the controller.

At about 60.9 camera Hz and ordinary game speed, controller dt values alternate around 16.333 and 16.500 ms. At half game speed they are about 8.167 and 8.250 ms. At about 30 camera Hz they are about 33.167 and 33.333 ms. QPC intervals jitter independently. The recordings establish these dt values and their observed one-frame association. They do not identify the engine's smoothing, quantization, catch-up or maximum-step policy.

During each qualified flight, there is one observed air-state update per camera sample. Its velocity change agrees with `world_gravity * controller_multiplier * history_multiplier * controller_dt`. The first launch impulse is about 327.8663 units per game second in every original run. Its post-callback velocity is about 312.57 to 312.73 at normal speed, or 320.22 to 320.30 at half speed, because the nested first air callback already subtracts gravity.

The independent recurrence predicts the change in apex when controller dt changes while the launch impulse remains constant. This supports velocity-before-position integration and the first-step history multiplier for these trajectories. It does not prove that an arbitrary render callback owns the physics step, nor that every native object shares this controller clock.

## Comparison with the baseline Mudcrab revision

[physics.rs](../../crates/engine/src/physics.rs) fixes both Bevy and Rapier to 60 Hz, sets gravity to `900`, and launches WALK at `340`. On the grounded launch tick it requests the full `340 * dt` displacement. Subsequent airborne ticks subtract `900 * dt` before displacement. After N unobstructed steps the displacement is `N * J * h - g * h² * N * (N - 1) / 2`.

Its idealized discrete apex is about `67.0833` Creation units. The continuous expression `340² / (2 * 900)` gives `64.2222` and omits the launch-tick convention. Neither should be confused with the internal `WalkState.jump_apex` diagnostic, which tracks `max(vertical_velocity * jump_time)` rather than accumulated or observed height. The evaluator measures height directly.

The native state evidence distinguishes world gravity, controller scaling, a height-history branch, launch-height scaling, and timestep-dependent integration. Substituting one inferred acceleration or launch number into Mudcrab would not reproduce those semantics. Production gravity, launch, fixed timestep and `HAVOK_TO_CREATION` remain unchanged.

## Unresolved hypotheses and next discriminating observations

| Unresolved question | Evidence needed before changing behavior |
| --- | --- |
| Which loaded setting and scale inputs produce 78.28 for this actor? | Record live `fJumpHeightMin`, reference scale, race height, NPC height and the setter argument. The file initializer alone is insufficient. |
| What schedules and bounds controller dt? | Observe the producer of the native step under achieved high cadence, intentional stalls and varied timescale. Requested 120 Hz was not achieved here. |
| Does the law transfer to other actors or states? | New frozen predictions for different actor scales, equipment, swimming, falling without jumping, slopes and contacts. |
| Do dynamic objects use the same effective acceleration? | Observe native rigid-body trajectories and world/controller ownership separately. Character-state gravity is not a blanket rigid-body replacement. |
| What causes the small landing offset? | Observe contact normals, support state and controller/actor synchronization through landing. The independent model intentionally has only an idealized floor. |
| How much does recording perturb scheduling? | Compare reduced-observation or buffered recorder builds under a new protocol. Callbacks run original code, but observation overhead is not zero. |

The current evidence supports implementing and reviewing these semantics in a separate physics change. It does not support copying one fitted scalar into every production consumer. MOVT, light conversion and other parts of #221 remain outside this study.

## Reproduction and evidence

Follow [the reproduction guide](../../tools/skyrim-recorder/README.md) to rebuild native observations or rerun the offline study. The [manifest](../../tools/skyrim-recorder/reference/jump-v3/manifest.json) identifies each compressed numerical trace and its uncompressed hash. The [decision trail](evidence/skyrim-physics-221/decisions.tsv) records protocol decisions, exclusions and verification. The [source archive](evidence/skyrim-physics-221/captured-source.zip) publishes the acquisition source with local paths and executable byte strings removed. Its original-to-published hash mapping is explicit in the [publication record](evidence/skyrim-physics-221/publication.json). The frozen evaluator and numerical traces retain their original bytes.

The evaluator is standalone Python with an independent binary decoder. Synthetic checks cover known-parameter recovery, the native recurrence against a separate closed form, Mudcrab's first displacement, missing records/footer, corrupt hashes, no jump, changed seals, training/held-out overlap, prediction input isolation and a score path that cannot call fitting. Native capture acceptance remains separate from those synthetic checks.

At the end of this research phase, verification passed all 13 evaluator tests and all 13 inherited/extended recorder tests. Repeating calibration from only the original three training traces produced a byte-identical frozen file. Repeating the static inspection produced a byte-identical report. Both campaigns' acquisition source receipts matched their archives and unchanged shared source. The source executable and source save still matched their initial hashes. No production file had changed at that point, so this phase required no Rust rebuild. The implementation report records the subsequent production checks.

The [independent audit](evidence/skyrim-physics-221/independent-audit.md) was performed by gpt-6.1-sol before acquisition finished. It verified evaluator isolation and the then-completed provenance chain, and caught the ineffective frame control. The original campaign and subsequent correction are both retained. A same-family reviewer is not a substitute for the different-family review, whose providers were disabled in this environment.

A fresh gpt-6.1-sol reviewer then audited the [completed evidence](evidence/skyrim-physics-221/final-audit.md). It reproduced all raw-trace scores and generated reports exactly, checked frozen files, source archives, the private trace/proof chain, actual cadence and static evidence, and found no blocker. This second review was also within the same model family. The renderer omits rejected rows from charts, so any future campaign must read rejection and attempt records alongside its charts; this study has zero rejected rows.

Exact native reruns require the operator-owned executable, save and installed game assets. The repository bundles only numeric traces, source and reports. Proton's launcher is hashed and the observed DXVK version is recorded; this is not a content-addressed image of every host library and game asset. Offline evaluation is fully reproducible from the bundled files. Capture receipts document the inspected local acquisition chain; they are not signatures against a malicious producer.
