# Bug reporting

Status: implemented locally; native platform and release checks remain open below.

Tracking: [#88, developer console and command parity](https://github.com/Mudcrab-Team/mudcrab/issues/88).
The issue is assigned to BimingtonBill, whose follow-up scope includes `bugreport`.
Coordinate implementation with that owner; this spec does not change the assignment.

## Purpose and first release

A player should be able to report the scene they are looking at without finding logs or copying
launch settings by hand. The first release saves a screenshot and a small diagnostic bundle,
then opens a prefilled GitHub issue form. The player supplies the symptom and reproduction steps,
reviews the collected information, attaches the bundle, and submits the issue themselves.

The first entry point is the existing developer console. A settings-menu action can invoke the
same service when that menu is available; it is not a dependency for this release. Automatic
crash recovery, a reporting backend, GitHub authentication inside Mudcrab, automatic uploads,
video capture, and gameplay replay are later work. A live report cannot recover a process that
has already crashed or a renderer that cannot deliver an image. Converter failures and engine
startup failures need a later launcher or command-line reporting path.

## Source baseline

Inspected on 2026-10-13 at `268f9884f7acaaa0302425331064f7268aac9d3c`, which matched remote
`main` when checked. These are existing hooks, not proof that reporting works:

- [console.rs](../../../crates/engine/src/console.rs) provides `ConsoleRegistry`,
  `AppConsoleExt::add_console_command`, synchronous command dispatch, and console pause/input ownership.
  At the inspected baseline it had no `bugreport` command.
- [app.rs](../../../crates/engine/src/app.rs) configures `LogPlugin` and an acceptance screenshot
  path. That screenshot path waits for successful asset readiness; a bug report must not use its gate.
- [shots.rs](../../../crates/engine/src/shots.rs) uses `ScreenshotCaptured` and has PNG writing,
  timeout, and atomic-publication helpers worth sharing. Its settle loop and exit behavior are specific
  to automated shots and should not be copied into interactive reporting.
- [metrics.rs](../../../crates/engine/src/metrics.rs),
  [streaming.rs](../../../crates/engine/src/streaming.rs), and
  [profiling.rs](../../../crates/engine/src/profiling.rs) expose useful diagnostics. Some collection
  runs only during benchmarks; their output cannot be assumed to exist during ordinary play.
- [01-bug-report.yml](../../../.github/ISSUE_TEMPLATE/01-bug-report.yml) already asks for
  description, reproduction, logs, system information, and severity.

The engine needs its own bounded log buffer so reports also work when launched directly from a
terminal. Launcher-captured output is optional supporting evidence, not a required input.

## Player workflow

1. Open the console and run `BugReport`. Command matching remains case-insensitive, so
   `bugreport` works. Help explains that it saves diagnostics and opens GitHub for review.
2. Immediately show `Collecting bug report…`. Record runtime context and recent logs, then request
   the current game-window image. Do not wait for loading to finish or errors to disappear.
3. Save the readable files and ZIP archive locally. Show the actual archive path and either
   `Report saved` or `Report saved with missing diagnostics`, naming any missing parts.
4. Open the system browser on the prefilled bug-report form. The form identifies the report and
   tells the player to inspect and attach the ZIP. The player writes the title, symptom, reproduction
   steps, and severity, then clicks GitHub's submission button.
5. Closing the console resumes play according to its existing pause rules. Report completion and
   browser launch never resume virtual time or capture the cursor on their own.

`BugReport local` performs the same collection without opening a browser. Extra arguments return
`usage: BugReport [local]` without starting work. This is also the offline and automation entry point.
Only one report may be active at a time; another request returns `A bug report is already being collected`.

Opening the browser is a handoff, not proof that a form loaded or that a report was submitted.
Mudcrab does not collect a GitHub token or make an issue-creation API call.

## Screenshot and pause behavior

Capture the primary game window after its usual post-processing, at its current physical pixel
dimensions. Capture no desktop or other application windows. Keep the HUD, collision outlines,
and other gameplay diagnostics as the player saw them. Hide only the console and reporting overlay
for the image, then restore their prior visibility.

Restore overlay visibility as soon as readback completes, is skipped, or times out, before encoding
and archive work. The console can show progress while that work continues. Only the image-capture
phase owns temporary overlay suppression; packaging must not hold that suppression for 30 seconds.

The image request must not change camera pose, FOV, exposure, graphics settings, or streaming
budgets. Gameplay input stays blocked during the temporary overlay hiding. A user who closes the
console during collection keeps control of that choice; restoration must not reopen it. If a menu
entry is added later, apply the same rules to that menu's visibility and pause ownership.

Record the context snapshot time/frame and image request time/frame separately. Rendering and
streaming can continue while virtual time is paused. Associate camera and settings metadata with
the frame submitted for capture, and identify a rendered frame only when the renderer supplies
that identity. A callback timestamp alone is not the rendered frame's timestamp. Do not describe
the bundle as a deterministic reproduction or an exact-pose replay.

Use real time for deadlines. Allow up to 10 seconds for GPU readback, restore overlays on every
exit path, and keep the text report if readback fails. A headless session, minimized/unrenderable
window, missing camera, or renderer failure yields a named unavailable component rather than a hang.
Late callbacks carry a report ID and cannot overwrite another report or revive an expired request.

## Bundle contents

Create a report ID from a UTC timestamp and random suffix. Publish a directory named after that
ID and a sibling `<report-id>.zip`. The ZIP contains one directory with these relative files:

- `manifest.json`: `schema_version: 1`, report ID, UTC creation time, collection duration, overall
  `complete` or `partial` status, and each component's status, size, SHA-256, and omission reason.
  The manifest does not hash itself. Failed collection with no publishable manifest is an error,
  not a saved report.
- `report.json`: the structured runtime snapshot described below. Unknown values are `null` with
  a reason in the component metadata; missing values must not become plausible defaults.
- `logs.txt`: bounded recent engine diagnostics with timestamp, level, target, and sanitized text.
  Strip ANSI/control sequences. Include the represented time range and counts of dropped,
  truncated, and omitted events.
- `screenshot.png`: the captured game-window image, when available.
- `report.md`: an editable issue draft with description and reproduction placeholders, a compact
  system summary, report ID, collection status, and instructions to attach the ZIP.
- `README.txt`: contents, missing components, and a short reminder to review logs and the image
  before sharing. A screenshot may contain player-visible names or text that cannot be redacted
  reliably by a text sanitizer.

Every referenced file must exist and have the declared size and hash before publication. ZIP entry
names are fixed relative paths; archive generation must not walk the asset directory or follow
symlinks to gather extra files. Prepare and validate both the directory and ZIP at task-owned
temporary paths on the output filesystem, then publish with separate renames. Never overwrite an
earlier report. The commit point is successful publication of both artifacts; only then show a
saved result or open the browser. If directory publication succeeds but ZIP publication fails,
preserve the valid directory and report `Report files preserved; ZIP could not be saved` with its
local path. Treat this as an archive failure and do not open GitHub automatically. Clean up only
task-owned temporary remnants. A saved report is immutable. Browser-open failures are shown in
the console and do not rewrite its contents.

### Runtime snapshot

Export a named allowlist rather than serializing the whole ECS world or `EngineConfig`:

- Build: Mudcrab version, build-supplied Git commit, dirty state when known, target triple, actual
  Cargo profile, and Bevy version. Release packaging supplies these values; source archives or
  builds without provenance report unknown. User-supplied profiling labels are not build identity.
- Platform: OS/version, architecture, CPU model/core count, total memory, available process-memory
  measurement, GPU adapter, driver and rendering backend when exposed. Do not collect hostnames,
  account names, serial numbers, or network addresses.
- Session: uptime, interactive/headless/fixture mode, WALK/NOCLIP state, paused state, current
  active space, and cell identity. Use runtime active-space state when available; otherwise state
  the limitation instead of claiming a launch worldspace is current after a transition.
- Pose: player position when present, camera position/orientation, render-origin grid, exterior
  worldspace or interior cell, projection/FOV, and explicit units/axis convention. Convert exterior
  render-local positions through the render origin; label render-local Bevy coordinates separately
  from absolute Creation coordinates. An absent player in NOCLIP is not a zero-position player.
- Rendering: actual window size/scale, present mode, MSAA, tone mapping/exposure, and effective
  supported lighting, fog, water, culling, and LOD settings. Distinguish configured values from
  effective runtime values, including settings changed during play.
- Streaming: effective radii and budgets, resident/loading/failed cell counts, pending asset/LOD
  work, validation/fallback counters, and up to 32 recent failure records. Do not clone an unbounded
  failure vector before limiting it; include omitted-record counts.
- Performance: the recent frame-time summary and memory values if measured during ordinary play,
  with sample count, sample interval, and measurement scope. A new lightweight frame-time ring may
  cover the last 10 seconds, capped at 2,048 samples. Do not enable the benchmark campaign or
  expensive profiling automatically, and do not call these samples performance acceptance evidence.
- Content: read-only database/converter schema versions, conversion-complete status, and existing
  LOD build identity or converter configuration hash when available. Identify what each hash means;
  a configuration hash does not fingerprint every asset. Export selected cached metadata, not the
  complete manifests, plugin inventory, world database, or a hash scan of the whole pack.

The selected-object reference ID may be added when selection exists. It does not require implementing
click-to-select or other remaining console features before bug reporting can ship.

Treat non-finite numeric measurements as unavailable with a reason, preserving valid JSON. Coordinate
tests must cover negative exterior grids, render-origin rebasing, and the Creation/Bevy axis conversion.

### Collection limits and storage

Use these first-release limits and report every truncation or omission:

- Recent logs: at most 2,048 events and 1 MiB in memory; each event at most 4 KiB of UTF-8.
  Evict oldest events first and preserve valid UTF-8. Export INFO/WARN/ERROR events already enabled
  by the current logger; do not enable verbose/debug logging or change terminal filtering. Keep the
  buffer independent of whether the console is open.
- Structured text: at most 2 MiB total. Bound field lengths and failure records before serialization.
- Screenshot: at most 16 million pixels of readback and 16 MiB of encoded PNG. If the window exceeds
  the readback limit or encoding exceeds the byte limit, omit the image with a reason. Do not resize
  the game window or silently reduce rendering quality to fit.
- ZIP: at most 20 MiB. Remove optional payloads with recorded reasons if needed; preserve a minimal
  valid manifest, runtime snapshot, and issue draft. If those cannot be written, report failure.
- Collection: 30-second real-time deadline for publication, including the 10-second readback deadline.
  At expiry, stop waiting, restore capture overlays, and report timeout. Give up waiting for browser
  launch separately after 5 seconds. These bound the player's wait; a blocking OS call cannot be
  guaranteed to terminate at the deadline. Keep its worker slot reserved until it actually exits.

Default output is the OS-specific per-user Mudcrab application-data directory under `reports/`:
`%LOCALAPPDATA%/Mudcrab/reports` on Windows, `~/Library/Application Support/Mudcrab/reports` on
macOS, and `$XDG_DATA_HOME/mudcrab/reports` on Linux, falling back to `~/.local/share/mudcrab/reports`.
Expose `--bug-report-dir <path>` for an explicit override and automated tests. Do not fall back to
the working directory or asset pack on permission failure. Show the resolved local output path only
in the local console; never embed it in the issue URL. Keep completed reports until the user deletes
them, and clean up only temporary files owned by the failed or canceled request.

## Privacy and export rules

Collection is local until the browser handoff. Opening GitHub sends the small prefilled summary
in the URL; it does not upload the diagnostic archive. Player review controls attachment and submission.

Before writing any export, replace known home, asset, and report roots with stable markers such as
`~`, `<assets>`, and `<reports>`. Match Windows drive/UNC paths and both path separators as well as
Unix paths. Mask other absolute machine paths. Preserve useful relative asset names and numeric
FormIDs. Sanitize error strings, nested JSON, issue text, and log messages through the same exporter.

Never collect environment variables, raw command lines, credentials, cookies, tokens, save files,
plugin binaries, meshes, textures, database dumps, or unrelated application logs. Start with an
allowlist of engine and relevant renderer/asset-loader log targets and reviewed fields. Mask known
credential patterns, omit events that cannot be exported safely, and count those omissions. Do not
claim that pattern matching proves all arbitrary text is secret-free; the readable bundle remains
available for player inspection before sharing. Sensitive optional data has no automatic fallback.

## GitHub handoff

Target `https://github.com/Mudcrab-Team/mudcrab/issues/new` with
`template=01-bug-report.yml`. Prefill the existing text-field IDs:

- `description`: a prompt to describe the symptom, report ID, complete/partial status, and reminder
  to review and attach the ZIP.
- `reproduction`: prompts for steps and expected/actual behavior. Do not invent a reproduction from
  camera coordinates or console history.
- `system-info`: sanitized version/commit, OS, GPU/backend, mode, and a compact scene identifier.

Leave the title and severity for the player. Do not send logs, screenshots, local paths, plugin lists,
or the complete snapshot in a URL. Do not request labels or assignees; the existing template owns
its bug label. Use a URL library to encode each parameter as data, never shell interpolation.

[GitHub documents issue-form text-field prefilling and URL-length failures](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/creating-an-issue#creating-an-issue-from-a-url-query).
Cap the complete encoded URL at 4,096 bytes as a product limit, not a claimed GitHub server limit.
If it is too long, keep the template and report ID, omit optional summary fields, and direct the
player to `report.md`. Store that readable draft regardless of browser availability.

Use the operating system's browser-opening API with the fixed HTTPS destination. Never use an
arbitrary log value or shell command as the URL. If opening fails, show the URL and the saved draft
path so the player can file later. If sign-in is needed, the template has changed, or GitHub cannot
be reached, the player can use the local draft and bundle; no network availability is required to
collect them. Launching a browser successfully does not verify the current template fields.

## Implementation boundaries

Add a `BugReportPlugin` and a service accepting a request from either the console or a future menu.
The console handler queues a request and returns immediately. It must not encode PNGs, create ZIPs,
probe Git synchronously, or perform filesystem/network work while it owns `&mut World`.

Keep the work in three parts:

1. On the game thread, take a bounded allowlisted snapshot, establish overlay/input ownership,
   and submit screenshot capture. Poll results and deadlines with real time, even while paused.
   Restore temporary overlay suppression when capture completes, is skipped, or times out.
2. Off the game thread, sanitize text, encode the received image, write/hash/archive files, and
   publish the bundle. Keep ECS references out of workers. Bound outstanding work, raw image memory,
   and queued results; checking a timeout must not leave an unlimited orphan-worker path.
3. After publication, report the local result and open the browser unless the request was `local`.
   Match every completion to its request ID and ignore stale completions.

Use explicit `capturing`, `packaging`, `publishing`, `saved`, `failed`, and `retiring` states. A timed-out
request with a worker still running enters `retiring`. Reject new requests with `The previous bug
report is still finishing` until that worker exits, rather than creating another worker. Check
cancellation and the deadline between phases and immediately before every publication step. An
expired request has no authority to start another rename, acknowledge a saved report, or open a
browser. An OS call already in flight can still finish after expiry; preserve any valid task-owned
files, report their local recovery path when available, and clean up temporary files after the worker
returns. A browser-open timeout must not trigger an automatic retry that opens duplicate tabs.

Reuse screenshot helpers and diagnostic structures through focused shared functions. Do not reuse
acceptance success gates, mutate benchmark reports, change logger filters, or introduce a second
logging subscriber that conflicts with Bevy's `LogPlugin`. A bounded tracing layer should feed the
recent-log ring while preserving existing terminal logging. Report generation must avoid logging
its own exporter failures back into an unbounded recursion.

Install support for interactive engine runs. Headless/testing paths can export text via the same
service with screenshot marked unavailable and browser opening disabled. Automated shots and
benchmark launches must never open a browser as a side effect.

## Acceptance and validation

The first release is complete when all of these hold:

- [ ] `Help bugreport` documents `BugReport [local]`; both spellings resolve through the existing
  case-insensitive registry, and invalid arguments create no files or browser launch.
- [ ] One command saves an inspectable directory and valid ZIP, prints the actual output path,
  and opens the correct prefilled GitHub form exactly once after publication.
- [ ] The player can review/edit the form and manually attach the ZIP. The engine never submits
  the issue or uploads an attachment.
- [ ] Capture works with pending loads, failed assets, and diagnostic fallbacks. It preserves the
  visible defect instead of waiting for acceptance readiness or changing rendering settings.
- [ ] Console text is absent from the PNG; prior console state, cursor ownership, and virtual-time
  pause state survive success, failure, user dismissal, and browser focus changes.
- [ ] Build, content, scene, camera, and effective settings metadata identify their sources; missing
  measurements and partial captures remain explicit. Window size and coordinates use actual values.
- [ ] Bounds hold before copying/allocating input, during logging, readback, serialization, and ZIP
  generation. Truncation counts and component errors are inspectable in the manifest.
- [ ] Repeated requests, late callbacks, capture timeout, headless mode, read-only storage, full disk,
  serialization/PNG/ZIP failure, and browser-open failure produce bounded, recoverable behavior.
- [ ] Privacy fixtures cover Unix and Windows/UNC paths, external asset roots, sensitive nested
  fields, credential patterns, Unicode, and issue-URL encoding. Excluded sentinel data never appears
  in exported files or the browser URL; allowed relative paths remain useful.
- [ ] Collection succeeds offline. Canceling or closing the GitHub form leaves the local bundle
  intact, and subsequent reports cannot overwrite it.

Implementation tests should cover the exporter/manifest contract, archive contents and hashes,
limits, path sanitization, URL-field mapping, and the request state machine under injected failures.
Use a headless scheduled console test to check paused progress, duplicate requests, stale results,
and input restoration. Pure serialization tests do not prove GPU readback or browser behavior.

Before release, run the interactive path on macOS/Metal, Linux/Vulkan, and Windows/DX12, including
at least one deliberately broken asset scene on each backend. Inspect the actual PNG and archive,
confirm prefilled fields in a browser without submitting a test issue, and exercise browser/focus
recovery. Measure idle ring-buffer cost and report-collection hitching against the same session with
reporting disabled, retaining the repository's existing performance acceptance rules. These are
required release checks; the local verification below does not establish full platform or
performance acceptance.

## Local implementation and verification

The implementation is [bug_report.rs](../../../crates/engine/src/bug_report.rs), with separate
snapshot, logging, privacy, and export modules. Run `BugReport` from the console for collection and
the browser handoff, or `BugReport local` for an offline report. `--bug-report-dir <path>` overrides
the normal per-user output directory. Reports also support direct engine launches.

For an asset-free native check, run:

```sh
cargo run --locked --profile quick -p engine --example bug_report_smoke -- /tmp/mudcrab-bug-report-evidence
```

The smoke example opens its own window, deliberately requests a missing GLB, keeps failure/fallback
counters nonzero, pauses virtual time, collects a local report, verifies image/archive existence
and pause preservation, then exits. It never opens a browser or submits an issue. Its procedural
scene checks native capture mechanics; it is not a full-pack visual or performance campaign.

Historical evidence collected at original PR head
`76048172882c837d0f7ad4ba211ce7c106cd6f7b` on macOS / Apple M1 Pro / Metal; these results were
not rerun at later heads:

- Engine library tests: 463 passed and 1 failed out of 464. The failure was
  `physics::simulation_tests::jump_launches_and_returns_to_ground`, with
  `jump apex 56.491875 too low above rest 50.825207`. An untouched archive of baseline
  `268f9884f7acaaa0302425331064f7268aac9d3c` failed the same test with those values.
- Native smoke capture produced an opaque game-window PNG with the console absent, despite the
  deliberate missing asset. Virtual time remained paused, console state stayed open, and capture
  suppression was restored after image collection. Actual physical dimensions were 1280 by 720
  at scale factor 2.
- The local ZIP passed CRC checks. Every included file matched its manifest size/hash and the
  corresponding file in the readable report directory. Path-redaction sentinels were absent from
  the text exports. The snapshot retained nonzero failed-cell, asset-failure, and fallback counters.
- Navigating the prefilled GitHub route reached GitHub sign-in with the intended destination and
  query preserved. The preview profile was not signed in, so authenticated field rendering and
  manual attachment/submission were not verified.

CPU validation at commit `52dffdd64d2b187e07d8cfd02962095965ef9a09` on the merged PR head:

- Locked engine all-targets/all-features check passed.
- Bug-report library tests: 53 passed.
- Engine library CPU tests: 979 passed and 1 ignored; the physics jump test passed.
- Strict engine Clippy (`--all-targets --all-features -- -D warnings`) and formatter check passed.
- This validation did not rerun the historical native smoke, Metal capture, archive inspection, or
  GitHub sign-in flow. Those historical results are not current-head runtime acceptance evidence.

Release checks still required: real-pack reporting, signed-in GitHub form prefilling, actual browser
focus recovery, Linux/Vulkan and Windows/DX12 capture, and idle/collection performance measurements.
The current engine has no public active-space state, so that identity is explicitly unavailable;
the configured launch worldspace and exterior coordinate mapping have separate labels. Unknown
essential diagnostics make the report partial rather than silently supplying guessed values.

## Delivery order

1. Add build provenance, bounded log capture, allowlisted snapshots, sanitizer, archive publication,
   and `BugReport local`, including failure-injection coverage.
2. Add asynchronous screenshot capture and overlay restoration; verify live success and partial
   reports without acceptance readiness gates.
3. Add the reviewed GitHub-form handoff and validate existing template IDs, URL encoding, length,
   browser failure, and sign-in/offline fallback.
4. Complete native backend, privacy, input, and performance checks. Coordinate a menu action with
   the settings owner if desired; console command parity and object selection remain independent.
