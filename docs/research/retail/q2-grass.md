# Q2 Grass: wind amplitude and appearance defaults

Status: answered. Wind amplitude and lighting inputs traced (high). The shader-side wind and lighting math was later confirmed in the compiled retail grass shaders.

## Wind amplitude (answer)

```
w      = blended weather wind speed in 0..1   (WTHR DATA wind-speed byte / 255, blended across a weather transition)
A      = fGrassWindMagnitudeMin + (fGrassWindMagnitudeMax - fGrassWindMagnitudeMin) * w     // 5 + 120*w with defaults
shader = min(A, 60.0) * fWindGrassMultiplier:Display                                       // 1.0 default
```

Defaults: `fGrassWindMagnitudeMin:Grass` 5.0 (record `0x142079458`), `fGrassWindMagnitudeMax:Grass` 125.0 (`0x142079440`), `fWindGrassMultiplier:Display` 1.0 (`0x1420d6d28`). Before any weather is applied, `w` holds 0.2 (data initializer at `0x1420d6818`). The 60.0 cap is a hard-coded constant (`0x141b5bec8`).

Evidence:
- `0x140206b21`–`0x140206b39` (grass manager init): copies Min and Max into globals `0x1433d4c3c` and `0x1433d4c40`.
- `0x14150c2d2`–`0x14150c2f2` (and the twin at `0x14150c733`): `A = Min + (Max - Min) * [0x1420d6818]`, stored to `0x1433d4c38`.
- `0x1404122d5`–`0x140412358` (Sky update; twin at `0x140413f45`): reads the byte at TESWeather `+0x664` of the current weather and of the outgoing weather, scales by 1/255 (`0x141b5bdc8`), blends by the sky transition factor (Sky `+0x1b8`), and writes the result to Sky `+0x18c` and to `0x1420d6818`. The `+0x664` byte matching WTHR DATA wind speed is inferred from layout; not proven.
- `0x141522b79`–`0x141522ba9` (grass shader per-draw setup): `min([0x1433d4c38], 60) * fWindGrassMultiplier` -> grass constant `0x1433d4d68`.

## Wind phase (partial)

`0x141522b5f`–`0x141522cdb`: `phase = TotalTime * (1/600) * 2*pi * prop[+0x184]`, stored to `0x1433d4d6c`. `TotalTime` is the running clock `0x1420d68e0` (sum of the per-frame delta, see Q1). `1/600` (`0x14197f108` = 0.0016667) and `2*pi` (`0x141b3f754`) are constants. The meaning of grass shader property `+0x184` is unverified (candidate: a per-grass wave value). The previous phase is kept at prop `+0x188`. Confidence: medium.

## Other grass values in the executable
- `fWaveOffsetRange:Grass` 1.75: copied into each grass type entry `+0x18` at `0x1402afc42` (grass type build). Consumer not traced.
- `fGrassFadeRange` 1000 (+ `fGrassStartFadeDistance` 7000) -> fade end at `0x140206a4e`, clamped to a cell-radius bound.
- `fShadowClampValue:Display` 0.3 is written to a grass constant at `0x141522d44`. Its use in the shader is unverified.

## Grass shader constants (CPU side, `0x141522870`, per draw)

The function fills the grass vertex-shader constant block at `0x1433d4c50`. The layout matches the `PerGeometry` block named in Community Shaders `RunGrass.hlsl` lines 64-86 (GPL-3.0 reconstruction):
| constant | value written by retail code |
|---|---|
| WindVector.xy | weather wind direction transformed into model space and normalized |
| WindVector.z | min(A, 60) * fWindGrassMultiplier (RF-GRASS-002) |
| WindTimer / PreviousWindTimer | phase above; previous kept in prop `+0x188` |
| DirLightDirection | the sun light's direction, negated (sun NiDirectionalLight `+0x140..+0x148`) |
| DirLightColor | the sun light's diffuse colour (`+0x11C..+0x124`), no scale |
| AmbientColor | the sun light's ambient colour (`+0x110..+0x118`), no scale |
| AlphaParam1 / AlphaParam2 | grass fade start / fade range globals (`0x1433d4a08`, `0x1433d4a0c`) |
| ScaleMask | (1,1,1) if a property flag (bit 43 of prop `+0x38`) is set, else (0,0,1) |
| ShadowClampValue | `fShadowClampValue:Display` 0.3 (`0x1420d6d48`) |
When the global flag `0x1420d68f5` is set (no sun available), it writes DirLightColor = 0, AmbientColor = (1,1,1), DirLightDirection = (1,0,0).

