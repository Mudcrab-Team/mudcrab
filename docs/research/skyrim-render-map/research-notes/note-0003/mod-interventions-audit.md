# Mod interventions audit

## What the light-cap evidence establishes

The pinned SE shader evidence supports a seven-entry point-light representation and a four-entry shadow selector. That establishes shader capacity. It does not establish which lights the target executable assigns to a geometry, how its native list is ranked or truncated, or whether the familiar four-light mesh behavior comes from the pass builder, a mesh-specific path, or legacy lore. The 4-, 6-, and 7-light claims therefore remain separate: source-backed representation, authoring workaround, and community observation.

The pinned Community Shaders Light Limit Fix code filters non-strict lights from portal graphs before the vanilla per-object path, then gathers lights for clustered forward shading. Its source also places a deferred interval around opaque batches for ambient, specular, and indirect composition. The captured LLF source has a 1024 gathered-light buffer, 64-pixel screen tiles, 32 depth slices, and a 15-entry strict-light payload. One capacity needs follow-up: C++ allocates 128 indices per cluster while the HLSL common header declares 256. The near and far depth planes are supplied from the camera, so values 1 and 16384 are not universal fixed limits.

The claim that current releases raise the shadow limit to 16 remains unverified. The cited v1.6 prerelease reference did not resolve through GitHub’s tag API. The official v1.9.1 page lists LLF 3-2-0 but does not establish that shadow change. These source revisions describe code capability; they do not show which CS features are installed or enabled.

## What the mods change

Light Placer creates `NiPointLight` objects and submits them through `ShadowSceneNode`. That puts its lights on an engine-light path; exact target selection still depends on flags, native behavior, and whether LLF is active. The Intellightent claim about choosing four shadow casters is forum-derived here, with no pinned plugin source or active configuration. Lux, ELFX, COTN, and Relighting Skyrim mesh or room workarounds are plausible asset practices, but the supplied pages do not establish a universal four-light cap or the precise cause of each split.

ENB’s author documentation exposes `UseOriginalPostProcessing`; it says enabling the setting uses the vanilla postprocessing algorithm. It does not establish the default. Pinned CS Effects11 code only takes over the tonemap when the feature is enabled, the setting is false, and the effect chain writes output. The supplied statement that CS dropped ENB-style particle lights is too broad for the pinned source: LLF still exposes optional particle-light collection, while compatibility with ENB Light emitters was not established. ReShade’s final-frame scope is retained as a guide-level claim; its page was not preserved in this audit.

## What remains open for the vanilla target

The Address Library values in the supplied note are source-side relocation labels, not proof that the matching executable or hook is active. Likewise, source capability and ecosystem popularity do not justify changing the requested vanilla SE/AE visual reference to a CS plus Light Placer baseline. Treat those mods as optional interventions for comparison.

Mudcrab’s current light budget is explicitly separate: its `SkyrimLight` marker lets `budget_lights` choose which converted Skyrim lights are enabled, while unmarked engine lights stay outside that budget. This is a useful place to record future selection evidence, but it is not proof of Skyrim’s native light ranking. The CS exponent 1.6 is an empirical fit in CS code, not a vanilla transfer-function mandate.

The remaining gates are target-specific: inspect the exact Skyrim 1.7.104 executable path, capture a controlled five-to-eight-light draw, trace shadow-mask channel assignment, resolve the pinned 128/256 cluster-cap mismatch, and pin the exact mod versions/configurations before making claims about active behavior. No game, GPU, or build run was part of this audit.

## Semantic check against N003

All 26 claims now carry an exact factual excerpt from `supplied-notes.md`; the excerpts were checked as literal substrings. The four-, six-, and seven-light reports remain separately attributed, and the four-pointer builder signature is present in N003 line 76. Lux mesh splitting is attributed to the supplied report, not treated as proof of a universal cap. ENB’s “normally” wording does not establish a default; CS’s 1.6 value remains an empirical fit; and the CS plus Light Placer baseline remains an attributed recommendation rather than evidence of vanilla target fidelity.
