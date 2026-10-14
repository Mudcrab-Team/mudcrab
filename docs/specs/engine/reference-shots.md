# Reference shots (`--shots`)

`--shots <file>` renders a list of exact camera poses, one PNG each, then exits. It exists for
visual review: take a screenshot in Skyrim, write down the camera pose, and render the same view
here, so the two images can be compared side by side and a regression shows up as a changed image
rather than a vague impression.

```
engine --assets <converted assets> --shots review/riverwood.json [--shots-out review/out]
```

Shots run without a window. The production world camera renders into an owned
`Rgba8UnormSrgb` image at the file's exact dimensions, and the capture reads that
same image. Display resolution, scaling and window visibility do not change the
PNG dimensions. The runner uses a minimum frame interval of 16 ms; this is a
visual capture mode, not a performance benchmark.

## The shots file

```json
{
  "width": 1400,
  "height": 1050,
  "shots": [
    {
      "name": "riverwood-bridge",
      "worldspace_id": 60,
      "interior_cell_id": null,
      "position": [21340.0, -44890.0, 520.0],
      "yaw": 152.7,
      "pitch": 7.9,
      "hfov": 75.0,
      "reference": "review/riverwood-bridge.jpg",
      "note": "the bridge from the south bank"
    }
  ]
}
```

| Field | Meaning |
| :-- | :-- |
| `width`, `height` | The render-target and PNG size in pixels, at most 8192 each. Use the size the reference screenshots were taken at. A capture with different dimensions fails the shot. |
| `settle_timeout_seconds` | Optional per-shot settling budget in seconds, finite and greater than zero, up to 300. Omission retains the 30-second default. |
| `name` | Output file stem: the image is `<out>/<name>.png`. A plain file name, unique in the file, that Windows can create (see below). |
| `worldspace_id` | The worldspace of an exterior shot (60 is Tamriel). `null` or absent leaves it to `--worldspace`. |
| `interior_cell_id` | An interior cell. Such a shot is skipped and logged: the streamer holds exterior cells only. |
| `position` | The camera's eye in Creation units, absolute (not relative to a cell or the render origin). |
| `yaw` | Skyrim heading in degrees: 0 looks north (`+Y`), 90 east (`+X`), clockwise seen from above. |
| `pitch` | Skyrim's X angle in degrees: positive looks down. |
| `hfov` | Horizontal field of view in degrees, between 0 and 180; the vertical one follows from the frame's aspect. |
| `reference`, `note` | Free text for people and comparison tools; the engine ignores them, and any other field. |

`name` has to be a file this engine can create on Windows, since it is written there: no `/`, `\`,
`:`, `<`, `>`, `"`, `|`, `?` or `*`, no control character, no trailing dot or space, and not one of the
reserved device names `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9` or `LPT1`-`LPT9` - with or without
an extension, and ignoring case, so `con.png` and `Nul.x` are refused too.

A file that cannot be read, is not JSON of this shape, has no shots, a zero frame, a side over 8192
pixels, a missing or non-numeric pose field, a field of view out of range, or a name that is empty,
repeated or not a name Windows can create stops the run before rendering starts, with a message that
names the file and the problem.

## What a run does

Streaming starts at the first exterior shot: its worldspace (when it names one) and the grid square
its camera stands over. The run streams that one worldspace; a shot in another worldspace is
skipped and logged, like an interior shot, so a file that mixes worldspaces is rendered one
worldspace per run.

For each shot in order, the run places the camera at the pose (the streamer then loads the cells
around it) and waits until the view has settled: no cell loading, no database request in flight,
no model or surface waiting for its assets, no model waiting to be armed, no out-of-range cell
still waiting to be unloaded, no new failed cells, asset-load failures or material, terrain,
water, transform-bounds or renderer validation failures since the shot was posed, and the
renderer's final path running. The current camera must also have a color output attachment,
uploaded visible meshes, prepared material bind groups, compiled color/prepass pipelines, and no
pending material queue or pipeline compilation. Its active shadow views must have prepared
caster meshes, materials, and shadow pipelines too. These conditions must hold for `SETTLE_QUIET_FRAMES`
(10) frames in a row, and not before `WARM_UP_SECONDS` (2 s) after start-up. The frame count is
exact: a view that has been quiet for ten frames running is
photographed on the tenth, and the `frames=` of its log line is then 10 - the count of quiet
frames is advanced before it is read, so it is not one more than the constant. The run then saves
the camera's owned image to `<out>/<name>.png`. A shot that has not settled within
its `settle_timeout_seconds` budget (30 s when omitted) is still captured. The log
records the budget, timeout, pending work and failures that prevented settling.
A new failure prevents that shot from
settling even after pending work drains; its timeout capture fails the run.

The Riverwood lighting fixture explicitly allows 90 seconds per shot. The full
asset pack exceeded 30 seconds while models were still waiting to be armed;
the longer budget allows those queues to drain. The same ten quiet frames and
zero pending or failed dependencies are required, and a timeout still fails the run.

The engine log records the current GPU dependency counts when each screenshot is requested.
`MUDCRAB_RENDER_OUTPUT_TRACE=1` also samples them during loading.

`--shots-out` defaults to a `<file stem>-shots/` folder beside the shots file. `shots.log` there
has one line per shot: its name, the frames it waited, whether it settled or timed out, the image's
path. Skipped shots have a line saying why. Each line is appended as its shot finishes,
and the file is created with the first line, so a run that is killed keeps the shots it had
already taken.

The run exits with success once every shot is written, and exits non-zero - the process's exit code
is not zero, and `main` reports the failure - when an image or the log could not be written, when a
shot was given up on, when the engine exited with an error for any other reason, and when an
acceptance run's gates did not pass. `scripts/phase2-acceptance.ps1` records that exit code per
command and marks the command failed on anything but 0, so a failed shots or acceptance run is now
visible in a campaign report instead of being read as a pass.

`--shots` always runs without a window, with or without `--headless`. It cannot be
combined with a benchmark, `--acceptance-screenshot`, `--auto-fly-speed` or a fixture.
Interactive views and benchmark screenshots retain their window-backed paths.
