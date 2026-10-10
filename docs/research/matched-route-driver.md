# Deterministic route and capture receipts

This follow-up supplies the missing driver for the October 9 matched protocol. It
changes diagnostics and camera automation, not renderer optimizations, dependency
ownership, PR bases or acceptance thresholds. PR #200 must remain draft.

```sh
"$engine" --assets "$MUDCRAB_PROFILE_ASSETS" \
  --worldspace 0x3c --stream-radius 2 \
  --max-model-spawns-per-frame 16 --max-upload-mib-per-frame "$budget_mib" \
  --matched-route scripts/profiling/fixtures/matched-route.json \
  --shots-out "$run/shots" --profile-output "$run/profile"
```

Use a fresh output directory for each process. The fixed capture frame is 1600×900.
`--matched-route` rejects other shots, auto-flight, benchmark camera modes and
fixtures. `--shots-out` names its output directory. Both comparison sides must
contain the identical driver and diagnostics. Run both 16 MiB and 1 MiB budgets.

Leg offsets are destinations relative to the start. The fixture expands to 3,073
poses, including step zero: exactly 3,072 movements of 64 Creation units. The route
never multiplies movement by frame duration. Each step is held through an extraction;
its absolute pose is reapplied using the current render origin. Movement pauses for
checkpoints immediately before and after cell boundaries and at every leg endpoint
(reversals and returns). These are also the fixture's terrain tile boundaries.

At each checkpoint `route-NNNNNN-transition.png` preserves the first arrival, then
`route-NNNNNN.png` captures the settled pose. Requests are serialized; the runner
holds the complete pose through asynchronous readback. Automatic scene-evidence
handoff images are suppressed during shots/route runs to avoid competing requests.

`shots.log` contains step positions and elapsed times plus JSON capture receipts.
A receipt records the requested main frame and pose, the corresponding extraction's
main frame and render-schedule frame, actual absolute pose, projection, origin,
image dimensions, extracted queue state, terrain selection masks/member coverage,
mesh generations, LOD gauges and render-side pending work. Callback arrival time is
not used as the captured frame. In Bevy 0.19 a primary-window Screenshot is extracted
once and marked Capturing in that extraction; the same entity's ScreenshotCaptured
readback completes that request. Missing camera/window, missing receipt, pose or
projection mismatch, wrong image size, write failure or missing readback fails the
capture. A late callback cannot satisfy a different step or capture phase.

Settling requires ten consecutive quiet updates, a running final render path and
warmup. It now includes ordinary queues, pending LOD queries/chunks, CPU batch work,
initial batch transfers, immutable selection uploads, unconsumed terrain upload
acknowledgments, mesh/image render-asset transfers, material/prepass/shadow queues,
missing-mesh specialization retries and pending pipelines. Render-side state is
unavailable until observed, rather than assumed zero. The captured settled frame
is checked again against its own extracted work and render-side readiness.

The existing 30-second settle and 60-second readback timeouts remain. A timed-out
settle may save a diagnostic image but fails the run. Atomic PNG writes, dimension
checks and receipt-write failure handling prevent partial files from counting as
success. A terminal `run_complete` record is required; absence after interruption
means incomplete evidence. A successful run still requires external image inspection
and masks: selected coverage and prepared descriptors do not prove pixels were drawn.

This instrumented, paused image run is coverage evidence infrastructure. Preserve
and compare timing by route-step ranges; exclude checkpoint pauses and image readback
from any traversal timing calculation. Do not reinterpret its total duration as an
uninstrumented throughput comparison. Follow the original protocol for release build
receipts, repetitions, RSS sampling, stability and native GPU/presentation attribution.
No runtime speedup, visual equivalence or Metal validation is established by the code
or its automated tests.

## Verification limits for this follow-up

On the Linux editing host, all 120 existing Python script tests passed (five tests
requiring macOS frameworks were skipped), and `git diff --check` passed. New Rust
regressions cover fixture interpretation/count validation, every fixed movement,
checkpoint returns, rebasing while an image is delayed, transition-to-settle
ordering, immutable frame receipts, missing readback and queue readiness resets.
The host has no Rust toolchain: those Rust tests, compilation, formatting and Clippy
have not been executed. The existing CI workflow automatically targets only main
and develop; this PR's preserved dependency base does not trigger it. Run the
workflow manually against the branch before treating implementation checks as passed.

```sh
cargo fmt --all -- --check
cargo clippy --locked -p engine --all-targets -- -D warnings
cargo test --locked -p engine --lib
```

No converted retail assets, macOS/Metal execution, image review, RSS campaign or
repeated matched comparison at either budget was available on this host.
