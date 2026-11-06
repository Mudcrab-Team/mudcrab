# Per-record object appearance

Part of [#435](https://github.com/Mudcrab-Team/mudcrab/issues/435), with authored-value resolution
in [#264](https://github.com/Mudcrab-Team/mudcrab/issues/264) and fidelity accounting in
[#498](https://github.com/Mudcrab-Team/mudcrab/issues/498).

This first implementation slice applies a **diffuse-only STAT MODS assignment to an ordinary,
single-shape NIF instance**. It does not complete per-record materials or directional snow.

## Consumer contract

1. Both readers resolve the winning STAT and remap its MODS TXST FormID. The optional
   `base_texture_swaps` table retains the base, entry position, shape name/index, TXST ID,
   resolved diffuse path and disposition. Only a live TXST with TX00 and no other nonempty
   texture slots receives `diffuse_only`. Missing, deleted and wrong-type targets receive
   `missing_texture_set`; an absent diffuse receives `missing_diffuse`; additional authored
   slots receive `unsupported_texture_slots`. Invalid array boundaries are warned and omitted.
2. Ordinary mesh conversion certifies a source with one geometry block, one validated named
   shape, one emitted mesh and primitive, no skin or parser/static fallback, and lighting shader type 0. Its primitive carries
   `extras.mudcrabObjectDiffuseV1 = { "shapeName": <source name>, "shapeIndex": 0 }`.
   This is source identity; no STAT-specific texture or layer enters the shared GLB.
3. `AssetCatalog` loads the optional projection once. `streaming::spawn_cell` attaches each
   base's immutable input to its placed root. Missing tables preserve old-package behavior.
4. After scene instantiation, `ObjectMaterialPlugin` requires exactly one certified surface,
   one MODS assignment, exact name agreement, index zero and a StandardMaterial binding without
   external emittance. It requires a 2D TX00 KTX2 with an sRGB-compatible format and derives a material from the surface's
   own material. Only the diffuse image changes. The cloned image uses an sRGB view and retains
   the original diffuse sampler; UV transforms, shader values, alpha mode, other images and
   shared mesh/scene assets remain intact. Equal source-material/image pairs share the derived
   material. Cache entries hold asset IDs rather than owning material handles.

Once the scene instantiates, its result is inspectable on the placed root as `ObjectTextureSwapState`: `Pending`,
`Applied { texture_set }`, `Unsupported(reason)` or `Failed(reason)`. Unsupported inputs and
image-load failures keep the original NIF material and log their base ID and reason. Scene loading
and failure remain with streaming; roots without scene children do not poll for appearance. An input
without MODS has no substitution component. Settled references are not scanned every frame.
Streaming readiness tracks model assets and does not wait for this asynchronous appearance
request. A `Pending` reference can temporarily show its original NIF material; appearance
acceptance must inspect its terminal swap state separately.
Cell unload releases the reference's image request and material handles; a newly streamed
instance resolves its current catalog input again. Catalog mutation and arbitrary live material
hot reload are not supported by this slice; load a newly converted dataset on restart.

## Deliberate limits

- The `mudcrab` visual profile is supported. The `skyrim` profile reports unsupported pending
  integration with #419's profile-owned raw UNORM material path. This change adds no profile,
  lighting, grade or native shader equation.
- This is a bounded diffuse substitution, not general native TXST replacement semantics.
  Multi-shape indexing, other model-bearing record types, multiple assignments, extended
  materials, external emittance and material playback integration remain open. #432/#437/#484
  retain NIF geometry/material/playback ownership; cluster appearance/certification stays with
  #320/#336. No cluster admission rule changes here.
- All eight TXST slots already have operational terrain/skin encoding or composition uses.
  Their existing publication and consumers are unchanged. Rejecting additional slots in this
  new object slice does not classify those slots as unused.
- STAT DNAM's MATO link, maximum angle and SSE considered-snow flag, and MATO's projection,
  falloff, UV, colour and pass flags remain outside this implementation. #435's existing local
  prototype owns that continuation. No maximum-angle cutoff, snow colour or layer is invented.
  Single-pass/multi-pass differences, max-angle-zero behavior, noise sampling and native shader
  coverage still require the owner's evidence and matched captures. Source publication and CPU
  material bindings do not establish snow, native runtime, performance or visual parity.

## Rebuild and verification

The table is additive; world schema stays unchanged. In-house adapter 13 and legacy projection 5
identify the metadata change. `single-shape-object-diffuse-v1` enters the mesh output contract,
so normal conversion invalidates old GLB reuse. Metadata-only rebuilding requires a retained
mesh configuration proof that matches this contract; a proof from before this contract is
rejected before publication and requires normal conversion. At runtime, an untagged legacy GLB
remains unsupported; metadata alone never certifies its shape. Use a separate output, never
repair a registered pack in place.

Procedural CPU fixtures cover both readers' relocated full/light-plugin links and winning
overrides, deleted/wrong-type/missing targets, MODS truncation, optional tables, unchanged TXST
slots, placed-root plumbing, actual NIF conversion/glTF loading/scene instantiation, shared mesh
identity, distinct bound image bytes, an authored-link intervention with unchanged controls,
same-input reuse, unload/reload, missing images, cube/non-colour rejection, name/index mismatch, old/duplicate scene tags
and the profile boundary. Passing these fixtures is source/CPU evidence only.

The retained native fixture verifies diffuse selection, instance isolation and positive spatial
agreement for its fixed UV/sampler settings. Its saturated colour probes do not measure a
mid-tone sRGB transfer response. Arithmetic mutant checks are finite source checks, not rendered
negative controls or proof for every UV/sampler configuration. Process-release evidence covers
the tracked family and completed launch; it does not prove arbitrary escaped descendants or
survival of an unhandled wrapper/controller death.
