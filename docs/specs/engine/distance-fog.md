# Skyrim distance fog: parity specification

This document defines the distance-fog behavior Mudcrab must reproduce for the copied Skyrim
Special Edition installation. It follows the path from plugin records through native CPU updates,
shader constants, and the shipped shader bytecode. The target is an exact visual match; the status
of each part is stated below so implementation work does not mistake an assumption for evidence.

The current exponential fog in `crates/engine/src/sky.rs` is an approximation. This specification
supersedes the vanilla-fog claims in [Sky](sky.md). It does not change the renderer.

## 1. Target and evidence

Research date: 2026-10-08. Mudcrab baseline: `f40ca1df770e`. Bevy: `0.19.0`.

| Input | Identity |
|---|---|
| `SkyrimSE.exe` | File/product version `1.7.104.0`; AMD64 PE32+; preferred image base `0x140000000` |
| Executable SHA-256 | `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f` |
| `Skyrim - Shaders.bsa` SHA-256 | `fd1ba630f443353ed21ed4ca0380968f9c7091c8434518ea3cc79bc6d38c18e7` |
| `shadersfx/shaders011.fxp` SHA-256 | `72cfc73eb0938d5949f0b3a8683722a33b671a34b41b750ba138de8d154c5fbc` |
| FXP extent | `67,768,128` bytes; `16,044` DXBC programs; every program's stage and extent validated |
| Plugin package manifest | [80-plugin manifest](../../player-movement-plugin-manifest.csv); SHA-256 `737c4478eecb434d0892e32d27f68b40f8c07e5d9fdbdddb768e9a94e470fb42` |

The copied installation's files match the source on `fiji-desktop`. The package manifest orders
the research inputs; it is **not a verified active `plugins.txt`**. A runnable retail session and
retail frame captures have not been established. Static recovery can specify an operation exactly
without proving that a complete Mudcrab frame matches Skyrim.

Evidence labels used throughout:

- **Record:** observed authored values, with plugin identity and source offset.
- **Native:** an operation recovered from this exact executable; instruction checks take
  precedence over decompiler output and historical header layouts.
- **DXBC:** an operation recovered from the shipped shader program, with its hash and FXP offset.
- **Design:** a Mudcrab implementation choice that preserves the recovered behavior.
- **Capture required:** a remaining runtime binding, pass-order, or image-comparison question.

Raw binaries, extracted shaders, disassembly, and the complete record scan stay outside the
repository. [Reference inputs](../../research/skyrim-distance-fog-reference.json) contain selected
interpreted record values and provenance, without proprietary shader or executable bytes.

The read-only record scan verified all 80 plugin hashes. It found 179 weather record versions,
giving 110 candidate package winners, 10 climates, 133 lighting templates, and 786 interiors.
These counts describe the copied inputs and documented package priority, not a running session's
selected weather or complete environment state.

## 2. Record inputs

### 2.1 Exterior weather: `WTHR`

`FNAM` is a 32-byte little-endian block of eight `f32` values. Offsets are relative to the
subrecord payload, not the record header or runtime object.

| Offset | Field | Meaning |
|---|---|---|
| `0x00` | Day Near | authored start distance |
| `0x04` | Day Far | authored end distance |
| `0x08` | Night Near | authored start distance |
| `0x0C` | Night Far | authored end distance |
| `0x10` | Day Power | distance-curve exponent |
| `0x14` | Night Power | distance-curve exponent |
| `0x18` | Day Max | ceiling on the fog factor |
| `0x1C` | Night Max | ceiling on the fog factor |

`NAM0` has separate Fog Near and Fog Far colors. Each color type occupies 16 bytes, containing
four RGB-plus-fourth-byte entries in sunrise, day, sunset, night order. Their payload offsets are:

| Color | Sunrise | Day | Sunset | Night |
|---|---|---|---|---|
| Fog Near, type index 1 | `0x10` | `0x14` | `0x18` | `0x1C` |
| Fog Far, type index 12 | `0xC0` | `0xC4` | `0xC8` | `0xCC` |

Preserve the authored RGB bytes until the native color preparation stage is applied. The fourth
byte is **not fog opacity**: it is zero in all 880 near/far fog color entries in the 110 candidate
package-winner weather records. Sky Upper, Sky Lower, and Horizon have their own color types at
indices 0, 7, and 8; substituting those colors for Fog Far changes the authored configuration.

These layouts are corroborated by pinned [xEdit definitions][xedit] and
[Mutagen weather definitions][mutagen-weather]. The GPU maximum comes from `FNAM`, independently
of the record color's fourth byte.

`WTHR.DATA` byte `0x03` is Trans Delta. Mutagen's storage conversion maps the byte to
`byte / 1020`, in the range `0..0.25`; this conversion alone does not establish a transition
duration or update rate. Byte `0x0B` holds weather-classification and aurora flags. Preserve these
inputs for the native weather resolver rather than deriving fog distances from the weather name.

### 2.2 Interiors: `CELL` and `LGTM`

Interior distance-fog inputs do not require an exterior sky. Read `CELL.XCLL`, its `LTMP`
lighting-template link, and `LGTM.DATA`. Resolve each inherited field independently. A single
"use template fog" boolean loses valid authored combinations.

| Payload offset | Field | `XCLL` inheritance bit |
|---|---|---|
| `0x08` | Fog Near color, four bytes | `0x00000004` |
| `0x0C` | Fog Near, `f32` | `0x00000008` |
| `0x10` | Fog Far, `f32` | `0x00000010` |
| `0x20` | Fog Clip Distance, `f32` | `0x00000080` |
| `0x24` | Fog Power, `f32` | `0x00000100` |
| `0x48` | Fog Far color, four bytes | `0x00000004` |
| `0x4C` | Fog Max, `f32` | `0x00000200` |
| `0x58` | CELL inheritance mask; reserved on LGTM | — |

A set inheritance bit selects that field from a non-null lighting template. Without a template,
the CELL field remains the source. Both colors share bit `0x04`; there is no independent far-color
inheritance bit. Native CELL near/far getters return zero without interior data, while power/max
getters return one. Resolve those getter values before applying the native interior sanitization.

The 92-byte modern lighting payload and the older 72/64-byte payloads must be parsed by size
and version. All 786 scanned interior CELL definitions have 92-byte XCLL payloads. The candidate
package winners include 128 modern templates, four 72-byte templates, and one 64-byte template.

The exact native LGTM initializer at VA `0x1402955D0` (RVA `0x2955D0`, executable file offset
`0x2949D0`) zeroes the 96-byte runtime lighting block, then sets Fog Max to `1.0`. Load at VA
`0x140295990` (RVA `0x295990`, file offset `0x294D90`) copies DATA into that block. Its generic
reader at VA `0x1401E37D0` copies `min(payload_length, 96)` bytes and leaves the remaining target
bytes untouched for LGTM. Consequently, a short payload retains max `1.0`. After loading, a zero
far-color word copies the near-color word. This fallback applies to missing far color **and to an
explicitly authored all-zero far color**. Parse into these defaults rather than zero-filling the
missing tail. The four 72-byte legacy templates and the 64-byte WindhelmLightingTemplate all
exercise the missing far-color/max case.

