# Mod interventions audit

Source: `supplied-notes.txt` (SHA-256 `20589ecb5bd3892a4671143a9159ca190b701fc62850dcd87a8e545276c870a6`).

Assessment contains 29 atomic claims: supported_static=2, partially_supported=18, contradicted=3, unverified=6. Evidence is static source or author documentation unless stated otherwise; it does not establish a loaded mod, enabled feature, retail behavior, or visual outcome.

The largest corrections are:

- **LLF is bounded.** The pinned CS commit builds finite light buffers, clamps the collected light count to `MAX_LIGHTS`, and caps per-cluster storage. The article’s “over two billion” and “every active light” claims do not match that source. The checked source exposes a further value mismatch: CPU `CLUSTER_MAX_LIGHTS=128`, HLSL `MAX_CLUSTER_LIGHTS=256`; resolve this before publishing a single cluster limit.
- **CS is a hybrid intervention.** It inserts selected replacement shaders and compute/deferred passes under feature predicates. Source does not prove the entire Creation Engine forward renderer is replaced or that available features are active.
- **Shadows are conflated.** The pinned Screen Space Shadows pass uses screen depth and directional-light projection; it does not prove contact shadows for all lights. Neither the global native four-shadow cap nor the claimed flickering cause has target 1.7.104 byte/dataflow proof in this audit.
- **ENB claims need versioned boundaries.** Author pages show Skyrim SE 0.505 as of the 2026-10-10 retrieval (news dated 2026-08-05). The effect documentation exposes a `UseOriginalPostProcessing` option, which contradicts the article’s unconditional “replaces native post-processing” formulation. The direct author-page fetches returned HTTP 406 in the existing receipt; browser-derived excerpts are recorded with that limitation.
- **Feature support is not feature activity.** TruePBR, SSS, HDR Display, Linear Lighting, SSGI, IBL and screen-space shadows appear as distinct CS features. Release metadata and feature settings must be checked before describing them as active. ENB LUT and particular `enbeffect*.fx` pipeline claims remain unverified here.
- **The asset-workaround paragraph overreaches.** Current author-page evidence documents several record, placement, radius/fade and module changes across Lux, ELFX and Relighting Skyrim. It does not establish general mesh splitting into dozens of BSTriShapes, room assignment that increases light capacity, or a shared implementation across those mods. Lux’s portal-strict behavior is version/option scoped and must not be generalized from v6.9 to current v7.1.
- **Native and Mudcrab evidence remain separate.** Mudcrab uses Bevy clustered forward lighting in `crates/engine/src/lights.rs`; no ENB or CS runtime integration exists. That is not evidence for Skyrim’s native light/shadow limits. The target-native executable remains SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`; offsets from other releases do not establish behavior for this target.

## Source pins and retrieval

- Community Shaders: commit `d001d4de3cde4feeec539dbaca5481cdda2b1b15`, branch `dev`, fetched 2026-10-10; 800 tracked files; manifest SHA-256 `173b498bedc3dff389d409bc32b634b3ea4dece91769e9dd710fa8b61e289584`. Local checkout: `/Users/taylor/.local/share/mudcrab-research/render-map-20261010/mod-hooks/community-shaders`.
- ENB: author page observed 0.505 for Skyrim SE; documentation fetch limitations are preserved as receipts M-D01–M-D03 in `mod-rendering-hooks.json`.
- Lux/ELFX/Relighting Skyrim: author page extracts are in `mod-content-rendering.json`; plugin/archive/NIF payloads were not acquired, so implementation assertions stay at documentation scope.

## Remaining gates

1. Trace the relevant hooks and light/shadow producers against the original 1.7.104 bytes and dataflow.
2. Resolve the LLF light and per-cluster buffer bounds for the pinned build; record configuration, truncation and overflow behavior.
3. Pin current ENB/CS releases and settings before comparing actual active features.
4. Compare exact Lux/ELFX/Relighting archives before attributing mesh partitions, room/portal behavior or light-radius edits.
5. Use controlled retail tests to support performance or visual-outcome claims. No mod, game or Mudcrab runtime was launched for this note.
