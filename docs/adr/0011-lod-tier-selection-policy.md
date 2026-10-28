# ADR-0011: Fixed spatial tiers; projected-error object detail as an extension

- **Status:** Proposed
- **Date:** 2026-09-27

## Context

The initial 2026-09-27 decision below describes the terrain-first delivery.
The 2026-10-12 hybrid amendment separates object detail selection from world
residency; it does not change the shipped terrain policy.

DynDOLOD allows fixed spatial tiers for Skyrim parity and projected-size
selection as a native extension. This engine streams on a cell grid with
an R-tree, a per-frame commit budget, and radius-derived camera far and
shadow cascades. A size-based policy needs per-frame projected-size
evaluation and a second eviction discipline; a tier policy maps directly
onto the grid and existing streaming machinery.

## Decision

Select LOD by fixed 4/8/16-cell tiers past the full-detail grid. Keep
block size, mesh source, distance policy, and variant as separate
dimensions so a projected-size policy can replace the distance mapping
later without touching chunk contents.

### Proposed hybrid object extension (2026-10-12)

Following [#203](https://github.com/Mudcrab-Team/mudcrab/issues/203#issuecomment-6086560142),
keep spatial admission, residency and handoff under native LOD. Within an
eligible resident object, compare generated conventional detail levels with
meshoptimizer cluster selection by projected geometric error. A complete
cluster cut may refine detail without loading another world region or
changing object state. Terrain, trees, small meshes and unsupported materials
keep their category policies.

Measure error under the object's transform and each active view's projection
and viewport. Main, reflection and shadow views need explicit coverage and
quality policies; the main-view cut alone does not establish their coverage.
Approximately one pixel is an experimental quality setting, not an accepted
default or proof of visual equivalence. Finer selection requires ready data
and bounded submission capacity; otherwise retain a complete drawable coarse
cut or verified conventional fallback. Future paging must keep coarse
ancestors resident. Detailed requirements and gates live in
[hybrid object LOD](../specs/engine/hybrid-object-lod.md).

## Consequences

- Chunk keys, budgets, and handoff tests stay simple and grid-aligned.
- Small-near vs large-far tradeoffs are coarser than a size-based policy;
  per-tier source choice (landmarks persist, clutter drops) covers the
  common cases.
- Revisit when tier captures show size-driven popping the tier policy
  cannot fix.
- Cluster detail, paging and spatial HLOD remain opt-in experiments until
  their category-specific correctness, matched-quality and backend gates
  pass. Geometry reduction alone cannot establish a frame-time improvement.
