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

- **Colours (see Fog below for the facts and their tags).** `SkyPalette` holds Sky-Upper, Sky-Lower, Horizon, Fog Near and Fog Far, the fog
  distances (`fog_ramp`), plus a linear brightness scale. With a world database the palette is
  built from the active weather record at the game clock's hour (`weather_sky.rs`, #891): the
  `NAM0` colours of the four time-of-day slots are mixed by the retail weights (RF-SUN-005). The
  active weather is `SkyrimClear`, a fixed Mudcrab choice, since weather changes are not
  simulated. Without a database (a fixture) the built-in palette is `SkyrimClear`'s day column
  (Upper 21,77,117; Lower 60,135,183; Horizon 125,163,183; Fog Near 35,98,124; Fog Far
  116,168,203, 8-bit sRGB; fog near 0, far 80,000, power 0.4, max 0.85). The table above is
  `DOME_MASKS`.
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

Source of the retail facts: [skyrim_decomp#812](https://github.com/Mudcrab-Team/skyrim_decomp/pull/812),
a static reading of 1.7.104.0 with an independent review. Each fact below carries its tag from
that finding: verified, inferred or unverified.

- **Fields (verified).** A weather's `FNAM` holds eight float32 values: day and night near, far,
  power and max. Fog near colour is row 1 and fog far colour is row 12 of `NAM0`, each with four day
  parts. `SkyrimClear` by day: near 0, far 80,000, power 0.4, max 0.85. Its night column: near 0,
  far 40,000, power 0.4, max 0.85.
- **Blend by day part (verified).** One day weight mixes near, far, power and max, each as
  `weight * day + (1 - weight) * night`. The weight ramps up from 0 to 1 across sunrise
  (`A` to `B`), is 1 from sunrise end to sunset begin (`B` to `C`) and ramps down across sunset
  (`C` to `D`), where `A` is sunrise begin less 0.5 hours (not below 0) and `D` is sunset end plus
  0.5 hours (not above 23.99). The colours use four weights with the half-interval blends in
  [visual-profiles.md](visual-profiles.md#sky-and-fog-source-891). During a weather change retail
  also mixes in the previous weather; the engine has no weather change.
- **Formula (verified).** With `d` the distance, `s = saturate((d - near) / (far - near))`,
  `F = min(s ^ power, max)` and the fog colour `Cf = near colour + F * (far colour - near colour)`.
  A fragment then runs from its lit colour to `Cf` by `F`. This is a power ramp capped by max,
  linear when power is 1. It is not exponential. With both endpoints zero retail uploads max 0, so
  no fog is drawn; Mudcrab draws no fog for `far <= near` or `max <= 0`.
- **Opaque draws under the `skyrim` profile.** Retail fogs opaque geometry in a full-screen composite,
  not in the material shader. See [visual-profiles.md](visual-profiles.md#sky-and-fog-source-891) and
  `crates/engine/src/retail_fog.rs`. The composite reads linear view depth `z` (axial), not the
  vertex fog's `|clip.xyz|`. The camera's `DistanceFog` then holds `FogFalloff::PowerRampBlended`,
  which only fogs the draws the composite does not cover.
- **What Mudcrab draws (`mudcrab` profile, and blended draws under `skyrim`).** `FogFalloff::PowerRamp`, a mode Mudcrab adds to its vendored `bevy_pbr`
  (`vendor/BEVY_PATCHES.md`). It takes near, far, power, the near colour and, in the fog colour's
  alpha, max. Every shader that applies fog through Bevy's `apply_fog` (materials, terrain, water
  and grass) gets it. The Graphics fog-density multiplier and the `cool-gray` density scale divide
  near and far.
- **UNVERIFIED, and not as retail does it.**
  - Retail's `d` is the length of the clip-space XYZ before the divide by W, which depends on the
    projection. Mudcrab uses the world distance from the camera. The size of the difference is
    not measured.
  - Retail evaluates the ramp at the vertices and interpolates the result. Mudcrab evaluates it
    per pixel.
  - After the fog blend retail applies further steps whose runtime flags are unverified, so they
    are not drawn.
  - Retail's object LOD and terrain LOD use the same constants and formula (verified for the
    checked techniques). The sky, cloud, sun and moon fog are unverified, and the dome is not fogged.
  - A scene far-distance override can replace far. Its gate starts closed in the image and its
    runtime writer is unverified, so it is not applied.
  - The interior weather overlap clamps near and far (`0.17 * far`, a far default of 163,840).
    That is the interior path (`interior-environment.md`); the exterior takes the record as it is.
- **Colour.** The far colour is the linear Fog Far colour the camera clears to, so at the cap
  fogged terrain, the dome's transparent band and the clear colour meet at the horizon without a
  step. In the `mudcrab` profile the sun's glow inside the fog uses Bevy's example warm
  directional tint, `srgba(1.0, 0.95, 0.85, 0.5)` with exponent 30. That glow is Mudcrab's own
  look (whether retail has one is unverified), and the `skyrim` profile draws none.
- **Which cameras.** Every camera marked `FogCamera`: the streaming world camera and the material,
  terrain/water, transform-bounds and renderer fixture cameras. Bevy applies `DistanceFog` to
  `StandardMaterial` meshes by itself and to the terrain and water shaders through
  `main_pass_post_lighting_processing`; the dome's shader does not ask for fog, so the dome stays
  unfogged. The water reflection camera is a `FogCamera` too: retail's screen-space reflection is reasoned (an inference from RF-WATER-009 and
  RF-WATER-011, not traced) to read the already-fogged frame, and the water surface is fogged once more
  as part of the main view.
- **Interiors.** An interior cell has no weather, so `CameraSpace::Interior` hides the dome and
  takes the weather fog off every `FogCamera`. In sky modes 1 and 2 `apply_fog` gives each
  `FogCamera` the cell's own fog (`InteriorFog`, `docs/specs/engine/interior-environment.md`):
  Bevy linear fog from the cell's near and far and near colour, with max as a cap. A cell without
  a fog ramp, and the one mode 3 interior, have no fog. The fog switch still applies; the fog
  density multiplier does not.
- **Range.** The weather's far distance (80,000 units for `SkyrimClear` by day) is longer than the
  default camera far plane, so terrain is hazed rather than swallowed inside the streamed ring,
  and the dome's transparent band keeps the horizon line itself soft.

## Not yet

- Weather selection and weather transitions: the active weather is the fixed `SkyrimClear`, and the
  transition factor is always 1. The sun and ambient light colours, the image space and the water
  multiplier do not follow the weather's time-of-day slots yet. External emittance (#242) already stores
  its sources' colours at sunrise, day, sunset and night, and the engine applies the day colour.
- The cloud layers (including layer 28, the fog band that straddles the horizon), stars, sun and
  glare, moons and aurora.
- A calibrated sky brightness against lit surfaces; the scale is 1.0 until it is measured on
  reference screenshots.
