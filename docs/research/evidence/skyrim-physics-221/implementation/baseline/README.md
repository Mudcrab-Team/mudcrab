# Before-change production evidence

These files were captured before changing production movement at revision `1dcca42fc398baaf626e588694f9e08084117f11`. The only Rust addition at acquisition was a test module that called the existing transition and real Bevy/Rapier fixture.

`result.json` scores that production transition against all 15 unique native recordings, including 12 held-out runs. It passes none of the held-out gates. This baseline uses each native timestep schedule. The original research also tested a fixed-60-Hz candidate; those two baselines are different experiments.

`input.json` contains only one original-training height and each run's timestep sequence. The old transition explicitly ignores the height and uses its original 900/340 values. `predictions.json` contains its independently evolved positions. `source.json` records input, source, corpus and archive hashes. `production-source.zip` preserves the source bytes listed in that receipt.

`headless.json` records raw post-Writeback transforms and KinematicCharacterController outputs from the real production fixture. The first resolved rise is 5.66666794 Creation units, the apex rise is 67.08328247, and landing occurs at tick 47. `first-step-failure.txt` records the old transition failing the native first-step assertion. The assertion process exited 101.

To reproduce the baseline, use a separate checkout at the revision above. Overlay `production-source.zip`, `scripts/evaluate_skyrim_jump.py`, both `tools/skyrim-recorder/reference/jump*-v3/` corpora and their two acquisition source archives from this change, preserving their repository paths. Run the commands below from that checkout. Choose a fresh output directory so the saved evidence remains intact.

```sh
python3 -B scripts/check_skyrim_jump_runtime.py --output /tmp/mudcrab-jump-before
MUDCRAB_JUMP_HEADLESS_OUTPUT=/tmp/mudcrab-jump-before/headless.json \
  cargo test -p engine --lib physics::jump_replay_tests::export_headless_jump \
  -- --ignored --exact --nocapture
cargo test -p engine --lib physics::jump_replay_tests::native_first_launch_step \
  -- --ignored --exact --nocapture
```

The last command must fail on this archived baseline. The ordinary test suite ignores artifact exports and this before-change regression. The implementation promotes the corrected first-step check into the ordinary suite.
