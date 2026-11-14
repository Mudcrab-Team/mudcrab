# Water (WATR) record research

The converter projects a WATR record's colours and reflectivity factors from its `DNAM` subrecord
into the `waters` table (`docs/specs/converters/db-schema.md` section 13). The engine reads the
above-water fog straight from the `DNAM` inside the table's `data` column, so fog needs no column.
This file records the `DNAM` byte layout both come from.

## 1. DNAM layout

### 1.1 Lengths

Skyrim Legendary Edition writes a 228-byte `DNAM` (slots `0x00`–`0xE0`). Skyrim Special Edition
writes 232 bytes (`0xE8`), adding one trailing float, `flowmap_scale`, at offset `0xE4` (228).
Every other slot is identical between the two, so the offsets below are present in both lengths.

### 1.2 Offset table

Field names are CommonLibSSE-NG `WaterShaderData`
(`include/RE/T/TESWaterForm.h`); slots that source leaves unnamed are marked *unknown*. Colour
slots are four-byte `Color`s (RGBA) and read little-endian like the floats; `u32` slots are noted.
The fog distances are floats in world units, and `above_water_fog_amount` is the curve exponent
(0 keeps the body clear).

| offset | field | decoded by |
| --- | --- | --- |
| 0x00 | unknown (`unk00`) | |
| 0x04 | unknown (`unk04`) | |
| 0x08 | unknown (`unk08`) | |
| 0x0C | unknown (`unk0C`) | |
| 0x10 | `sun_specular_power` | |
| 0x14 | `reflection_amount` (reflectivity) | converter |
| 0x18 | `fresnel_amount` | converter |
| 0x1C | unknown (`unk1C`, u32) | |
| 0x20 | `above_water_fog_dist_near` | engine, from `data` |
| 0x24 | `above_water_fog_dist_far` | engine, from `data` |
| 0x28 | `shallow_water_color` | converter |
| 0x2C | `deep_water_color` | converter |
| 0x30 | `reflection_water_color` | converter |
| 0x34 | unknown (`unk34`, u32) | |
| 0x38 | unknown (`unk38`) | |
| 0x3C | unknown (`unk3C`) | |
| 0x40 | unknown (`unk40`) | |
| 0x44 | unknown (`unk44`) | |
| 0x48 | `displacement_size` | |
| 0x4C | `displacement_force` | |
| 0x50 | `displacement_velocity` | |
| 0x54 | `displacement_falloff` | |
| 0x58 | `displacement_dampener` | |
| 0x5C | unknown (`unk5C`) | |
| 0x60 | `noise_falloff` | |
| 0x64 | `noise_wind_direction_a[0]` | engine, from `data` |
| 0x68 | `noise_wind_direction_a[1]` | engine, from `data` |
| 0x6C | `noise_wind_direction_a[2]` | engine, from `data` |
| 0x70 | `noise_wind_speed_a[0]` | engine, from `data` |
| 0x74 | `noise_wind_speed_a[1]` | engine, from `data` |
| 0x78 | `noise_wind_speed_a[2]` | engine, from `data` |
| 0x7C | unknown (`unk7C`) | |
| 0x80 | unknown (`unk80`) | |
| 0x84 | `above_water_fog_amount` | engine, from `data` |
| 0x88 | unknown (`unk88`) | |
| 0x8C | `underwater_fog_amount` | |
| 0x90 | `underwater_fog_dist_near` | |
| 0x94 | `underwater_fog_dist_far` | |
| 0x98 | `refraction_magnitude` | |
| 0x9C | `specular_power` | |
| 0xA0 | unknown (`unkA0`) | |
| 0xA4 | `specular_radius` | |
| 0xA8 | `specular_brightness` | |
| 0xAC | `uv_scale_a[0]` | engine, from `data` |
| 0xB0 | `uv_scale_a[1]` | engine, from `data` |
| 0xB4 | `uv_scale_a[2]` | engine, from `data` |
| 0xB8 | `amplitude_a[0]` | engine, from `data` |
| 0xBC | `amplitude_a[1]` | engine, from `data` |
| 0xC0 | `amplitude_a[2]` | engine, from `data` |
| 0xC4 | `reflection_magnitude` | engine, from `data` |
| 0xC8 | `sun_sparkle_magnitude` | |
| 0xCC | `sun_specular_magnitude` | |
| 0xD0 | `depth_properties.reflections` | engine, from `data` |
| 0xD4 | `depth_properties.refraction` | engine, from `data` |
| 0xD8 | `depth_properties.normals` | engine, from `data` |
| 0xDC | `depth_properties.specular_lighting` | engine, from `data` |
| 0xE0 | `sun_sparkle_power` | |
| 0xE4 | `flowmap_scale` (SSE only) | |

