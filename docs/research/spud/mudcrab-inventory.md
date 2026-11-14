# Mudcrab feature inventory

> **Snapshot, stale.** This inventory describes Mudcrab `main` as of 2026-11-12 (commit `3ce1fb42`) and was read on 2026-11-13. Mudcrab has changed since. Treat every status and count as dated and re-check it against current `main` before relying on it.
> Written with an AI assistant; research notes, not checked line by line by a human. Read-only inventory; nothing was built or run. See the [README](README.md).
> **Topic:** Mudcrab feature inventory by area, used as the "Mudcrab" side of the spud comparison.

Read-only inventory of Mudcrab `main` as of 2026-11-12 (commit `3ce1fb42`), read from a checkout that was a merge of `main` into a work branch. Public tracker data was read through the GitHub API. Feature status describes merged main, not whether an open PR may already be available. Module presence was checked against code. Test counts are measured `#[test]` attributes across crate Rust sources, not executed tests. Rust line counts are measured source lines under each crate's `src` tree.

## Feature counts by area and status

Counts below are rows in this inventory. PR and issue rows are distinct from implemented mainline rows.

| area | shipped on main | partially | open PR | draft PR | issue only | absent | total |
|---|---:|---:|---:|---:|---:|---:|---:|
| importer | 9 | 2 | 0 | 1 | 1 | 0 | 13 |
| formats | 6 | 0 | 0 | 1 | 0 | 0 | 7 |
| engine-app | 3 | 1 | 0 | 0 | 0 | 5 | 9 |
| engine-core | 4 | 3 | 0 | 0 | 0 | 0 | 7 |
| game | 0 | 0 | 0 | 0 | 0 | 8 | 8 |
| ui | 3 | 0 | 0 | 0 | 0 | 2 | 5 |
| render | 6 | 3 | 1 | 1 | 0 | 0 | 11 |
| anim | 1 | 2 | 0 | 0 | 0 | 2 | 5 |
| physics | 1 | 2 | 0 | 0 | 0 | 1 | 4 |
| audio | 0 | 0 | 0 | 0 | 1 | 2 | 3 |
| meta | 6 | 1 | 0 | 0 | 0 | 0 | 7 |

## Inventory method and maturity measures

| crate | Rust source lines (measured) | `#[test]` attributes (measured) |
|---|---:|---:|
| converter | 111,403 | 1,034 |
| engine | 103,463 | 1,303 |
| shared | 4,773 | 64 |
| launcher | 7,284 | 122 |
| mo2 | 641 | 1 |
| dummy-content | 8,133 | 63 |

Status labels: `shipped on main` means implementation is in this checkout; `partially` means an identifiable subset exists and named gaps remain; `open PR #n` / `draft PR #n` / `issue only #n` reflect current public tracker listings; `absent` means no implementation was found in the surveyed modules/docs. PRs are cited when their title directly matches a gap. Tracker completeness is limited to fetched public listing pages. Older issue and PR history may not be complete.