Preserve `Fog Max` values above one: `DLC1VolkhairCastleCloseDarkLT` authors `1000.0`. Any later
clamp belongs to the recovered native/shader path, not the record importer.

Fog Clip Distance is a separate input. Do not substitute it for Fog Far or use it as a distance
curve exponent. Its culling/projection consumer must be resolved separately from fog shading.

The cell's `ShowSky` and `UseSkyLighting` flags affect source selection. The reference set has
504 interiors with neither flag, 281 with `ShowSky` alone, and one with both. Their DATA bits are
`0x80` and `0x100`, respectively. `CELL.XCCM` refers
to a **`REGN`**, not a `CLMT`; that region can supply weather for an interior sky.

### 2.3 Climate, regions, and worldspace

Preserve these links and flags when converting records:

| Input | Role |
|---|---|
| `WRLD.CNAM` | climate link |
| `WRLD.WNAM` and `PNAM` parent flags | parent worldspace; `0x10` inherits its climate, `0x40` its sky cell |
| `WRLD.DATA` | worldspace flags, including `0x20` No Sky |
| `WRLD.LTMP` | interior lighting-template link |
| `CLMT.TNAM` | sunrise begin/end, sunset begin/end, volatility and moon/phase data |
| `CLMT.WLST` | weather candidates and weights |
| `CELL.XCLR` | exterior region list |
| `CELL.XCCM` | region supplying an interior sky/weather |
| `REGN` weather entries and areas | weather choices, weights, spatial priority and boundaries |
| `WTHR.IMSP` and `HNAM` | four time-key links to image spaces and volumetric lighting |

The first four climate timing bytes represent ten-minute units (`byte / 6` hours).
`SkyrimClimate` (`Skyrim.esm:000812`) stores `[33, 60, 96, 123]`, giving `5.5, 10.0, 16.0, 20.5`
hours. The scalar day/night blend and four-key color blend are separate native operations.

`CLMT.WLST` and `REGN.RDWT` entries each occupy 12 bytes: WTHR FormID, 32-bit chance field,
and optional GLOB FormID. Preserve the chance bits; pinned xEdit and Mutagen differ on REGN
chance signedness, while the observed vanilla weights used here are positive. A REGN `RDAT`
header occupies eight bytes: 32-bit data type, flags byte, priority byte, and two remaining bytes.
Weather data has type 3. `RPLI` stores edge falloff and `RPLD` stores polygon points. These inputs
must remain available to spatial resolution even when a CELL already supplies an XCLR region list.

Riverwood exterior cell `4,-12` and the Sleeping Giant Inn link to `WeatherPineForest`
(`Skyrim.esm:02A72D`, package winner from `Dawnguard.esm`). Its weather weights are Cloudy 35,
Clear 35, Fog 10, OvercastRain 5, StormRain 5, Clear_A 5, and Cloudy_A 5. These are selection
weights, not simultaneous fog-color weights. Region overlap and weather progression require
their native rules or a recorded runtime state; the record list alone does not determine them.

## 3. Reference authored configurations

The following candidate package winners are in `Update.esm`. Their record identities remain
relative to `Skyrim.esm`. Values are rounded for display; the reference JSON retains the decoded
binary32 values. These are authored inputs, before native interpolation or sanitization.

| Weather | Form identity | Day Near/Far | Night Near/Far | Day/Night Power | Day/Night Max |
|---|---|---|---|---|---|
| SkyrimClear | `00081A` | `0 / 80000` | `0 / 40000` | `0.4 / 0.4` | `0.85 / 0.85` |
| SkyrimCloudy | `012F89` | `0 / 100000` | `0 / 50000` | `0.4 / 0.3` | `0.875 / 0.875` |
| SkyrimFog | `0C821E` | `0 / 25000` | `0 / 15000` | `0.4 / 0.375` | `0.95 / 0.95` |
| SkyrimOvercastRain | `0C821F` | `0 / 15000` | `0 / 15000` | `0.5 / 0.5` | `0.9 / 0.925` |
| SkyrimStormRain | `0C8220` | `0 / 25000` | `0 / 10000` | `0.45 / 0.45` | `0.92 / 0.92` |
| SkyrimOvercastSnow | `04D7FB` | `0 / 50000` | `0 / 35000` | `0.3 / 0.3` | `0.9 / 0.95` |

For Clear, Fog Near/Far RGB at day are `(35,98,124) / (116,168,203)`, and at night
`(24,45,61) / (8,28,33)`. The near color can be brighter than the far color at night. A renderer
must interpolate the actual two colors, rather than assuming a brighter far color.

Sleeping Giant Inn (`Skyrim.esm:0133C6`) is the inheritance fixture:

- Cell-authored Near/Far: `0 / 4000`; Power/Max: `1 / 1`; inheritance mask `0x9F`.
- `FarmLightingTemplate` (`Skyrim.esm:0A1196`) Near/Far: `100 / 4000`.
- Resolved Near/Far: `100 / 4000`; both colors: `(31,52,46)` from the template.
- Power/Max remain `1 / 1` from the cell; Clip Distance `0` comes from the template.

Replacing the whole cell fog block with the template, or disabling interior fog, fails this case.

HelgenKeep01 (`Skyrim.esm:05DE24`) tests the power/max inheritance bits. Its cell-authored
Power/Max are `0.9 / 1`, but mask `0x3FF` selects HelgenKeepSunlightTemplate's `0.7 / 0.5`.
Thirteen FX weather definitions instead author Day/Night Near/Far as `0 / 0`; they exercise the
native zero-range path and must not be replaced with an arbitrary positive range.

## 4. Native CPU resolution

### 4.1 State and precedence

**Native:** `Sky`'s scalar updater at VA `0x140413A30` writes Near, Far, Power and Max at
runtime offsets `+0x194`, `+0x198`, `+0x1A8`, `+0x1AC`. Its control flow is stateful:

| State | Scalar source |
|---|---|
| Mode `3`, current weather exists | room-template transition if either room template exists; otherwise exterior weather |
| Mode `1` or `2` | room-template transition if either exists; otherwise current cell, then exterior lighting override, then retained Sky values |
| Other modes, or mode `3` without current weather | Near `163840`, Far `163841`, Power `1`, Max `1` |

The current-room and previous-room templates are looked up through separate handles. An absent
endpoint can fall back to the current cell, then Sky's exterior lighting override, then retained
Sky state. Do not replace a missing endpoint with an invented clear-weather preset.

