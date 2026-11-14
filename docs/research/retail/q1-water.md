# Q1 Water: noise scroll from WATR DNAM wind fields

Binary: `SkyrimSE.exe` SHA-256 `846efccf…1402f`, 1.7.104.0, image base `0x140000000`. Static analysis only (capstone disassembly of an APFS clone). Addresses are VAs.

## Answer (formula)

For each noise layer `i` in 0..2, every frame:

```
theta_i = DNAM.noise_wind_direction[i] (degrees) * 0.017453292   // deg -> rad
d_i     = DNAM.noise_wind_speed[i] * T                             // T = water clock, seconds (double)
scroll_i.u = frac( sin(theta_i) * d_i )                            // wrapped to [0,1): fmodf(x,1), +1 if x<0
scroll_i.v = frac( cos(theta_i) * d_i )
NormalsScroll0 = (scroll_0.u, scroll_0.v, scroll_1.u, scroll_1.v)
NormalsScroll1 = (scroll_2.u, scroll_2.v, <unchanged>, <unchanged>)
```

The shader then samples noise layer `i` at `uv_i = world_xy / DNAM.uv_scale[i] + scroll_i` (NormalsScale.xyz = the three uv scales).

**Angle convention:** compass bearing. 0 deg = +V = world +Y (north), 90 deg = +U = world +X (east), clockwise seen from above. Because the offset is ADDED to the UV, the visible pattern moves the opposite way: toward bearing `theta + 180 deg`. The angle is the direction the wind comes FROM (meteorological convention).

**Units:** `noise_wind_speed` is noise-tile repeats per second (UV units/s). World speed = `speed * uv_scale` units/s (example: DefaultWater layer 3 = 0.096 * 488 = 47 units/s). This is derived, not separately measured.

**RiverWaterFlow names agree** with the "from" convention (pattern moves toward theta+180):
| record | layer dirs (deg) | flow toward (layer 1) | name |
|---|---|---|---|
| RiverWaterFlowNE 0xE717C | 233 / 267 / 252 | 53 (NE) | NE |
| RiverWaterFlowNW 0x1063AA | 141 / 106 / 177 | 321 (NW) | NW |
| RiverWaterFlowSE 0x108AC7 | 340 / 14 / 260 | 160 (SSE) | SE |
| CreekWaterFlowSW 0x106A24 | 33 / 67 / 120 | 213 (SW) | SW |
| CreekWaterFlowSE 0x106A20 | 360 / 309 / 256 | 180 (S) | SE |
The earlier "names disagree" note (#299 `docs/research/water.md`) assumed the angle is the travel direction. With the retail convention they agree. No fifth "RiverWaterFlowSW" record exists in Skyrim.esm/Update.esm.

## Binary evidence

- `0x1403065e0` TESWaterForm vtable slot 6 (Load; vtable `0x14182ac68` from RTTI `.?AVTESWaterForm@@`). On DNAM it copies up to `0xE8` bytes to form `+0xD0`. So DNAM offset `X` is form `+0xD0+X`: wind direction at form `+0x134/0x138/0x13C`, wind speed at `+0x140/0x144/0x148`. Same function: an older short DNAM path writes default uv scale 100/100/100 and amplitude 1.0/0.5/0.25 (form `+0x17C..+0x190`).
- `0x1405290ac`–`0x140529356` (inside the water-system per-water update): for each layer it loads the direction, multiplies by the float constant `0x1418121c8` (= 0.0174533), calls `0x1415a8050`, multiplies both result lanes by the layer speed and then by the double at `0x143204040` (T), and stores the pair at form `+0x44/48`, `+0x54/58`, `+0x64/68`. Each of the six values is then wrapped: `>1` -> `fmodf(x,1)`; `<0` -> `fmodf(x,1)+1` (thunk `0x1415a988e` -> IAT `0x1417c8e58` = `api-ms-win-crt-math` `fmodf`). It packs layer 1+2 into a float4 at material `+0x100` and layer 3 into material `+0x110` lanes x,y (lanes z,w keep material `+0x118/+0x11C`).
- `0x1415a8050` is a sincos helper. Lane 0 = sin(x) (small-angle path returns x). Lane 1 = cos(x) (computed as sin(|x|+pi/2) via constant `0x141b53ad0` = 1.5708, or 1 - k*x^2 for small x). So U gets sin, V gets cos.
- Clock T: `0x140529ab8`–`0x140529acf` add the float at `0x1420d68d4` to the double at `0x143204040` once per frame (guarded by a frame-id compare at object `+0xBC`). `0x1420d68d4` is slot 1 of a 4-float per-frame delta array at `0x1420d68d0` that the main loop fills (`0x1402c65e6`, `0x140658d24`) and also sums into a global running clock at `0x1420d68e0`. Units are seconds by its use as frame delta; whether it is the time-scaled or real delta, and whether it stops in menus, is unverified.
- Shader side (not disassembled from the retail DXBC): Community Shaders `package/Shaders/Water.hlsl` (GPL-3.0, a reconstruction of the retail shader) lines 151–153 and 201–244: `NormalsScroll0` c3, `NormalsScroll1` c4, `NormalsScale` c5; `TexCoord = NormalsScroll + (worldPos.xy + PosAdjust)/NormalsScale`. This is a cross-check, not retail proof. The material `+0x100/+0x110` -> c3/c4 mapping is inferred from layout and from this source.

## Related executable defaults (INI, `[Water]`/`[Display]`)
`fSurfaceTileSize:Water` 2048, `fTileTextureDivisor:Water` 4.75, `iWaterNoiseResolution:Water` 256, `uSurfaceFPS:Water` 12, `fWaterSSRIntensity:Display` 1.3 (this install's Skyrim.ini sets 0.5), `fWaterSSRBlurAmount:Display` 0.3, `fWaterSSRNormalPerturbationScale:Display` 0.05, `fReflectionsIntensityScale:Display` 0.8. Their consumers were not traced.

## Reflection and fresnel inputs
Not traced in the binary in this pass. CS Water.hlsl names the pixel constants `FresnelRI.x` = DNAM fresnel amount, `VarAmounts` = (sun specular power, reflection amount, alpha, refraction magnitude), `ReflectionColor`, `ShallowColor`, `DeepColor`. That indicates they are record fields copied through, not code constants. Confidence: medium, unverified in the binary. The #299 `REFLECTION_SHARE` 0.6 has no retail source.

## Other engines (leads only)
- spud (`src/engine/water.cpp:117-127`, `shaders/water.vert:66-68`): same formula (x = d*sin, y = d*cos, frac, uv = base/scale + offset). It is consistent with the binary, so derived. spud `c[12]=(1.3,0.3)` and `c[13]=0.05` equal the exe defaults of fWaterSSRIntensity, fWaterSSRBlurAmount, fWaterSSRNormalPerturbationScale. spud `0.4` (c[3].y) and `1.2` (c[7].w) have no source found.
- OpenSky: not checked for water.

## Confidence
- Formula, field offsets, sin->U / cos->V, wrap: high (direct instruction trace).
- Clock = accumulated per-frame delta in seconds: medium-high.
- "Pattern moves toward theta+180": medium-high. Depends on the shader adding the offset (CS reconstruction) and on UV x/y = world X/Y. The five directional record names agree.
