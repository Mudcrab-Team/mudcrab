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
The fog distances are floats in world units, and `above_water_fog_amount` is the curve exponent: the material copy writes the exponent
`max(4 * amount, 1)`, so an amount of 0 does not clear the body (RF-SHADER-006).

| offset | field | decoded by |
| --- | --- | --- |
| 0x00 | unknown (`unk00`) | |
| 0x04 | unknown (`unk04`) | |
| 0x08 | unknown (`unk08`) | |
| 0x0C | unknown (`unk0C`) | |
| 0x10 | `sun_specular_power` | engine, from `data` |
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
| 0x58 | `displacement_dampener` in CommonLibSSE-NG; the wading ripple normal amplitude in retail (RF-WATER-021) | |
| 0x5C | unknown (`unk5C`) | |
| 0x60 | `noise_falloff` | engine, from `data` |
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
| 0x90 | `underwater_fog_dist_near` | engine, from `data` |
| 0x94 | `underwater_fog_dist_far` | engine, from `data` |
| 0x98 | `refraction_magnitude` | engine, from `data` |
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
| 0xC8 | `sun_sparkle_magnitude` | engine, from `data` |
| 0xCC | `sun_specular_magnitude` | engine, from `data` |
| 0xD0 | `depth_properties.reflections` | engine, from `data` |
| 0xD4 | `depth_properties.refraction` | engine, from `data` |
| 0xD8 | `depth_properties.normals` | engine, from `data` |
| 0xDC | `depth_properties.specular_lighting` | engine, from `data` |
| 0xE0 | `sun_sparkle_power` | engine, from `data` |
| 0xE4 | `flowmap_scale` (SSE only) | |

