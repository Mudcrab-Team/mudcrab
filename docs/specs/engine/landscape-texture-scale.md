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
- In retail, `fLandTextureTilingMult:Landscape` defaults to 3.0. The
  per-quadrant UV step is the reciprocal of `4 / setting`, and it's applied
  to sample indices 0..16 on both axes of the 17x17 quadrant grid.

For the nonzero default this is equivalent to:

```text
step = 1 / (4 / setting) = 3 / 4
quadrant_uv(x, y) = (x * step, y * step), x,y in 0..16
```

Sixteen intervals give 12 repeats across a quadrant, and 32 intervals give 24
across a cell. Thus `8 * 3` is the equivalent full-cell formula `(32 / 4) * 3`;
eight is not an already-applied effective default.

## Engine mapping and limits

The shared crate defines [`LAND_TEXTURE_REPEATS_PER_CELL`](../../../crates/shared/src/lib.rs)
as `32.0 * (3.0 / 4.0)`, exactly `24.0`. The engine imports this constant;
converter terrain LOD baking can use the same value without depending on the engine.

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
