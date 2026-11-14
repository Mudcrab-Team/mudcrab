# Master report: spud versus Mudcrab, wave 1

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Cross-area summary of the eleven wave-1 reports: what spud is, claims to verify, and pointers to the atlas, gaps, liberties and wave-2 plan.

## Executive summary

Spud is a broad, early Skyrim engine prototype with substantially more gameplay code than Mudcrab.
Its source implements saves, quests, dialogue, combat, audio, menus, animation graphs, and navigation.
Source implementation does not establish that these systems work correctly across Skyrim content.
Mudcrab has stronger documented automated testing and a cross-platform Rust and Bevy foundation.
Study Spud's Papyrus scheduler and missing-native reporting.
Study its menu adapter and converted movie runtime.
Study its audio descriptor pipeline and game-event interfaces.
Study its sectioned saves and plugin compatibility checks.
Study its animation graph, foot placement, and navigation interfaces.
Spud's rapid development, sparse conventional tests, incomplete native functions, Windows focus, and approximations limit its value as a correctness oracle.
Measured atlas counts are rows, not feature completeness or runtime results. Order: parity, Spud ahead, Mudcrab ahead, different approach, both missing, unclear.
Importer: 4/8/0/6/0/5; formats: 1/0/0/4/0/2.
Engine app: 2/21/0/0/0/3; engine core: 3/11/0/5/0/9.
Game: 0/28/0/0/0/0; UI: 1/22/0/0/0/1.
Render: 4/9/0/5/0/16; animation: 0/7/0/0/0/6.
Physics: 1/4/0/1/0/7; audio: 0/13/0/0/0/0.
Meta: 2/0/4/9/0/5. Total: 18/123/4/30/0/54, measured across 229 atlas rows.

## What spud is

The reports measure 679 visible files and 53 commits over November 6–7, 2026, attributed to chasmlol.
The reported checkout is 8c99042; the audit did not independently recount history.
Spud is MIT licensed and discloses AI assistance; its HRTF data has a separate Apache-2.0 notice.
It uses C++23, Vulkan, SDL3, Jolt, XAudio2, CMake, and vcpkg.
The checked-in build configuration targets Windows; broader platform support remains unverified.
Reports identify extensive implementation but found no conventional test framework, CTest registration, or CI workflow.
Headless scenarios, image audits, and UI tooling exist, but their presence does not establish passing tests.
Vanilla gameplay, visual parity, and complete runtime support remain unverified.
The importer report measures 107 files and 35,689 lines; the formats report measures 55 files and 10,413 lines.
The engine-app report measures 46 files and 27,293 lines; scopes overlap and must not be summed.

## Cross-report conflicts and claims to verify

All references below are file paths in the spud repository at the pinned commit. These are targeted source observations, not runtime tests.

