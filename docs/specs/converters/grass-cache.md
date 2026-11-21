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
by the winning LTEX record, in GNAM occurrence order. Duplicate occurrences are
retained; unresolved or deleted species are filtered without reordering
survivors. A winning override replaces the entire list, including an empty list.
Runtime species caps can take the first N in this order; the global rule table
remains sorted by FormID for binary search. Its 17 by 17 row-major blend field stores
transformed byte-opacity weights and the base residual. Base layers are marked explicitly and ordered first. Existing databases without stored GNAM
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
The placement layer adds these sampling fields and base flags and changes selection
opacity inputs. The current producer (`GRASS_CACHE_VERSION`) also retains non-emitting species slots and recovers malformed LAND items locally;
producers ten through twelve, both prior producer-nine variants and older grass artifacts rebuild. Other asset cache identities
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
Incomplete grass rules and missing model paths retain non-emitting species slots;
unresolved/deleted species links are silently filtered. Empty model paths mark these
placeholders. Decodable rule fields retain the reference's conditional random draws;
missing DATA fields use zero defaults, and nonfinite fields are cleared safely.
Negative spacing produces no grid. Placeholders count toward the per-texture and
per-spot limits and never publish instances. Raw database records remain available for diagnostics.
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

When terrain contains no texture linked to a grass rule with a model, the producer
skips obstruction-reference collection and model inspection. Rule warnings and
cache publication still apply. The terrain/link gate is conservative: a linked
modeled rule is sufficient to retain the ordinary obstruction path. This scan
gate leaves the cached rule and link selection unchanged.

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

## Obstruction policy and integrity

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
Malformed assignments, individual weights and trailing weight bytes are omitted
with a bounded report warning; usable layers and weights in the same cell and
neighboring cells remain publishable. Only unreadable record framing skips a whole cell. This projection does not alter terrain's NULL policy.

Permanent obstruction masks include only STAT, MSTT, ACTI, FURN, CONT and DOOR
bases. Pick-up clutter, vegetation and unknown base types do not obstruct grass.
G2 producer 9 introduced static-like obstruction filtering; this combined selection
producer advances through 10 and 11 to the current `GRASS_CACHE_VERSION` so earlier selection producers cannot reuse their masks. Older masks
are rebuilt. Corrupt GLB containers remain limited to their projected bounds.
Each selected scene node is inspected once, including on static skin fallback.
Repeated roots or child edges omit that optional model with a diagnostic. The
cell-input hash includes the unique-node inspection recipe, so earlier masks
rebuild once before warm reuse. Raw model digests and archive versions retain
their existing contracts.

Before publication, the producer rereads all initially readable fingerprinted
models, including geometry cache hits and models used only by reused cells.
Replacement or removal during baking requests a retry; initially unavailable
models remain optional omissions. Full conversion
checks cancellation between references, cells and quadrants.

The grass archive is recorded as `derived/grass-cache` in manifest entries, with
its size and SHA-256, so quick/full checks detect removal and full checks detect
byte changes. Metadata rebuild regenerates this entry after baking. Mesh and texture recipes retain the pinned main contracts.

Malformed LAND paint is recovered before either reader admits the winning record.
Height, normal, colour dimensions and complete record framing retain their existing
validation. The in-house projection recovers original BTXT/ATXT/VTXT occurrences
when typed array rejection would otherwise erase valid neighboring weights, remaps
their identities through the same load-order mapping, and retains the existing
opaque-field policy for other fields. Adapter 14 and legacy projection 6 invalidate
older metadata projections; converted mesh/texture bytes remain independently proven.
Grass selection restores only the single LAND chosen by terrain's supplying-plugin,
native-source-order and FormID precedence, never a merge of losing source records.

Repeated valid BTXT assignments and VTXT vertex writes follow source order: the
last valid base per quadrant and last valid opacity per vertex win, matching the
reference importer. This defined precedence does not count as malformed data loss.
Earlier assignments do not reject the LAND or remove healthy neighboring items.

### Placed-STAT MODS visibility (producer 10)

Permanent obstruction masks omit lighting primitives hidden by the winning STAT's
NullTextureSet MODS entry in the runtime's supported standard-material swap path.
Signed negative indices are ignored; the last entry at each geometry index wins,
so a later retexture or missing target restores the model's visible shape. Only
matching eligible geometry tags enter each reference's hidden-index set. That set
enters cell input identity, so MODS-only hide/reveal overrides rebuild affected
masks while unrelated cells and texture-only changes remain reusable. Models shared
by distinct bases retain separate visibility variants inside the bounded triangle
cache. Source GLB bytes remain immutable and their full digest still guards reuse
and changes during baking.

The offline material classifier mirrors the current runtime binding exclusions:
external emittance and native depth, skin, hair or palette wrappers retain their
geometry because the current MODS query cannot hide those bindings. Native materials
that still use the standard binding participate. This is a bounded mirror of that
current path, not a claim that every material path supports MODS. The raw-colour
Skyrim visual profile still declines MODS at runtime; these masks describe the normal
converted-world path. Producer 10 rejects producer-9 masks even when their other
inputs happen to match. Mesh, texture and terrain cache identities are unchanged by
this grass policy change.

The current producer (`shared::grass::GRASS_CACHE_VERSION`, 13) combines MODS-aware obstruction masks with reference-order
placement, retained non-emitting species slots and item-local LAND recovery. It
rejects prior G7 producer-11 artifacts and the lower layers' producer-12 layout.
G2 through G6 use `GRASS_CACHE_VERSION` 12 to restore model-less selection slots. Mesh, texture and terrain cache
identities remain independently versioned.

Resolved model-less species retain their authored cap slots and decodable sampling
fields, using zero for absent or non-finite scalar values. Per-spot selection consumes
the reference random draws for those slots before skipping emission. Thus
`[A, model-less, B]` with two admitted slots emits A only. G7's configured limit is
checked against the count already added, before insertion: setting 1 admits two slots, while default 2
admits all three. Each admitted model-less slot counts toward that limit; the G7
reference stream also preserves subsequent species' positions.

Runtime activates a reference's texture swaps as a group once its replacement
textures are ready. If a replacement texture on another shape fails to load, the
reference keeps its original shapes, including a shape targeted by NullTextureSet.
The baked mask already omits that hidden shape, so grass can occupy geometry that
stays visible in this failure case. Applying NullTextureSet hides independently of
replacement-texture failures is a runtime follow-up; the current bake retains the
bounded successful-swap contract above.
