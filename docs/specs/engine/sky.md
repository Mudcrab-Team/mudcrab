# Sky

The engine draws Skyrim's daytime sky as a gradient dome around the world camera and fogs the
world to meet it (`crates/engine/src/sky.rs`, `crates/engine/src/shaders/sky.wgsl`). This is one
weather at one time of day: no clouds, sun, moons or stars yet.

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
parity gaps. Existing encoded weather mixing and fog equations remain provisional pending retail
comparison; the output correction does not validate those assumptions.

- **Colours.** `SkyPalette` holds Sky-Upper, Sky-Lower, Horizon, Fog Near and Fog Far, plus a
  linear brightness scale. The built-in palette is `SkyrimClear`'s day column (Upper 21,77,117;
  Lower 60,135,183; Horizon 125,163,183; Fog Near 35,98,124; Fog Far 116,168,203, 8-bit sRGB).
  The table above is `DOME_MASKS`. Both are plain constants so that weather records can replace
  them later.
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

## Fog

- **What vanilla does.** A weather's `FNAM` carries four fog distances per time of day, `Near`,
  `Far`, `Power` and `Max`. The fog amount at a distance `d` is `min(Max, t ^ Power)`, where
  `t = clamp((d - Near) / (Far - Near), 0, 1)`, and the fog colour is the weather's Fog Near to Fog
  Far colour lerp taken at that amount. `SkyrimClear`'s day column is `Near` 0, `Far` 80,000,
  `Power` 0.4, `Max` 0.85: a 0.4-power curve that reaches its full strength of 0.85 at 53,289 units
  and stays there. Its night column is the same curve over 40,000 units.
- **The Bevy setup.** The engine no longer fits vanilla's `FNAM` power curve. It uses Bevy's
  visibility-based atmospheric fog: `FogFalloff::from_visibility_colors` with
  `FOG_VISIBILITY = 48_300` world units, Bevy's revised Koschmieder contrast threshold of 5%.
  That distance is where a perfectly black object against the horizon sky has lost 95% of its
  contrast. The extinction tint is Bevy's atmospheric fog example colour
  `srgb(0.35, 0.50, 0.66)`, so distant light loses red fastest. Bevy's example inscattering
  tint `srgb(0.80, 0.844, 1.0)` read too blue here, so the engine uses a desaturated
  `srgb(0.84, 0.89, 0.90)` while keeping blue marginally fastest.
- **Colour.** The fog colour itself is still the linear Fog Far colour the camera clears to, and
  it is opaque, so at full strength fogged terrain, the dome's transparent band and the clear
  colour meet at the horizon without a step. The sun's glow inside the fog uses Bevy's example
  warm directional tint, `srgba(1.0, 0.95, 0.85, 0.5)` with exponent 30.
- **Which cameras.** Every camera marked `FogCamera`: the streaming world camera and the material,
  terrain/water, transform-bounds and renderer fixture cameras. Bevy applies `DistanceFog` to
  `StandardMaterial` meshes by itself and to the terrain and water shaders through
  `main_pass_post_lighting_processing`; the dome's shader does not ask for fog, so the dome stays
  unfogged. The water reflection camera is left unfogged, because the water surface it renders into
  is fogged once already.
- **Interiors.** An interior cell has no weather, so `CameraSpace::Interior` takes the fog off
  every `FogCamera`, as it hides the dome.
- **Range.** The visibility distance is longer than the default camera far plane, so terrain is
  hazed rather than swallowed inside the streamed ring, and the dome's transparent band keeps
  the horizon line itself soft. Tune `FOG_VISIBILITY` first; tune tints only after that.

## Not yet

- Weather and climate records from the world database, time of day and weather transitions: the
  fog density and colour are the built-in clear-day ones. External emittance (#242) already stores
  its sources' colours at sunrise, day, sunset and night, and the engine applies the day colour.
- The cloud layers (including layer 28, the fog band that straddles the horizon), stars, sun and
  glare, moons and aurora.
- A calibrated sky brightness against lit surfaces; the scale is 1.0 until it is measured on
  reference screenshots.