## importer

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| importer-esm-records | ESM/ESP/ESL record parsing and SQLite projection | shipped on main | `crates/converter/src/esm`, `records.rs`, `pipeline.rs` | converter 1,034 test attrs; parser parity tests | Active converter area; docs `docs/specs/converters/esm-to-sqlite.md` | Projection is intentionally scoped; issue #498 tracks source-field accounting and silent loss. |
| importer-load-order | Plugin order, creations, strings, and MO2 profile selection | shipped on main | `esm/load_order.rs`, `mo2/src/lib.rs`, `shared/src/plugin_selection.rs` | mo2 1 test attr; converter/shared tests | MO2 path resolution is read-only | Does not run MO2 or SKSE extensions. |
| importer-bsa | BSA/archive extraction and plugin-to-archive association | shipped on main | `converter/src/archive`, `pipeline.rs` | converter tests; fixes #237/#238 merged | Archive ingestion supports reuse | BA2 is fixture-side; BA2 game ingestion was not established. |
| importer-nif | NIF geometry/material/skin extraction into glTF | shipped on main | `converter/src/mesh`, `material.rs`, `actor_models.rs`; vendored `project-wormhole-nif` | converter tests and NIF parity tests | Open issue #445 handles hostile depth/skin parse limits | Not every NIF feature is represented; animation preservation work has draft #432. |
| importer-textures | DDS to KTX2, texture metadata and GPU encoding | shipped on main | `converter/src/texture*` | converter tests; roadmap documents desktop contract | Portable BC7/ASTC/ETC2 proposal #548 open | Mobile/web output not demonstrated; arrays excluded by Phase 1 contract. |
| importer-audio | Audio source inventory and conversion | issue only #521, #522 | no audio ingestion module found | No importer tests found | Open help-wanted issues | Audio overlay and archive recipe are explicitly proposed, not implemented. |
| importer-navmesh | Navmesh extraction and runtime navigation | partially | ESM records and links are retained in database | converter has broad parser tests | #385 closed for link decoding | No gameplay navigation/pathfinding consumer found. |
| importer-lod | Object LOD and terrain LOD compilation | shipped on main | `converter/src/lod`, `terrain_pipeline.rs`, `terrain_chunks.rs` | converter tests; ADR 0010/0011; PRs #235/#240 merged | LOD uses separate schema/provenance | Runtime coverage varies by LOD type; grass LOD still developing. |
| importer-grass | Grass placement rules and obstruction masks | partially | converter grass modules; `docs/specs/converters/lod-compiler.md` | converter tests | #282 open; runtime grass #539 open | Placement baking exists; streamed grass rendering/settings are open. |
| importer-terrain | LAND layers, arrays, merged terrain, collision and texture bake | shipped on main | `terrain_arrays.rs`, `terrain_pipeline.rs`, shared `terrain_mesh.rs` | converter/shared/engine tests; #235/#236 merged | #449 closed | Specialized authored LTEX parameters remain tracked by #547. |
| importer-cluster-overlays | Certified virtualized-geometry cluster overlays | draft PR #439 | converter LOD/derive code; cluster stack PRs #427-#462 | Current base has LOD tests, not complete cluster path | Draft stack is marked S-blocked | Not shipped as an end-to-end package and renderer feature. |
| importer-reuse-cache | Resumable stages, content/provenance cache, reuse-assets, repair/check | shipped on main | `cache.rs`, `database_cache.rs`, `pipeline.rs`, `repair.rs`, CLI | 1,034 converter test attrs | Recent fixes #448/#450 merged | Conversion is expensive; compatibility depends on schema and provenance. |
| importer-pack | Atomic conversion manifest and runtime pack publication | shipped on main | `pipeline.rs`, shared world assets and launcher readiness | converter/launcher tests | Mainline pack format is documented | Output pack is directory-based, not a single archive. |

## formats

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| formats-pack-layout | Directory pack, manifest, assets and database layout | shipped on main | converter pipeline; `shared/src/world_assets.rs`; `docs/specs/converters/data-formats.md` | converter/shared tests | Immutable registered packs per project policy | Manifest schema compatibility is bounded by runtime support. |
| formats-gltf | glTF/GLB mesh output and runtime material metadata | shipped on main | converter mesh/material and engine NIF material | converter/engine tests | Core static world path | Some NIF material extensions/animation remain missing; cluster work separates derived payloads. |
| formats-ktx2 | KTX2 textures, mip ordering, cubemap/volume data | shipped on main | `converter/src/texture_ktx2.rs`; `docs/specs/converters/dds-to-ktx2.md` | converter tests; #222 merged | Desktop profile | Ordinary texture arrays unsupported. |
| formats-rkyv | rkyv cell/terrain cache contracts | shipped on main | `shared/src/lib.rs`, `shared/src/terrain_mesh.rs` | shared tests | `CELL_CACHE_VERSION=3` | Cache contract is versioned and not a general save format. |
| formats-world-db | SQLite world database and normalized record tables | shipped on main | converter ESM; `engine/src/world/database.rs`; db-schema spec | converter/engine tests | schema 9; runtime accepts 3-9 | Optional fields and projection gaps remain. |
| formats-schema-versions | Converter, LOD and database schema negotiation | shipped on main | shared schema constants; converter manifest | tests check supported version bounds | Explicit compatibility window | Producers older than minimum runtime versions cannot load. |
| formats-cluster-payload | Certified cluster payload contract | draft PR #430 | stack #427-#462 | Not in complete current mainline path | Draft stack | Certification/package compatibility is still in progress. |

