# Riverwood glow isolation, 2026-10-10

The user rejected the fog-enabled native-texture-view screenshot as glowing.
The target is our saved `native-unorm-fog-hdr/riverwood-terrace.png`, SHA-256
`1c4ae0e9feee61e7c63d4d292f7fea8d565724d5d8b0f82dde2e2ba50c3038d5`.
Its successful capture and texture-sampling checks did not establish an acceptable
Skyrim SE/AE appearance.

## Controlled captures

Three settled 1920×1080 captures retain clear weather `0x81a`, noon, the same
camera fixture, fog, authored IMGS grading, display-encoded output and the
source-qualified eight-texture pack. Every run exits successfully with zero
pending or unavailable capture dependencies. New GPU diagnostics read eight
pixels before image-space composition without changing the production target.
They record source RGB, spatial adaptation, bloom, native tone output and the
display bridge. The last diagnostic sample is frame 510; screenshots are later
settled frames, so these are paired observations, not a claim of identical frames.

| Change from the new baseline | Roof mean Y | Stone mean Y | Stone pixels with a 255 channel |
| --- | ---: | ---: | ---: |
| Baseline | reference | reference | 1.740% |
| Bloom off | −1.63% | −3.13% | 1.712% |
| Native specular fade zero | −13.43% | −4.91% | 1.135% |

Y decodes the final PNG into linear sRGB/D65; these regions contain texture,
normal and shadow variation. The new baseline repeats the rejected frame's
roof, wood, stone, ground, fern, sapling and sky region metrics exactly. The
mountain region differs by −0.0065% in mean Y.

Bloom off preserves source and adaptation exactly at all eight measured points.
It leaves wood, ground, fern and sapling region metrics unchanged. A nearby
background strip darkens by 2.84%, while the farther strip is effectively
unchanged. Bloom contributes some nearby spill, but disabling it leaves the
pale surfaces and dark surroundings.

Specular zero changes the source image and adaptation. Its regional difference
therefore measures the integrated control, not a pure specular percentage.
The selected stone point remains source RGB `[1,1,1]` with both controls. Its
mapped luminance is approximately 0.913, above the 0.8 bloom receive threshold,
so its bloom weight is zero. Authored grading produces approximately
`[1.309,1.276,1.235]` before final saturation, leaving that point white.
This locates that hotspot upstream of bloom; it does not identify the correct
replacement light, texture or output coefficient.

## Input consistency

The earlier correction qualified eight diffuse texture sources. Other native
materials retain their compatibility sampling views, while terrain uses Bevy's
material and ambient response. The mixed scene reaches one HDR reduction and
grading pass. These controls measure that eight-source pack. The subsequent
[full-pack coverage test](skyrim-native-diffuse-coverage-20261010.md) qualifies
62,864 diffuse views without changing normal or PBR texture bytes; its separate
captures retain the appearance failure.

The default native sun producer audit finds no missing normalization:
`Sun::Init` initializes the light dimmer to exactly 1.0; `Sun::Update` copies
prepared Sky sunlight directly to diffuse RGB; the inspected consumer applies
dimmer and `SunlightScale`. All 1,995 exported instruction starts and requested
spans match the selected executable bytes. Other writers, active-light selection
and live dimmer remain unresolved. This evidence supplies no darker replacement
coefficient. The controls above retain the earlier approximate noon zenith.
The subsequent [local sun recovery](skyrim-native-sun-trajectory-20261010.md)
replaces that approximation using original climate endpoints and compiled
settings, with an explicitly assumed identity parent. Its captures change the
lighting distribution but do not fix the hotspots.

## Diagnostic controls and validation

These controls require `--image-space` to enable HDR composition:

```text
--image-space-bloom on|off
--image-space-adaptation x,y
--image-space-report path
--native-specular-fade 0..1
```

Adaptation overrides require finite `x > 0` and `y >= 0`. They freeze native
history inputs; they are not an exposure setting or retail calibration.
The specular override changes only the runtime native fade uniform and restores
the original source value when cleared. Newly loaded materials receive it.
Defaults and authored grading are unchanged.

All 450 engine library tests and strict engine Clippy pass. The windowless Metal
scene probe passes seven composition cases and 40 diagnostic field checks in
each of RGBA16Float and R11G11B10Float. All 24 world samples independently
reproduce the recovered tone equation within `1e-5` using measured inputs.
These gates validate the controls and observations. Visual acceptance and
matched retail comparison remain open.

The frozen engine SHA-256 is
`6f780f73cf13e1eba0aa50bf09b904f90fc89ce29ec9d457a6eb4e421b4dbdf6`;
the fixture SHA-256 is
`1dd33ff311976ad366d95b793398e75f7c4cfb4a76d959ee458e903e8c769336`.
Private capture provenance, GPU reports, independent measurements and the
bounded sun audit are under
`/Users/taylor/.local/share/mudcrab-research/glow-20261010`.
