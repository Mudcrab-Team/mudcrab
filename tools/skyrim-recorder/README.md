# Native Skyrim physics observations

This recorder extends the instrumentation developed for [issue #520](https://github.com/Mudcrab-Team/mudcrab/issues/520) to investigate [issue #221](https://github.com/Mudcrab-Team/mudcrab/issues/221). It runs the original game in a private Proton prefix, delivers scripted input through Gamescope/libei, and observes native input, camera and character-state callbacks. The default v3 profile observes unchanged movement parameters. The separate v4 input campaign applies declared controls in its private process. Neither profile changes production Mudcrab physics.

Read the [evidence report](../../docs/research/skyrim-physics-221.md) for results and limitations. The [numeric corpus](reference/jump-v3/manifest.json) contains compressed instrument records, not game assets, saves or executable code. Recorder source imported from the earlier workspace is identified by [source hashes](../../docs/research/evidence/skyrim-physics-221/recorder-origin.json).

## Reproduce the offline evaluation

Use Python 3.11 or newer. No game, native compiler or third-party Python package is needed.

```sh
python3 scripts/evaluate_skyrim_jump.py score \
  --manifest tools/skyrim-recorder/reference/jump-v3/manifest.json \
  --frozen tools/skyrim-recorder/reference/jump-v3/frozen.json \
  --output /tmp/skyrim-jump-recheck
python3 scripts/report_skyrim_jump.py \
  --corpus tools/skyrim-recorder/reference/jump-v3 \
  --results /tmp/skyrim-jump-recheck
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p 'test_*skyrim_jump.py'
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p test_record_skyrim_motion.py
```

Expect the same per-run scores and frozen-model seal. The evaluator verifies its own source hash, the frozen artifact, and every raw trace. A valid trace can fail a model's acceptance thresholds. An invalid trace is reported separately with a rejection reason.

Repeat the two score/report commands with `jump-cadence-v3` in place of `jump-v3` to reproduce the separate corrected 30 Hz experiment. Its three training traces and frozen artifact are exact copies from the original campaign. Its three held-out traces are new observations. Do not count the copied training traces as new runs. Run identifiers are unique within each corpus, so retain the corpus name when combining results.

To reproduce calibration without overwriting the frozen artifact, run:

```sh
python3 scripts/evaluate_skyrim_jump.py freeze \
  --manifest tools/skyrim-recorder/reference/jump-v3/manifest.json \
  --output /tmp/skyrim-jump-refrozen.json
```

`freeze` opens only training traces. `score` cannot invoke fitting. The direct discrete model takes calibrated parameters, a launch boundary and recorded controller dt. It evolves its own velocity, height and maximum height. Future observed position and velocity are scoring targets only.

## Capture a new campaign

Install LLVM with `clang-cl`, `lld-link` and `llvm-dlltool`, a C compiler, libei development files, libX11, Gamescope, xprop and Tesseract with English data. Start authenticated Steam. Use the exact runtime in [runtime.json](runtime.json) and an installed Proton distribution.

The published acquisition plan pins a particular Whiterun save by hash. Another save requires a separately surveyed plan. Supply the original save, not a numerical artifact from this repository.

```sh
python3 scripts/record_skyrim_motion.py prepare \
  --game '/path/to/Skyrim Special Edition' \
  --save '/path/to/fixture.ess' \
  --proton '/path/to/Proton/proton' \
  --session '/path/to/new-private-session'
python3 scripts/capture_skyrim_jump.py \
  --session '/path/to/new-private-session' \
  --private-output '/path/to/new-private-campaign' \
  --corpus '/path/to/new-numeric-corpus' \
  --ocr-prefix / --split train
python3 scripts/evaluate_skyrim_jump.py freeze \
  --manifest '/path/to/new-numeric-corpus/manifest.json' \
  --output '/path/to/new-numeric-corpus/frozen.json'
python3 scripts/capture_skyrim_jump.py \
  --session '/path/to/new-private-session' \
  --private-output '/path/to/new-private-campaign' \
  --corpus '/path/to/new-numeric-corpus' \
  --ocr-prefix / --split heldout \
  --frozen '/path/to/new-numeric-corpus/frozen.json'
```

Use an OCR prefix containing `usr/bin/tesseract`, its libraries and tessdata. The supplied plan makes three training captures at requested 60 Hz. It then alternates 30 Hz, 120 Hz, and 60 Hz with `sgtm 0.5`, repeating each three times. Each run starts a fresh process and reloads a copied save. VSync is disabled; Gamescope refresh and DXVK frame caps receive the requested rate. These requests are not proof of achieved cadence.

The original campaign used the removed `DXVK_FRAME_RATE` environment variable and achieved about 61 Hz for both its 30 and 120 Hz requests. Current source uses `DXVK_CONFIG` and records its value. The corrected follow-up used [jump-cadence-v3.json](fixtures/jump-cadence-v3.json), passed through `--plan`, and reused the original frozen file without training again. Its independent cadence check requires every observed median camera rate to be within 27 to 33 Hz. The evaluator reports actual QPC intervals; it does not silently reject otherwise valid traces merely because a frame request failed.

The exact original acquisition and evaluator source is in [captured-source.zip](../../docs/research/evidence/skyrim-physics-221/captured-source.zip). [cadence-source.zip](../../docs/research/evidence/skyrim-physics-221/cadence-source.zip) overlays only the two changed acquisition scripts and the new plan. The overlay also fixes a stdout log-name collision for fractional timescale names. Raw trace and proof directories were always unique. Source receipts bind each campaign to its own source and plan; do not resume the original campaign with current acquisition source.

The runner sets the fixture pose, waits for stationary first-person observations, and sends a 500 ms W+Space chord. Native handlers record delivered input; the player trajectory and native launch callback prove that a jump occurred. Recording ends two wall seconds after native W release. The sealed scoring window is 1.15 game seconds so the half-speed condition has full coverage. Captures retain logs and screenshots privately. Failed attempts remain in the manifest and can be resumed with the same command. The frozen artifact is hashed before held-out acquisition starts.

The private game directory links source assets but copies mutable files. The original executable and save are hashed. Do not run parallel captures against one prefix or display. Preparation builds a private `dinput8.dll` which forwards DirectInput and installs only pinned instrumentation hooks.

## Wire format and boundaries

Each little-endian record has format `<4Q4I40f48s` and is 256 bytes. Fields are sequence, QPC, camera sample, input sequence, kind, thread, code, state, 40 float slots and an ASCII label. The header is `mudcrab-motion-256-v3`; its sample field holds QPC frequency. Kind 5 is the completion footer. Sequence and sample counters must be contiguous.

| Kind | Observation |
| --- | --- |
| 1 | Header and QPC frequency. |
| 2 | Original movement/jump handler returned. Values 0/1 are button value and held seconds; 2/3 are movement input. |
| 3 | Full `PlayerCamera::Update` returned. Values 0..2 are player XYZ. Other slots retain camera data from #520. |
| 6 | Paired pre/post camera spring data. Values 17 and 32 are camera dt. Its velocity fields are camera spring velocity, never actor velocity. |
| 7 | Character snapshot at the camera return boundary. Same camera sample number as kind 3. |
| 8 | Before/after the original InAir callback for the player. |
| 9 | Before/after the original Jumping callback for the player. |

Character snapshots contain the following 18 floats. Kinds 8 and 9 store pre-state at 0..17 and post-state at 18..35. Kind 7 has one snapshot at 0..17.

| Offset | Meaning |
| --- | --- |
| 0 | Raw proxy Z in Havok coordinates, without the actor's center offset. |
| 1 | Reserved zero. |
| 2, 3 | Controller delta time and reciprocal. The unused native step start/end fields are not recorded. |
| 4..6 | Controller output velocity XYZ. |
| 7..9 | Proxy velocity XYZ. |
| 10 | Controller gravity multiplier. |
| 11 | Controller jump height in Havok length units. |
| 12 | Stored maximum centered height in Creation units. |
| 13..15 | Player XYZ in Creation units. |
| 16, 17 | Current state and requested state, stored as exact small integers in floats. |

Kind 7 additionally stores world gravity XYZ at 18..20, conversion scale at 21 and fall multiplier at 22. Kinds 8/9 store conversion scale at 36, fall multiplier at 37, callback duration in wall seconds at 38, and world gravity Z at 39. Kind 7 requires code 1; kinds 8/9 require code 3 for valid before and after reads.

The camera callback runs on the game thread; character callbacks can run on worker threads. A lock serializes emissions after original callbacks return. QPC timestamps are observation times, not exact launch times. The nested air callback emits before its enclosing launch callback. Their post states therefore cannot be counted as two independent gravity steps.

The profile pins image-relative addresses, signatures and vtable slots. The build checks the executable SHA-256 before compilation, and the DLL checks signatures before installing hooks. Inspect the static evidence independently with:

```sh
python3 scripts/inspect_skyrim_physics.py '/path/to/SkyrimSE.exe'
```

No native position interpolation, controller filter, collision solver or GPU consumption is established by a hook's name. The report states the measured support and limits for each interpretation.

## Observe jump inputs with the separate v4 profile

The [v4 input report](reference/jump-inputs-v4/implementation-inputs.md) records live factors, exact setter arguments, and fresh-process controls. V3 traces, frozen models, and source archives remain separate.

```sh
python3 scripts/record_skyrim_motion.py prepare \
  --game '/path/to/Skyrim Special Edition' \
  --save '/path/to/fixture.ess' \
  --proton '/path/to/Proton/proton' \
  --session '/path/to/new-input-session' --profile inputs-v4
python3 scripts/capture_skyrim_jump_inputs.py \
  --session '/path/to/new-input-session' \
  --private-output '/path/to/new-input-campaign' \
  --corpus '/path/to/new-input-corpus' --ocr-prefix /
python3 scripts/verify_skyrim_jump_inputs.py
```

The header is `mudcrab-jump-inputs-256-v4` with version code 4. The shared wire record remains 256 bytes. Kind 10 has label `setter` or `launch`. `code` is a validity mask. For `setter`, `state` identifies the observed CALL RVA. This profile verifies CALLs at `0x7cf269` and `0x6752ba`, both targeting the unchanged setter at `0x10645e0`. The measured player input path uses `0x6752ba`. Its wrapper receives `controller*` in RCX and a float in XMM1, preserves both, and calls the original exactly once. The wrapper holds no record lock across native code. The native launch callback also remains unchanged.

| Float slots | Input observation |
| --- | --- |
| 0, 12 | Live `fJumpHeightMin` before and after the setter. |
| 1 | Reference `+0x98` unsigned 16-bit scale, converted losslessly to float. |
| 2, 3, 4 | Base type, NPC sex index, and NPC height. |
| 5, 6 | Both race heights. |
| 7 | Exact setter argument in Creation length units. |
| 8, 9, 10 | Controller height before and after the setter, then requested state. |
| 11 | Native game-to-Havok constant. |
| 13, 14, 15 | Swimming setting, actor flag bit 1, and current controller state. |
| 16..21 | Reference, NPC, and race form IDs as low then high unsigned 16-bit halves. |
| 22 | Native reference scale unit. |

Validity bits are setting before `1`, reference scale `2`, base pointer `4`, NPC fields `8`, race pointer `16`, race heights `32`, controller before `64`, controller after `128`, conversion `256`, setting after `512`, swimming setting `1024`, actor flags `2048`, current state `4096`, reference unit `8192`, reference identity `16384`, NPC identity `32768`, and race identity `65536`. Accepted ordinary-ground records require every relevant bit. Failed reads never qualify as zero measurements. Form identity reads use checked base `+0x14` reads. The decoder joins the two halves without float precision loss.

The declared order is baseline, `setgs fJumpHeightMin 64`, default-setting `player.setscale 1.25`, then unchanged baseline. Each condition starts a fresh process and copies the unchanged source save. The fixture accepts typed `jump_height_min` and `reference_scale` controls. It accepts no command strings. Each control must change its observed memory field and preserve the recorded NPC and race factors. `capture_skyrim_jump_inputs.py` writes factor-product predictions before starting each control. `verify_skyrim_jump_inputs.py` recomputes them from the compressed records and checks the source receipts.
