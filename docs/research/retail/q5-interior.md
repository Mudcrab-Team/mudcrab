# Q5 Interior lighting (CPU side): XCLL, LGTM inheritance, ambient cube, directional, fades (#480)

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Static trace. Shader-side use is not traced here (CPU side only).

## Runtime layout (INTERIOR_DATA = cell `+0x68`, lighting template LGTM = cell `+0x138`)
XCLL offsets used by the getters: ambient `+0x00`, directional `+0x04`, fog near colour `+0x08`, rotation XY `+0x14`, rotation Z `+0x18`, directional fade `+0x1C`, DALC 6 colours `+0x28..+0x3C`, DALC specular `+0x40`, DALC scale `+0x44`, fog far colour `+0x48`, light fade begin `+0x50`, light fade end `+0x54`, inherit flags `+0x58`. Template data starts at LGTM `+0x20` (same layout).

## Inheritance (cell getters `0x1402bbe40`..`0x1402bc790`)
Each getter returns the template value when the XCLL inherit bit is set and the cell has a template; otherwise the cell's own value; zero/black when the cell is not interior or has no XCLL.
| bit | field | getter |
|---|---|---|
| 0x001 | ambient colour AND the DALC (ambient cube) | `0x1402bbe40`, `0x1402bc790` |
| 0x002 | directional colour | `0x1402bbf30` |
| 0x004 | fog near colour and fog far colour | `0x1402bc1b0`, `0x1402bc240` |
| 0x020 | directional rotation XY and Z | `0x1402bbfe0`, `0x1402bc050` |
| 0x040 | directional fade | `0x1402bc0c0` |
| 0x080 | clip distance (`+0x20`) | `0x1402bc4d0` |
| 0x100 | fog power (`+0x24`) | `0x1402bc3f0` |
| 0x200 | fog max (`+0x4C`) | `0x1402bc460` |
| 0x400 | light fade begin / end | `0x1402bc540`, `0x1402bc5b0` |
(Bits 0x08 fog near and 0x10 fog far follow the same pattern per xEdit; their getters were not opened in this pass.)
- Clip distance: in an interior (Sky mode 1), `0x140665740` caps the camera far distance at the clip distance when it is > 0 (`min(far, clip)`). high.
- Fog power and fog max: copied to Sky `+0x1A8` and `+0x1AC` (`0x140413dd6`–`0x140413df1`). Their shader use was not traced.
Colours are RGBA8 bytes x 1/255. high.

## Per-frame application (Sky update `0x140412df0`, interior mode Sky `+0x1BC` = 1, or 2 with a current interior cell)
- Sky directional colour (`+0xD8`) <- XCLL directional; Sky ambient (`+0xCC`) <- XCLL ambient; fog near/far colours (`+0xB4`, `+0x138`).
- Ambient cube <- DALC via `0x1414ec3b0`:
  ```
  c[0..5] = the six DALC colours in record order
  halfdiff_x = (c[1] - c[0]) / 2   halfdiff_y = (c[3] - c[2]) / 2   halfdiff_z = (c[5] - c[4]) / 2   (per channel)
  mean = (c[0] + c[1] + c[2] + c[3] + c[4] + c[5]) / 6
  ambient(n) = mean + n.x * halfdiff_x + n.y * halfdiff_y + n.z * halfdiff_z          (evaluated at 0x1414ec730)
  specular colour = DALC specular; fresnel/scale = max(DALC scale, 1.0) (values <= 1 become 1.0)
  ```
  This is a 3x4 matrix per channel (the "directional ambient" form). The sign convention against xEdit's "X+, X-, ..." labels is unverified: with record order X+ first, n = +X gives the X- colour unless the normal is negated by the caller. Check against a captured shader constant before relying on the sign. medium (sign), high (mean and half-differences).
