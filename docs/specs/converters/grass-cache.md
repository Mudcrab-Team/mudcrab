# Grass placement cache

`grass_cache.rkyv` is a separate, versioned artifact produced after converted
model bounds have been written to `skyrim_world.db`. Its contract and version
constant live in `shared::grass`. Grass cache changes do not change the terrain
cache version or the mesh/texture conversion configuration hash.

The format identified by `shared::grass::GRASS_CACHE_VERSION` contains sorted
grass rules and sorted exterior LAND-bearing
cells with a resolved grassy base or nonzero painted grass coverage.
Grassless cells are omitted. Each stored cell keeps its 33 by 33 height field,
resolved water height, and
four quadrants in Creation XY order (southwest, southeast, northwest,
northeast). Each terrain layer retains every resolved grass species assigned
by the winning LTEX record, in GNAM occurrence order. Duplicate links retain their
first occurrence; unresolved or deleted species are filtered without reordering
survivors. A winning override replaces the entire list, including an empty list.
Runtime species caps can take the first N in this order; the global rule table
remains sorted by FormID for binary search. Its 17 by 17 row-major blend field stores raw
painted opacity and the base residual. Base layers are marked explicitly and ordered first. Existing databases without stored GNAM
ordinals recover order from the winning remapped raw LTEX payloads, including
during asset repair. An unreadable legacy payload omits that list; an invalid GNAM
field omits only that field. Both produce a bounded warning while usable entries,
neighboring lists and cells still publish. Both record readers supply the complete
`cell_cache_preserved.rkyv` terrain projection to the grass bake, retaining layers
beyond the runtime terrain material's one base plus five overlays per quadrant.
Selection follows spud (MIT, chasmlol/spud): only the first five painted layers
contribute, ordered by LAND layer index. Their opacity is truncated to byte values (opacity * 255) then divided by 255,
without normalization; the base residual is max(0, 255 - sum of painted bytes) / 255. Grassless painted textures
also reduce the base residual. Later painted layers neither select grass nor
attenuate the base. Grassless and zero-weight painted layers are then omitted; a grassy base is
retained even with zero residual because it consumes the selection budget. Original
LAND signed normals and RGB vertex colors are retained (33 by 33 by 3 components);
empty synthetic fields allow runtime fallbacks. Invalid field lengths skip the
affected cell with a warning while healthy neighbors publish. No instance lists
are saved; generation remains deterministic at runtime.
Version nine adds these sampling fields and base flags and changes selection
opacity inputs. Version-eight grass artifacts rebuild; other asset cache identities
are unchanged. Runtime generation-only changes do not require a cache version bump.
Grass cache version two invalidates version-one coverage derived from the capped
runtime terrain cache; terrain, mesh and texture cache identities stay unchanged.
Version three rebuilds exclusion masks using skinned obstructions in their rest
pose; older grass masks are not reused. Other asset/cache identities stay unchanged.
Version four rebuilds masks using the runtime's initial reference enable state
and conservative skinned obstruction selection. It changes neither the archive
layout nor the mesh, texture or terrain cache identities.
Version five adds each cell's omitted-obstruction outcomes and skinned sampling
admissions alongside its masks, so reuse retains the same bounded notices without
decoding geometry again. Older grass artifacts rebuild; mask sampling and the
mesh, texture and terrain cache identities are unchanged.
Version six rebuilds masks without invisible or explicitly excluded primitives
and preserves authored GNAM species order. Only the grass artifact is invalidated.
Version seven excludes bases whose winning record header has Is Marker set,
using the existing `statics.flags` projection. Older grass masks rebuild; the
database schema, archive layout and other asset/cache identities are unchanged.
Version eight accompanies static NIF tangent-frame output (`TANGENT` XYZ and
handedness) reconstructed from retained positions, normals and UVs. The native
area-weighted frame follows U/V coordinates, rather than assuming the packed NIF
axis labels match them. Native Bevy-compatible bitangent handedness and the
existing DirectX normal-texture tag keep the normal decode consistent. Packed
normal textures use lossless linear RGBA8, retaining every supplied mip. Mesh
and texture settings proofs invalidate earlier converted bytes; the grass cache
rebuilds under the new producer version, including its 128-unit contact band.
Vertex RGBA remains intact. Degenerate optional UV frames warn and are omitted;
geometry remains usable and runtime can fall back to position/UV derivatives.
Incomplete grass rules and missing model paths are skipped; unresolved/deleted
species links are silently filtered. Raw database records remain available for diagnostics.
Incomplete or invalid rules that have a model, and invalid terrain cells, produce
bounded report warnings and mark the published run incomplete. Healthy neighboring
cells still bake. A grass type without a model draws nothing in game either, so it
is counted in one notice and does not affect completeness.
Only these grass warnings also clear the manifest's record-data completeness
proof (`record_data_complete = Some(false)`); model-less types and optional
obstruction omissions produce notices, and silently filtered species links do not
affect that proof. Repairing an unrelated asset cannot restore a lost proof.

