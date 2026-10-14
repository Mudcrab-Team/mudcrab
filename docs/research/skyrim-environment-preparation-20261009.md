# Skyrim environment color preparation

The **Skyrim SE 1.7.104.0** executable establishes several previously unresolved
parts of the weather-to-lighting path: normalized byte-color transfer, climate
color transitions, the six-face directional ambient matrix, and its lighting
constant upload. These are static dataflow findings. The complete effective sun
color, active climate/GMST overrides, interior cube selection, and image-space
output still require additional evidence.

The engine applies the recovered ambient stage in the explicitly approximate
`authored-preview` mode. Its fixed timing reference, sun trajectory, interior
selection policy, local-light budget, and photographic display adapter remain
declared approximations. No running retail frame or effective runtime constant
capture was obtained for this investigation.

## Evidence boundary

| Item | Pinned input or verification |
| --- | --- |
| Executable | `SkyrimSE.exe`, AMD64 PE32+, version `1.7.104.0` |
| Executable SHA-256 | `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f` |
| Address convention | Preferred image addresses; image base `0x140000000` |
| Native analysis | Existing REA setup, independent clone of saved partial Ghidra project; read-only queries with analysis disabled |
| Decoder check | 8,415 distinct Ghidra instructions checked against executable bytes; 440 compared independently with LLVM 21.1.8; no mismatch |
| Static record view | `Skyrim.esm` followed by `Update.esm`; excludes other plugins and live state |
| Skyrim.esm SHA-256 | `e198c3b85e5e48e0c92a6580d8f66e644256b68d812ee32b61735cf9b753df73` |
| Update.esm SHA-256 | `b298b0f65fa0127fd13c1f5ef6bb30eaf9d21fd19f5e6f8f91f829c9007d3280` |

The [derived evidence index](skyrim-environment-evidence-20261009.json) contains
addresses, hashes, arithmetic summaries, and verification counts. Executable
bytes and disassembly remain in private research artifacts. Missing references
in the partial saved analysis do not prove that a runtime write is absent.

Source reconstruction helped locate the upload. It is a separate evidence tier:
its older-build addresses were not transplanted into this target. The
[CPU lighting reconstruction][cpu-lighting] loads the ambient transform and
rotates it for model-space inputs; the [shader utility][shader-util] serializes
matrix rows with translation components. The retail addresses below were
recovered independently.

## Weather RGB and weight preparation

`0x140412DF0` loops over the 17 `NAM0` color categories and prepares a packed
four-color input for `0x140414CF0`. The latter blends the RGB byte values with
four supplied weights, then multiplies by `f32(1/255)`:

```text
preparedRGB = sum(weight[k] * byteRGB[k]) * 0.003921568859368563
```

Its lightning branch adds/clamps using weather lightning RGB; the ordinary
branch calls `0x140EF2E80`, which caps components greater than one. No sRGB
decode occurs in these inspected preparation stages. This establishes how
`NAM0` reaches the corresponding `Sky` colors, including its sunlight slot.
It does not establish the final directional-light shader coefficient: light
dimmer, sunlight scale, and other later preparation remain untraced.

`0x140416490` performs the parallel `DALC` preparation. It reads seven packed
colors per time slot from weather offset `0x7F8`, preserves their stored order,
extracts each low RGB byte, scales it by the same `f32(1/255)`, and blends the
provided weights. The first six float RGB values go to `Sky+0x200`; the seventh
goes to the ambient-specular RGB at `Sky+0x248`. It also blends each slot's
final float into `Sky+0x254`, then calls the ambient builder below.

The [weather layout][weather-layout] and [Sky layout][sky-layout] identify the
serialized/runtime field names. These labels support interpretation of the
observed offsets; they do not supply a missing native arithmetic step.

## Six-face ambient matrix

At `0x1414EC3B0`, the executable receives six consecutive float RGB values.
With their stored order named `X+`, `X-`, `Y+`, `Y-`, `Z+`, `Z-`, it constructs:

```text
dx = (Xminus - Xplus) * 0.5
dy = (Yminus - Yplus) * 0.5
dz = (Zminus - Zplus) * 0.5
c  = (Xplus + Xminus + Yplus + Yminus + Zplus + Zminus + G) * f32(1/6)

ambientRGB(N) = dx * N.x + dy * N.y + dz * N.z + c
```

