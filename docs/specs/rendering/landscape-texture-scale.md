# Landscape texture scale

The default LAND texture frequency is **24 repeats per full cell per axis**,
or 12 across a quadrant. This scales diffuse/normal sampling only; it does not
rescale the 17x17 quadrant paint-weight grid.

## Evidence

- Sheson's August 23, 2020 [xLODGen explanation](https://stepmodifications.org/forum/topic/15184-xlodgen-terrain-settings-compare/page/4/)
  explicitly gives 24x24 tiles per Skyrim cell and 96x96 per four-cell-wide
  level-4 terrain LOD texture.
- DoubleYou's [Skyrim SE default-setting investigation](https://stepmodifications.org/forum/topic/11523-skyrim-special-edition-default-values-for-all-valid-ini-settings/)
  records `fLandTextureTilingMult=3.0000` under Landscape. These are defaults
  when the setting is absent from the INI, not recommended custom settings.
- A read-only static inspection on 2026-09-30 independently recovered the
  calculation below from SkyrimSE.exe, file version 1.7.104.0, SHA-256
  `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.

The exact named setting `fLandTextureTilingMult:Landscape` has a static float
value of 3.0 at RVA `0x207b868`. In the function beginning at RVA `0x2b3ce0`,
the setting is read at `0x2b4052`; the code calculates `4 / setting`, then its
reciprocal at `0x2b41d1`. Two loops with bounds 17 multiply indices 0..16 by
that increment and store pairs of floats at `0x2b4252` and `0x2b4257`.
Function boundaries were taken from the PE runtime-function table; no symbol
name is assumed. For the nonzero default this is equivalent to:

```text
step = 1 / (4 / setting) = 3 / 4
quadrant_uv(x, y) = (x * step, y * step), x,y in 0..16
```

Sixteen intervals give 12 repeats across a quadrant, and 32 intervals give 24
across a cell. Thus `8 * 3` is the equivalent full-cell formula `(32 / 4) * 3`;
eight is not an already-applied effective default.

## Engine mapping and limits

`build_terrain_quadrant_mesh` uses full-cell UVs `x / 32, y / 32`.
Multiplying by 24 produces the same 0.75 UV increment per interval. Adjacent
quadrants differ by 12 whole repeats, preserving texture phase under repeat
wrapping. The renderer regression test checks all four quadrant spans and
that paint-weight coordinates still cover samples 0..16.

This establishes the default sampling frequency, not exact lighting or visual
parity. User/mod overrides of the original game's tiling setting are not
imported. Arbitrary future tiling values also require consistent phase at
quadrant/cell boundaries and agreement with terrain LOD. Shared glTF/LAND
image sampler ordering is a separate issue tracked in
[#101](https://github.com/Mudcrab-Team/mudcrab/issues/101), with follow-up
[#102](https://github.com/Mudcrab-Team/mudcrab/pull/102).
