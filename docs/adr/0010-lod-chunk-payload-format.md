# ADR-0010: LOD chunk payloads are GLB files, indexed by manifest

- **Status:** Proposed
- **Date:** 2026-09-27

## Context

This records the initial 2026-09-27 storage inventory. The terrain GLB/index
path subsequently shipped; the optional object extension below remains
proposed.

Phase 2 of the LOD plan needs a storage format for spatial chunks. Two
candidates exist. The `lod` table (`exporter.rs`) has a `(cell_id,
lod_level, mesh_data)` schema but no writers or readers, so it is a name,
not a working path. The GLB path is fully worked: atomic publish,
structural validation, bounds audit, `AssetServer` loading, readiness
tracking with dependency chains, and schema-versioned invalidation.

## Decision

Chunks ship as GLB files through the existing asset pipeline, with the
database holding an index, not blobs: chunk key `(world, tier, anchor)`,
bounds, batch list, content hashes, and manifest references. The `lod`
table is reshaped to that index or replaced; it never stores geometry.
Chunk rows are additionally indexed in an R-tree over world bounds so
range queries stay spatial; key lookup alone would force the runtime to
enumerate candidate anchors per tier.

The initial terrain payload encodes RGBA sRGB mip inputs as UASTC Basis KTX2.
It declares `KHR_texture_basisu` in `extensionsUsed` and supplies its image
source on the texture. Bevy 0.19's glTF dependency does not support this
extension as required; its loader reads the ordinary texture source and
decodes KTX2 through Bevy's image loader. The ordinary source therefore
also points to the same KTX2 image. This remains a native runtime contract,
not portable core-glTF texture conformance: a portable fallback must use
PNG/JPEG, or a loader must support a required Basis extension. No portable
glTF or other-loader acceptance is claimed.

### Proposed hybrid object extension (2026-10-12)

The [#203 agreement](https://github.com/Mudcrab-Team/mudcrab/issues/203#issuecomment-6086560142)
keeps cluster geometry inside native LOD. Retain GLB terrain chunks and their
database/R-tree index. Add an optional, versioned per-model cluster sidecar
for the scoped object experiment; resolved world records and source GLBs
remain authoritative and usable without it.

Bind the sidecar to source/material hashes, source primitive and attribute
layout, compiler/algorithm/settings identity, payload checksum, hierarchy
group links, conservative bounds and errors, and complete replacement cuts.
Audit its serialized ABI and limits before a loader consumes it. No filename,
numeric format version, or database schema is assigned by this amendment.
Any required migration must update producer, manifest, loader, launcher and
acceptance checks together. See the
[hybrid contract](../specs/engine/hybrid-object-lod.md) for validation and
fallback requirements.

The first prototype holds eligible hierarchies resident. Page layout,
compression and spatial HLOD are later decisions, after runtime correctness
and measured costs establish a need. A sidecar is derived render data, not
an alternative source of object state or a replacement for terrain payloads.

## Consequences

- Chunk loading reuses strict readiness, fallback diagnostics, and cache
  invalidation instead of inventing a parallel binary path.
- The proposed cluster loader needs an explicit ABI and independent malformed
  payload tests. Missing or rejected optional sidecars preserve the verified
  source/conventional route and report why clusters were not used.
- Very large worlds pay file-per-chunk overhead; mitigate with tier
  packaging (one file per tier per region) if measured, not upfront.
- The `lod` table reshape ships with a world DB version bump, together
  with the LOD-origin columns. Unlike the additive `waters` columns, LOD
  changes query shape and streaming behavior at a scale where silent
  mixed-version LOD operation is not acceptable: a package advertising LOD
  with an old database fails fast. Supported schema-3/4 packages remain
  full-detail-only and do not query the new tables.