Each symbol is RGB, so this forms three affine rows. The output is the ambient
`NiTransform` at `0x1420D6978`; its scale field is set to one. The builder also
stores the separately supplied ambient-specular RGB and final float. It does
not multiply this diffuse matrix by `NAM0 Ambient`.

`G` is the RGB global at `0x14331D6F0`. It lies in the writable, zero-filled
virtual tail of `.data`. Its initial image value is zero; its live value and
writers remain unresolved. The preview assumes `G=0` and reports that
assumption. Zero-filled storage does not establish an immutable black term.

`0x14154B880` uploads the matrix as three `vec4` rows, using translation RGB
as the fourth components. Its model-space branch multiplies the matrix by the
model-to-world rotation. The [previous retail shader investigation](skyrim-shadow-intensity-20261008.md#ordinary-surface-lighting) establishes the
consumer's three dot products with `float4(N,1)`, added outside direct-light
shadow visibility.

The axis sign matters. A serialized positive-axis face corresponds to the
opposite cardinal input normal in this observed affine stage. Treating the
stored labels as samples at matching surface-normal endpoints would reverse
the gradient. Whether those labels describe incoming light direction is an
interpretation; the arithmetic above is the verified statement.

Engine coordinates use `Creation(x,y,z) -> engine(x,z,-y)`. For a world-space
engine normal, the rows therefore become:

```text
ambientRowsEngine = [ dx, dz, -dy, c ]       # one row per RGB channel
```

An affine function has four coefficients per channel, while six arbitrary
face values provide six constraints. This stage reproduces all six associated
endpoints only when the three opposite-face pair means agree. The preview
reports its maximum endpoint residual and minimum over unit normals; it does
not add a hidden ambient clamp. An isolated bright face can produce negative
ambient values for some normals.

### Shipped clear-weather daytime example

In the two-plugin static view, winning `SkyrimClear` is WTHR `0000081A` from
`Update.esm`, record offset `1229994`. Its daytime cube yields these engine
rows with the recovered transfer/arithmetic and the declared `G=0` assumption:

| Channel | Engine X | Engine Y | Engine Z | Constant |
| --- | ---: | ---: | ---: | ---: |
| R | -0.027451 | 0.223529 | 0.001961 | 0.438562 |
| G | -0.023529 | 0.166667 | 0.019608 | 0.590196 |
| B | -0.043137 | 0.166667 | -0.001961 | 0.635294 |

An upward normal `(0,1,0)` receives approximately
`[0.662092, 0.756863, 0.801961]`; a downward normal `(0,-1,0)` receives
`[0.215033, 0.423529, 0.468627]`. These are derived linear coefficients,
not measured pixel values. The daytime sunlight slot is `[190,181,171]/255`,
while `NAM0 Ambient` is `[180,192,197]/255`; the latter is retained separately
and is not an extra multiplier on the cube.

## Climate color transitions

`0x140416760` produces four combined time/weather weights. Its boundaries
come from the active climate timing bytes and `fDaytimeColorExtension`:

```text
colorBegin = max(0, sunriseBeginByte/6 - extension)
sunriseEnd = sunriseEndByte/6
sunsetBegin = sunsetBeginByte/6
colorEnd = min(24, sunsetEndByte/6 + extension)
```

The dawn and dusk intervals each split at their midpoint. Colors interpolate
Night -> Sunrise -> Day during dawn and Day -> Sunset -> Night during dusk;
Day and Night remain constant between these intervals. The two selected time
weights are multiplied by current-weather percentage and its complement to
produce the four inputs to weather color preparation. The
[climate implementation][climate-source] independently documents the timing
byte's ten-minute unit.

The two-plugin record view gives Tamriel WRLD `0000003C` a `CNAM` reference to
`SkyrimClimate` CLMT `00000812`. Its timing bytes `[33,60,96,123]` mean
sunrise `5.5..10` and sunset `16..20.5`. The native setting object named
`fDaytimeColorExtension`, value address `0x14209E5A0`, has compiled initializer
`0.5`. Its RTTI owner is `GameSettingCollection`: **it is a GMST, not an INI
key**. The two extended intervals consequently give these reference anchors:

| Hour | Color slot |
| --- | --- |
| 0..5 | Night |
| 5..7.5 | Night -> Sunrise |
| 7.5..10 | Sunrise -> Day |
| 10..16 | Day |
| 16..18.5 | Day -> Sunset |
| 18.5..21 | Sunset -> Night |
| 21..24 | Night |

The runtime catalog currently lacks active CLMT/GMST resolution. Preview uses
these shipped/compiled values as an explicit fixed reference policy, reports
both source IDs and unresolved override status, and treats hour 24 as hour 0.
A non-Tamriel worldspace reports a project timing fallback; it is not assigned
SkyrimClimate ownership implicitly. Runtime weather transitions are also not
simulated: preview interpolates the selected weather's own four time slots.

## Preview implementation and remaining gates

The pure resolver in [environment_preview.rs](../../crates/engine/src/environment_preview.rs) returns `InjectedLighting`,
source/provenance reports, optional sky colors, and fog source metadata. It
uses an explicit weather ID when supplied; otherwise it finds `SkyrimClear`
by editor ID in the ordered catalog. Missing explicit weather does not select
another weather silently. Missing inputs use reported fixed coefficients.

Exterior ambient uses all six time-blended `DALC` faces and the recovered
affine stage. Interior ambient currently couples cube selection to the
Ambient Color inheritance bit: template cube when inherited, CELL cube
otherwise. That selection is an approximation, with both raw cubes retained.
Resolved interior directional RGB uses byte/255 as preview policy; rotation
signs and directional fade remain untraced. ShowSky/UseSkyLighting are retained
without a guessed override rule.

The runtime separately selects seven closest enabled spawned lights using
their actual rebased engine positions and effective ranges. Local RGB uses
normalized source bytes and FNAM fade as a named approximation. This camera
budget does not reproduce native per-draw/room selection, reference fade
offsets, or local shadow visibility.

For composition, native preview coefficients are scaled by `12000/pi` and
the view's Bevy exposure. The matching approximate directional light uses
12000 lux times the preview coefficient RGB. The main view uses EV100 `9.7`
and TonyMcMapface. Those values calibrate the preview against the engine's
existing PBR daylight baseline; they are not Skyrim IMGS defaults or evidence
of retail screenshot parity. The existing sky uses its own normalized-color
mixing/display path; fog is a declared exponential approximation to source
distances. PBR terrain uses scalar ambient fill rather than the recovered cube.

The remaining evidence gates are concrete:

- Resolve active WRLD -> CLMT and GMST overrides, then compare captured runtime
  time/weather weights at dawn, day, dusk, and night against this preparation.
- Find writers to additive ambient RGB `0x14331D6F0`; capture its value alongside
  the six prepared faces and uploaded matrix. Check one changing weather and
  one image-space transition before treating the zero assumption as effective.
- Trace final sun color/dimmer/visibility from `Sky` through the selected light
  and shader upload; compare the final coefficients with the prepared NAM0 slot.
- Trace interior cube ownership and angle/fade preparation, including opposite
  CELL/template Ambient inheritance and ShowSky/UseSkyLighting cases.
- Keep IMGS/IMAD, exposure/adaptation, bloom and native fog as separate output
  gates. Do not interpret an attractive preview as validation of those stages.

Focused deterministic tests cover timing endpoints and continuity, actual
shipped clear-day cube orientation, affine residuals, missing-weather behavior,
and interior source ownership. They validate the implementation's arithmetic
and declared policy; GPU and retail scene acceptance remain separate.

[cpu-lighting]: https://github.com/Nukem9/skyrimse-test/blob/328916305165a46c4e4b527735bbcfd46b09a0ca/skyrim64_test/src/patches/TES/BSShader/Shaders/BSLightingShader.cpp
[shader-util]: https://github.com/Nukem9/skyrimse-test/blob/328916305165a46c4e4b527735bbcfd46b09a0ca/skyrim64_test/src/patches/TES/BSShader/BSShaderUtil.cpp
[weather-layout]: https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESWeather.h
[sky-layout]: https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/S/Sky.h
[climate-source]: https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/src/RE/T/TESClimate.cpp