The converter decodes five slots into columns: +20 (reflectivity), +24 (fresnel) and
+40/+44/+48 (shallow/deep/reflection colour). The engine decodes the rest it uses from `data` (every row marked "engine, from `data`" above, which includes the sun terms, noise falloff, refraction magnitude and underwater fog),
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
| S2 | (cross-check only; S7 replaces it for the mix) Community Shaders `package/Shaders/Water.hlsl` @ `dc89d339` (https://github.com/doodlum/skyrim-community-shaders) | GPL-3.0 (Mudcrab is GPL-3.0, so the formulas may be followed with credit; no code is copied) | the water vertex and pixel shader: noise normals, Fresnel, depth factors, fog curve, reflection direction, composite. It is a re-implementation of the retail shader by modders, used only as a cross-check: the retail shader bytecode was read for S7, which replaces it for the mix |
| S3 | xEdit `Core/wbDefinitionsTES5.pas` (https://github.com/TES5Edit/TES5Edit) | MPL-2.0 | the record layout |
| S4 | the `waters` table of the two registered local packs, read-only (60 records, `data` column) | local game data, not published | retail values of every field, and the checks below |
| S6 | reverse-engineering analysis of `SkyrimSE.exe` 1.7.104.0 (SHA-256 `846efccf…1402f`), `evidence/rea-20261114/q1-water.md` (static analysis, capstone disassembly of the owner's own copy; nothing from the binary is published here beyond the formula, addresses and constants) | the owner's own analysis | the noise scroll formula, units and angle convention, at VA `0x1405290ac` to `0x140529356`; the degrees-to-radians constant at `0x1418121c8` |
| S7 | reverse-engineering analysis of the retail water pixel shaders (`shaders011.fxp` group 8, keys 0x1C, 0x11C, 0x18) and of the WATR material copy and constant writer in `SkyrimSE.exe` 1.7.104.0, `evidence/rea-20261114/s1-water-mix.md`, registry RF-SHADER-001 and RF-SHADER-006 to RF-SHADER-010 | the owner's own analysis | the retail mix, opacity, Fresnel, reflection, depth factors, the WATR to constant map, FramebufferRange and the Water Multiplier |
| S5 | `awarebayes/skyrim-research` `reconstruction/` @ local checkout | no licence granted to reconstructions | consulted; its water files declare structure sizes only and give no shader or scroll maths, so nothing was taken |

### 2.2 What each field does, and the state in the engine

| `DNAM` field (offset) | Skyrim (source) | engine | state |
| --- | --- | --- | --- |
| `uvScaleA[3]` (+0xAC) | world units per tile of the three noise layers: `uv = (world.xy + QPosAdjust) / scale + scroll` (S2 vertex shader) | `water_noise_*` UVs | implemented |
| `amplitudeA[3]` (+0xB8) | layer weights: `normalize((0,0,1) + a0*n0 + a1*n1 + a2*n2)`, layer 0 decoded as `2t + (-1,-1,-2)`, the others `2t - 1` (S2 `GetWaterNormal`) | `noise_normal` | implemented |
| `NAM2`..`NAM4` | the three noise normal maps (S1, S2) | read from the record bytes, no reconversion | implemented |
| `depthProperties.normals` (+0xD8) | `normalize(lerp((0,0,1), n, D8 * (distanceMul.z - 1) + 1))`: normals flatten in shallow water (S2) | `normals_depth` | implemented |
| `depthProperties.reflections` (+0xD0), `.refraction` (+0xD4), `.normals` (+0xD8), `.specularLighting` (+0xDC) | each factor is `lerp(1, r, D)` with `r` = `sat(L / F)` (reflections, refraction) or `sat(h / F)` (normals, specular lighting), `L` the view ray length in the water, `h` the vertical depth, `F` the fog far (RF-SHADER-010) | reflections: the weight of the reflection (below); refraction: the refraction offset factor (`refracted_scene`); normals: `normals_depth`, and the flow program's depth factor with its 0.4 floor; specular lighting: the sun terms' depth factor | all four implemented |
| `fresnelAmount` (+0x18) | `F = f + (1 - f) * (1 - sat(-V.N))^5`, five multiplies (RF-SHADER-008, S7) | `fresnel` | implemented |
| mix weight and colour | `m = lerp(1, F * dR, distT)` and `color = lerp(diffuse, reflection, m)`, `dR` the reflections depth factor (RF-SHADER-008). With the frame flag `w = 1`, `distT = 1`, which is what the engine uses | `m = F * dR * the user multiplier`; the pixel is `lerp(lerp(refr, body, a), reflection, m) + sun`, `refr` the refracted scene copy, written opaque (alpha 1) in Bevy's transmissive phase (see the refraction and output rows) | implemented; the main pass value of the frame flag `w` is unverified (#698) |
| `reflectionAmount` (+0x14) | `reflection = lerp(ReflectionColor, environment, amount)`: the flat reflection colour toward the sampled environment. It is not the mix with the refraction (RF-SHADER-009) | `mix(flat colour, environment, amount)` | implemented |
| `reflectionMagnitude` (+0xC4) | the reflection vector is `reflect(V, lerp(up, N, magnitude))` (RF-SHADER-009) | the planar reflection image is looked up where that direction lands, derived from the camera geometry with no constant. Retail has a planar-texture variant (key 0x1C) as well as the cube-map variant (0x11C) | implemented; which variant a given pass uses is unverified |
| above-water fog near (+0x20), far (+0x24), amount (+0x84) | the material copy writes range `far - near` (1e-5 when zero), `max(far, 1)` and the exponent `max(4 * amount, 1)`; opacity `a = 1 - sat((F - min(L, F)) / range)^exponent` and body colour `lerp(shallow, deep, sat(L / F))` (RF-SHADER-006, RF-SHADER-010) | `water_body` | implemented. A fog amount of 0 does not clear the body: the exponent floors at 1 |
| `noiseFalloff` (+0x60) | `distT = sat(lerp(FrameParams.w, 1, (D - 8192) / (noiseFalloff - 8192)))`, the falloff clamped to [1, 8191] (RF-SHADER-007, RF-SHADER-008, RF-WATER-009); it weights the mix `m = lerp(1, F * dR, distT)` and the reflection image `sat(distT * fWaterSSRIntensity * image alpha)` | `dist_t` in `water.wgsl`, the frame flag `w` taken as 1 | implemented; the main pass's `w` is unverified (#698) |
| `noiseWindDirectionA[3]` (+0x64), `noiseWindSpeedA[3]` (+0x70) | per layer `i`: `scroll_i = frac(sin(theta_i) * speed_i * T, cos(theta_i) * speed_i * T)` with `theta_i` the direction in degrees times `0.017453292`, `T` the water clock in seconds, speed in tiles per second; the pixel UV is `world.xy / uvScale_i + scroll_i`. The direction is the compass bearing the wind comes FROM (0 north, 90 east); the pattern moves toward bearing + 180 (S6; the UV form is S2 `NormalsScroll`) | `noise_scroll` in `water.wgsl`, mirrored by `noise_scroll` in `render.rs` and tested | implemented. Clock caveat: S6 reads `T` as the accumulated per-frame delta in seconds; whether it is the scaled or real delta, and whether it stops in menus, is unverified. The engine uses its shader clock (`globals.time`, or `--water-time`) |
| shallow (+0x28), deep (+0x2C), reflection (+0x30) colours | each is `colour / 255` times the weather's Water Multiplier (Sky `+0x150`, the `WTHR` `NAM0` Water Multiplier colour, written once per frame), multiplied in sRGB values (RF-SHADER-007) | `water_srgb`, then to linear. The multiplier is SkyrimClear's day value (175, 237, 214), read from the `weather_colors` table | implemented for the static day palette; a weather and time-of-day driven value, and the interior value, are not (#698) |
| FramebufferRange | the retail water shader scales every texture it reads by 1.2 and its output by 1/1.2, so the colours it computes from constants reach the screen divided by 1.2 (RF-SHADER-001; only the console command `SetFramebufferRange` changes it) | `FRAMEBUFFER_RANGE`: the body and the flat reflection colour are divided, the sampled environment is not | implemented |
| refraction magnitude (+0x98), refraction | the scene colour is read at `P' = worldPos + (N.xy * 0x98 * lerp(1, sat(L / F), 0xD4), 0)`, projected; the depth there is reloaded and the offset is dropped when its scene point is on the camera side of the plane (RF-WATER-049; river 40, creek 8). The colour is `lerp(refr, body, a)` (RF-WATER-048) | `refracted_scene` in `water.wgsl`: Bevy's view transmission texture (the scene copy after the opaque pass), `reads_view_transmission_texture` through `specular_transmission > 0`, the water drawn opaque in the transmissive phase; the depth snapshot runs before that pass | implemented; the sign of the leak test is medium-high (RF-WATER-049); a no-depth water has no offset |
| output alpha | the retail programs write alpha 0 and a saturated colour; where the blending is done was not traced | the water writes the whole colour `lerp(lerp(refr, body, a), reflection, m) + sun` opaque over the refracted scene copy | engine mechanism |
| reflection source (cube + screen-space reflection) | `refl = lerp(ReflectionColor, env, DNAM 0x14)`, `env = lerp(cube * FBR, ssr, sat(distT * fWaterSSRIntensity * ssr.a))`; with `bUseCubeMapReflections` = 1 every exterior depth water, flow rivers included, uses the cube program; the flat colour is only `1 - DNAM 0x14` (0.2 for the rivers, 0 for the creeks); `fWaterSSRIntensity:Display` is 1.3 in the executable and 0.5 in this install's `Skyrim.ini`, a cap (RF-WATER-009, RF-WATER-050, RF-WATER-051; RF-SHADER-007 and -009 corrected) | the sky dome's colour along `reflect(V, lerp(up, N, magnitude))`, kept in step with the live palette, as the stand-in for retail's rendered six-face sky cube (Tamriel's worldspace name is a 2D path, so retail falls back to a rendered cube; `quicksky_e` only when no name exists); the planar reflection image takes the screen-space reflection's place with the same weight, 0.5. The 32 by 32 `quicksky_e` cube was tried and read as coarse rectangular blocks | implemented; the dome has no clouds or sun disc, which a rendered cube would show (#698) |
| reflection camera fog | retail's screen-space reflection reads the already-fogged frame; the water fog is the scene fog (RF-WATER-009, RF-WATER-011) | the reflection camera is a `FogCamera` | implemented; REASONED from the SSR source, not traced (medium confidence) |
| variants without depth, interior | the exterior no-depth program is `lerp(Shallow, Deep, F) * sat(sunDir.N)` with the ANAM opacity; the interior program is `lerp(Shallow, Deep, F)`, the flat reflection colour times the Reflection Amount, no sun, no light (RF-WATER-017, RF-WATER-026) | both are in `water.wgsl`; the interior flag comes from the cell | implemented |
| underwater fog, `DNAM` +0x90 (near), +0x94 (far), +0x8C (amount) | below the surface the view is `lerp(scene, C, sat((d - near) / (far - near)))` with `C` the Shallow Color times the Water Multiplier and `d` the exact view-space axis depth rebuilt from the depth buffer, every pixel, the sky included, applied by the depth-of-field image-space effect's fog program. Mode 2 (fully below) uses `ISDepthOfFieldFogged`, mode 1 (crossing) the masked variant. The mode comes from the SURFACE height against the near-plane corner heights (RF-WATER-031, -033, -052, -057). The IMGS tint and saturation have no reader; `DNAM` +0x8C and `fUnderwaterFullDepth` are never read (RF-WATER-031, -042) | `underwater_pass.rs`: one post-pass on the camera over the water bed-depth snapshot, in mode 2 only; the atmospheric distance fog is off while it runs | implemented for mode 2 and the sky; the crossing mode (needs the plane mask) and the surface seen from below (the snapshot holds no water) are open (#698) |
| underwater blur, `INAM` DOF block | the composite blends the frame toward a blurred copy by `sat((d - focus) / range) * strength` (far side only for UnderwaterImageSpace: 1, -50, 500, flags 18), the blur a separable five-tap pair with weights 0.38774, 0.24477, 0.06136 whose depth-edge term keeps up to half the sharp frame (RF-WATER-032, -053, -054) | the five-tap pair and the ramp, with the UnderwaterImageSpace values (RF-WATER-015) for every water and no depth-edge term (its scale depends on the retail camera's near and far planes, which the registry does not give) | implemented in part; which blur feeds the composite is medium (RF-WATER-054), the edge term and per-record values are open (#698) |
| interior cells and `XCLW` | retail never uses `XCLW` for an interior; only placed water counts (RF-WATER-041) | the flat water comes from an exterior LAND height field, so an interior gets none; a test holds it | implemented |
| `FNAM` (1 byte) | bit 3 selects the flow-map program, which ignores the three amplitudes; bit 4 blends normals; bit 0 is dangerous. RiverWaterFlow and DefaultWaterFlow 0x08, the Blend records 0x18, DLC2ApocryphaWater 0x01 (RF-WATER-013, RF-WATER-019, RF-WATER-023). `waters.flags` holds the record header flags, so FNAM is read from the record bytes | decoded (`WaterColors::uses_flow_map`) and drawn (next row) | implemented |
| flow-map program (FNAM bit 3), NAM5 flow normals, per-cell flow map | a 64 by 64 map per cell (`Textures\Water\skyrim.esm\Flow.<x>.<y>`) steers four reads of the tiling flow-normal texture: taps A (0, h), B (h, h), C (0, 0), D (h, 0), rates 10.64 / 9.92 / 8 / 8.48, offsets 0.27 / 0 / 0 / 0.62, scale `64 * sqrt(1.01 - b)`, phase `0.001 * clock * a`, bilinear weights `1 - \|2 * fract(64 * uv) - 1\|`, renormalised, times `max(lerp(1, sat(h / F), 0xD8), 0.4)`; the normal is `lerp(tiled, flow, distT)`; the clock is the game hour times 3600 (RF-WATER-036 to -039, -044 to -047; 618 effective cells) | `flow_normal_xy` in `water.wgsl`; the player's wave settings (waves toggle, speed, strength, scale) apply to it as to the noise path and are neutral at their defaults: strength scales the normal (0 with waves off), speed scales the clock's advance (0 freezes the phase) and scale the ripple frequency, an engine extension since the retail flow program reads no amplitude (RF-WATER-023); each cell binds its own flow map (read from the pack's folder, no conversion) and the maps of its east, south and south-east neighbours, because retail's atlas puts geographic neighbours side by side and its POINT + WRAP sampler lets the taps shifted half a texel east and south read the neighbour's first texel, which makes the result continuous across every cell border (RF-WATER-063; a side with no neighbour map clamps to the cell's own edge texel); the flow map is point-sampled (a filtered direction and phase speed smear the ripples into flat patches); the clock starts at hour 12 and runs at the retail time scale of 20 | implemented; the engine has no game clock, the ripple UV is the cell's mesh UV plus its grid coordinates (RF-WATER-046, RF-WATER-061), and the atlas sampler is POINT with wrap addressing (RF-WATER-063, read from the sampler table builder, not from a runtime capture); the pack's flow maps of cells whose water type is not a flow type are read as neighbours too, where retail's atlas would not hold them; how many there are is unverified (a join of the 641 file coordinates to the Tamriel cell census gives 33, a separate census gives about 199) |
| `sunSpecularPower` (+0x10), `sunSpecularMagnitude` (+0xCC), `sunSparklePower` (+0xE0), `sunSparkleMagnitude` (+0xC8), specular depth factor (+0xDC) | `spec = sat(R.sunDir)^power * magnitude + sat(N.(-0.099,-0.099,0.99))^sparklePower * sparkleMagnitude`, times the sun colour and `dS = lerp(1, sat(h / F), +0xDC)`, added after the mix; no ambient and no other light (RF-WATER-010) | `water.wgsl` sun terms: the four fields per record, the sun colour the weather's sunlight (SkyrimClear day 190, 181, 171), the sun direction the engine's first directional light. The sun intensity `SunDir.w` is not traced and is 1; an interior gets no sun | implemented; `SunDir.w` open (#698) |
| `specularPower`, `specularRadius`, `specularBrightness`, displacement fields (and wading) | point-light specular and the wading ripples (S2); the registry's ripple normal is `((t.zw - 0.5) * DNAM 0x58, 0.04)` with an untraced source (RF-WATER-018) | not used | not implemented: point-light specular and the wading ripples (#698); `refractionMagnitude` is implemented (row above) |

Each UNKNOWN row has an issue. Every number in the water shader is a WATR field, a number from S2, S6 or S7, or an engineering guard (a divisor floor, a bottomless-path value, the clock wrap).

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

For the waters without a flow map this holds: what a record cannot give is the direction of a particular river: every cell that uses one record scrolls the same way, so a river's course comes from which directional record its cell names, as in retail. River cells whose water has FNAM bit 3 and a per-cell flow map take their direction from that map instead (the flow row above). The earlier experiments are kept as findings: the cell water height is the worldspace's own (many neighbouring cells share one value, for example -250, 600 or -5050), the LAND bed slope under the water points a different way in almost every one of 52 retail river and creek cells (Tamriel grids -4..16 by -22..0), and the flat planes carry no flow map. None of them is used.