## engine-app

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| engine-app-launch | Launcher detects Skyrim, converts assets, checks output and launches engine | shipped on main | `crates/launcher/src` | 122 launcher test attrs | GUI state machine and headless tests | Release browser listing issue #550 open. |
| engine-app-mo2 | MO2 profile and asset resolution | shipped on main | `crates/mo2/src/lib.rs`; launcher integration | MO2 tests limited (1 attr plus integration suite) | Read-only resolver | No MO2 process hosting or SKSE native extensions. |
| engine-app-doors | Door crossing and transition positioning | shipped on main | `engine/src/doors.rs`, `door_crossing.rs` | engine tests; #481 merged | Door transitions work for supported authored transitions | Issue #473 reports non-transition doors unavailable; continued parity issues exist. |
| engine-app-saves | Skyrim-compatible save/load | absent | no save subsystem found in surveyed modules | none | No matching open PR surfaced in fetched pages | Player state persistence is absent. |
| engine-app-combat | Combat encounter and weapon gameplay integration | absent | no combat app/system found | none | No implementation identified | No Skyrim combat loop. |
| engine-app-dialogue | Dialogue UI and conversation integration | absent | database records only | none | Dialogue fixture generation is not runtime integration | No conversation system found. |
| engine-app-items | Inventory, item use and equipment interaction | absent | record data may be imported | none | No gameplay item system found | No player inventory/equipment loop. |
| engine-app-mounts | Mounts and riding | absent | none found | none | No tracker item surfaced | No mount interaction. |
| engine-app-actors | Actor/NPC spawning and rendering integration | partially | `engine/src/actors.rs`, actor modules; converter actor catalog | engine tests; actor PRs #357/#382 merged | Actor catalog and animation code exist | Open issues #470/#471 identify visible rig/idle gaps. |

## engine-core

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| engine-core-main-loop | Bevy app, schedules, config and frame lifecycle | shipped on main | `engine/src/app.rs`, `lib.rs`, `main.rs` | 1,303 engine test attrs | Bevy 0.19 | Not a full Skyrim simulation loop. |
| engine-core-streaming | Cell streaming, requests, admission, reservations, pacing and LOD | shipped on main | `engine/src/streaming*`, `pacing.rs` | tests in streaming runtime/merged modules | Extensive profiling and route tools | Adaptive and static behaviors are bounded implementation policies. |
| engine-core-camera | First-person camera and camera controls | partially | app/camera setup, settings, console | engine tests; stair smoothing issue #511 open | Camera is integrated with movement and start-shot tooling | Stair height smoothing open; camera parity still under study. |
| engine-core-movement | Walk/run/jump and collision-based movement | partially | `engine/src/physics.rs`, actors, shared movement records | stair tests; #482 merged | Defaults and routes implemented | Open #544 jump semantics, #464 stairs, #506 authored selection. |
| engine-core-input | Controls and settings UI integration | shipped on main | settings and console modules | engine tests; #229 merged | Native settings/controls menu exists | Gameplay action coverage remains limited. |
| engine-core-papyrus | Papyrus/PEX runtime | partially | `engine/src/papyrus_runtime.rs`; converter `script.rs` | sparse implementation tests | Roadmaps describe Lua/Papyrus work | No general Papyrus VM parity established; PEX to Lua is incomplete. |
| engine-core-console | Developer console and cell commands | shipped on main | `console.rs`, `cell_commands.rs`, `live_console.rs` | engine tests; #494 merged | Developer-oriented command surface | Not a full in-game command/quest scripting system. |

## game

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| game-ai | NPC decision-making and schedules | absent | actor catalog/records only | none for behavior systems | Dummy fixtures include AI-shaped records | No gameplay AI planner or schedule execution found. |
| game-combat | Weapons, damage, blocking and combat AI | absent | none found | none | No matching engine subsystem | Imported data does not establish runtime rules. |
| game-magic | Spells, effects and casting rules | absent | dummy fixture records only | none | No runtime magic system found | No spell execution. |
| game-quests | Quest stages, objectives and scripting | absent | imported records only | none | No quest manager found | No quest progression. |
| game-crime | Crime, bounty, factions and law response | absent | none found | none | No runtime rules module | No crime consequences. |
| game-perks | Perk evaluation and progression | absent | fixture record support | none | No runtime subsystem | No perk tree/progression. |
| game-conditions | Skyrim condition evaluation | absent | converter record projection and fixtures | no runtime evaluator identified | Conditions are data, not behavior | No condition VM or general evaluation engine. |
| game-items | Item effects, inventory and economy rules | absent | imported records only | none | No runtime gameplay rules | No player-facing item loop. |

