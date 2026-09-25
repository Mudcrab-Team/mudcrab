# Sky dome

The engine draws Skyrim's daytime sky as a gradient dome around the world camera
(`crates/engine/src/sky.rs`, `crates/engine/src/shaders/sky.wgsl`). This is the first step of the
sky: one weather at one time of day, no clouds, sun, moons, stars or distance fog yet.

## What vanilla does

Skyrim's sky dome is `meshes/sky/atmosphere.nif` (one shape, `AtmosphereDome:0`). Its vertex
colours are weights, not colours: the sky shader forms
`R * Horizon + G * Sky-Lower + B * Sky-Upper` from the current weather's `WTHR` `NAM0` colours,
and the vertex alpha makes the lowest 2.1 degrees transparent. The dome is alpha-blended,
depth-tested, writes no depth and sits on the far plane. It is not fogged. Behind its transparent
band the viewer sees the fog colour, which is what hides the seam between land and sky.

Read ring by ring from the dome, the weights are:

| Elevation | Horizon | Sky-Lower | Sky-Upper | Alpha |
|---|---|---|---|---|
| 0.0° | 1.000 | 0 | 0 | 0.00 |
| 0.5° | 1.000 | 0 | 0 | 0.22 |
| 1.5° | 1.000 | 0 | 0 | 0.80 |
| 2.1° | 1.000 | 0 | 0 | 1.00 |
| 3.6° | 0.933 | 0.071 | 0 | 1 |
| 5.6° | 0.804 | 0.220 | 0 | 1 |
| 9.7° | 0.490 | 0.573 | 0 | 1 |
| 14.5° | 0.157 | 0.910 | 0 | 1 |
| 18.0° | 0.020 | 1.000 | 0.016 | 1 |
| 19.4° | 0.008 | 1.000 | 0.020 | 1 |
| 26.8° | 0 | 0.769 | 0.306 | 1 |
| 34.9° | 0 | 0.471 | 0.584 | 1 |
| 47.0° | 0 | 0.184 | 0.831 | 1 |
| 59.1° | 0 | 0.063 | 0.941 | 1 |
| 73.9° | 0 | 0.012 | 0.984 | 1 |
| 90.0° | 0 | 0 | 1.000 | 1 |

The converted `atmosphere.glb` keeps no vertex colours, so the engine builds the dome itself.

## What the engine does

- **Colours.** `SkyPalette` holds Sky-Upper, Sky-Lower, Horizon and Fog Far, plus a linear
  brightness scale. The built-in palette is `SkyrimClear`'s day column (Upper 21,77,117;
  Lower 60,135,183; Horizon 125,163,183; Fog Far 116,168,203, 8-bit sRGB). The table above is
  `DOME_MASKS`. Both are plain constants so that weather records can replace them later.
- **Gradient.** The CPU mixes each table row into a colour (in sRGB, as vanilla mixes the 8-bit
  values) and uploads the 16 rows. The fragment shader takes the elevation of the view ray,
  interpolates between rows, converts to linear and scales by the brightness.
- **Geometry and depth.** A procedural hemisphere follows the camera's position (never its
  rotation). Its vertex shader writes clip depth 0, the far plane of Bevy's infinite reverse-Z
  projection, so the dome is behind every other surface. The material is alpha-blended, unlit,
  unculled, writes no depth, has no prepass and casts no shadow, and sorts behind every other
  transparent draw.
- **Clear colour.** The camera clears to Fog Far, which shows through the dome's transparent band
  and below the horizon wherever no terrain is drawn.
- **Interiors.** `CameraSpace` says whether the camera is in an exterior or an interior cell
  (`CameraSpace::of(CellKey)`). In an interior the dome is hidden and the camera clears to black.
- **Which cameras.** Only a camera marked `SkyCamera` gets a dome and a sky clear colour: the world
  camera. The material, texture and renderer fixtures are unchanged.

## Not yet

- Distance fog in the Fog Far colour, weather and climate records from the world database, time of
  day and weather transitions.
- The cloud layers (including layer 28, the fog band that straddles the horizon), stars, sun and
  glare, moons and aurora.
- A calibrated sky brightness against lit surfaces; the scale is 1.0 until it is measured on
  reference screenshots.
