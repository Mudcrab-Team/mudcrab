# Sky

The engine draws a gradient dome around each sky camera and resolves its weather colors from
the environment catalog (`crates/engine/src/sky.rs`, `crates/engine/src/environment.rs`). Clouds,
sun, moons and stars remain unimplemented.

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

Scene cameras now compose sky, background and fog with surfaces in HDR, then apply one shared
display transform. See [L1 color pipeline](color-pipeline.md) for domains, tests and remaining
parity gaps. The [distance-fog spec](distance-fog.md) defines the recovered weather mixing and
fog equations. Retail captures still need to establish the complete working color domain.

- **Colours.** `SkyPalette` holds Sky-Upper, Sky-Lower, Horizon and Fog Far, plus a linear
  brightness scale. The built-in palette is `SkyrimClear`'s day column (Upper 21,77,117;
  Lower 60,135,183; Horizon 125,163,183; Fog Far 116,168,203, authored 8-bit RGB). The environment
  resolver replaces those defaults using four-key weather mixing and explicit transition state.
  The table above is `DOME_MASKS`.
- **Gradient.** The CPU mixes each table row from normalized byte RGB and uploads the 16 rows.
  The fragment shader takes the elevation of the view ray, interpolates between rows and scales
  by the brightness. It performs no sRGB decode, matching the recovered color preparation. The
  analytic row interpolation remains an approximation to retail's vertex-colored sky mesh.
- **Geometry and depth.** A procedural hemisphere follows the camera's position (never its
  rotation). Its vertex shader writes clip depth 0, the far plane of Bevy's infinite reverse-Z
  projection, so the dome is behind every other surface. The material is alpha-blended, unlit,
  unculled, writes no depth, has no prepass and casts no shadow, and sorts behind every other
  transparent draw.
- **Clear colour.** The camera clears to Fog Far, which shows through the dome's transparent band
  and below the horizon wherever no terrain is drawn.
- **Interiors.** Resolved Sky mode and the worldspace NoSky flag control dome visibility. Interiors
  can show a sky, including mode 2. An interior without known CELL inputs hides it and clears to
  black. `--fog-interior-cell` supplies a diagnostic environment identity without loading geometry.
- **Which cameras.** Only a camera marked `SkyCamera` gets a dome and a sky clear colour: the world
  and water reflection cameras. A dedicated layer per camera prevents either view from drawing
  the other view's dome and doubling the horizon alpha. Fixture cameras opt in explicitly.

## Fog

`FogCamera` views now use the recovered capped power curve and both fog colors. A depth pass
composes opaque fog before transparent geometry. Converted NIF materials and water use a
separate geometry path; stock Bevy `DistanceFog` is removed. CELL/template inheritance supplies
interior fog, and inherited Fog Clip caps camera far metadata separately from the fog range.
Fog reconstruction uses that distance; ordinary geometry far clipping remains unimplemented.

The sky dome remains unfogged. Water reflections use the same resolved world environment as
the main camera; native reflection ownership still needs a retail capture. Older databases with
no environment projection retain an explicit clear-day exterior fallback and disable interior
fog. See [implementation status](distance-fog-implementation.md) for supported paths and limits.

[Skyrim distance fog: parity specification](distance-fog.md) defines the replacement from the
copied records, native code, and shipped shader bytecode. It also records the retail captures
required before claiming that the complete result matches Skyrim.

## Not yet

- Automatic region weather selection, GLOB conditions, game-clock advancement and room scheduling.
  The resolver consumes explicit current/previous weather, hour and room state.
- The cloud layers (including layer 28, the fog band that straddles the horizon), stars, sun and
  glare, moons and aurora.
- A calibrated sky brightness against lit surfaces; the scale is 1.0 until it is measured on
  reference screenshots.

## Stable image-space atmosphere inputs

With `--image-space` active for a supported stable record, atmosphere RGB follows the recovered
scale and bias `k * weightedRGB + k * SkyScale`, with `k` supplied by the environment's framebuffer
reciprocal. Sky Scale is additive. The native dither sample and exact vertex interpolation remain
parity limits. The fog-far background uses `k * FarRGB` as a project fallback; the native clear
producer is unverified. The default diagnostic sky path remains selectable without image space.
See [image-space.md](image-space.md) for native provenance and the remaining transfer gates.
