# Finding a Metal GPU fault

Use Metal shader validation to find the pass that makes a bad shader access. This guide also
explains why the engine can exit 0 after a GPU fault, and what each exit code means.

## What a fault looked like on 2026-11-18

A run on 2026-11-18 hit a Metal page fault. The engine log said exit 0 and had no error line. The
only record was an `.ips` file in the system diagnostic reports. That file gives only a trace id
and an address. Hidden runs gave no `.ips` file.

The cause was a Bevy 0.19 morph descriptor bug,
[bevyengine/bevy#25966](https://github.com/bevyengine/bevy/issues/25966). Mudcrab guarded it in
[#799](https://github.com/Mudcrab-Team/mudcrab/pull/799) and fixed it in
[#801](https://github.com/Mudcrab-Team/mudcrab/pull/801), which carries
[bevy#26504](https://github.com/bevyengine/bevy/pull/26504). The out-of-bounds read does not fault
on every run, so one clean run proves nothing.

## Find the faulting pass

Metal shader validation reports invalid Metal usage. When a shader access is invalid, the log names
the encoder label of the pass. The labels come from the Bevy and Mudcrab render passes.

Run the engine with these variables and options:

```sh
MTL_SHADER_VALIDATION=1 MTL_SHADER_VALIDATION_REPORT_TO_STDERR=1 MTL_SHADER_VALIDATION_FAIL_MODE=allow \
RUST_LOG=info,wgpu_hal=warn,wgpu_core=warn,naga=warn \
  python3 scripts/workspace.py cargo run --locked --bin engine -- \
  --assets <pack with actors> --worldspace 60 --grid-x 6 --grid-y -12 --stream-radius 2 \
  --always-active --hidden-window --no-render-diagnostics --route <closed-loop route.json> \
  --benchmark-duration 300 --benchmark-vsync --benchmark-output <bench.json> > <run.log> 2>&1
echo "engine exit: $?"
grep -c "Invalid Metal usage" <run.log>
```

Use a converted pack with streamed actors, and a closed-loop route JSON around the start cell. Add
`--release` for a faithful comparison. The log line that names the pass looks like this:

```
Error detected in dispatchThreadgroups:threadsPerThreadgroup: in encoder with label "probe_pass_faulting"
```

The label after `in encoder with label` is the pass name. The example is a probe pass from a test
of this method. In the engine, the label is the name that Bevy or Mudcrab gives the pass.

## Pitfalls

- A run with no error lines is evidence, not proof. Read the engine exit status first. A zero
  count from a run that failed to start proves nothing. A hidden run without validation did not
  fault even before the fix.
- Do not set `MTL_DEBUG_LAYER=1`. It writes about 700 MB of warnings per minute.
- The recipe uses `MTL_SHADER_VALIDATION_FAIL_MODE=allow`. `zerofill` also printed the encoder
  label in the probe test. Record the mode with the result.
- Validation reports the bad shader access, and the command buffer still completes. Treat the
  validation line as the evidence.
- Hidden runs gave no `.ips` file in the 2026-11-18 case. Use the log from a hidden run with
  validation on.
- Without the morph fix, Bevy 0.19.0 gave 20 `Invalid Metal usage` lines in 5 minutes on a release
  build. The target is 0.

## What does not work today

A failed Metal command buffer is not reported. In the locked wgpu-hal 29.0.4, the completion
handler in `Queue::submit` ignores the finished buffer's status. It advances the fence whether the
buffer succeeded or failed. A failed buffer therefore looks successful, and the engine does not
notice it.

The error code and text are not reachable from wgpu, from Bevy's `RenderErrorHandler`, from
`as_hal`, from `on_uncaptured_error` or from the device-lost callback. wgpu 30.0.1 does not report
them either. The open change
[wgpu#10875](https://github.com/gfx-rs/wgpu/pull/10875) would turn a failed buffer into a lost
device. That would give exit 3, not a separate fault report.

Exit code 3 ([#553](https://github.com/Mudcrab-Team/mudcrab/pull/553)) covers a lost device only.
A failed buffer does not lose the device on wgpu 29.

[#873](https://github.com/Mudcrab-Team/mudcrab/issues/873) tracks the report of a failed command
buffer. [#872](https://github.com/Mudcrab-Team/mudcrab/issues/872) tracks the run record for every
run.

## Read the exit code

The engine documents these exit codes in `--help` (`crates/engine/src/config.rs`):

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | The run failed |
| 2 | The command line could not be used |
| 3 | The GPU device was lost (retry the run; repeated losses can still be an engine fault) |

Exit 0 means success. It does not prove the GPU had no fault. The 2026-11-18 run is an example:
it exited 0 and still had a page fault. `EXIT_DEVICE_LOST` (`3`) is set in
`crates/engine/src/app.rs` when Bevy reports a lost device.