No brightness, saturation, sun-strength or ambient-strength multiplier exists on the CPU side. Light colours are passed through unchanged. The pixel shader then scales the final colour by one global output factor, 1 / FramebufferRange (initial value 0.8333333, fallback 1.0, runtime value not confirmed; RF-GRASS-024), which the CPU passes in the fog constant.

Shader side (Community Shaders reconstruction, GPL-3.0, written here as a formula; later confirmed in the compiled retail shader): per vertex, `dirLightAngle = dot(DirLightDirection, instance_normal)` (the instance's normal, not the mesh normal); diffuse factor = instance brightness (InstanceData1.w) * vertex colour; ambient term = instance brightness * AmbientColor * vertex colour; ShadowClampValue is passed to the pixel shader as the floor of the shadow term. Wind displacement: `angle = 0.4 * (WindTimer - (inst.x + inst.y)/128)`, `disp = WindVector.xy * WindVector.z * ((sin(pi*sin a) + sin(2pi*sin a))*0.3 + 0.2*cos(pi*cos a)) * 0.5 * alpha^2`. The compiled retail grass shaders confirm this wind formula and the lighting: the 0.3 is the floor of the shadow term (lerp(0.3, 1, shadow) on the direct light only; the ambient term is not shadowed), and the final colour is multiplied by the global output factor above.

## Verdict for #539 / #541 / #611
- Retail grass light = sun diffuse colour * max(0, N_instance . L) with shadow floored at 0.3, plus the sun's ambient colour, both times instance brightness and vertex colour, and the result times the global output factor 1 / FramebufferRange (initial value 0.8333333, fallback 1.0, runtime value not confirmed; RF-GRASS-024). No tuning multipliers.
- #541 tuning values sun-angle weight 0, sun strength 0.1, ambient strength 1.15, saturation 0.75, brightness 1.15: no retail source (invented by comparison). Shadow strength 0.3 numerically matches fShadowClampValue 0.3; its semantics in #541 must be checked against "shadow floor 0.3".
- Wind: use RF-GRASS-001..004, not tuned values.

## Other engines
- spud `src/render/renderer.cpp:876-878`: `min(5 + 120 * wind_speed, 60)` with comment naming fGrassWindMagnitudeMin/Max and fWindGrassMultiplier. Derived; matches the binary. (spud uses literal 5/120, not the live INI values.)
- spud `shaders/grass.vert:42-46`: timer = TotalTime * 0.0016667 * 2pi * GRAS wave_period, and the CS wave formula. Derived; matches the binary phase formula and identifies prop `+0x184` as the GRAS DATA wave period (lead; not proven in the binary). spud `grass.frag:26`: shadow = s + (1-s)*0.3 (matches fShadowClampValue). spud `grass.frag:28` final `* 0.8333333`: matches the retail output factor 1 / FramebufferRange (derived).
- OpenSky `src/Sources/OpenSkyShaders/GrassShaders.metal:28-34`: bend = h^2 * (72 + 24*sin(2pi*t/wavePeriod)), wind scale clamp 2 (`GrassRendering.swift:20`). Invented; not a source. Lines 203-212: lambert on the instance up axis, `mix(0.3, 1.0, shadow)`, plus one ambient colour, with the comment "the game's grass light". Consistent with retail (0.3 floor, instance normal); no citation given. Metal-native renderer; its output scale differs from retail.
