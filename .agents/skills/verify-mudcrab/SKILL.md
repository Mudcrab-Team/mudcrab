---
name: verify-mudcrab
description: "Drive Mudcrab's desktop launcher, engine, and production converter CLI with isolated synthetic data. Use to verify user flows, capture terminal and rendering evidence, and check published asset packs after changes."
---

# Verify Mudcrab

Read [the feature map](features/README.md) and select the changed journey. Mudcrab is a Bevy desktop app with a production conversion CLI. The helper proves conversion through the CLI. Native window recipes cover the launcher and engine, but require separate evidence for each button or physical key entry point.

## Launch

Run from the repository root on Linux with Nix and devenv. Python 3.11 or newer must be on PATH inside `devenv shell`. The runtime environment also needs native libraries on its loader path, Vulkan, and Xvfb for a virtual display. Check these prerequisites before building.

```bash
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py build
```

This builds `dummy-content`, `converter`, `engine`, and `launcher` in release mode. It records source and binary SHA-256 hashes in `target/verification/mudcrab-build.json`. Build transcripts live beside that receipt. A changed build input requires another build and a new session.

Use a new evidence directory for every run. The following complete CLI journey seeds an empty temporary directory, checks readiness, converts, checks the published pack, and cleans up:

```bash
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py prove --evidence reports/verification/cli-$(date -u +%Y%m%dT%H%M%S)-$$
```

To prove graceful interruption and resume through the production CLI, use another fresh evidence directory:

```bash
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py stop-resume --evidence reports/verification/resume-$(date -u +%Y%m%dT%H%M%S)-$$
```

This publishes a baseline pack, starts replacement conversion with cache invalidation, and sends one SIGINT when the Textures stage appears in the PTY. It requires exit 130, `complete: false`, retained staging, and unchanged published files. It executes the printed resume command with the recorded converter binary and a separate JSON report, then requires complete publication and full check. `stop-resume.json` records both outcomes. Cleanup runs after resume or failure. A conversion that finishes before interruption fails this proof.

For a journey that needs a live window, prepare and convert without cleanup:

```bash
export MUDCRAB_VERIFY_EVIDENCE="$PWD/reports/verification/desktop-$(date -u +%Y%m%dT%H%M%S)-$$"
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py prepare --evidence "$MUDCRAB_VERIFY_EVIDENCE"
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py doctor --evidence "$MUDCRAB_VERIFY_EVIDENCE"
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py convert --evidence "$MUDCRAB_VERIFY_EVIDENCE"
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py window --evidence "$MUDCRAB_VERIFY_EVIDENCE" --binary engine --timeout 180
```

The helper creates its own `Data`, output, configuration, cache, and runtime directories. No migrations or account setup are needed. The generator writes a deterministic plugin, textures, scripts, a mesh, and reciprocal load doors. Conversion creates the database and schema. Readiness requires a healthy doctor result before driving. Engine readiness additionally requires a rendered scene and `SceneState` reporting `ready: true`, not merely a window opening.

The fixture worldspace is **1**, with exterior grid **0,0** and interior **GeneratedInterior**. The engine helper passes these overrides and `--stream-radius 0`. The seed contains one exterior cell. The default radius requests absent neighbors and prevents `SceneState` readiness. The launcher starts the engine with the normal default worldspace, so its **PLAY MUDCRAB** journey needs a separate pack containing that worldspace. Do not count a synthetic CLI launch as proof of that button.

## Doctor

```bash
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py doctor --evidence "$MUDCRAB_VERIFY_EVIDENCE"
```

This check is read-only. It verifies the checkout, build receipt, current build inputs, all four binary hashes, fixture hashes, and scratch ownership. It refuses an active or interrupted child. Healthy output contains `"healthy": true` and the owned scratch path. There is no server port or authentication for this local CLI journey.

Run doctor before a drive or after unexpected behavior. While a window is open, inspect its recorded `*.command.json` PID, `/proc` identity, native window, and logs. An active window deliberately makes doctor refuse a second drive. If the previous driver was interrupted, run cleanup and create a new session.

## Drive

Each app invocation uses a fresh PTY. Commands and combined output are recorded without shell interpolation. The helper never changes application state through internal setters.

```bash
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py check --evidence "$MUDCRAB_VERIFY_EVIDENCE"
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py check --evidence "$MUDCRAB_VERIFY_EVIDENCE" --full
```

