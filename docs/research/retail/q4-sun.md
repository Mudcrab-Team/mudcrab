# Q4 Sun: position, direction, day/night (#588, #653)

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Static trace; game data from Skyrim.esm.

## Inputs
- Game hour h: Sky `+0x1B0`.
- Climate TNAM bytes (CLMT, Sky `+0x40` -> `+0x78..+0x7B`): sunrise begin, sunrise end, sunset begin, sunset end, each x 1/6 h (`0x141869dd8`). DefaultClimate `0x15F` and SkyrimClimate `0x812`: 33/60/96/123 -> 5.5 h, 10.0 h, 16.0 h, 20.5 h.
- GMSTs (Skyrim.esm values win over the exe defaults):
| GMST | exe default | Skyrim.esm | meaning (traced) |
|---|---|---|---|
| fSunXExtreme | 400 | 400 (`0x12FC9`) | east-west half-span of the sun path (x amplitude) |
| fSunYExtreme | 25 | 40 (`0x12FCB`) | constant north offset (tilt) of the path |
| fSunZExtreme | -100 | -150 (`0x12FCA`) | vertical component of the light direction |
| fSunDirXExtreme | 400 | 200 (`0x93FA8`) | scales the horizontal part of the light direction |
| fSunAlphaTransTime | 2.0 | 0.5 (`0x10FDC3`) | sun fade-in/out window (h) around the sunrise/sunset midpoints |
| fSunMinimumGlareScale | 0.25 | 0.25 | glare floor |
These are not fallbacks. They define the sun path itself (#588 reads them as fallbacks; that reading is wrong).

## Sun update (`0x14041bd90`, Sky mode 3 = exterior with sky)
```
riseMid = (riseBegin + riseEnd)/2   setMid = (setBegin + setEnd)/2   trans = fSunAlphaTransTime/2
A = riseMid - trans     B = setMid + trans                        (DefaultClimate: A = 7.5 h, B = 18.5 h)
day   (A < h < B):  u = (h - A)/(B - A);                x = (1 - 2u) * fSunXExtreme     (+400 -> -400)
night (otherwise):  u = (h - B or h + 24 - B)/(24 - (B - A)); x = (2u - 1) * fSunXExtreme (-400 -> +400)
sun disc node translate = (x, fSunYExtreme, |fSunXExtreme| - |x|)                (also Game.GetSunPosition)
sun light direction (travel) = normalize( -(fSunDirXExtreme/fSunXExtreme) * x,
                                          -(fSunDirXExtreme/fSunXExtreme) * fSunYExtreme,
                                          fSunZExtreme )        = normalize(-0.5x, -20, -150)
```
The light is the node's local X axis (NiDirectionalLight convention); the three components are written into the rotation's first column (`0x14041c4a5`–`0x14041c540`, normalized by `0x1402b6540`).

Derived path (ESM values; toward-sun vector, X = east, Y = north): sunrise point A 7.5 h: azimuth 84 deg (east, 6 deg north of east), elevation 36.7 deg; noon (x = 0) at (A+B)/2 = 13.0 h: elevation 82.4 deg, tilted 7.6 deg to the north; B 18.5 h: azimuth 276 deg, 36.7 deg. At 12:00: elevation 74.5 deg. The light never goes below 36.7 deg elevation, day or night. high (formula), derived numbers.

**Night:** the same directional light keeps the same geometry and runs back from west to east (x from -400 to +400), so night light comes from above at 36.7-82.4 deg. Its colour comes from the weather's night colours (below). The sun disc is hidden by alpha 0. Masser/Secunda are separate (fMasserSpeed 0.25, fSecundaSpeed 0.3, ZOffset 35/50, sizes 94/40, angle fades 55->45 / 55->40); not traced further.

**Sun visibility alpha:** 0 outside [A, B]; ramps 0->1 over [riseMid - trans, riseMid + trans]; 1 between; ramps 1->0 over [setMid - trans, setMid + trans]. Glare alpha = alpha x WTHR byte `+0x668` / 255 x weather transition (current and outgoing weathers blended). high.

**Interior / no-sky mode** (Sky mode 0): the light rotation comes from the cell's lighting (`0x140295700`, XCLL directional rotation; see Q5) through sincos. With no cell: a fixed default rotation with -pi/4 entries. medium.

## Day/night light colour and intensity (#653) (`0x140416760`)
Time-of-day colour weights (WTHR colour slots 0 sunrise, 1 day, 2 sunset, 3 night):
```
ext = fDaytimeColorExtension (0.5 h)
R0 = max(riseBegin - ext, 0)     S1 = min(setEnd + ext, 24)
R0 < h < riseEnd:  mid = (R0 + riseEnd)/2; sunrise weight w = 1 - |h - mid| / ((riseEnd - R0)/2); other slot = night (h < mid) or day (h >= mid), weight 1 - w
riseEnd <= h <= setBegin: day 1
setBegin < h < S1: mid = (setBegin + S1)/2; sunset weight w as above; other = day (h < mid) or night (h >= mid)
otherwise: night 1
```
Each weight is multiplied by the weather transition t (current weather) and 1 - t (outgoing weather). No separate day/night intensity scalar exists here. Intensity comes from the WTHR colours for each slot (sunlight, ambient DALC, fog, sky) and from the weather's image space per time of day. high (weights), medium (which colours use them: the weight function is shared).

DefaultClimate example: R0 = 5.0, sunrise peak 7.5 h, day 10.0-16.0 h, sunset peak 18.5 h, night from 21.0 h.

## Other engines
- spud (`src/engine/weather.cpp:1043-1086`, `src/formats/osa.hpp:275-279`): sun node = (x, fSunYExtreme, |XExtreme| - |x|) with fSunXExtreme etc. Derived; matches. Its compiled fallbacks are the exe defaults (400/400/2.0), not the ESM values.
- OpenSky (`OpenSkyRenderCore/Weather/WeatherRuntime.swift:20-60`): four-slot weights with the same shape but without fDaytimeColorExtension; default windows 5-7 h / 17-19 h ("chosen as sane vanilla-like midpoints"): invented. Default sun direction (0.4, 0.35, -0.85) (`SceneCamera.swift:37`): invented.