The converter decodes five slots into columns: +20 (reflectivity), +24 (fresnel) and
+40/+44/+48 (shallow/deep/reflection colour). The engine decodes the rest it uses from `data`,
with no reconversion: the fog (+32, +36, +132), the three noise layers' wind direction, wind speed,
UV scale and amplitude (+0x64, +0x70, +0xAC, +0xB8), the reflection magnitude (+0xC4) and the
depth properties (+0xD0).

### 1.3 DefaultWater reference values

The engine falls back to Skyrim's DefaultWater fog per water when a record's `DNAM` does not carry
a complete, finite fog triple. In the registered packs (`schema_info` 9) the `waters` table has 60
rows and no fog columns; reading `data` gives for example DefaultWater -10 / 150 / 0.7 and the
creek waters 10 / 90 / 0.71 and 20 / 160 / 0.87. DefaultWater (Skyrim.esm, Update.esm) holds near −10.0, far 150.0, amount
0.7. Other observed retail values: `PuddleWater` far 10.0, `ClearInteriorWaterAnkleHeightStill`
amount 0.0, and `DLC1FrostreachWaterFlowNE` (Dawnguard.esm) −290.0 / 720.0 / 0.7.

Sources: xEdit `Core/wbDefinitionsTES5.pas` WATR `DNAM` struct (TES5Edit/TES5Edit);
CommonLibSSE-NG `include/RE/T/TESWaterForm.h` `WaterShaderData` byte offsets; UESP
"Skyrim Mod:Mod File Format/WATR".

## 2. How Skyrim draws water, and what the engine does about it

### 2.1 Sources