These run production `converter check` and `converter check --full`. Both must exit zero, print `All good:`, and leave every pack file's contents unchanged. Full checking verifies hashes in addition to sizes. `prove` also changes one byte in a disposable copy without changing its size. It requires quick check to accept that copy and full check to reject it, while preserving the original pack.

To open the launcher, replace `--binary engine` with `--binary launcher` in the window command. Its title is `Mudcrab Launcher & Mod Manager`. Verify the displayed Data and Output paths before conversion. Native Bevy widgets have no browser selectors. Use a desktop driver that can target this window, capture the window before and after each action, and record actual pointer and keyboard input. Do not substitute a unit test for a window journey.

The engine window command enables the existing `--stdin-console`. Send newline-terminated production commands through the running helper's stdin. `Help`, `SceneState`, `coe 0 0`, `tcl`, `SceneShot <absolute-path>.png`, and `BugReport local` use the engine's registered command handlers. `coc` accepts exterior editor IDs in the current worldspace and rejects `GeneratedInterior`. This proves command behavior only. Opening the console with the physical backtick key, overlay focus, movement suppression, and settings still require native input. The window command times out after 180 seconds by default and records input lines separately.

For renderer-only capture on a machine without a usable display, wrap the window command in `devenv shell -- xvfb-run -a python3 ...`. Record Xvfb use and the actual Vulkan adapter in evidence. A virtual display and software Vulkan do not prove physical desktop behavior or GPU performance. `--headless` skips window rendering and cannot prove a screenshot flow.

## Evidence

Keep all proof in the chosen `reports/verification/<run>/` directory. The helper retains:

- `build.json`, `fixture.json`, and `doctor.json` identifying the inputs and readiness check.
- `*.command.json` with argv, cwd, PID identity, timeout, and actual exit code.
- `*.terminal.txt` with the real PTY output, including progress and errors.
- `conversion.json`, the published manifest, and `integration-report.json`.
- `database.json` with read-only SQL and results, including integrity, worldspace, interior, and reciprocal doors.
- `pack-files.json` with hashes, and check observations comparing contents before and after checking.
- `integrity-negative.json` and damaged-pack check transcripts proving rejection of same-size corruption.
- `result.json` for a successful `prove`, or `failure.json` with the failed command.
- `cleanup.json` confirming scratch removal and listing retained evidence.

Conversion passes only when the real CLI exits zero, reports converted assets and `complete: true`, publishes a complete manifest without failures, and passes integration validation. Independent SQLite observations must contain worldspace 1, GeneratedInterior, placed references, and both reciprocal door links. A successful exit alone is insufficient.

For native journeys, retain the action log, before and after window captures, stdout, and the resulting files. For settings, reopen the app and observe the saved value. For captures, bind PNGs to their JSON receipts and assert actual readiness. For bug reports, inspect the archive and report body. A local or preview name is not evidence of skipped side effects. In particular, repair preview can recover an interrupted publication; compare directory contents before and after it.

The fixture is synthetic and uses CPU encoders. It does not cover retail asset compatibility, MO2 selection, GPU encoding, or physical input. Mark each mapped entry point separately as passed, failed, or unverified. The formal subsystem skills check models and remain separate from this real-app proof.

## Cleanup

Close a window through its normal close action. Then run:

```bash
devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py cleanup --evidence "$MUDCRAB_VERIFY_EVIDENCE"
```

Cleanup checks the session token and removes only its temporary directory. It signals only a recorded process group whose PID, Linux start ticks, and boot ID still match. Never kill by process name. It preserves evidence and can run again after successful cleanup.

`prove` and `stop-resume` always clean up after a drive, including failures. Other mutating helper commands clean up their session on failure. If the driver itself is killed, run cleanup explicitly. Check `cleanup.json`, confirm the scratch directory is absent, and confirm the terminal transcripts and result still exist. Never remove the user's `Data`, `modern_assets`, caches, locks, or existing engine sessions.

## Helpers

[scripts/control.py](scripts/control.py) is executable and uses Python's standard library. Its commands are `build`, `prepare`, `doctor`, `convert`, `check`, `window`, `cleanup`, `prove`, and `stop-resume`. Run `devenv shell -- python3 .agents/skills/verify-mudcrab/scripts/control.py --help` for argument help. Evidence directories are exclusive to one run. Mutation commands serialize with a file lock, and separate runs use separate scratch directories.

Maintain the journeys and their coverage with `/maintain-verification-skill` when the app changes. The [feature map](features/README.md) is the maintained list of entry points and proof requirements.