Each quadrant stores a 64 by 64 exclusion field: 32 Creation units per bit,
512 bytes, row-major with the least significant bit first. For static models the
converter uses validated transformed model bounds to find references overlapping each
LAND-bearing cell, including neighboring cells outside the reference's owning
CELL. Persistent CELL records sharing coordinates cannot displace terrain cells.
Potentially skinned models are candidates throughout their worldspace: raw
POSITION bounds cannot enclose arbitrary rest-pose skin deformation. After a
cell misses reuse, bounds of the actual evaluated triangles, transformed with
the sampler's double-precision reference math, reject distant quadrants before
triangle sampling. Those bounds survive triangle-cache eviction. No accessor
min/max metadata is trusted as proof of a skinned envelope. This conservative
selection can invalidate cells distant from a changed skinned reference.
Bounds and CELL-relative transforms use double precision to preserve small
footprints at far modded coordinates. Negative reference scale is supported;
zero-scale references and references without validated bounds are skipped.
The unbounded-reference count includes only eligible modeled obstructions, not
actors, invisible records or excluded vegetation/effects. This count is advisory;
model conversion failures are still reported by asset integration.

Obstructions use the same shared initial enable-state resolver as runtime
spawning. A root follows the REFR Initially Disabled header flag; a reference
with an enable parent follows that parent's resolved state, inverted for the
opposite-state flag. Parent chains and the built-in enabled PlayerRef follow the
runtime rules. Missing parents, cycles and unreadable enable data default to
hidden without aborting healthy neighboring cells. This is the initial package
snapshot; save-game and script changes are not simulated. Hidden candidates
remain in cell input keys so later parent-state changes invalidate their masks.

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

