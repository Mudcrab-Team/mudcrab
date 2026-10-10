# Skyrim subsurface and foliage audit — 2026-10-10

The current renderer supplies no Skyrim soft-light, rim-light, back-light or subsurface response. Two-sided foliage receives ordinary lighting. The missing response may contribute to dark tree crowns, but it does not establish the cause of dark wood walls. The separately proven legacy DDS input-view discrepancy is tracked in [the native texture transfer audit](skyrim-native-texture-transfer-20261010.md).

This is a source, converted-package and static shipped-shader audit. It does not establish a selected retail draw, runtime parameter values or visual parity. [The companion JSON](skyrim-subsurface-audit-20261010.json) retains counts, source identities, shader hashes and limits without including game binaries or shader bytecode.

## Active shading and lost inputs

[Ordinary native shading](../../crates/engine/src/nif_material/native_common.wgsl) adds shadowed Lambert diffuse, affine ambient, constant emission and an injected IBL term, with separate half-vector specular. It has no wrap, thickness, scattering texture or transmitted-light term. Native two-sided meshes disable culling while retaining the original normal. The Bevy PBR fallback flips backface normals, but `diffuse_transmission`, `specular_transmission` and `thickness` remain zero. No converted GLB publishes transmission or volume extensions.

| Authored input | Retained | Current gap |
| --- | --- | --- |
| Double sided, SLSF2 bit 4 | Flag and `double_sided` | Culling/ordinary backface lighting is active; it does not imply transmission |
| Soft lighting, bit 25 | Raw flag and slot 2, classified `unclassified` | Native support rejects it; fallback does not sample a matching soft response |
| Rim lighting, bit 26 | Raw flag and texture slots | Matching authored response unsupported |
| Back lighting, bit 27 | Raw flag and slot 7 | Slot 7 is always classified `specular`; no matching back-light response |
| Lighting Effect 1/2 | Vendor parser reads `lighting_effect_1` and `lighting_effect_2` | `ValidatedNifMaterial` omits both, losing source rolloff/power in `nativeSurface` |
| Tree animation, bit 29/type 12 | Flag/type and converted static geometry | Specialized lighting, wind/deformation and native tree variant unsupported |
| Face/skin/hair and snow | Generic type/flags; snow SSS/rim INI entries catalogued | Specialized scattering, tint, anisotropic or snow responses unsupported |
| GRAS records | 33 grass types and 54 LTEX links | No RunGrass material/population path; placed fern/shrub NIF surfaces are separate |

The [native whitelist](../../crates/engine/src/nif_material.rs) rejects the soft/rim/back/tree bits. The [converter](../../crates/converter/src/material.rs) keeps the texture views but loses Lighting Effect 1/2 and assigns slot 7 the specular meaning unconditionally. A back-light texture can therefore reach a compatibility specular-color texture. Its visual impact has not been measured, and the audited Riverwood foliage subset has no back-light flags.

## Authored and loaded scope

All 25,388 GLBs parse successfully. The package contains 63,981 classified source materials and 1,657 non-rendering placeholders. Counts below describe authored materials, not visible scene instances or pixels.

| Scope | Models | Materials | Soft light | Tree animation | Double sided | Rim light | Back light |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Whole converted package | 25,388 | 63,981 classified | 2,128 | 498 | 5,484 | 272 | 2,236 |
| Riverwood foliage paths in 25 static-reference cells | 69 | 162 | 53 | 69 | 81 | 0 | 0 |

The Riverwood query covers 294 distinct static model paths around grid `(5,-12)`, including disabled and out-of-view references. NPC and other non-static model paths are outside its scope. All 53 soft-light foliage materials retain slot 2. The common-domain fog capture reports 687 loaded primitive instances rejected for the soft bit, including soft/tree and soft/tree/skinned combinations; this does not count visible foliage pixels.

`meshes/landscape/trees/treepineforest01.glb` provides a concrete case. Branch materials 0 and 3 carry SLSF2 `0x22008031`, combining soft lighting, tree animation, two-sided cutout rendering and slot-2 `treepineforestbranchcomp.dds`. The slot-2 view is retained as linear/unclassified but its soft response is omitted. Bark materials remain separate and can qualify for ordinary native shading.

## Recovered directional soft response

Shipped Lighting PS `00006601` adds the soft permutation to ordinary PS `00006201`. Tree PS `0C006601` contains the same inspected arithmetic. Six DXBC files and their decoded assembly files were independently hash-checked against the private extraction evidence. Their identities are retained in the companion JSON. Source-assisted naming identifies `t12` as the RimSoftLighting texture and `cb1[7].x` as subsurface rolloff `p`:

```text
x = dot(N,L)
u = saturate((x+p)/(1+p))
d = saturate(x)
q(t) = t*t*(3-2*t)
softFactor = saturate(q(u)-q(d))
addedDiffuse = softTextureRGB * sunRGB * softFactor
```

In these inspected directional branches, the added soft term has no directional-shadow-sample multiplier; the ordinary Lambert term does. It can add material-specific fill around the terminator and into a wrapped back-facing range. This statement is restricted to the recovered static directional arithmetic. Runtime `p`, texture binding transfer, CPU flag translation and selected retail draws remain unverified. Local-light arithmetic is not generalized. Rim/back variants are hash-verified, but their equations were not recovered.

Global Bevy diffuse transmission would not reproduce this texture-weighted equation. An implementation needs retained source rolloff/power, correct shared texture semantics and transfer, explicit permutation selection, defined tree/skin/deformation limits, independent equation probes and matched retail validation. Missing foliage fill and dark wall texture inputs require separate controls; neither finding justifies a whole-scene brightness change.

The source snapshot hashes are recorded alongside the rechecked hashes. `nif_material.rs` changed during the independent texture-transfer work; the inspected response shader, converter contract, parser and other recorded files still match the audit snapshot. This audit made no renderer, asset or process changes and ran no build or GPU probe.