**Native:** mode construction at VA `0x1401A3F40` leaves existing mode `0` unchanged. Otherwise
it maps a null/exterior cell to mode `3`.
Interior cells select mode `1` without `ShowSky`, mode `2` with `ShowSky` but without
`UseSkyLighting`, and mode `3` with both. Only a resulting mode `3` can be overridden by a
current-worldspace lighting template: it selects mode `1` if `WRLD.DATA.NoSky` (`0x20`) is
set, otherwise mode `2`, and installs that template as the exterior lighting override. The
predicate is verified at VA `0x14030DD90`; NoSky also controls the hide-sky flag without a
template. Sleeping Giant Inn is mode `2`;
its visible interior sky does not remove cell/template fog.

### 4.2 Time and weather scalar blending

Let `h` be the game hour and `e` the effective `fDaytimeColorExtension` (initialized default
`0.5`). The native interval endpoints are:

```text
a = max(0, sunriseBeginHours - e)
b = sunriseEndHours
c = sunsetBeginHours
d = min(f32(23.99), sunsetEndHours + e)

dayWeight = (h-a)/(b-a)       when a < h < b
dayWeight = 1                when b <= h <= c
dayWeight = (d-h)/(d-c)       when c < h < d
dayWeight = 0                otherwise
```

Climate byte-to-hour multiplication uses `f32` bits `0x3E2AAAAB`, not an independently rounded
binary64 division. With SkyrimClimate and the initialized extension, the fog intervals are
`5–10` and `16–21`, rather than the raw climate's `5.5–10` and `16–20.5`.

For each scalar `X` in Near/Far/Power/Max:

```text
currentX = dayWeight * current.dayX + (1-dayWeight) * current.nightX
lastX    = dayWeight * last.dayX    + (1-dayWeight) * last.nightX
resolvedX = currentX * weatherProgress + lastX * (1-weatherProgress)
```

With no last weather, use current alone. The native code performs scalar binary32 multiply/add
operations. There is no smoothstep or fog-specific exponential weather blend. Exterior values
do not pass through the interior distance sanitizer below. Preserve branch order for malformed
climate intervals; sorting the authored times would introduce different behavior.

Weather progress is updated at VA `0x140412810`, with duration arithmetic at
`0x140412CAD..0x140412D71`:

```text
elapsed = h - startHour + (startHour > h ? 24 : 0)
duration = fWeatherTransMin + (current.TransDeltaByte / 255) *
                              (fWeatherTransMax - fWeatherTransMin)
progress = elapsed / duration
```

Initialized defaults are Min `0.01f`, Max `0.25f`, Accel `4`. These are GMST collection settings,
not INI keys. The copied plugins override Accel to `16.0` through GMST `Skyrim.esm:0B93D4`;
no later override was found in the 80-plugin package scan. No current weather gives progress
`0`; no last weather gives `1`. Under acceleration flag `0x8`, native code replaces progress
with `saved + (progress-saved)*(1+Accel)`. When progress exceeds one, it clears last weather and
the acceleration flag, sets progress to one, and clears the saved scalar. Weather selection,
forced changes and start-hour reset remain inputs to this fog resolver; a region's chance list
does not specify the currently active pair.

### 4.3 Four-key color blending

**Native:** the time-key helper at VA `0x140416760` uses the same extended `a,d` and unextended
`b,c`, but splits the sunrise and sunset intervals at their midpoints:

| Interval | Color interpolation |
|---|---|
| `a → (a+b)/2` | Night → Sunrise |
| `(a+b)/2 → b` | Sunrise → Day |
| `b → c` | Day only |
| `c → (c+d)/2` | Day → Sunset |
| `(c+d)/2 → d` | Sunset → Night |
| Outside | Night only |

For the initialized SkyrimClimate, the color midpoints are `7.5` and `18.5`. At hour `8`, scalar
day weight is `0.6`, while color weights are Sunrise `0.8`, Day `0.2`. Reusing scalar day weight
for colors therefore fails even in a stable weather.

The helper selects key indices `A,B` and key-A weight `w`, then combines time and weather into
four weights, in this order:

```text
w*p, (1-w)*p, w*(1-p), (1-w)*(1-p)
```

The first pair multiplies current-weather RGB bytes at `A,B`; the second pair multiplies
last-weather bytes at the same keys. With no last weather, all weight belongs to current.
The color helper at VA `0x140414CF0` adds four byte×weight products per channel, then multiplies
by `f32(1/255)` with bits `0x3B808081`. It clips channels above one, without a lower clamp in
the reviewed no-lightning fog calls. It applies **no sRGB decode**. Interior/template getters
also use byte×the same reciprocal-255 constant without gamma conversion.

Do not convert the individual weather keys to Bevy `Color::srgb` before mixing. That changes
both interpolation and uploaded RGB. The framebuffer's actual color domain is a separate
rendering contract in §5.6, rather than a reason to alter the recovered color preparation.

### 4.4 Interior and room transitions

Resolve CELL inheritance first, then sanitize each room endpoint's distances:

```text
if not (0 < Far <= 163840): Far = 163840
if not (0 < Near <= Far): Near = Far * f32_from_bits(0x3E2E147C)
```

The second constant is `0.17000001668930054`. Unordered/NaN comparisons take the fallback
in the native comparison path. Near==Far is accepted here. Power and Max are copied without
a `0..1` clamp. Apply this sanitation only to the recovered interior/template paths.

With room factor `q>0`, resolve and sanitize both endpoints, then compute each scalar as
`previous + (current-previous)*q`. Color endpoint resolution follows the same fallbacks and
lerps without a gamma transform. Do not add a clamp of `q` that the reviewed updater lacks.

For `q<=0`, scalar resolution selects current template if present, otherwise previous. The
color helper at VA `0x140415D50` instead resolves the current template through the fallback
resolver. Retain this asymmetry, including the previous-only case.

### 4.5 Scenegraph fog property and final remap

`Atmosphere::Update` at VA `0x14040B950` transfers Sky values to `BSFogProperty`. Its active bit
is set iff `Far>Near`. Colors are copied on every reviewed update; scalars are copied under an
atmosphere update flag. `BSFogProperty` offsets are Near `+0x50`, Far `+0x54`, Power `+0x84`,
Max `+0x88`, Near RGB `+0x38`, Far RGB `+0x44`. Constructor defaults are Near `27852.796875`,
Far `163840`, Power/Max `1`. They are replaced when the resolved environment updates.

Fog is selected per scenegraph through getter VA `0x1414EC040`. The world global alone does
not establish offscreen or first-person fog ownership. The reviewed geometry and image-space
packers do not consult the property's active bit; do not use that bit as their only enable gate.

A final scalar-update branch at VA `0x140413E86` checks internal byte `0x1420B3368`, whose
initialized value is one. If it is zero, native code queries the world-root virtual far getter
and applies:

```text
oldFar = Far; oldNear = Near
cameraNearSetting = fNearDistance:Display
C = scenegraphFar
ratio = (oldFar-C)/(oldFar-cameraNearSetting)
Near = min(oldNear-ratio*(oldNear-cameraNearSetting), C)
Far = C
```

This byte is **not** `bDo30VFog` or `bFogEnabled`. Its runtime owner is unresolved. Keep it as
an explicit recovered internal condition; do not bind it to a guessed INI key. The verified
virtual getter is VA `0x140665740`. It can use graph overrides, profile settings, and the
interior Fog Clip Distance with per-field template inheritance. Fog Clip therefore affects
camera/projection state separately from the fog Far scalar. Capture the actual frustum planes.

### 4.6 Controls and their proof limits

| Control | Initialized value | Established behavior |
|---|---|---|
| `fDaytimeColorExtension` | `0.5` | extended color/scalar intervals |
| `fWeatherTransMin` / `Max` / `Accel` (GMST) | `0.01 / 0.25 / 4` | duration/progress equations; copied package overrides Accel to `16` |
| `fNearDistance:Display` | `15` | camera near-plane input and optional fog remap |
| `bSAOApplyFog:Display` | `1` | gates the image-space fog variant |
| `bSAOEnable:Display` | `1` | chooses SAO+fog versus fog-only composite |
| `bFogEnabled:Weather` | `1` | registration/default proven; direct effect still unresolved |
| `bDo30VFog:Display` | `1` | registration/default proven; direct effect still unresolved |
| `fLightingOutputColourClampPostLit:General` | `1` | Lighting diffuse clamp |
| `fLightingOutputColourClampPostEnv:General` | `1` | environment-light clamp in source shading |
| `fLightingOutputColourClampPostSpec:General` | `1` | Lighting specular-stage clamp |

The image-space owner at VA `0x141541F40` chooses a fog variant when the selected property is
non-null and the copied `bSAOApplyFog` flag is nonzero. It does not inspect property active.
These initialized values are not a captured effective user profile. GMST override priority, INI
layering, runtime overrides and direct readers must be retained in provenance. The four GMSTs
are independently identified by `SettingT<GameSettingCollection>` vtable `0x1417E0360`, distinct
from the INI collections. The transition-acceleration override has DATA file offset `0xEF9E`
in `Skyrim.esm`, binary32 bits `0x41800000`.

The three Lighting clamp INIs are copied by shader initialization at VA `0x141547454` and
SetupTechnique at `0x141548633` into the pixel-shader color-clamp vector. They are rendering
controls, rather than weather fog fields. Reciprocal framebuffer range initializes to
`0.8333333134651184` (approximately `1/1.2`). A console writer at VA `0x14037AC30` stores
`1/value`; another rendering scope temporarily sets it to one and restores it. No named
framebuffer-range INI key has been established. Capture the effective value per pass.

## 5. Shipped shader contract

### 5.1 Constant buffers

**Native + DXBC:** the ordinary parameter vector is prepared with binary32 arithmetic:

```text
invRange = 1 / (Far-Near)
FogParam = (Near*invRange, invRange, Power, Max)
```

For ordered finite inputs, image-space fog substitutes `(5000000, f32(0.1), 1, 0)` for
**any Near==Far**. Lighting geometry substitutes it only when **both Near and Far are zero**.
The native guards use `UCOMISS → JNZ`, without a separate unordered branch. Define
`unorderedOrEqual(a,b)` as `isNaN(a) || isNaN(b) || a==b`: image-space uses
`unorderedOrEqual(Near,Far)`, while Lighting uses `unorderedOrEqual(Near,0) &&
unorderedOrEqual(Far,0)`. This differs from ordinary language equality on NaNs. Lighting has
no equal-nonzero epsilon guard. Keep these contracts, rather than adding a generic degenerate
range or nonpositive-power fallback.

If the Lighting packer receives a null selected property, it returns without writing fog
constants, retaining their prior contents. The image-space owner instead selects a non-fog
composite when its property is null. Null-property handling is therefore a separate state
transition, not an instruction to upload the equal-range zero-fog vector.

**Native, independently decoded with LLVM:** Effect SetupTechnique at VA `0x141556840`
also skips fog writes for a null property. With ordered finite inputs, it uses the special
vector only when both Near/Far are zero; its unordered comparisons match the Lighting guard
above. Equal-nonzero ranges remain unguarded. Its fog parameters are
VS `b0.c0`, Near RGB/k `b0.c1`, and Far RGB `b0.c2`; FarColor.w is not initialized by this
upload. The vertex shader passes `k` in `TEXCOORD5.x` to the pixel shader.

Water SetupMaterial at VA `0x14154D700` divides by `Far-Near` without a zero/equality fallback.
Its Near W comes from WaterMaterial `+0x160`, rather than framebuffer scale. The shader's
`k` is uploaded separately in Water SetupTechnique to PS `b0.c1.w`. The reviewed function
contains a fog pointer test around some writes but later dereferences its Max field; a supported
null Water property is not established. Require a valid property for the recovered Water path;
do not turn a partial pointer guard into an invented null or zero-range behavior.

| Path | Range/power/max | Near color | Far color | Camera planes |
|---|---|---|---|---|
| Image-space | `b2.c0` | `b2.c1=(NearRGB,k)` | `b2.c2=(FarRGB,k)` | `b2.c3=(cameraNear,cameraFar,0,0)` |
| Lighting VS | `b0.c1` | `b0.c2=(NearRGB,k)` | `b0.c3.xyz=FarRGB`; W unspecified | supplied by the geometry projection |
| Lighting PS | interpolated fog | `b0.c0.w=k` | in vertex varying | projection handled in VS |

`k` is reciprocal framebuffer range, copied from native render global `0x1420D694C`.
It is independent of `FogMax` and authored color alpha. The current native geometry packer does
not initialize FarColor.w before copying that vector; the decoded fog equation reads only its
RGB. Do not treat an older source's explicit zero W as evidence for this image. Geometry packer
VA `0x14154C010` and
image-space packer VA `0x14154300A` corroborate these values. The full-screen setter binds a
176-byte parameter structure at constant-buffer slot 2; the fog block occupies its first
64 bytes. Camera near/far are read from the active camera frustum at `+0x160/+0x164`, not
from `WTHR.FNAM`.

### 5.2 Opaque image-space fog

Let `z` be sampled forward D3D depth in `[0,1]`, `N,F` the camera planes, `S,E` fog Near/Far,
`P` Power, and `M` Max. `Csrc` is the composed scene input to this pass.