1. Original UI movies versus native formats: resolved as conversion, not direct runtime SWF loading. [`README.md:6`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/README.md#L6) says original formats are importer-only; [`docs/UI.md:3`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L3) says the importer converts movies to `.osu`, which the runtime player executes. The original content and movie semantics remain relevant.
2. Original formats versus generic assets: the importer-only wording is too broad if it includes generic DDS. [`docs/UI.md:60`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L60) identifies cached `.dds` textures; [`src/formats/osm.hpp:140`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osm.hpp#L140) stores DDS paths. Runtime use of each format still needs an end-to-end check.
3. Importer navmesh uncertainty versus game navigation: the game reader accesses forms-backed navmesh geometry at [`src/game/navmesh.cpp:128`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/navmesh.cpp#L128), and path queries are present at [`src/game/navmesh.cpp:275`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/navmesh.cpp#L275). This settles the existence of a consumer, not completeness of imported records or runtime navigation.
4. Importer grass uncertainty versus runtime grass: [`src/engine/grass.cpp:189`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/grass.cpp#L189) implements generation and [`src/engine/grass.cpp:472`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/grass.cpp#L472) calls it. The importer report only failed to establish imported grass geometry; the findings are compatible.
5. No temporal anti-aliasing versus water history: no contradiction. [`README.md:7`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/README.md#L7) excludes TAA; [`docs/RENDERING.md:7`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/RENDERING.md#L7) allows effect-local history, and [`shaders/water.frag:207`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/shaders/water.frag#L207) is the reported water example. A static policy is not a passing audit result.
6. `.osm` version: [`docs/formats/osm.md:1`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osm.md#L1) says version 10; [`src/formats/osm.hpp:20`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osm.hpp#L20) says 11. The document itself lists version 11 at [`docs/formats/osm.md:57`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osm.md#L57). Treat the title as stale.
7. `.osr` version: [`docs/formats/osr.md:1`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osr.md#L1) says version 5; [`src/formats/osr.hpp:23`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osr.hpp#L23) says 6. The document describes a version 6 chunk at [`docs/formats/osr.md:27`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osr.md#L27).
8. `.osw` version: [`docs/formats/osw.md:1`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osw.md#L1) says version 12; [`src/formats/osw.hpp:18`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osw.hpp#L18) says 16. The title is stale; compatibility behavior needs separate inspection.
9. `.osv` version: [`src/formats/osv.hpp:18`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osv.hpp#L18) says version 2 and describes a renamed version 1 layout. The formats report says the document also says version 2; there is no established version mismatch.
10. Ragdoll API versus gameplay support: [`docs/MOVEMENT.md:74`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/MOVEMENT.md#L74) says ragdolls and constraints are not imported or simulated. The API noted in the physics report does not establish an end-to-end gameplay path.
11. Static shadow-cache example versus current targets: [`docs/RENDERING.md:28`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/RENDERING.md#L28) lists invalidated static shadow maps as an allowed cache. [`src/render/renderer_shadows.cpp:75`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/render/renderer_shadows.cpp#L75) and [`:106`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/render/renderer_shadows.cpp#L106) allocate shadow targets with frame lifetime. This supports the report's concern, but does not alone establish every redraw or caching path.
12. Date rollover: [`src/engine/game_clock.cpp:30`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/game_clock.cpp#L30) explicitly describes one month rollover per call even when several days pass. Whether this reproduces Skyrim requires comparison; it is not independently established as an accidental bug.
13. Door status: [`docs/MOVEMENT.md:72`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/MOVEMENT.md#L72) says load doors go nowhere, while the engine-app report identifies a transition implementation at [`src/engine/app_doors.cpp:906`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_doors.cpp#L906). Documentation or scope may be stale. Full integration and runtime behavior remain open.
14. Test count: the meta report identifies measured scenario files and poses, while UI documentation mentions a suite. These count different artifacts. The suite's runner, total tests, and passing state remain open.
15. Headless equivalence: [`docs/AUTOMATION.md:12`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/AUTOMATION.md#L12) describes matching render passes but a hidden window and nonblocking presentation. The performance report warns about presentation differences. Neither establishes equivalent visible-window frame pacing.
16. New-game opening: documentation and code establish a startup hook, but the exact opening content depends on imported configuration. End-to-end opening parity remains open.

## Process lessons

The meta report recommends a shared command language for headless runs and live control, explicit machine-specific performance budgets, contamination detection, and image-history comparisons. These mechanisms are implemented, but this audit did not run them. Fixed poses and a permitted pixel difference establish limited coverage rather than broad visual parity.

The measured commit history is unusually concentrated. AI assistance, broad scope, and sparse conventional test evidence make independent source review and representative runtime checks important. Mudcrab should keep platform-specific tools and machine budgets portable across its target systems.

## Feature atlas

Each table compares concepts at the granularity of its source report. The gap classes describe report evidence, not tested parity.

- [Importer](atlas-importer.md): source data, conversion, caches, and derived assets.
- [Formats](atlas-formats.md): containers, forms, texture formats, and inspection.
- [Engine app](atlas-engine-app.md): gameplay integration and player-facing systems.
- [Engine core](atlas-engine-core.md): runtime, movement, streaming, and Papyrus.
- [Game](atlas-game.md): gameplay rules, AI, quests, and navigation.
- [UI](atlas-ui.md): movie runtime, menus, HUD, and interaction.
- [Render](atlas-render.md): graphics architecture, materials, and visual effects.
- [Animation](atlas-anim.md): rigs, clips, behavior graphs, and IK.
- [Physics](atlas-physics.md): collision, movement, and dynamic-body coverage.
- [Audio](atlas-audio.md): playback, sound rules, voice, and spatialization.
- [Meta](atlas-meta.md): platform, automation, quality, and governance.

## Where Mudcrab is behind

[Ranked gaps](gaps.md) lists 25 priorities, estimated effort, first source references, and ideas to study.

## Where Mudcrab is ahead

[Mudcrab strengths](ahead.md) compares platform foundations, testing, fixtures, governance, and provenance.

## Liberties

[Liberties](liberties.md) separates deliberate choices from gaps and groups them by impact.

## Where to go deeper

[Wave 2](wave2.md) ranks targeted source investigations and identifies their issues or milestones.
