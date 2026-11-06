# Native jump inputs for implementation of issue 221

The goal is a proved native jump-height input for the character implementation, with the PR left open for review. Four fresh-process observations identify the loaded factors and check two separate controls. [The result](result.json) passes without fitting or changing the frozen v3 study.

| Condition | Live GMST | Raw reference u16 | Exact setter argument, Creation | Stored height, Havok | Launch velocity, Havok per second |
| --- | --- | --- | --- | --- | --- |
| baseline | 76 | 100 | 78.279998779297 | 1.118425488471985 | 4.684390068054199 |
| setting-64 | 64 | 100 | 65.919998168945 | 0.9418319463729858 | 4.298690795898438 |
| reference-125 | 76 | 125 | 97.849990844727 | 1.398031711578369 | 5.237307071685791 |
| repeat-baseline | 76 | 100 | 78.279998779297 | 1.118425488471985 | 4.684390068054199 |

All four setter and stored-height predictions match exactly. Float32 launch predictions also match exactly. The launch acceptance threshold was `1e-6` Havok units per second. The unchanged repeat reproduces all baseline inputs and values.

The guarded live reference is `00000014`, with base NPC `00000007` of type `43`. Its sex index is `0` and NPC height is `1`. The NPC race pointer resolves form `00013746`. Both race heights are `1.0299999713897705`. These factors remain unchanged across all controls. The baseline therefore uses a unit reference factor, `f32(100 × f32(0.01)) = 1`, and the NPC/race factor `f32(1 × f32(1.03))`. The exact baseline setter argument is `78.27999877929688`, rather than the file initializer `64`.

A production policy that uses reference factor one has direct support for this canonical baseline. Dynamic save changes to reference scale or race remain separate behavior. These measurements do not establish defaults for arbitrary NPCs, active trait inheritance, swimming, negative heights, or missing package records.

The setting control measures live `fJumpHeightMin = 64`, with the raw reference still `100`. The reference control measures raw reference `125`, with a fresh-process default setting of `76`. Each control preserves the base NPC and race fields. The runner sends the typed controls through its private batch file. A successful console transcript alone does not qualify a measurement.

The reconstruction preserves float32 products. `reference = f32(raw_u16 × native_0.01)`. `npc = f32(selected_race_height × npc_height)`. `scale = f32(reference × npc)`. `argument = f32(scale × live_setting)`. `stored = f32(argument × native_game_to_Havok)`. Native game-to-Havok is `0.014287499710917473`. Launch prediction uses the observed native gravity and controller gravity multiplier with float32 `sqrt(2gh)`. The observed effective gravity is `9.8100004196167` Havok units per second squared. Baseline launch is `327.8663120620113` Creation units per second.

The separate profile is [runtime-inputs-v4.json](../../runtime-inputs-v4.json). Its header is `mudcrab-jump-inputs-256-v4`, version code `4`. The [shared extension](../../jump_inputs.h) observes the unchanged `void(controller*, float)` setter at `0x10645e0`. It verifies CALL `0x7cf269` and the measured player CALL `0x6752ba`. The latter supplied every accepted setter record. Windows x64 passes the controller in RCX and height in XMM1. The wrapper preserves both and calls the original exactly once in each branch. No record lock is held across native callbacks.

Kind 10 snapshots use checked reads and explicit validity bits. The full setter mask is `131071`, and the launch snapshot mask is `130431`. Raw form IDs use two lossless 16-bit float slots each. Setter setting-before and setting-after samples agree, as do setting and actor factors between setter and launch. The setting sample is near the original native load, rather than a capture of its XMM6 register. The protocol has no concurrent setting writer during a jump.

[The static check](static.json) independently validates the pinned executable, both CALL signatures, their shared setter leaf ABI, and the player setting/scale products. The build separately checks the executable identity and signatures. Both v3 and v4 native builds pass `clang-cl /W4 /WX`. The default build retains the v3 header and behavior. The recorder tests pass 13 checks, and the input proof tests pass 10 checks.

[The manifest](manifest.json) stores the executable, save, DLL, input sender, Proton, profile and source hashes, exact commands, fixture, pre-capture control predictions, and recomputed results. The private session is `/home/aivv/.cache/mudcrab-221/session-inputs-v4`. Logs and screenshots remain in `/home/aivv/.cache/mudcrab-221/campaign-inputs-v4`. The original session and save remain unchanged. Captures used requested 60 Hz, `DXVK_CONFIG = dxvk.maxFrameRate = 60`, Gamescope, and libei. A frame-rate request is not a scheduler result.

There were six attempts. Baseline attempt 1 was interrupted after a view toggle was lost around the survival modal. The readiness loop now waits after modal handling and retries the view toggle. Attempt 2 recorded live factors and launch but missed the exact setter because it observed only CALL `0x7cf269`. The additional player CALL was independently verified before attempt 3. Both failed attempts retain compressed numeric traces, failure reasons, commands, and native build receipts. Their pre-capture Python source receipts are incomplete, and neither counts as accepted input evidence.

The [capture source archive](capture-source.zip) preserves the exact source bytes recorded by all four accepted receipts. The verifier checks those archived hashes so later recorder maintenance does not rewrite the acquisition history. Its SHA-256 is `1a412594a03829d11d761bb484a94bc81a6df915c79f3de27aad7ba801a5efbd`.

Reproduce the numeric result from the repository root.

```sh
python3 scripts/verify_skyrim_jump_inputs.py \
  --output /tmp/skyrim-jump-inputs-rechecked.json
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p test_record_skyrim_motion.py
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p test_analyze_skyrim_jump_inputs.py
python3 scripts/inspect_skyrim_jump_inputs.py '/path/to/SkyrimSE.exe'
```

Fresh native collection uses `record_skyrim_motion.py prepare --profile inputs-v4` and `capture_skyrim_jump_inputs.py` with a new private session and corpus. The [recorder guide](../../README.md#observe-jump-inputs-with-the-separate-v4-profile) gives the complete preparation command. Accepted raw records are compressed under [traces](traces). Existing v3 traces, frozen models, source archives, the sealed evaluator, and production constants were not edited by this measurement.