```text
zb = f32(1.01) * z - f32(0.01)
q  = 2 * zb - 1
distance = 2*N*F / (N+F - q*(F-N))

t = saturate(distance*FogParam.y - FogParam.x)
a = min(M, exp2(P*log2(t)))
Cfog = NearRGB + a*(FarRGB-NearRGB)

if z < f32(0.999998987):
    C = k * (Csrc + a*(Cfog-Csrc))
else:
    C = Csrc
```

`distance` is axial camera depth with the shader's depth bias. There is no screen-ray radial
correction, height dependence, density volume or sun-angle term in this fog block. Adding
height fog or physical scattering changes this distance-fog contract.

For ordinary finite D3D projection, substituting the depth equation gives the algebraic form
`viewZ / (1.01 - 0.01*viewZ/F)`. This is useful for translating Bevy reverse-Z; it is not a
license to change binary32 constants or operation ordering. The original depth equation is
the rounding reference.

The nested RGB blend expands, before `k`, to:

```text
(1-a)*Csrc + a*(1-a)*NearRGB + a*a*FarRGB
```

A capped factor below one leaves both some source color and some near fog color. Max `0.85`
does not mean replacing the scene by `0.85*FarRGB`. Fog color at the plateau remains
`mix(NearRGB,FarRGB,0.85)`.

Depth at or above the sentinel bypasses distance fog and framebuffer scaling in this block.
With the snow diagnostic/contribution controls disabled, the final shader outputs
`saturate(C)` in RGB and the source alpha subject to saturation. Preserve that output clamp.
Do not append a second exponential fog or an extra color alpha multiplier.

### 5.3 Composite inputs and sampling

The identified shaders are `ISSAOCompositeFog` and `ISSAOCompositeSAOFog`:

| Resource | Role in the inspected composite |
|---|---|
| `t0/s0` | scene source color |
| `t1/s1` | optional AO |
| `t2/s2` | depth |
| `t3/s3` | normal/SSR mask |
| `t4/s4` | snow/specular contribution |
| `t5/s5` | shadow mask |
| `t6/s6` | SSR source |

SSR and snow contribute to `Csrc` before distance fog. In the SAO variant, AO multiplies the
composed source RGB before fog, leaving the added fog contribution unoccluded. Final snow-only
suppression and snow sparkles occur after the fog block, before saturating output. The inspected
fog shader does not explicitly sample volumetric lighting; any volumetric contribution already
present in `t0` requires render-order evidence.

UV is scaled by `b12.c43.xy`, lower-clamped to zero, and upper-bounded by `b12.c44.z` for U and
`b12.c43.y` for V before source/depth/auxiliary sampling. The equivalent viewport/dynamic-
resolution behavior and retail sampler filters must be matched, rather than sampling a
neighboring depth texel or padding outside the active viewport. A capture must establish the
actual SRVs, formats and sampler state.

### 5.4 Lighting geometry fog

Every one of the 127 Lighting VS variants has the range/power fog calculation. For ordinary
variant `00000000`:

```text
clip = RetailViewProjection * World * float4(localPosition,1)
dVS = length(clip.xyz)             // before perspective divide
aVS = min(M, exp2(P*log2(saturate(dVS*invRange-S*invRange))))
vertexFog = float4(mix(NearRGB,FarRGB,aVS), aVS)
```

The fog varying (`COLOR1`) is interpolated with ordinary perspective-correct `linear`
semantics, not `noperspective` or per-fragment distance recomputation. Retain the exact matrix
branch for skinned, instanced and other variants. The inspected matrix operations carry precise
intermediates. This metric is **not world-space camera distance** and **not axial opaque fog
distance**. Reusing Bevy's reverse-Z `clip.xyz` changes the metric; produce the equivalent
retail forward projection for this calculation.

All 6,924 Lighting PS variants reference fog scale and two shared flags. Fog is coupled to
post-lighting clamps. Define:

```text
V = interpolated vertexFog.rgb
a = interpolated vertexFog.a
k = b0.c0.w
f = b12.c42.y = (renderFlags & 0x2) ? 0 : 1
g = b12.c42.z = (renderFlags & 0x4) ? 1 : 0
D(C) = C - k * (C + a*(V-C))
```

With diffuse `L`, diffuse clamp `Klit`, and no post-specular stage:

```text
Ld = min(L, Klit + f*D(L))
outputRGB = Ld - g*f*D(L)
```

With specular/ambient/eye contribution `S` and post-specular clamp `Kspec`:

```text
Ld = min(L, Klit + f*D(L))
C = Ld + S
outputRGB = min(C, Kspec + f*D(C)) - g*f*D(C)
```

Clamps and min operations act per RGB channel. There are 1,716 diffuse-only clamp variants and
5,208 with the post-specular stage. The latter exactly matches descriptors with Specular
`0x200`, Ambient Specular `0x20000`, or Eye type `14` in the inspected archive.

The shared-buffer updater at VA `0x14100AE80` writes these exact flag-derived values when
its update-enabled byte `0x143336400` is set; otherwise it retains previous buffer contents.
The
legacy first-person/alpha labels describe rendering bits, not a direct test of player camera
state. The verified first-person body draw uses render flags `3` (`f=0,g=0`). Normal world opaque
draws use `0x41 | (boolean<<4)`, giving `f=1,g=0`; designated group-7/alpha finishing ORs
bit `0x4`, giving `f=1,g=1`. Individual special/offscreen material routes still require their
owner/capture. Clamp registers are `b0.c1.x=Klit` and `b0.c1.z=Kspec`.
Shared `b12.c42.x` is `1/fGamma:Display`, a separate source-shading/display input.
Do not reduce the equations to a simple fog-enable lerp before assigning the real flags.
Large clamps or particular flags can make simplified equations appear correct in one image
while failing on bright surfaces or another pass.

### 5.5 Other material families

| Family | Recovered fog behavior | Implementation consequence |
|---|---|---|
| Effect, ordinary `00000000` | clip-XYZ vertex fog; PS blends toward interpolated fog RGB and applies interpolated framebuffer scale | use its vertex varying and blend mode |
| Effect, additive `00000400` | RGB attenuated by `1-a`; no fog-color addition | adding fog color would make additive particles glow through haze |
| Effect, multiplicative `00000801` | RGB mixes toward white with `saturate(1.5*a)`; alpha preserved; no framebuffer scale | white is its blend-neutral fog target |
| Effect, MultBlendDecal `00400042` | same white fog, then RGB multiplied by alpha | preserve operation and blend order |
| Water, ordinary `00000000` | clip-XYZ fog with per-vertex color/factor; PS water color blend and saturating RGB scale | retain water-specific opacity/refraction behavior |
| DistantTree, RunGrass | no range/log/exp fog in all inspected VS variants | full-screen ownership requires depth/pass proof |
| Particle | no range/log/exp fog in all six inspected VS variants | do not assign a generic geometry fog shader by name |
| Sky | no range/log/exp fog in all nine inspected VS variants | preserve sky composition and opaque clear-depth bypass |