Base records with header flag `0x00800000` (Is Marker) are excluded before bounded
candidate selection and unbounded-reference counting, regardless of model path.
The existing exporter already persists those winning flags in `statics.flags`;
this policy needs no new column or plugin reingest. TREE and FLOR base records,
generic `meshes/markers/` and `meshes/effects/` paths, and alpha-blended primitives
remain excluded. Converted primitive `openSkyrim.materialExclusion` declarations
and material `openSkyrim.nonRenderingExclusion` declarations are omitted before
typed geometry validation, so a broken excluded material cannot discard healthy
sibling geometry. Live siblings retain their original material validation.
Opaque and visible alpha-mask primitives count as solid triangles; alpha textures
are not sampled. The shared visibility contract mirrors the engine's strict MASK
discard (`alpha < cutoff`, default cutoff 0.5): without vertex colors or
alpha-replacing extensions, the base-color alpha factor bounds sampled alpha.
A factor below the cutoff therefore never obstructs. Equality and OPAQUE alpha
zero remain visible; palette-alpha replacement and unknown extensions do not use
this proof. Node children and skin joints remain available when a primitive is omitted.
Skinned meshes use the unanimated scene's global
joint transforms multiplied by inverse bind matrices (identity when omitted),
blended by the vertex joint weights. The skinned mesh node's transform is ignored,
as defined by the [glTF skinning specification](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html#skins).
All declared joint/weight sets participate, with weights normalized per vertex.
If skin data cannot be evaluated (for example missing joints, singular matrices,
invalid accessor ranges or joint indices), only that model falls back to its
static positions under scene/node transforms. If those positions or triangles
are also unusable, that model is skipped with the bounded diagnostic below;
healthy neighbouring models and cells continue baking. No filled-bounds fallback
is used. Unskinned geometry retains its existing transform and sampling behavior.
Missing/invalid meshes,
external buffers and unsupported primitives produce model diagnostics and no
bounds-based fallback. These optional mask omissions produce advisory notices,
including the skipped-model count and at most 32 model diagnostics, and do not
make an otherwise complete conversion incomplete. This can permit grass through unavailable obstruction
geometry. The asset root is explicit; canonicalized model files must remain
inside it. Geometry is decoded only while rebuilding exclusion masks, after
checking reuse. Fingerprints hash readable canonical file bytes even when their
geometry is malformed; inaccessible or noncanonical files use the existing
unavailable sentinel. Fully reusable warm runs decode no geometry. JSON-only
inspection classifies potential skins without reading vertex attributes; an
unclassifiable readable model takes the conservative candidate path.
Fingerprinting records missing or unreadable files immediately, including eligible
references outside grass-bearing cells or initially hidden references. Each cell
also stores the sorted unique model paths and failure reasons encountered while
baking its masks, plus a byte whose lower four bits mark expanded quadrant
admissions and whose bit four marks an expanded cell admission. Only hash-matched
reused cells restore those outcomes; rebuilt or removed cells discard their old
outcomes. Current file-read errors take precedence over stored reasons. Reports
deduplicate failures by model path, count every failed model, and retain the first
32 path-sorted diagnostics. This keeps counts and examples stable across repeated
reuse while repairs and changed reference inputs remove obsolete decode notices.
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
payloads and resolved species links in GNAM order, and every candidate reference's identity,
source payload, transform, finalized bounds, initial enable inputs throughout
its parent chain (including missing/cyclic/malformed terminals), canonical model path and SHA-256
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

Metadata rebuilds regenerate the full terrain projection and grass artifact
after terrain and converted model bounds are available. The source package's
`grass_cache.rkyv` is the previous cache, so unchanged cells can be reused. The rebuilt artifact is
validated before publication. A source version-one grass artifact can be rebuilt;
its masks are not reused and the source package remains unchanged.
`metadata-rebuild.json` records
`grass_baked_cells` and `grass_reused_cells` for that rebuild. Optional obstruction
mask omissions use the same notice policy as a full conversion.
Normal conversion with `--invalidate-cache` supplies no previous grass cache,
so every retained cell is baked. Metadata rebuild and asset repair retain their
existing rejection of that flag.

Asset repair re-bakes and validates the staged grass cache after finalizing model
bounds, reusing unchanged cells from the published cache and publishing the replacement
in the same transaction as the repaired assets and database.
If `cell_cache_preserved.rkyv` is absent, normal conversion regenerates the full
terrain projection. Metadata rebuild is another route when a complete verified
asset pack is available; repair never substitutes the capped runtime terrain cache.

## Optional ground-contact policy

The native 128-unit band keeps intersections from terrain minus 8 through terrain
plus 128, including ties. It derives from the measured terrestrial grass envelope
(94.49 units), rounded to the 32-unit mask grid plus one interval of margin.
Elevated surfaces above that band release their mask samples; grounded parts of
the same model still block. Tall aquatic grass is outside that measured envelope.
The dependency discriminator invalidates warm masks. This policy is prepared
separately pending visual acceptance; it is not a category/model denylist.

The grass-only selection projection also follows spud's importer quadrant step:
close empty painted slots, merge duplicate-texture byte weights with wrap and
remove base-equal weights on a copy, while keeping the closed texture bindings.
Thus a transformed weight can remain paired with an earlier texture binding.
The sixth slot has no authored weights; only the first five closed bindings
participate. Terrain rendering continues to use its own preserved-layer policy.
Repeated GNAM occurrences are recovered from canonical LTEX payloads even when
the database association projection keeps only one row per species. Older
packages with unavailable canonical payloads can use their explicit ordinals.

Selection restores original NULL/repeated/clamped ATXT assignments from the
canonical LAND payload when terrain normalization would lose their slots or
weights. NULL paint binds the default texture for selection and still covers
the base; it does not grow a species itself. LTEX binding and merging require
an available TXST, and that availability participates in incremental keys.
Malformed recovered selection records warn and skip locally; neighboring cells
remain publishable. This projection does not alter terrain's NULL policy.
