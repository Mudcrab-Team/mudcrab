# Spud cross-check reports

> Drafted with AI assistants (Claude and other models); research notes, not checked line by line by a human. A human owner must read the source behind any claim before building on it. See [AI_POLICY.md](../../AI_POLICY.md).

These are Mudcrab's read-only notes on [spud](https://github.com/chasmlol/spud), another engine that runs Skyrim SE from the player's own install. They map what spud implements, area by area, and compare it with Mudcrab. They are shared so every contributor can use the same findings. Nothing in them was built or run by us.

**Spud is a map, not an oracle.** It is a one-contributor, days-old, AI-assisted project, with no conventional tests or CI found by these audits. Nobody has compared its behaviour with Skyrim. Use the reports to see what a subsystem has to handle. Do not treat spud's numbers or quirks as the truth about Skyrim.

**Mudcrab code is written by a human owner from the written description, with attribution to spud, per [AI_POLICY.md](../../AI_POLICY.md). Never copy spud code.** Spud's code is MIT and Mudcrab is GPL-3.0; the licences are compatible only while spud's notice stays with any copied code. Re-implementing from the description avoids the question.

## Contents

- [Pinned commits](#pinned-commits)
- [Licence and credit](#licence-and-credit)
- [How to read these reports](#how-to-read-these-reports)
- [Top findings](#top-findings)
- [Known stale: spud moved after the audit](#known-stale-spud-moved-after-the-audit)
- [Index by milestone](#index-by-milestone)
- [All reports](#all-reports)
- [Corrections made when publishing](#corrections-made-when-publishing)

## Pinned commits

| Role | Full SHA | Date (UTC) | Subject |
| --- | --- | --- | --- |
| Audited (wave 1, master, wave 2) | [`8c9904207a5c573a0193fb903fb5c433f127f102`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102) | 2026-11-07 | Merge branch 'main' into claude/player-db97be |
| Re-audited (player controller, UI, stale-claim check) | [`e5f2de4b5a612eef3f2cd15e4a84672e9e0a0f1c`](https://github.com/chasmlol/spud/commit/e5f2de4b5a612eef3f2cd15e4a84672e9e0a0f1c) | 2026-11-13 | Comments say what the engine does in our own words |

Every `file:line` reference to spud in these reports is a permalink at the commit that the report used: `8c990420` for the wave-1, master and wave-2 reports, and `e5f2de4b` for the re-audit. Some bare file names in the original notes were matched to a path in the pinned tree; where two spud files share a name, the choice follows the report's topic. Check the link target if a line looks wrong.

**Line numbers can be off.** An independent review before publication spot-checked about 400 claims against the pinned source and fixed the errors it found. Other line numbers may still be off by a few lines or more, and a range that ran past the end of a file now links to the end of that file. Read the surrounding code before relying on a citation.

The pinned range `8c990420..e5f2de4b` is 110 commits (80 non-merge, 30 merge; 186 files, +7,681 / -841 lines), measured by the re-audit.

## Licence and credit

Spud is by its authors, the "spud contributors" (repository owner [chasmlol](https://github.com/chasmlol)). We thank them for publishing it under a permissive licence. The reports quote only short snippets for evidence and copy no code.

Spud's root `LICENSE` at both pinned commits, kept here so the notice travels with the reports:

```text
MIT License

Copyright (c) 2026 spud contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

One spud data file is an exception: `src/audio/hrtf_data.inc` (headphone HRTF filters) derives from the SADIE II database of the University of York under Apache-2.0 (`tools/hrtf/LICENSE-SADIE-II.txt` in spud). The re-audit found no licence change in the pinned range ([section 6](reaudit-e5f2de4b.md)). Spud's own README says much of its code was written with AI assistance.

## How to read these reports

**What was done.** Every spud finding is a source read: someone read the cited lines. Nothing was built, run, profiled or compared with Skyrim by us. Counts are marked measured (`rg`, `wc`, `find`) or estimated. Spud's own "measured" comments are claims, not receipts.

**Evidence levels** in the feature tables (wave 1 and wave 2):

| Mark | Meaning |
| --- | --- |
| `impl+tests` | Implemented, and covered by tests we found. Spud has almost no such rows. |
| `impl` | Implemented in code that was read; no test found. |
| `partial` | Some paths work; the row names the TODOs or stubs that remain. |
| `stub` | Declared but not working. |
| `doc-only` | Claimed in spud's docs; not found in code. |
| `unverified` | Could not be decided; the row says why. |

The master atlas uses gap classes instead: parity, spud ahead, Mudcrab ahead, different approach, both missing, unclear. A gap class is a reading of the reports, not a tested result. The re-audit adds `derived` (computed from read values), `STALE` (a line number or claim that moved) and `NOT FIXED` / `UNCHANGED` (a past finding re-checked at the new commit).

**Read `unverified` literally.** It means the claim has no evidence here. Do not turn it into a requirement or a fact.

**Source versus doc.** Spud's docs are sometimes stale against its code (for example `docs/MOVEMENT.md` under-claims, and several format documents carry old version numbers). The reports mark which side they trust.

**Mudcrab references** are repository-relative and describe `main` as of 2026-11-13 unless a report names a commit. Mudcrab line numbers drift; re-check them. [mudcrab-inventory.md](mudcrab-inventory.md) is a dated snapshot and is stale.

**Local receipts.** A few numbers come from runs on a developer machine whose receipt is not in this repository, for example the 34.5 GiB Apple Silicon cluster working set in [render-submission](wave2/render-submission.md). The report text says "measured locally; receipt not in the repository". Treat those numbers as unreproduced.

**Wave 1** is a coarse source map of 11 areas plus a master comparison. **Wave 2** is 14 deep dives that the master ranked. Wave 1 is coarser, and the wave-2 and re-audit reports correct it in places (for example the physics report).

## Top findings

1. **Mudcrab's gap is gameplay rules, not rendering.** Spud has quests, conditions, Papyrus, saves, animation graphs, AI packages and combat in code. Mudcrab's converter, formats, streaming and renderer are level or ahead. Mudcrab's runtime for scripts, quests, saves, graphs and navigation does not exist yet. See [summary](summary.md) and the [master report](master/README.md).
2. **The recurring spud failure is silent defaults.** A missing Papyrus native returns a default after one warning. An unknown condition function returns 0. A missing Havok node class is dropped at debug level. Audio conditions return 0. Mudcrab should count and report every fall-through; that is the best parity metric. The re-audit found three silent-default problems still unfixed at `e5f2de4b`: a dead `broad_phase_dirty` flag, a hard-coded character layer mask, and NPC perks that act as identity ([re-audit section 5.2](reaudit-e5f2de4b.md)).
3. **Player controller feel differs in ten ways.** The re-audit ranks them by estimated impact; all are read-source or derived, none tested. The top three: spud climbs steps as a continuous ramp (Mudcrab lifts the body in one tick), spud's first-person eye trails on a spring (Mudcrab's is rigid), and spud interpolates the view between 60 Hz ticks (Mudcrab shows none; unverified). The controller core did not change between the two pinned commits ([re-audit section 3](reaudit-e5f2de4b.md)).
4. **Tree impostors are the biggest far-view visual win on offer.** Spud bakes an 8x8 hemi-octahedral impostor atlas with coverage in alpha and a three-frame blend, where Mudcrab has billboards only ([grass-far-water](wave2/grass-far-water.md)). The same report lists the risks: atlas seams, dark crowns if coverage is lost in mips, double trees at the swap, and alpha-test threshold mismatch.
5. **Papyrus:** all 36 opcodes exist in Mudcrab's emitter but the runtime is unwired. Six design rules transfer: attachment order, init gate, latent tokens, clock domains, bounded slices, and ranked missing natives. One small converter bug is filed as [#571](https://github.com/Mudcrab-Team/mudcrab/issues/571) (of 23,539 conditional jumps scanned in one converted pack, 7 test an `Int` and 1 tests an array directly) ([papyrus-vm](wave2/papyrus-vm.md)).
6. **Behaviour graphs are the animation state machine, not the AI.** Mudcrab has no decoder. A separate Rust Havok library is under licence discussion; see issue [#572](https://github.com/Mudcrab-Team/mudcrab/issues/572) ([anim-graphs](wave2/anim-graphs.md)).
7. **Save/load:** spud never compares plugin hashes on load and keys everything by FormID, so a reordered plugin set can load wrong values. Mudcrab can do better with the content-hash design it already has ([save-load](wave2/save-load.md)).
8. **Quests and conditions:** unsupported conditions pass `== 0` tests by default; stage order and two-phase alias fill are the traps ([quests-conditions](wave2/quests-conditions.md)).
9. **Navigation:** no path spans a space; links ignore type and flags; cover, search grid and precomputed paths are decoded but unused; off-screen actors teleport. Mudcrab has the data and no consumer. The re-audit says the "links ignore type" claim is now stale ([navigation](wave2/navigation.md)).
10. **UI:** the movie-to-engine channel is one function. The cost is about 500 lines of C++ per-frame HUD rules, which a Bevy port must own. The AVM1 interpreter does not transfer ([ui-movie](wave2/ui-movie.md)).
11. **Audio:** ingestion (26 record types to 34 chunks) is reusable; playback is Windows-only. A portable backend needs 15 capabilities. Record the codec and HRTF licences from the start ([audio](wave2/audio.md)).

## Known stale: spud moved after the audit

Spud moved 110 commits after `8c990420`. **All wave-1 and wave-2 line numbers predate `e5f2de4b`.** Their permalinks stay correct for the audited commit, but the lines may have moved, and some claims changed. The re-audit lists the changes ([section 5](reaudit-e5f2de4b.md)). Four wave-2 claims are now wrong or incomplete: "multi-body dynamic models never simulate", "navmesh links ignore type", "conditions on aliases are unsupported", and "acceleration is a placeholder ramp". The files `world.cpp`, `ai.cpp`, `actors.cpp`, `app_items.cpp` and `player.cpp` moved most. Issue [#608](https://github.com/Mudcrab-Team/mudcrab/issues/608) tracks a re-run at a current spud head. Re-read the code at the current spud head before you rely on a finding.

## Index by milestone

Milestone titles are from [the milestone list](https://github.com/Mudcrab-Team/mudcrab/milestones). "Reports" lists the reports that bear on the milestone; "Issues" lists issues, filed from these reports, that carry that milestone. Each issue number was checked on GitHub on 2026-11-14. Issues [#600](https://github.com/Mudcrab-Team/mudcrab/issues/600) to [#606](https://github.com/Mudcrab-Team/mudcrab/issues/606) do not cite these reports and are not listed.

| Milestone | Reports | Issues |
| --- | --- | --- |
| [A1: Luau runtime runs converted Papyrus](https://github.com/Mudcrab-Team/mudcrab/milestone/2) | [papyrus-vm](wave2/papyrus-vm.md), [engine-core](wave1/engine-core.md) | [#571](https://github.com/Mudcrab-Team/mudcrab/issues/571) |
| [A2: Game facts and condition evaluator](https://github.com/Mudcrab-Team/mudcrab/milestone/11) | [quests-conditions](wave2/quests-conditions.md), [game](wave1/game.md) | none |
| [A3: World state and change records](https://github.com/Mudcrab-Team/mudcrab/milestone/12) | [save-load](wave2/save-load.md) | none |
| [A4: Save and load](https://github.com/Mudcrab-Team/mudcrab/milestone/13) | [save-load](wave2/save-load.md), [engine-app](wave1/engine-app.md) | [#567](https://github.com/Mudcrab-Team/mudcrab/issues/567) |
| [A5: Papyrus and Luau semantic equivalence](https://github.com/Mudcrab-Team/mudcrab/milestone/14) | [papyrus-vm](wave2/papyrus-vm.md) | none |
| [B1: Actor assembly](https://github.com/Mudcrab-Team/mudcrab/milestone/15) | [actor-assembly](wave2/actor-assembly.md) | [#555](https://github.com/Mudcrab-Team/mudcrab/issues/555) |
| [B2: Animation: behaviour graphs](https://github.com/Mudcrab-Team/mudcrab/milestone/16) | [anim-graphs](wave2/anim-graphs.md), [anim](wave1/anim.md) | [#557](https://github.com/Mudcrab-Team/mudcrab/issues/557), [#572](https://github.com/Mudcrab-Team/mudcrab/issues/572) |
| [B3: Animation: IK, events, paired animations and ragdoll](https://github.com/Mudcrab-Team/mudcrab/milestone/17) | [anim-graphs](wave2/anim-graphs.md), [physics-movement](wave2/physics-movement.md) | none |
| [B4: Physics and movement](https://github.com/Mudcrab-Team/mudcrab/milestone/18) | [physics-movement](wave2/physics-movement.md), [physics](wave1/physics.md), [re-audit](reaudit-e5f2de4b.md) | [#565](https://github.com/Mudcrab-Team/mudcrab/issues/565), [#592](https://github.com/Mudcrab-Team/mudcrab/issues/592), [#593](https://github.com/Mudcrab-Team/mudcrab/issues/593), [#594](https://github.com/Mudcrab-Team/mudcrab/issues/594) |
| [B5: Navigation](https://github.com/Mudcrab-Team/mudcrab/milestone/19) | [navigation](wave2/navigation.md) | [#556](https://github.com/Mudcrab-Team/mudcrab/issues/556) |
| [B6: AI packages and schedules](https://github.com/Mudcrab-Team/mudcrab/milestone/20) | [navigation](wave2/navigation.md), [game](wave1/game.md) | none |
| [B7: Detection, stealth and crime](https://github.com/Mudcrab-Team/mudcrab/milestone/21) | [game](wave1/game.md) | [#559](https://github.com/Mudcrab-Team/mudcrab/issues/559) |
| [C1: Items, inventory, containers and activation](https://github.com/Mudcrab-Team/mudcrab/milestone/22) | [combat-inventory-magic](wave2/combat-inventory-magic.md), [engine-app](wave1/engine-app.md) | [#563](https://github.com/Mudcrab-Team/mudcrab/issues/563), [#595](https://github.com/Mudcrab-Team/mudcrab/issues/595) |
| [C2: Actor values, skills and leveling](https://github.com/Mudcrab-Team/mudcrab/milestone/23) | [combat-inventory-magic](wave2/combat-inventory-magic.md), [game](wave1/game.md) | [#597](https://github.com/Mudcrab-Team/mudcrab/issues/597) |
| [C3: Combat and projectiles](https://github.com/Mudcrab-Team/mudcrab/milestone/24) | [combat-inventory-magic](wave2/combat-inventory-magic.md) | [#558](https://github.com/Mudcrab-Team/mudcrab/issues/558), [#598](https://github.com/Mudcrab-Team/mudcrab/issues/598) |
| [C4: Magic, effects and shouts](https://github.com/Mudcrab-Team/mudcrab/milestone/25) | [combat-inventory-magic](wave2/combat-inventory-magic.md) | [#596](https://github.com/Mudcrab-Team/mudcrab/issues/596) |
| [C5: Perks, enchanting, alchemy and smithing](https://github.com/Mudcrab-Team/mudcrab/milestone/26) | [combat-inventory-magic](wave2/combat-inventory-magic.md) | [#599](https://github.com/Mudcrab-Team/mudcrab/issues/599) |
| [C6: Quests, stories and scenes](https://github.com/Mudcrab-Team/mudcrab/milestone/27) | [quests-conditions](wave2/quests-conditions.md) | [#566](https://github.com/Mudcrab-Team/mudcrab/issues/566) |
| [C7: Dialogue and voice](https://github.com/Mudcrab-Team/mudcrab/milestone/28) | [quests-conditions](wave2/quests-conditions.md), [game](wave1/game.md) | [#560](https://github.com/Mudcrab-Team/mudcrab/issues/560) |
| [C8: Factions, followers, barter and services](https://github.com/Mudcrab-Team/mudcrab/milestone/29) | [game](wave1/game.md) | none |
| [D1: UI runtime and host](https://github.com/Mudcrab-Team/mudcrab/milestone/30) | [ui-movie](wave2/ui-movie.md), [ui](wave1/ui.md), [re-audit](reaudit-e5f2de4b.md) | [#569](https://github.com/Mudcrab-Team/mudcrab/issues/569) |
| [D2: Game menus](https://github.com/Mudcrab-Team/mudcrab/milestone/31) | [ui-movie](wave2/ui-movie.md), [ui](wave1/ui.md) | none |
| [D3: Main menu, new game, character creation and loading](https://github.com/Mudcrab-Team/mudcrab/milestone/32) | [ui](wave1/ui.md), [engine-app](wave1/engine-app.md) | none |
| [D4: Input and settings](https://github.com/Mudcrab-Team/mudcrab/milestone/33) | [ui](wave1/ui.md) | none |
| [E1: Skyrim authored audio integration](https://github.com/Mudcrab-Team/mudcrab/milestone/5) | [audio](wave2/audio.md), [audio (wave 1)](wave1/audio.md) | none |
| [E2: Virtualized geometry: clusters on, zero pop-in](https://github.com/Mudcrab-Team/mudcrab/milestone/6) | [render-submission](wave2/render-submission.md) | none |
| [E3: Bevy 0.20: upgrade and feature adoption](https://github.com/Mudcrab-Team/mudcrab/milestone/7) | none | none |
| [E4: Visual profiles: Skyrim parity by default, Mudcrab look on toggle](https://github.com/Mudcrab-Team/mudcrab/milestone/8) | [render](wave1/render.md), [materials-weather](wave2/materials-weather.md) | none |
| [E5: Terrain, LOD, grass and trees](https://github.com/Mudcrab-Team/mudcrab/milestone/34) | [grass-far-water](wave2/grass-far-water.md) | [#561](https://github.com/Mudcrab-Team/mudcrab/issues/561) |
| [E6: Water, sky and weather](https://github.com/Mudcrab-Team/mudcrab/milestone/35) | [grass-far-water](wave2/grass-far-water.md), [materials-weather](wave2/materials-weather.md) | none |
| [E7: Materials, lighting, imagespace and effects](https://github.com/Mudcrab-Team/mudcrab/milestone/36) | [materials-weather](wave2/materials-weather.md), [render](wave1/render.md) | [#564](https://github.com/Mudcrab-Team/mudcrab/issues/564) |
| F1 to F7: Main quest, factions, DLC, miscellaneous and Creation Club content (milestones [37](https://github.com/Mudcrab-Team/mudcrab/milestone/37) to [43](https://github.com/Mudcrab-Team/mudcrab/milestone/43)) | none | none |
| [G1: Skyrim reference harness: controlled, repeatable, headless captures of retail Skyrim SE](https://github.com/Mudcrab-Team/mudcrab/milestone/9) | [meta](wave1/meta.md), [render-submission](wave2/render-submission.md) | [#568](https://github.com/Mudcrab-Team/mudcrab/issues/568) |
| [G2: Performance and memory budgets](https://github.com/Mudcrab-Team/mudcrab/milestone/44) | [render-submission](wave2/render-submission.md), [meta](wave1/meta.md) | none |
| [G3: Mod compatibility](https://github.com/Mudcrab-Team/mudcrab/milestone/45) | [format-contracts](wave2/format-contracts.md) | none |
| [G4: v0.1.0, Phase 1: Asset Pipeline](https://github.com/Mudcrab-Team/mudcrab/milestone/10) | [format-contracts](wave2/format-contracts.md), [importer](wave1/importer.md), [formats](wave1/formats.md) | [#562](https://github.com/Mudcrab-Team/mudcrab/issues/562) |
| [G5: Packaging and v1.0](https://github.com/Mudcrab-Team/mudcrab/milestone/46) | none | none |
| [R: Research and reuse evaluations](https://github.com/Mudcrab-Team/mudcrab/milestone/47) | [summary](summary.md), [master report](master/README.md) | [#570](https://github.com/Mudcrab-Team/mudcrab/issues/570) (tracking), [#607](https://github.com/Mudcrab-Team/mudcrab/issues/607), [#608](https://github.com/Mudcrab-Team/mudcrab/issues/608) |
| [P: Post-parity enhancements](https://github.com/Mudcrab-Team/mudcrab/milestone/48) | none | none |

The mapping of reports to milestones is a reading of each report's scope, not a statement by the report itself.

## All reports

| Report | What it covers |
| --- | --- |
| [summary](summary.md) | Short findings summary: core insights and delivered issues. |
| [master report](master/README.md) | Cross-area summary, claims to verify, and pointers to the atlas, gaps, liberties and plan. |
| [master/gaps](master/gaps.md), [master/ahead](master/ahead.md), [master/liberties](master/liberties.md) | 25 ranked gaps; where Mudcrab is ahead; spud's deviations from vanilla. |
| [master/wave2](master/wave2.md) | How the 14 deep dives were chosen and ranked. |
| master/atlas-*: [importer](master/atlas-importer.md), [formats](master/atlas-formats.md), [engine-app](master/atlas-engine-app.md), [engine-core](master/atlas-engine-core.md), [game](master/atlas-game.md), [ui](master/atlas-ui.md), [render](master/atlas-render.md), [anim](master/atlas-anim.md), [physics](master/atlas-physics.md), [audio](master/atlas-audio.md), [meta](master/atlas-meta.md) | Feature atlas: spud rows compared with Mudcrab, one table per area. |
| Wave 1: [importer](wave1/importer.md), [formats](wave1/formats.md), [engine-app](wave1/engine-app.md), [engine-core](wave1/engine-core.md), [game](wave1/game.md), [ui](wave1/ui.md), [render](wave1/render.md), [anim](wave1/anim.md), [physics](wave1/physics.md), [audio](wave1/audio.md), [meta](wave1/meta.md) | Coarse source map of each area, with feature tables. |
| [wave2/anim-graphs](wave2/anim-graphs.md) | Havok behaviour graphs, transitions, events and IK. |
| [wave2/papyrus-vm](wave2/papyrus-vm.md) | Papyrus VM, natives, scheduler. |
| [wave2/quests-conditions](wave2/quests-conditions.md) | Quests, conditions, story manager, scenes, dialogue. |
| [wave2/save-load](wave2/save-load.md) | The `.ost` save, section by section. |
| [wave2/navigation](wave2/navigation.md) | NPC navigation and AI packages. |
| [wave2/audio](wave2/audio.md) | Audio ingestion and a platform-neutral interface. |
| [wave2/ui-movie](wave2/ui-movie.md) | UI movie conversion and the host boundary. |
| [wave2/actor-assembly](wave2/actor-assembly.md) | Actor assembly: bodies, heads, skin, rigs, idles. |
| [wave2/physics-movement](wave2/physics-movement.md) | Collision, layers, character controller, water, ragdolls. |
| [wave2/combat-inventory-magic](wave2/combat-inventory-magic.md) | Combat formulas, items, magic archetypes, perks. |
| [wave2/materials-weather](wave2/materials-weather.md) | Materials, lighting and weather data to shader. |
| [wave2/grass-far-water](wave2/grass-far-water.md) | Grass, far views (impostors, proxies) and water. |
| [wave2/render-submission](wave2/render-submission.md) | Render submission and evidence tools. |
| [wave2/format-contracts](wave2/format-contracts.md) | Format and import contracts, versions, provenance. |
| [reaudit-e5f2de4b](reaudit-e5f2de4b.md) | Re-audit at spud `e5f2de4b`: player controller, UI, stale claims. |
| [mudcrab-inventory](mudcrab-inventory.md) | Stale snapshot of Mudcrab's own features, used as the comparison side. |

## Corrections made when publishing

The reports were edited once, for publication. Findings and caveats were kept. These are the substantive changes:

- `actors.cpp` start and finish constants (`k_start_per_frame`, `k_finish_per_frame`) are at lines 58-59 of `src/engine/actors.cpp`. The wave-2 [actor-assembly](wave2/actor-assembly.md) report cited lines 32-33. The links now point to 58-59 (issue [#608](https://github.com/Mudcrab-Team/mudcrab/issues/608) records the error).
- The combat report said Mudcrab's exporter publishes no item, spell or perk tables. Checked 2026-11-14: `crates/converter/src/esm/exporter.rs` publishes only two GMSTs and no item, spell, AVIF or perk tables. The text now says so.
- A few ranges that ran past the end of the cited file were corrected where the intended file was clear: a sync-scene range in [anim-graphs](wave2/anim-graphs.md) belongs to `generators.cpp`, a `game.hpp` member is at line 610, and several bare line numbers in [grass-far-water](wave2/grass-far-water.md) and [save-load](wave2/save-load.md) now name their file.
- An independent review before publication found further errors in the original notes. Each is marked in place with "Correction (publication review, 2026-11-14)" or fixed silently when it was only a citation: [papyrus-vm](wave2/papyrus-vm.md) said `Quest`, `Scene` and `Topic` natives were unbound (the game-layer binders bind them); [navigation](wave2/navigation.md) read the stuck test as 90% progress (it is 10%); [grass-far-water](wave2/grass-far-water.md) said tree-atlas file names keep path separators (only the base name is kept, so same-named models can collide); [combat-inventory-magic](wave2/combat-inventory-magic.md) said enchantment charge is never spent (the caster spends it, so that claim is now `unverified`); and the `story_manager.cpp`, `container.cpp`, `physics.hpp` and `combat.hpp` citations in [quests-conditions](wave2/quests-conditions.md), [save-load](wave2/save-load.md), [physics-movement](wave2/physics-movement.md) and [combat-inventory-magic](wave2/combat-inventory-magic.md) were re-pointed.
- Where an issue could not be read when a report was written, the text now says "had not been read when this report was written".
- Local paths, tracker-cache file names and working notes from the research process were removed.