| id | source | licence | what it gave |
| --- | --- | --- | --- |
| S1 | CommonLibSSE-NG `include/RE/T/TESWaterForm.h` @ `b93280e` (https://github.com/CharmedBaryon/CommonLibSSE-NG) | MIT | the `WaterShaderData` (`DNAM`) field names and offsets; `NAM2` to `NAM5` as the three noise textures and the flow map; `FNAM` flag bits (flow map `0x08`, blend normals `0x10`); `texScroll[3]` as a runtime field |
| S2 | Community Shaders `package/Shaders/Water.hlsl` @ `dc89d339` (https://github.com/doodlum/skyrim-community-shaders) | GPL-3.0 (Mudcrab is GPL-3.0, so the formulas may be followed with credit; no code is copied) | the water vertex and pixel shader: noise normals, Fresnel, depth factors, fog curve, reflection direction, composite. It is a re-implementation of the retail shader by modders. The retail shader bytecode was not read for this PR |
| S3 | xEdit `Core/wbDefinitionsTES5.pas` (https://github.com/TES5Edit/TES5Edit) | MPL-2.0 | the record layout |
| S4 | the `waters` table of the two registered local packs, read-only (60 records, `data` column) | local game data, not published | retail values of every field, and the checks below |
| S6 | reverse-engineering analysis of `SkyrimSE.exe` 1.7.104.0 (SHA-256 `846efccf…1402f`), `evidence/rea-20261114/q1-water.md` (static analysis, capstone disassembly of the owner's own copy; nothing from the binary is published here beyond the formula, addresses and constants) | the owner's own analysis | the noise scroll formula, units and angle convention, at VA `0x1405290ac` to `0x140529356`; the degrees-to-radians constant at `0x1418121c8` |
| S5 | `awarebayes/skyrim-research` `reconstruction/` @ local checkout | no licence granted to reconstructions | consulted; its water files declare structure sizes only and give no shader or scroll maths, so nothing was taken |

### 2.2 What each field does, and the state in the engine

| `DNAM` field (offset) | Skyrim (source) | engine | state |
| --- | --- | --- | --- |
| `uvScaleA[3]` (+0xAC) | world units per tile of the three noise layers: `uv = (world.xy + QPosAdjust) / scale + scroll` (S2 vertex shader) | `water_noise_*` UVs | implemented |
| `amplitudeA[3]` (+0xB8) | layer weights: `normalize((0,0,1) + a0*n0 + a1*n1 + a2*n2)`, layer 0 decoded as `2t + (-1,-1,-2)`, the others `2t - 1` (S2 `GetWaterNormal`) | `noise_normal` | implemented |
| `NAM2`..`NAM4` | the three noise normal maps (S1, S2) | read from the record bytes, no reconversion | implemented |
| `depthProperties.normals` (+0xD8) | `normalize(lerp((0,0,1), n, D8 * (distanceMul.z - 1) + 1))`: normals flatten in shallow water (S2) | `normals_depth` | implemented |
| `depthProperties.reflections`, `.refraction`, `.specularLighting` (+0xD0, +0xD4, +0xDC) | same form `D * (distanceMul - 1) + 1`: the refraction offset and sun specular fade with depth (S2) | decoded, not used (the engine has no refraction and keeps Bevy's sun lighting) | not used |
| `fresnelAmount` (+0x18) | `F + (1 - F) * (1 - saturate(n.v))^5` (S2 `GetFresnelValue`) | `fresnel` | implemented |
| reflection weight | `lerp(body colour, reflection, fresnel)` with no other factor (S2 `main`) | `fresnel * the user's multiplier` | implemented; the earlier `reflectivity` factor and `max(body, fresnel)` alpha are removed |
| `reflectionAmount` (+0x14) | only scales the flat Reflection Color used when there is no reflection image (S2 `GetWaterSpecularColor`) | the fallback colour where the reflection image has no coverage | implemented |
| `reflectionMagnitude` (+0xC4) | the reflection direction is `reflect(view, m * n + (0,0,1 - m))` (S2) | the planar reflection is looked up where that direction lands (the engine's planar reflection is Mudcrab's technique; Skyrim samples a cube map). Derived from geometry, no constant | implemented |
| above-water fog near, far, amount (+0x20, +0x24, +0x84) | `refractionMul = 1 - saturate((far - path) / (far - near))^amount` is the body's share over what lies behind; the body colour is `lerp(shallow, deep, path / far)`, `path` being the view ray's length through the water (S2 `distanceMul.x`, `.y`) | `water_body`; the body colour now follows the path (it followed the vertical depth) | implemented |
| `noiseFalloff` (+0x60) | far distance blend `distanceFactor` uses it with a frame parameter (S2) | not used | UNKNOWN: the frame parameter's value. The invented 2048 to 16384 ripple fade is removed |
| `noiseWindDirectionA[3]` (+0x64), `noiseWindSpeedA[3]` (+0x70) | per layer `i`: `scroll_i = frac(sin(theta_i) * speed_i * T, cos(theta_i) * speed_i * T)` with `theta_i` the direction in degrees times `0.017453292`, `T` the water clock in seconds, speed in tiles per second; the pixel UV is `world.xy / uvScale_i + scroll_i`. The direction is the compass bearing the wind comes FROM (0 north, 90 east); the pattern moves toward bearing + 180 (S6; the UV form is S2 `NormalsScroll`) | `noise_scroll` in `water.wgsl`, mirrored by `noise_scroll` in `render.rs` and tested | implemented. Clock caveat: S6 reads `T` as the accumulated per-frame delta in seconds; whether it is the scaled or real delta, and whether it stops in menus, is unverified. The engine uses its shader clock (`globals.time`, or `--water-time`) |
| `FNAM` flow map bit, `NAM5` | a per-cell flow map plus its normals, `GetFlowmapNormal` (S2) | not used. The previous tilt added by the flow-normal texture used an unsourced strength and is removed; the "Water flow normals" setting does nothing until this is sourced | UNKNOWN: the engine has no per-cell flow map |
| `sunSpecularPower` (+0x10), `sunSparkle*`, `sunSpecularMagnitude` | sun glint `pow(saturate(dot(reflect(view, n), sun)), power) * sunColor * (1 - exp(-DeepColor.w))` (S2 `GetSunColor`) | not used; Bevy's lit specular stays | UNKNOWN: what the shader's `DeepColor.w` holds, and the sun colour used |
| `specularPower`, `specularRadius`, `specularBrightness`, `refractionMagnitude`, displacement fields | point-light specular, refraction offset, wading (S2) | not used | not implemented |

Each UNKNOWN row has an issue. Every number in the water shader is a WATR field, a number from S2 or S6, or an engineering guard (a divisor floor, a bottomless-path value, the clock wrap).

### 2.3 Flow

The waves scroll as the retail engine scrolls them (row `noiseWindDirectionA` above, S6). The earlier note in this file said the wind directions disagreed with the directional record names. That was wrong: it assumed the angle is the direction the water travels. With the retail convention (the angle is where the wind comes from, so the pattern moves toward angle + 180) the five directional retail records agree with their names, using the first layer's direction:

| record | first-layer direction (degrees) | flows toward (degrees) | name |
| --- | ---: | ---: | --- |
| `RiverWaterFlowNE` | 233 | 53 | NE |
| `RiverWaterFlowNW` | 141 | 321 | NW |
| `RiverWaterFlowSE` | 340 | 160 | SE (south-south-east) |
| `CreekWaterFlowSW` | 33 | 213 | SW |
| `CreekWaterFlowSE` | 360 | 180 | SE (south) |

No `RiverWaterFlowSW` record exists in Skyrim.esm or Update.esm. A test (`the_wind_direction_is_the_bearing_the_wind_comes_from`) holds these five rows.

What a record cannot give is the direction of a particular river: every cell that uses one record scrolls the same way, so a river's course comes from which directional record its cell names, as in retail. The earlier experiments are kept as findings: the cell water height is the worldspace's own (many neighbouring cells share one value, for example -250, 600 or -5050), the LAND bed slope under the water points a different way in almost every one of 52 retail river and creek cells (Tamriel grids -4..16 by -22..0), and the flat planes carry no flow map. None of them is used.
