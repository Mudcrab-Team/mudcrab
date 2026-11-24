# Reference shots (`--shots`)

`--shots <file>` renders a list of exact camera poses, one PNG each, then exits. It exists for
visual review: take a screenshot in Skyrim, write down the camera pose, and render the same view
here, so the two images can be compared side by side and a regression shows up as a changed image
rather than a vague impression.

```
engine --assets <converted assets> --shots review/riverwood.json [--shots-out review/out]
```

Add `--hidden-window` to render and capture without displaying a game window.
The primary window starts with visibility disabled and keeps rendering in the
background; screenshots still read the same render target. This option also
works with benchmark `--acceptance-screenshot` captures. It keeps an actual
rendering window, whereas `--headless` normally removes that window. When both
are supplied, `--hidden-window` retains the hidden render target. Hidden runs
do not capture interactive mouse/controller input.

Use the same shots file and asset pack with `--shots-no-room-culling` for a full-camera control.
The capture still waits for room graph reads. In `shots.log`, `room_culling.drawn` equals
`room_culling.listed` on each disabled frame.

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
| `width`, `height` | The frame size in pixels, at most 8192 each. The production camera renders into a dedicated image target at this size, independent of window size or display scaling. Use the size the reference screenshots were taken at. |
| `name` | Output file stem: the image is `<out>/<name>.png`. A plain file name, unique in the file, that Windows can create (see below). |
| `worldspace_id` | The worldspace of an exterior shot (60 is Tamriel). `null` or absent leaves it to `--worldspace`. |
| `interior_cell_id` | An interior cell to load before posing the camera. `null` or absent selects the exterior worldspace. |
| `position` | The camera's eye in Creation units, absolute (not relative to a cell or the render origin). |
| `yaw` | Skyrim heading in degrees: 0 looks north (`+Y`), 90 east (`+X`), clockwise seen from above. |
| `pitch` | Skyrim's X angle in degrees: positive looks down. |
| `hfov` | Horizontal field of view in degrees, between 0 and 180; the vertical one follows from the frame's aspect. |
| `probes` | Optional named world positions to measure. The run writes `<out>/<name>.stages.json` with each probe's pre-tonemap HDR color and output bytes; the format, the probe fields and the V1 probe set are in [visual-profiles.md](visual-profiles.md#writing-stage-receipts-from-a-shots-run). |
| `reference`, `note` | Free text for people and comparison tools; the engine ignores them, and any other field. |

`name` has to be a file this engine can create on Windows, since it is written there: no `/`, `\`,
`:`, `<`, `>`, `"`, `|`, `?` or `*`, no control character, no trailing dot or space, and not one of the
reserved device names `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9` or `LPT1`-`LPT9` - with or without
an extension, and ignoring case, so `con.png` and `Nul.x` are refused too.

A file that cannot be read, is not JSON of this shape, has no shots, a zero frame, a side over 8192
pixels, a missing or non-numeric pose field, a field of view out of range, or a name that is empty,
repeated or not a name Windows can create stops the run before a window opens, with a message that
names the file and the problem.

## What a run does

The first exterior shot selects this run's exterior worldspace and initial grid square. Without
exterior poses, the configured worldspace is retained. A shot in another exterior worldspace is
skipped and logged, so a file that mixes exterior worldspaces is rendered one worldspace per run.
Interior poses load their named cell and can be mixed with exterior poses. Switching spaces
unloads the previous space before loading the next one.

For each shot in order, the run places the camera at the pose (the streamer then loads the cells
around it) and waits until the view has settled: no cell loading, no database request in flight,
no model or surface waiting for its assets, no model waiting to be armed, no out-of-range cell
still waiting to be unloaded, no new failed cells, asset-load failures or material, terrain,
water, transform-bounds or renderer validation failures since the shot was posed, and the
renderer's final path running, for `SETTLE_QUIET_FRAMES`
(10) frames in a row, and not before `WARM_UP_SECONDS` (2 s) after start-up, while the first
pipelines compile. The frame count is exact: a view that has been quiet for ten frames running is
photographed on the tenth, and the `frames=` of its log line is then 10 - the count of quiet
frames is advanced before it is read, so it is not one more than the constant. The run then saves
the camera's dedicated image target to `<out>/<name>.png`. A shot that has not settled after
`--shots-settle-timeout` seconds (default 30 s; positive, finite, at most 600 s) is still captured, and the log says the timeout took it and what
was still pending or which failures prevented settling. A new failure prevents that shot from
settling even after pending work drains; its timeout capture fails the run.

With grass enabled, settling also requires completed desired grass generation,
batch admission, model attachment and actual grass draws. Missing grass metrics,
current pool rejections and grass model failures prevent settling; an earlier
failure that still affects grass cannot be hidden by posing the next shot. A
timeout still captures the resident view for diagnosis and fails the run.

`--shots-out` defaults to a `<file stem>-shots/` folder beside the shots file. `shots.log` there
has one line per shot: its name, the frames it waited, whether it settled or timed out, the image's
path and exact render-target dimensions. Skipped shots have a line
saying why. Each line is appended as its shot finishes, and the file is created with the first
line, so a run that is killed keeps the shots it had already taken.

