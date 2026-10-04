# ADR-0009: Normalize vertex alpha on Cutout shapes during conversion

- **Status:** Superseded for schema 19 by the source-channel rule below
- **Date:** 2026-09-26

## Context

Bethesda foliage NIFs carry per-vertex `COLOR_0` alpha as an artistic edge fade on
alpha-tested needles and leaves. The runtime cutout decision multiplies vertex alpha
into the alpha test, so minified distant foliage discards to sky at the authored
0.44 cutoff and pines render white at distance. PR #27 expands model extraction to
all placeable types, which makes the forest visible and the failure mode prominent.
A survey of converted assets found 244 of 25,391 GLBs carry sub-1 vertex alpha on
MASK primitives; the affected shapes are not only pine needles (e.g. wolf fur
cards).

## Decision

`normalize_cutout_vertex_alpha` in `crates/converter/src/mesh.rs` forces vertex
alpha to 1.0 on Cutout shapes on both NIF export paths, so the cutout decision
uses texture alpha alone. The change ships with the schema 14 to 15 migration
(GLBs only; textures and scripts are reused).

## Consequences

- Distant conifers render solid dark-green beside yellow aspens instead of
  stippled white, verified by matched before/after acceptance screenshots.
- Bethesda's per-vertex edge fade is lost on every touched Cutout shape (244
  files); fur cards and similar geometry render slightly fuller.
- Near-field renders already matched this behavior and look right, so the change
  is a fidelity tradeoff, not a regression in the common view.
- Revisit when engine LOD billboards exist: scope the normalization by material
  semantics, or replace the converter-side normalization with a renderer-side
  rule that preserves edge fade without the distance collapse.

## Schema 19 correction (L1)

The original workaround applied one foliage observation to every cutout. Native
lighting reads vertex RGB only with `Vertex_Colors` and vertex alpha only with
`Vertex_Alpha`; tree animation and object LOD paths do not multiply vertex alpha
into their material alpha. `apply_vertex_color_contract` now follows those rules
on both export paths. Ordinary cutouts retain enabled authored alpha. The vendored
BSTriShape adapter also copies the source RGBA bytes that it previously dropped.

The engine integrates PR #99 so depth and shadow alpha tests include the same
vertex alpha as the visible pass. The `material_alpha_probe` compares a fading
quad with explicit reference geometry in all three passes; the legacy prepass
is a failing control. This supersedes blanket cutout normalization without
claiming tree-animation or LOD shader parity.

Source: Community Shaders `Lighting.hlsl`, revision
`2f2919a71bed6132b125e41781304c8f6f73d002`, vertex color output and pixel alpha
expression excluding `TREE_ANIM`, `LODOBJECTS`, and `LODOBJECTSHD`. This is reverse
engineering evidence about native shader structure, not an enhanced-rendering
target. Schema 19 rebuilds GLBs; unchanged textures and scripts remain reusable.
