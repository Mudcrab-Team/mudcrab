# Grass placement cache

`grass_cache.rkyv` is a separate, versioned artifact produced after converted
model bounds have been written to `skyrim_world.db`. Its contract and version
constant live in `shared::grass`. Grass cache changes do not change the terrain
cache version or the mesh/texture conversion configuration hash.

The format identified by `shared::grass::GRASS_CACHE_VERSION` contains sorted
grass rules and sorted exterior LAND-bearing
cells with effective nonzero coverage from at least one resolved grass species.
Grassless cells are omitted. Each stored cell keeps its 33 by 33 height field,
resolved water height, and
four quadrants in Creation XY order (southwest, southeast, northwest,
northeast). Each terrain layer retains every resolved grass species assigned
by the winning LTEX record. Its 17 by 17 row-major blend field stores normalized
overlay weights and the base residual. The converter normalizes all texture
layers before omitting grassless or zero-weight layers, so bare textures still
reduce grass coverage. Normals are derived from heights at runtime rather than
duplicated in this cache. No instance lists are saved.
Incomplete grass rules, missing model paths, and unresolved/deleted species
links are skipped; raw database records remain available for diagnostics.
Incomplete or invalid rules and invalid terrain cells produce bounded report
warnings and mark the published run incomplete. Healthy neighboring cells still bake.
These omissions also clear the manifest's record-data completeness proof, so
repairing an unrelated asset cannot mark the pack complete without restoring grass.

Each quadrant stores a 64 by 64 exclusion field: 32 Creation units per bit,
512 bytes, row-major with the least significant bit first. The converter uses
validated transformed model bounds only to find references overlapping each
LAND-bearing cell, including neighboring cells outside the reference's owning
CELL. Persistent CELL records sharing coordinates cannot displace terrain cells.
Bounds and CELL-relative transforms use double precision to preserve small
footprints at far modded coordinates. Negative reference scale is supported;
zero-scale references and references without validated bounds are skipped.
The unbounded-reference count includes only eligible modeled obstructions, not
actors, invisible records or excluded vegetation/effects. This count is advisory;
model conversion failures are still reported by asset integration.

The mask samples each tile center against actual converted GLB solid triangles.
Scene/node transforms, indexed and unindexed triangle primitives, reference
rotation and scale follow the shared Creation/glTF coordinate contract. Following
the earlier precacher's upward-ray policy, a sample is excluded when a triangle
covers its XY position and its height is between terrain minus 8 and terrain plus
8192 Creation units. Vertical/degenerate projected triangles do not block the
upward ray. Terrain uses the rendered `[a,b,c, b,d,c]` topology, rather than
bilinear interpolation. Hollow mesh openings remain clear even when their
combined model bounds enclose them. Elevated roofs within the ray window can
block grass beneath them, while triangles below the surface or beyond the upper
window do not.

TREE and FLOR base records, generic `meshes/markers/` and `meshes/effects/` paths,
and alpha-blended primitives are excluded from obstruction inspection, matching
the earlier policy. Opaque and alpha-mask primitives count as solid triangles;
alpha textures are not sampled. Missing/invalid meshes, skinned geometry,
external buffers and unsupported primitives produce model diagnostics and no
bounds-based fallback. These optional mask omissions produce advisory notices,
including the skipped-model count and at most 32 model diagnostics, and do not
make an otherwise complete conversion incomplete. This can permit grass through unavailable obstruction
geometry. The asset root is explicit; canonicalized model files must remain
inside it. Geometry is decoded only for references touching grass-bearing cells.
A 64 MiB cache accounts for retained triangle allocation capacity and evicts
least-recently-used models; one oversized model can be decoded transiently but
is not retained. Model fingerprints remain available after geometry eviction.

This is a sampled offline mask, not collision-exact placement: a feature narrower
than a tile may be missed by its center, and a blocked sample excludes the whole
32-unit tile. Bounds only select candidate references; masks use triangles rather
than filled bounds. No instance lists are saved, and this
does not claim Skyrim's original placement RNG.

The converter validates binary accessor layouts before reading obstruction geometry,
including sparse accessors. An unreadable declared index accessor is rejected,
never interpreted as unindexed geometry. Terrain sample dimensions,
finite heights and blend indices/values are checked before mask generation.
The terrain cache continues to use `shared::CELL_CACHE_VERSION` independently.

Each entry carries a SHA-256 input key covering the grass cache version, cell
identity/coordinates, terrain fields and raw LAND, relevant winning LTEX/GRAS
payloads and resolved species links, and every overlapping reference's identity,
source payload, transform, finalized bounds, canonical model path and SHA-256
model contents. Replacing triangles or materials invalidates affected cells even
when model bounds and plugin records remain unchanged. Unavailable files have a
distinct fingerprint, so restoring geometry also invalidates affected cells.
Unrelated records leave their keys unchanged.

The converter validates the previous archive's bytes, version, sorting, grid
lengths, finite terrain/rule values and species links before incremental reuse.
Matching cells reuse their masks; invalid caches or versions that differ from
`shared::grass::GRASS_CACHE_VERSION` are rebuilt. The new
archive replaces any staged hard link, including when its cell list is empty,
and is validated before runtime-pack publication. The engine can sample the
validated archived arrays directly in background jobs without making owned
copies of the terrain or masks.

Metadata rebuilds regenerate the grass artifact after terrain and converted
model bounds are available. The source package's `grass_cache.rkyv` is the
previous cache, so unchanged cells can be reused. The rebuilt artifact is
validated before publication. `metadata-rebuild.json` records
`grass_baked_cells` and `grass_reused_cells` for that rebuild. Optional obstruction
mask omissions use the same notice policy as a full conversion.