The run exits with success once every shot is written, and exits non-zero - the process's exit code
is not zero, and `main` reports the failure - when an image or the log could not be written, when a
shot was given up on, when the engine exited with an error for any other reason, and when an
acceptance run's gates did not pass. `scripts/phase2-acceptance.ps1` records that exit code per
command and marks the command failed on anything but 0, so a failed shots or acceptance run is now
visible in a campaign report instead of being read as a pass.

`--headless` is ignored during a shots run, because the shots runner requires the render loop. `--shots`
cannot be combined with a benchmark, `--acceptance-screenshot`, `--auto-fly-speed` or a fixture.
`--run-label` names the run in the window title (`Mudcrab - shots: <label>`).

`--water-time <seconds>` pins the water's wave phase at that time, so two captures of the same
pose draw the same ripples and reflection distortion. Without it the waves follow the clock, so
interactive runs and benchmarks are unchanged; the terrain/water fixture keeps its own fixed phase
unless `--water-time` overrides it. A negative or non-finite value is refused.

## Game clock and sun direction

`--game-hour <hour>` starts the game clock at a finite hour in `[0,24)` (default 8, the retail `GameHour` record: GLOB 0x38 in the pack data). A capture that needs another hour
passes it explicitly, for example `--game-hour 12`.
Ordinary play advances at `--timescale <scale>` game seconds per real second (default 20, the retail `TimeScale` record: GLOB 0x3A;
0 freezes). Shots, acceptance screenshots, benchmarks (including `--benchmark-only` alone) and matched routes freeze at the launch
hour regardless of timescale, so streaming duration cannot move shadows between captures.
The clock advances with Bevy virtual time, which clamps each frame delta to its default maximum
of 250 ms. A frame slower than 4 FPS therefore advances the clock slower than real time, and a
pause of virtual time (the console, the settings menu) stops it. Retail behaviour is not
established.

The directional light follows the retail sun update (SkyrimSE.exe 1.7.104.0 `0x14041bd90`;
registry RF-SUN-001 to RF-SUN-004). Every value comes from the pack. With `T = fSunAlphaTransTime`,
the active worldspace's climate TNAM gives sunrise and sunset midpoints (including PNAM-requested
parent climate inheritance; TNAM bytes are ten-minute units). The day window is
`A = riseMid - T/2` to `B = setMid + T/2`.

- Day (`A < h < B`): `u = (h - A) / (B - A)` and `x = (1 - 2u) * fSunXExtreme` (east to west).
- Night: `u` runs 0..1 from `B` back to `A` and `x = (2u - 1) * fSunXExtreme` (west to east).
- The light travels along `normalize(-(fSunDirXExtreme / fSunXExtreme) * x,
  -(fSunDirXExtreme / fSunXExtreme) * fSunYExtreme, fSunZExtreme)` in Creation space. The engine
  uses the opposite (toward-sun) vector and maps axes to `(x,z,-y)`.
- `sun_alpha` is 0 outside `[A, B]`, ramps 0 to 1 over `riseMid +- T/2`, is 1 between the ramps and
  ramps 1 to 0 over `setMid +- T/2` (RF-SUN-004). Nothing consumes it yet: the visible sun disc and
  its glare (alpha times the weather byte at `+0x668` over 255) are not part of this change.

For the Skyrim.esm values (`fSunXExtreme` 400, `fSunYExtreme` 40, `fSunZExtreme` -150,
`fSunDirXExtreme` 200, `fSunAlphaTransTime` 0.5) and the `DefaultClimate` TNAM 33/60/96/123,
`A` is 7.5 h and `B` is 18.5 h. The elevation is 36.7 degrees at `A` and `B` and 82.4 degrees at
13:00, with a slight north tilt. The light keeps this geometry at night, so it never drops below
36.7 degrees. The night look comes from the weather's night colours, not from this vector.

The five GMSTs are the sun path itself (RF-SUN-003). The winning canonical GMST records supply
them. Optional `[Sky]` keys of the same names in `--ini` files override them individually (an
engine extension; retail reads only the GMSTs). A pack that lacks a GMST uses the retail exe
default of that setting, so the same formula runs: `fSunXExtreme` 400, `fSunYExtreme` 25,
`fSunZExtreme` -100, `fSunDirXExtreme` 400, `fSunAlphaTransTime` 2.0 (`rea-20261114/q4-sun.md`;
confirmed against the exe settings dump: record addresses 0x14209e940, 0x14209e958, 0x14209e970,
0x14209e988, 0x14209e9a0, with the value at record + 8). A world without a usable climate timing has
no sunrise or sunset, so no sun path applies and its sun has no authored direction (identity
rotation), both at startup and when travel enters such a world, so lighting does not depend on
travel history. Retail worlds always carry climate timing, so this is not retail behaviour and
affects only fixtures and benchmarks (a test-only path). A fade that makes the day window 24 hours
or longer (for example `fSunAlphaTransTime` 14 with the 33/60/96/123 TNAM) is reported once with a
warning that names the value and the climate; that world also gets no sun path, and no recovery
value is invented.
Invalid record/INI fields warn and are ignored.
`--sun-position` remains a fixed explicit bearing/elevation override. Capture-frame receipts
include the actual clock and engine-space toward-sun vector.

This step changes direction only. No moon light, weather selection or night palette is introduced.