Effect has fog in 342 of 606 VS variants; 264 omit it. Water has it in 2,168 of 2,172 VS
variants; four special variants omit it. Treat descriptor flags as the routing key; a blanket
"all transparent draws get fog" rule fails these differences. Standard Water bindings are
range in `b1.c0.xy`, exponent `b1.c5.w`, Near RGB `b1.c1`, Far RGB/Max `b1.c2`.

Effect SkyObject variant `01000043` retains vertex fog. It sends clip position
`(clipX,clipY,clipW,clipW)` and uses `length(clipX,clipY,clipW)` for fog. This is distinct from
the BSSky family: a sky-tagged effect must not inherit the atmosphere dome's fog bypass.
Effect MotionVectorsNormals variants omit fog.

Ordinary/additive Effect representatives do not saturate their fogged RGB; their distance fog
leaves the separately computed alpha unchanged. Multiplicative and mult-decal representatives
also omit output saturation and framebuffer scale. Water PS `00000000` saturates scaled RGB
and writes alpha zero; this one variant's alpha must not be generalized to all Water programs.

Across Water PS variants, 2,064 consume the interpolated atmospheric fog and write alpha zero.
Another 52 underwater-type variants (type `8`, descriptor `0x4000`) write alpha zero but ignore
that atmospheric varying. A further 52 light-pass variants ignore it and write alpha one
(light indices `1–7`, for example technique `00000800`). All 2,168 non-motion-vector variants
saturate scaled RGB. Four motion-vector variants are separate. A fog calculation in the paired
VS therefore does not prove the PS uses it.

Underwater absorption, water depth colors, sky/cloud coloration, particle blending and volumetric
lighting have separate algorithms. They are scene inputs or later compositions at fog boundaries,
not substitutes for the recovered distance curve. Their pass ownership must still be matched
for an exact final frame.

### 5.6 Color and precision contract

The recovered CPU uploads normalized byte colors, without sRGB decode. The fog shader blends
in its sampled render-target domain, without an adjacent gamma conversion. It also applies
framebuffer scaling and an output clamp. These facts do not prove a physically linear domain.

**Design:** use an explicit Skyrim working color domain for this pass, with conversions at
established input/output boundaries. Record the source target's format, SRV/RTV sRGB flags,
effective framebuffer range, exposure and image-space state. Do not apply Bevy's automatic
`Color::srgb().to_linear()` or tone mapping inside the recovered fog operation.

The existing [color pipeline](color-pipeline.md) uses Bevy exposure and TonyMcMapface as a
diagnostic baseline; it is not Skyrim image-space parity. A numerically correct fog curve can
still look wrong if scene RGB and fog RGB use different domains or if the final display
transform differs. Image-space parity and source lighting are required boundaries for the
user's exact appearance target.

CPU scalar work uses `f32` with the observed operation order. GPU SM5 `log2`/`exp2`, fused
operations, interpolation precision and depth quantization need measured tolerances on the
target backend. Matching mathematical formulas does not establish cross-GPU bit identity.

For invalid-value compatibility, preserve SM5 instruction semantics: `_sat(NaN)` yields zero,
and `min` with one NaN operand returns the other operand. A general-purpose `pow`, or a backend
minimum with different NaN rules, need not match the `log2 → multiply → exp2 → min` sequence
at zero/nonpositive powers or invalid ranges. See Microsoft's [saturate reference][sm-saturate]
and [SM4/5 minimum instruction][sm-min]. The analytical fixtures cover ordinary positive powers;
GPU edge fixtures must establish any wider supported input contract.

### 5.7 Output depth bias for LOD and water

Of the 127 Lighting VS programs, 123 measure the clip XYZ that they also send to `SV_POSITION`.
The four LOD-land variants `09000004`, `09040004`, `12000004`, `12040004` instead measure the
unbiased clip XYZ, then alter the output clip Z:

```text
outputClipZ = clipZ + 0.5 * clamp((clipZ-70000)*0.0001, 0, 1)
```

Water VS `00000000` has the same output-depth bias. Their geometry fog uses the original
metric while opaque fog samples the resulting biased depth. Preserve that distinction in
far-terrain and water comparisons. Reconstructing an unmodified geometric viewZ in the
full-screen pass loses a retail operation even if the power curve matches.

## 6. Mudcrab integration requirements

### 6.1 Data ownership

Keep authored data, resolved scene state, and GPU constants as separate types. Each resolved
field must retain its source identity for diagnostics. Proposed ownership:

| Owner | Required work |
|---|---|
| `crates/converter/src/esm/` | parse fog/weather/climate/template fields; preserve links, flags, payload versions and override provenance |
| `crates/converter/src/esm/exporter.rs` | add typed environment rows while retaining raw records and compatibility with older databases |
| `crates/engine/src/world/database.rs` | load winning weather, climate, region, cell and template data |
| new environment resolver | resolve per-field inheritance, time keys, weather/room transitions and native settings |
| new fog module | prepare constants and distinguish fog pass ownership for every camera/material class |
| new shared WGSL functions | geometry fog curve and blend in the recovered color domain |
| new full-screen fog system | reconstruct distance from scene depth and apply the opaque fog pass |
| `sky.rs` | consume the resolved environment; keep dome composition separate from interior fog |
| `terrain.wgsl`, `water.wgsl`, NIF materials | remove stock fog where the Skyrim paths own fog; avoid applying fog twice |
| `skyrim_ini.rs` | accept recovered settings with their native defaults and exact enable semantics |

The existing `records` table already retains raw winning subrecords. Typed export is an
implementation convenience, not permission to discard unknown fields. Form links must be
resolved with the converter's master/light-plugin mapping, not raw load-order bytes.

### 6.2 Bevy 0.19 pass placement

**Native:** world render owner VA `0x141539F10` calls opaque wrapper `0x1415152D0`, then
image-space owner `0x141541F40` in preparation stage zero, then an intervening image-space
step, then that owner in composite stage one. Stage zero returns before the composite. Only
after the actual fog/SAO composite does the world owner call finishing wrapper `0x141515360`;
its designated alpha batches get flag bit `0x4`. This establishes the ordinary world ordering
independently of the reconstructed shader source.

Frame owner VA `0x140656E60` calls that world/fog owner before `MainRenderFirstPerson`.
The first-person body is therefore drawn after the world fog composite, with the flags described
in §5.4; it must not be included in a full-screen fog pass over the finished image.

Bevy 0.19 uses a `Core3d` schedule. Its stock main-pass systems are chained as
`main_opaque_pass_3d → main_transparent_pass_3d`; the older `Node3d` graph API is not the current
integration point. **Design:** place the replacement opaque fog between these systems for
the recovered opaque-then-later-geometry routes, matching the ordinary native world order.
Draws such as Water, Particle and Effect may need their own routes; do not assign every
transparent draw to that later group. Merely adding a default full-screen material after the
main pass would also fog transparency against the opaque surface behind it.

