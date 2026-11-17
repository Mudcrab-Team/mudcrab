# Capture a scene and save a bug report

## Sub-features

- Save a local bug-report archive with engine context and a screenshot when available.
- Open the browser-assisted bug-report review flow.
- Capture a settled live frame with SceneShot.
- Run a batch of named shots with capture receipts.

## How to get to it (user POV)

- In an interactive engine session, run `BugReport local` to save locally, or `BugReport` to open the browser-assisted flow.
- Start with `--stdin-console` and send `SceneState` or `SceneShot <png-path>`.
- Start the engine with `--shots <file> --shots-out <directory>`.

## Driving it with the PTY, native desktop, and shot runner

Preconditions:

- Prepare and convert the fixture, then open the engine with the skill's window command.
- Use unique capture paths under the evidence directory. SceneShot refuses an existing PNG or JSON path.
- For a screenshot, require a renderer and `SceneState` reporting `ready: true`.
- For browser mode, use a controlled browser session. Opening review does not authorize submitting a GitHub issue.

Send `SceneState`, retain the output, and wait for readiness. Send `SceneShot <absolute-evidence-directory>/scene.png`. Retain `scene.png` and `scene.json`. Inspect the actual image, dimensions, readiness, grading, and captured frame receipt. A requested screenshot or PNG file without its successful receipt is insufficient.

Send `BugReport local` through the physical console or enabled stdin and record which entry point was used. The window helper directs `--bug-report-dir` to `<evidence>/bug-reports`. Wait for the report result, inspect the archive and report text, and confirm the expected logs and context are present. Inspect the screenshot if available. Record partial reports as partial. Observe browser windows and process events during this command to confirm local mode did not open a browser. The command name alone does not prove that side effect was skipped.

For batch shots, save this JSON as `<evidence>/shots.json`:

```json
{"width":640,"height":480,"shots":[{"name":"generated-world","worldspace_id":1,"position":[2048,512,512],"yaw":0,"pitch":20,"hfov":75}]}
```

Run `control.py window --evidence <directory> --binary engine --timeout 180 -- --shots <absolute-evidence-directory>/shots.json --shots-out <absolute-evidence-directory>/shots --shots-settle-timeout 30`, using the helper path in the skill. On a machine without a display, wrap the command in `devenv shell -- xvfb-run -a python3`. Retain `generated-world.png` and `shots.log`. Inspect the image and captured-frame JSON in the log. Require `timed_out: false`, the requested dimensions, quiet settle counts, and successful engine exit. A separate `.stages.json` is written only for shots with explicit probes. The example above has none. Record the adapter and Xvfb setup if used.

## Gotchas

- `--headless` is not screenshot evidence. Xvfb runs a real window on a virtual display and has different desktop coverage.
- A shot can time out and still write a PNG. Assert readiness and `timed_out: false`.
- `BugReport` without `local` requests a browser in interactive sessions. Headless, benchmark, batch-shot, and automated fixture runs force local behavior. Use an interactive session to verify the browser entry point.
- If screenshot capture fails but the report archive exists, report the failed screenshot instead of treating the entire journey as passed.