## ui

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| ui-launcher | Conversion, setup, mod dropzone and Play UI | shipped on main | `crates/launcher/src/ui.rs`, `conversion` | 122 test attrs | Main user-facing interface | Launcher is a setup interface, not a Skyrim menu suite. |
| ui-settings | Settings and controls menu | shipped on main | `engine/src/settings_ui.rs` | engine tests; #229/#262 merged | Settings include rendering controls | #466 reports start-shot settings override behavior. |
| ui-hud | In-game health/magicka/stamina/crosshair HUD | absent | none found | none | No HUD framework found | Player HUD is absent. |
| ui-menu-framework | Inventory, dialogue, journal, map and pause menus | absent | none found | none | No general UI framework found | Skyrim menu flows are absent. |
| ui-console | Developer console | shipped on main | `engine/src/console.rs`, `live_console.rs` | engine tests | Developer/debug tool | Does not provide general game menus. |

## render

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| render-bevy | Bevy renderer integration and render schedule | shipped on main | `engine/src/render.rs`, renderer init | engine tests; Bevy 0.19 | Main rendering path | Backend/device bugs remain tracked (#467, #525). |
| render-nif-materials | NIF materials, alpha, normal, emission, palette and skin | shipped on main | `nif_material.rs`, `skin_material.rs`, shader checks | engine tests and probe examples | Continuous authored material coverage | Open #484 UV animation; #490 cubemaps; #435 directional materials. |
| render-shadows | Shadow maps and shadow pipeline | shipped on main | render/shaders and actor/material pipeline | engine tests | Static scene lighting has shadows | Actor shadow/pipeline and device differences require coverage. |
| render-terrain | Terrain meshes, arrays, merged cell terrain | shipped on main | `terrain_merge/`, `terrain_upload.rs`, world water | engine tests; #236 merged | Runtime terrain present | LTEX specular work #547 open; water authored consumers #501 open. |
| render-water | Flat water and reflected/rippled water paths | partially | `world/water.rs`, render shaders/specs | engine tests; #299 open; #499 merged | Flat-water coverage shared | Current authored WATR parameter gaps tracked #545; reflection parity still in progress. |
| render-sky | Sky, weather, time-of-day and fog | partially | `sky.rs`, `world/environment.rs` | engine tests; #546 cross-check open | Static environment data imported | Weather/cloud parity and authored environment gaps remain. |
| render-grass | Streamed grass rendering | open PR #539 | converter placement is partial; engine not complete | no complete shipped runtime evidence | PR #541/#540 adjacent | Grass runtime and appearance settings are unfinished. |
| render-clusters | Cluster/virtualized geometry drawing | draft PR #441, #533 | cluster stack #427-#462 | main has ordinary LOD/indirect paths | Draft and blocked stack | No end-to-end certified virtualized geometry shipped. |
| render-lod | Object LOD, terrain LOD and runtime proxies | shipped on main | converter LOD; `engine/src/streaming/lod.rs` | converter/engine tests | Hybrid LOD specs | Grass and cluster LOD are incomplete. |
| render-post | Color pipeline, tonemapping, HDR/settings | shipped on main | `color_pipeline.rs`, `tone_mapping.rs`, settings | engine tests and probes | Color fixture evidence present | Platform-specific visual parity is not universal. |
| render-lighting | Local lights, emittance, interiors and imagespace | partially | `lights.rs`, `emittance.rs`, world modules | engine tests | Open fixes #480/#488 | Authored cubemaps and some local-light parameters remain open. |

## anim

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| anim-actor-rigs | Actor model assembly, skinning, head and face details | partially | converter actor models; engine actor modules | converter/engine tests | #357/#382 merged; #408/#405 merged | Open child rig/mesh gaps in #470. |
| anim-authored-clips | Havok clip decoding and idle playback | partially | converter animation; engine actor_animation | tests; #402/#381 merged | Creature authored-idle stack #512-#515 | Open draft playback and publisher; issue #471 confirms missing visible creature idles. |
| anim-gaits | Speed-driven walk/run gaits | shipped on main | actor drive/animation | engine tests; #382 merged | Actor catalog path | Full behavior graph parity not established. |
| anim-ik | Inverse kinematics and foot placement | absent | no IK module found | none | No active implementation found | No IK or procedural foot solver. |
| anim-behaviour-graphs | Skyrim behavior graph state machine | absent | no runtime graph evaluator found | none | Separate from clip decoder | No full behavior graph system. |

## physics

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| physics-rapier | Rapier physics integration | shipped on main | `engine/src/physics.rs`; Cargo dependency | tests incl. stair tests | Character/world movement path | Not a complete Havok replacement. |
| physics-character | Character controller, walking, jumping and stairs | partially | engine physics and actor drive | tests; #482 merged; #544 open | Active parity fixes | Known stairs and jump edge cases remain. |
| physics-collision-import | NIF collision extraction and runtime attachment | partially | converter collision; engine collision placement | converter/engine tests; #420/#486 merged | Open ownership/state stack #474-#477 | Coverage is selective; static fixed containers dominate supported path. |
| physics-navmesh | Navmesh traversal and pathfinding | absent | imported navmesh links only | no runtime pathfinder tests | no pathfinding module found | NPC navigation absent. |

## audio

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| audio-runtime | Playback, spatial audio, music and voice | absent | no audio engine subsystem found | none | Audio record decoding only | No playback or mixer. |
| audio-ingestion | Audio archive inclusion and loose-file overlay | issue only #521, #522 | no converter audio importer found | none | Open issues | Audio assets are not yet part of the pack pipeline. |
| audio-dialogue | Dialogue voice playback | absent | no runtime dialogue/audio path | none | none | No dialogue playback. |

## meta

| id | feature | status | where | maturity evidence | owner/state notes | known limitations |
|---|---|---|---|---|---|---|
| meta-docs-specs | Architecture, formats, guides, ADRs and roadmaps | shipped on main | `docs/` | 10+ ADRs and extensive guides/specs | Phase roadmaps mark static scope and later gameplay | Some docs are proposals; status must be checked against implementation. |
| meta-tests | Unit, integration, parity and fixture tests | shipped on main | all crates, `tests/`, test scripts | measured table above | Counts are attributes only, not passing status | No tests were run. |
| meta-dummy-content | Deterministic generated ESM/BSA/BA2/NIF/DDS/PEX fixtures | shipped on main | `crates/dummy-content` | 63 test attrs plus performance integration tests | Synthetic data avoids proprietary files | Fixtures do not prove retail asset compatibility by themselves. |
| meta-ci | GitHub Actions, code quality and GPU/performance gates | shipped on main | `.github` workflows and `scripts/` | issue #421 governance gate open; #524 receipt validator open | Public PR listing shows active gate work | Some hardware validation needs external target hosts. |
| meta-tools | Converter CLI, world-inspect, profiling, captures, schema and audit tools | shipped on main | converter CLI; engine bins; `scripts/`, `tools/` | many crate tests | Developer-oriented tooling is broad | Tools do not imply game feature completeness. |
| meta-release | Changelog and automated release process | partially | `cliff.toml`; no top-level CHANGELOG found | PR #426 closed | Release tooling exists in some form | No CHANGELOG detected at repository root; inspect release state separately. |
| meta-ownership | Labels, code owners, contribution and AI policy | shipped on main | `CODEOWNERS`, docs/contributing, `docs/AI_POLICY.md` | issue #421 still open | Active review and evidence processes | Governance gate remains proposed/open per tracker listing. |

## Areas with no work explicitly present

No runtime work was found for game AI, combat rules, magic execution, quest progression, crime/bounty, perks, general conditions evaluation, saves, inventory/items, mounts, HUD, the Skyrim menu framework, IK, behavior graphs, audio playback, dialogue playback, or navmesh pathfinding. Data import or generated test fixtures exist for some of these concepts, but they do not establish runtime features.

## Milestones

Local roadmaps identify Phase 1 (Asset Modernization Pipeline), Phase 2 (Core Engine Runtime and Vercidium Renderer), Phase 3 scripting/Luau and UI, Phase 4 gameplay/physics, and Phase 5 multiplatform/networking. README marks Phase 1 and Phase 2 as incomplete. Exact GitHub milestone names, counts, and states were not established when this inventory was written; see the README index for the current milestone list. Public PR data did show `v0.1.0 — Phase 1: Asset Pipeline` on issue/PR #493.

## Five thinnest areas

1. Game rules: no runtime combat, magic, quests, crime, perks, or AI systems.
2. UI: launcher and settings exist, but HUD and player menus do not.
3. Audio: no playback or ingestion pipeline.
4. Persistence: no save/load subsystem.
5. Navigation and animation behavior: no navmesh pathfinding, IK, or behavior graph evaluator.

## Tracker and scope caveats

This is a local source inventory plus public tracker titles. The public API response included the most recent issue/PR listing pages, but the full issue history was not proven complete. The repository has no top-level `CHANGELOG` file in the inspected root. No builds or tests were run.