Turn off `DistanceFog` for paths owned by the replacement. `StandardMaterial` meshes and the
custom terrain/water shader hooks currently receive Bevy fog, so changing only terrain would
leave meshes inconsistent. Preserve depth, masks and alpha coverage while fog changes RGB.

Use the actual camera's inverse projection and viewport for depth reconstruction. Mudcrab has
reverse-Z with a far-depth sentinel of zero, whereas the retail pass has its own reconstruction
constants. Translating the projection convention is required; copying retail depth constants
into Bevy is invalid. Treat unloaded terrain and sky pixels separately from valid scene depth.
The retail bypass includes near-far-plane geometry whose forward depth exceeds the sentinel,
not only pixels at exactly clear depth. Reconstruct equivalent finite forward D3D depth and
compare `f32(0.999998987)`, in addition to testing Bevy's clear-depth mask. Testing reverse-Z
depth equal to zero alone loses that case.

Each rendering camera needs explicit fog ownership: world, first person, reflection, refraction,
and fixture. The implementation now opts reflection cameras into world fog; that ownership is
a project choice pending retail capture. Keep reflection tests in the acceptance set.

### 6.3 Approximation replaced by this implementation

Before this implementation the engine used fixed clear-day parameters, only Fog Far color, Bevy exponential
density `6.2e-5`, and fog-color alpha `0.85`. Bevy computes
`0.85 * (1 - exp(-6.2e-5 * distance))`. The shader's maximum is a cap on a powered normalized
distance, not a multiplier on an exponential. Those functions cannot be made identical by
adjusting one density. The old path removed interior fog entirely.

The fitted curve and the removed helper named `VanillaFog` are not parity evidence. Tests of
those helpers established their implementation, not the retail renderer's behavior.
[Implementation status](distance-fog-implementation.md) records the replacement paths,
validation and remaining retail-capture gates.

## 7. Implementation sequence and acceptance

### 7.1 Build sequence

1. Add typed record extraction with field provenance, native legacy defaults and per-field
   inheritance. Check Clear, Fog, Sleeping Giant Inn, Helgen, a legacy template and the max-1000
   template against the reference JSON before changing rendering.
2. Implement the environment resolver as an independently inspectable binary32 calculation.
   Export hour, effective climate intervals, current/last weather, progress, room endpoints,
   inheritance sources, resolved colors/scalars, scenegraph, settings, and packed constants.
3. Establish the retail working color domain and framebuffer scaling from a captured render
   frame. Match the active camera's forward projection and output-depth biases alongside Bevy's
   reverse-Z projection. Implement opaque depth fog with the original forward-depth sentinel
   and an explicit Bevy clear-depth bypass.
4. Route material descriptors to their recovered geometry/neutral-blend behavior. Preserve
   vertex interpolation, lighting clamps, Effect modes and Water handling. Remove Bevy fog only
   from routes the new implementation owns.
5. Compare each pass in isolation, then controlled complete frames. Extend sky, lighting and
   image-space work at the measured boundaries; do not compensate for an unrelated exposure
   difference by changing the recovered fog exponent, color or maximum.

The resolver and shader constants should be frozen per render frame so all cameras/materials
participating in that frame see a coherent environment. Streaming or an origin rebase must
not change the weather interval, resolved colors, or distance metric for unchanged geometry.

### 7.2 Original numerical fixtures

[Synthetic math fixtures](../../research/skyrim-distance-fog-math.json) cover range boundaries,
powered curves, caps, nested two-color blending, framebuffer scaling and depth bias. They are
original analytical examples, not game pixels or measured GPU outputs.

[Interactive curve comparison](../../research/skyrim-distance-fog-curves.html) shows the
recovered factor against the current exponential for selected authored day/night weathers.
Its axis is the already-computed fog metric; it does not identify a radial world distance or
serve as a final-image reference.

Run the dependency-free checker from the repository root:

```sh
python3 scripts/check-distance-fog-spec.py
```

The checker also distinguishes equal-range packing between image-space and Lighting, verifies
that max `1000` still yields a factor at most one for a positive-power saturated input, compares
the project's exponential to the powered curve, and checks selected record inheritance.
Binary64 analytical checks are not GPU bit-parity tests; the script states that limitation.

Useful hand-calculated nested-color fixture, in the fog pass's working domain:

```text
source = (0.1,0.2,0.3)
near   = (0.2,0.4,0.6)
far    = (0.8,0.6,0.4)
a      = 0.5; k = 1
fogColor = (0.5,0.5,0.5)
output   = (0.3,0.35,0.4)
```

With a quarter factor, output is `(0.1625,0.2625,0.3625)`. A far-color-only implementation
fails these cases. A separate GPU fixture must inspect sky sentinel behavior, alpha preservation,
final saturation, interpolation on a large triangle and every supported material blend mode.

### 7.3 Retail capture protocol

Use the copied installation's matching executable/shader archive, or prove a source installation
has the same hashes. A runnable session has not yet been validated. The capture host must provide
a stable D3D/Proton rendering session and a GPU frame capture with shader resources/constants;
a screenshot alone cannot establish depth, color domain or blend state.

For every reference capture record:

- Executable and shader archive hashes; active plugin order and winning record values; any
  injected renderer/mods; effective GMSTs, INIs and runtime overrides.
- Cell/world/region/climate identities; numerical Sky mode; game hour; current/last weather;
  progress; current/previous room templates and room factor; resolved `BSFogProperty` fields.
- Scenegraph and active camera; position, rotation, FOV, near/far, projection matrices; viewport,
  resolution scaling, depth format and depth bias.
- Shader family/descriptor/bytecode hash, constant buffers, SRVs/RTVs and color transfer flags,
  sampler filters, framebuffer range, blend state, depth writes and pass order.
- Source color/depth immediately before fog, fog output immediately after, and final display
  output; image-space/exposure/tone-map settings and temporal history.

Freeze weather/time using a controlled retail test state and verify the memory/constants after
the control takes effect. Disable or fix temporal effects in both captures for the isolated pass;
restore their actual configuration for the final-image comparison. Align camera and geometry
before comparing fogged pixels. Missing LOD or a lighting difference is a separate mismatch.

### 7.4 Required comparison matrix