- Light fade: `fadeBegin^2`, `fadeEnd^2` -> `0x1433d3d34`/`0x1433d3d38`. Point lights (`0x141575d20`): with q = |camera - light|^2 - r^2 (r: a per-light value at the same site), factor = 1 if q < begin^2; 1 - (q - begin^2)/(end^2 - begin^2) up to end^2; 0 (light off) beyond. Exterior defaults: begin = fLightLODStartFade (Display), end = begin + fLightLODRange 500. high (structure).
- **The per-light flag** is BSLight byte `+0x63` ("fades"). BSLight init sets it to 1 (`0x141575a3f`). ShadowSceneNode::AddLight (`0x14150c810`) copies it from byte 5 of the light-create parameters (`0x14150ca3f`). For a placed light (`0x140274390`) that byte = NOT (reference vfunc +0xD0), and that vfunc (`0x1402e3bf0`) returns the **REFR record flag 0x00010000 when the base form is a LIGH** (form type 0x1F). UESP names this REFR flag "Never Fades" (LIGH references). So every placed light fades with distance unless its reference has Never Fades. high.
  Same site, other create-parameter bytes: byte 2 = LIGH flag 0x2000 (Portal-strict per UESP/xEdit), byte 3 = NOT REFR flag 0x100, byte 4 = NOT REFR flag 0x20000 (UESP: "Doesn't light landscape" / "Doesn't light water"; names from UESP, bit tests traced).
- Directional light direction (`0x14041be6e`–`0x14041bf8x`, in the sun update): angle a = XCLL rotation XY (integer degrees) and b = rotation Z (integer degrees), both x pi/180. The light's rotation = R_z(a) x R_y(b) (two sincos rotations multiplied); the light shines along its local X axis (RF-SUN-002 convention). So rotation XY turns the light in the horizontal plane and rotation Z tilts it. Exact sign/axis convention: medium; superseded by the update below.
- Directional fade (`+0x1C`): **retail stores it but no runtime reader was found.** The inheritance-aware getter `0x1402bc0c0` has no direct call, no pointer (vtable or table) and no RIP reference; the template getter `0x140295760` is reached only from it. A scan of every `mov r, [r + 0x68]` followed by a `+0x1C` access (13 sites) finds only this getter, its setter `0x1402bc10c` (editor/console path), and exterior-cell paths (interior bit clear, where `+0x68` is exterior data). Shader-side reads of a value copied elsewhere cannot be excluded by this scan, but no copy of `+0x1C` was found. medium-high.
- Sky mode-0 branch: directional (0.95, 0.85, 0.85), ambient (0.35, 0.35, 0.43), every point light ambient 0.1, light fade from fLightLODStartFade/Range. This is not the no-cell case: with no current cell the sky mode is 3 (exterior path), and mode 0 has no effect in practice.
- Weather lightning flash adds fWeatherFlashDirectional (1.0) / fWeatherFlashAmbient (0.3) x flash amount in the exterior path (not interiors).
- A "lighting template override" path (`0x140419b30`, `0x140419c80` -> `0x140415d50`, `0x140416fc0`, `0x140416240`) blends two templates over time. Its source (likely a cell/region or script override) was not traced.

> Update (2026-11-18): later research traced the rotation exactly: with a = rotation XY and b = rotation Z in degrees, A = rows (cos a, sin a, 0), (-sin a, cos a, 0), (0, 0, 1) and B = rows (cos b, 0, -sin b), (0, 1, 0), (sin b, 0, cos b); the light rotation is A x B and the light travels along its world rotation column 0. The shader-side combine is covered by separate shader notes.

## Not traced (next steps)
- How the lighting shader combines ambient cube, directional light (with or without shadow), specular and the cell image space (XCIM/IMGS HDR, eye adaptation). This needs the retail lighting DXBC or a CS reconstruction cross-check. For #480 ("very dark interiors"): retail ambient on a surface is the DALC cube value (mean plus half-differences), not the XCLL ambient colour; and the cell image space can raise exposure. Both are candidates; neither is proven as the cause.
- Where Sky ambient `+0xCC` is consumed (sun NiLight ambient for grass and legacy paths is the likely target; unverified).

## Other engines
- spud: reads XCLL/LGTM and the DALC (see #480 review notes); not compared line by line in this pass.
- OpenSky: not checked for interiors in this pass.
