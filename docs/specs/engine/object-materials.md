# Per-record object appearance

Part of [#435](https://github.com/Mudcrab-Team/mudcrab/issues/435), with authored-value resolution
in [#264](https://github.com/Mudcrab-Team/mudcrab/issues/264) and fidelity accounting in
[#498](https://github.com/Mudcrab-Team/mudcrab/issues/498).

This implementation applies **the alternate-texture entries of a base record the way the retail
game applies them**. The rules come from a static trace of `SkyrimSE.exe` 1.7.104.0 (REA Q8,
`evidence/rea-20261114/q8-mods.md`; registry IDs RF-TEX-001 to RF-TEX-005). It does not complete
per-record materials or directional snow.

## Retail rules and where each is implemented

| Rule | Registry | Where |
|---|---|---|
| A reference's MODS list is applied at its 3D load, and every entry applies. Entries are keyed by 3D Index; index -1 is skipped; a later entry with the same index replaces the earlier one. | RF-TEX-001 | `ObjectMaterialPlugin::apply_swaps` (`by_index`) |
| 3D Index is a pre-order, depth-first count of the model's geometry leaves in child order. Every geometry counts, whether or not it is swapped. The 3D Name and its `:N` suffix are not read. | RF-TEX-002 | converter `geometry_preorder_indices`; the table keeps the name as record data only |
| Only shapes with a lighting shader property are swapped (the converter has no grass shader family, so grass-shader shapes are not swapped). Other shapes are skipped but still counted. | RF-TEX-003 | converter `tag_geometry_indices` tags lighting primitives only |
| The new texture set is built from the TXST alone. A slot the TXST leaves empty stays empty; the model's own texture is not kept. The whole set replaces the shape's material set (a shared material is cloned). | RF-TEX-004 | `apply_swaps` derives a material from a clone of the model material |
| An entry that names the NullTextureSet (FormID 0x28) hides the shape. | RF-TEX-005 | `Plan::Hide` sets `Visibility::Hidden` |

## Consumer contract

1. Both readers resolve the winning record of every base with a world model and remap its TXST
   FormIDs: `MODS`, or for `ARMO` the `MO2S` of its male world model, else the `MO4S` of the
   female one. The optional `base_texture_swaps` table keeps **every entry** in record order with its signed 3D Index, its
   TXST ID and a disposition: `texture_set` (a live TXST), `null_texture_set` (RF-TEX-005) or
   `missing_texture_set` (deleted, wrong type or absent). It has no one-entry, index, name or
   slot gate. Invalid array boundaries are warned and omitted.
2. Ordinary mesh conversion computes each geometry block's retail 3D Index from the source NIF
   and writes it on the exported lighting primitive as `extras.mudcrabGeometryIndex`
   (contracts `object-texture-swap-geometry-index-v1` and `-v2`). The number comes from the NIF,
   not from the order of meshes in the GLB, so exporter ordering does not matter. Shapes the
   converter excludes or whose shader is not lighting still take part in the count and carry no
   tag. No tag is written when the count is not provable (several scene roots, a block reached
   twice, an unparsed block in a child list, or an unparsed block of any type other than Havok
   collision), for skinned models, or for a static fallback: a fallback keeps no per-shape proof,
   so such a mesh receives no swaps and keeps its material.
   Havok blocks (`bhk*` and `hkPackedNiTriStripsData`) are the one known exception. The render
   parser does not parse them, but none of them derives from `NiAVObject`: collision hangs off a
   scene object's collision reference, never off a node's child list, so it cannot change the
   count. Contract `-v2` ([#723](https://github.com/Mudcrab-Team/mudcrab/issues/723)) tags
   collidable statics; under `-v1` every NIF with collision stayed untagged, which left
   1,588 of 1,607 swap bases in the `4c30b187` candidate pack without swaps.
3. `AssetCatalog` loads the optional projection once and resolves each entry's TXST slots from
   `texture_sets`. `streaming::spawn_cell` attaches each base's immutable input to its placed
   root. Missing tables preserve old-package behavior.
4. After scene instantiation, `ObjectMaterialPlugin` matches each tagged shape's index to its
   entry. A texture-set entry derives a material from the surface's own material, plain or with an
   authored depth state: base colour from TX00 and normal map from TX01, each with the original
   sampler; the emissive, specular, metallic-roughness and occlusion textures are dropped because
   the TXST does not supply them (RF-TEX-004). `mudcrab` views TX00 as sRGB. `skyrim` reads every
   swapped texture raw, and also applies the glow map to a surface that reads one, the height map
   (TX04) to a parallax surface, and the environment cube (TX05) and mask (TX02) to an
   environment surface. UV transforms, alpha mode, shader values and the shared mesh and
   scene assets stay intact. Equal (source material, TXST) pairs share the derived material.
   Cache entries hold asset IDs rather than owning material handles.

Once the scene instantiates, its result is inspectable on the placed root as `ObjectTextureSwapState`: `Pending`,
`Applied { textured, hidden }`, `Unsupported(reason)` or `Failed(reason)`. Entries that name no
tagged shape apply to nothing (`Applied { 0, 0 }` when none matches). An entry whose TXST is
missing, or that fills an unsupported slot, is reported and skipped for that shape only; the
model material stays. Reports are aggregated: one `warn!` per base record and reason, with each
instance logged at `debug!`. Scene loading and failure remain with streaming; roots without scene
children do not poll for appearance. An input without MODS has no substitution component.
Streaming readiness does not wait for this asynchronous appearance request. A `Pending`
reference can temporarily show its original NIF material; appearance acceptance must inspect its
terminal swap state separately. Cell unload releases the reference's image requests and material
handles; a newly streamed instance resolves its current catalog input again. Catalog mutation and
arbitrary live material hot reload are not supported by this slice; load a newly converted dataset
on restart.

## Retail coverage

Measured read-only on the registered pack `user-testing-main-b5259d33` (a pack converted before
this change, so the counts come from its STAT and TXST records). 1,607 of 13,152 STATs carry MODS,
with 2,616 entries and 36,711 placed references; 655 STATs carry several entries (15,819
placements) and no list repeats an index. Of the 2,616 entries, the engine can apply at least
1,994: 1,928 name a TXST that fills only TX00 and TX01, and 66 name the NullTextureSet. Ten
entries (nine STATs, 32 placements) name a TXST that also fills TX05 and are reported and skipped.
612 entries did not resolve in this measurement, which maps FormIDs outside the converter; the
conversion's own remap decides them. These STATs apply at least one entry: 1,317 of 1,607
(35,490 of 36,711 placements). Whether an entry reaches a shape also depends on that shape being
a lighting-shader primitive with a geometry-index tag, which needs the converted GLB; no NIF was
read for this count.

## Deliberate limits

- A TXST that fills a slot its profile does not apply is not applied, and its shape keeps the
  model material: TX02 to TX07 under `mudcrab`, and the specular (TX07) and detail (TX06) slots
  under `skyrim`.
- STAND-IN(#490): the swapped glow map reaches a surface only when its model has a glow map.
- The texture set flags (`DNAM`) are not read: SPEC-1 does not define their effect.
- Models with a skin, a static fallback or
  an unprovable count carry no geometry tags. Shapes whose material needs the native effect or
  emittance paths report `Unsupported`. Extended materials, external emittance
  and material playback integration remain open. #432/#437/#484 retain NIF
  geometry/material/playback ownership; cluster appearance/certification stays with
  #320/#336. No cluster admission rule changes here.
- Retail behaviour for a MODS entry whose TXST link does not resolve was not traced (RF-TEX has no
  entry). The engine reports it and keeps the model's material.
- The FormID 0x28 of the NullTextureSet is medium confidence (RF-TEX-005); the hide itself is high.
- STAT DNAM's MATO link, maximum angle and SSE considered-snow flag, and MATO's projection,
  falloff, UV, colour and pass flags remain outside this implementation. #435's existing local
  prototype owns that continuation. No maximum-angle cutoff, snow colour or layer is invented.
  Single-pass/multi-pass differences, max-angle-zero behavior, noise sampling and native shader
  coverage still require the owner's evidence and matched captures. Source publication and CPU
  material bindings do not establish snow, native runtime, performance or visual parity.

## Rebuild and verification

The table is additive; world schema stays unchanged. In-house adapter 14 and legacy projection 6
added it for `STAT`. Adapter 16 and legacy projection 7 add the rows of every other base with a
world model. A pack converted earlier needs a metadata rebuild with `--reuse-assets` to get them;
until then the engine applies the rows the pack has. `object-texture-swap-geometry-index-v1` enters the mesh output
contract, so normal conversion invalidates old GLB reuse. `object-texture-swap-geometry-index-v2`
changes only the `nif` proof again (the DDS and PEX proofs stay the same), so a pack converted
under `-v1` needs a normal conversion of its meshes; a metadata rebuild with `--reuse-assets`
refuses its `nif` kind. Metadata-only rebuilding requires a
retained mesh configuration proof that matches this contract; a proof from before this contract is
rejected before publication and requires normal conversion. At runtime, a GLB without tags
reports `Unsupported`; metadata alone never certifies a shape. Use a separate output, never
repair a registered pack in place.

Procedural CPU fixtures cover both readers' relocated full/light-plugin links and winning
overrides, MODS truncation, optional tables, unchanged TXST slots, kept entry order and signed
indices, the three retail STATs 101534, 126810 and 126829, a multi-entry list applied by index,
index -1, a repeated index, an untagged (non-lighting) shape that is counted but not swapped, an
empty TXST slot, a NullTextureSet hide, a collidable static that is tagged and swapped, a
non-Havok unparsed block that refuses the tag, a pre-order index over a built scene tree and the cases
that refuse a count, placed-root plumbing, actual NIF conversion/glTF loading/scene
instantiation, shared mesh identity, distinct bound image bytes, same-input reuse,
unload/reload, missing images, cube/non-colour rejection, old scene tags, aggregated reports and the
profile boundary. Passing these fixtures is source/CPU evidence only.

The retained native fixture verifies diffuse selection, instance isolation and positive spatial
agreement for its fixed UV/sampler settings. Its saturated colour probes do not measure a
mid-tone sRGB transfer response. Arithmetic mutant checks are finite source checks, not rendered
negative controls or proof for every UV/sampler configuration. Process-release evidence covers
the tracked family and completed launch; it does not prove arbitrary escaped descendants or
survival of an unhandled wrapper/controller death. The native fixture was not re-run for this
rewrite.