| Case | What it must distinguish |
|---|---|
| Clear day and night at fixed hour | day/night ranges, both fog colors, capped plateau |
| Sunrise and sunset endpoints/midpoints | extended intervals and four-key colors versus scalar weights |
| Clear→Fog at progress `0,0.25,0.5,0.75,1` | scalar/color blending and effective transition controls |
| Sleeping Giant Inn and Helgen | independent inheritance, room fog, sky visibility separate from fog |
| Previous-only/current-only room endpoints | asymmetric scalar/color fallbacks |
| Legacy Windhelm template and explicitly black far color | constructor defaults and zero-color fallback |
| Zero/zero FX weather; equal-nonzero synthetic range | distinct packing paths, property flag not the only gate |
| Max `1000` template and disabled image-space fog | saturation location and real enable controls |
| On-axis/off-axis surfaces at multiple FOVs | axial opaque versus clip-XYZ vertex distance |
| Large transparent triangles and depth edges | vertex fog interpolation and opaque depth sampling |
| Bright specular, first-person and alpha draws | clamp-coupled Lighting formulas and flags |
| Additive, multiplicative, premultiplied decal effects | black/white fog targets and blend order |
| Distant terrain beyond `70000` clip Z; water | output-depth bias and water/LOD fog ownership |
| Sky silhouette, cloud/Effect sky objects, grass/tree/particles | sentinel and descriptor-specific routing |
| Reflection/refraction and origin rebase | per-camera/scenegraph state, fog count and metric stability |
| AO off/on, SSR/snow/volumetric controls | source composition and before/after-fog boundaries |

### 7.5 Pass/fail criteria

**Design acceptance criteria:** these are proposed engineering thresholds, not existing results.

1. Record fields, inheritance ownership, constant bits, mode and selected shader descriptors
   agree exactly for supported reference cases. CPU formulas preserve the recovered binary32
   constants and operation order; no fitted fog parameter may replace them.
2. For isolated GPU fog with identical input textures/constants, compare the entire target and
   the sky/depth/material masks. Require alpha/mask preservation where the bytecode preserves
   them. Start with RGB absolute error `<=2e-5` in normalized working space, separately record
   max error and edge errors, and adjust only with documented backend log/exp/interpolation or
   texture-format evidence. No accepted tolerance may hide a different distance metric or pass.
3. In deterministic final SDR comparisons, aim for at most one 8-bit code value per RGB channel
   on aligned stable pixels. Report all failing pixels and structured edge/LOD differences.
   Temporal, geometry, lighting, sky and image-space differences require matching inputs or a
   documented separate issue; a small global average cannot establish exact appearance.
4. Every required matrix row must pass before calling the fog replacement **1:1**. Where a draw
   category or boundary is unproven, label the implementation partial rather than widening the
   visual tolerance or changing weather data.

### 7.6 Unresolved gates

| Gate | Current evidence | Needed to close |
|---|---|---|
| Active retail session/profile | complete matching files and package scan | validated run, active plugins/INIs and runtime settings |
| Region selection | flag→mode map, updater and region inputs recovered | captured selected region/weather state or native spatial selection rule |
| `bFogEnabled`, `bDo30VFog`, internal remap byte | defaults/addresses known; remap math recovered | reader/owner and controlled toggle results |
| Special/offscreen draw ownership | exact shader formulas, flag packing, ordinary world order and clamp INIs recovered | remaining special material routes and capture |
| Color/display boundary | normalized colors, framebuffer-scale consumer, shader arithmetic recovered | actual render-target domain/format, scale and image-space order |
| Per-family render ownership | bytecode equations and variant coverage recovered | blend/depth/pass states, offscreen camera captures |
| Water null-state support | scalar upload and zero-range behavior recovered | demonstrate whether a valid retail route ever supplies null |
| Final visual identity | no retail/Mudcrab paired fog frame | isolated-pass and full-frame comparison matrix |

These gates are required work for an exact renderer. The spec records the recovered algorithms
and concrete closure tests; it does not claim that static evidence has already proven a complete
frame match.

## 8. Sources

[Evidence metadata](../../research/skyrim-distance-fog-evidence.json) records source pins,
30 native function coordinates/query hashes, complete shader-family counts and 14 selected
shader identities. The native Ghidra/LLVM cross-check compared 17,784 decoded instructions
without encoding or boundary mismatches; all listed normative functions have complete
instruction evidence. The persistent project's original whole-program analysis was partial.
Read-only queries and instruction agreement do not establish complete indirect-call coverage.
Independent artifact validation also checked all 16,044 shader offsets, container lengths,
stages, extracted bytecode hashes and assembly hashes against the FXP manifest.

Principal shipped shader identities (offsets within the extracted FXP):

| Program | DXBC offset | SHA-256 |
|---|---|---|
| Fog composite PS `00000000` | `67678920` | `d4b6f615b16fbcf0015716c373b75608b9bf3c8ba2af864d095655daa82ca799` |
| SAO+fog composite PS `00000000` | `67686096` | `df277f776cd0c130f00e8b52ca55b4d58611b631f16a04cf12f8e80c74663aeb` |
| Lighting VS `00000000` | `8468132` | `ffc03a09de2a2ef661b7b7f97ef399ffceaa432f0b834f426b514d132b604e5f` |
| Lighting PS `00000001` | `9055392` | `b408e36e3c9188db139e29a48ec7a31e0fdf1538b9c5168ee754c1a2eba5f504` |
| Lighting PS `00000201` | `9065960` | `21615f9b90510a9b76964054b78333492171651a34072dab3fe7ae7e93e55856` |

Public shader reconstruction was used for discovery and naming, then checked against the owned
bytecode. The [historical extractor][fxp-reader] omits later compute families; its image-space
names require correction. The [older Lighting PS reconstruction][lighting-source] also diverges
from the shipped clamp/fog arithmetic. Community Shaders adds rendering features and is not
a substitute for the vanilla shader archive.

The private evidence root is `~/.local/share/mudcrab-research/fog-20261008/`. It contains the
record scan/report, native read-only queries/index, original extraction helpers, shader
disassembly/manifest and interpreted summaries. Wine vkd3d-shader `2.0` decoded DXBC without
executing the game or shader. No proprietary binary, shader or raw disassembly is included
in this repository.

[xedit]: https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas
[mutagen-weather]: https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/Weather.xml
[sm-saturate]: https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/saturate
[sm-min]: https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/min--sm4---asm-
[fxp-reader]: https://github.com/Nukem9/skyrimse-test/blob/328916305165a46c4e4b527735bbcfd46b09a0ca/shader_analyzer/FXPPackageExtractor.cs
[lighting-source]: https://github.com/aers/Skyrim-SE-Shader-Tools/blob/b54d184a5c3acc25c60e6666a46c68b09a3759ed/old/shaders/Lighting/BSLightingShader.ps.hlsl

Record layout cross-checks use xEdit revision `9fb016884bec138ea6c7b872cec831537d464c3e` and
Mutagen revision `414371eedc94a3e0c28f082180e0492df67ab8ca`. Native header names are leads only;
historical layouts do not supersede the exact executable. The numerical behavior comes from
the copied records, the exact native code, and the shipped DXBC.
