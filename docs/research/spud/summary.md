# Spud audit summary

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source reviews of [spud](https://github.com/chasmlol/spud) (MIT) at commits [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102) and [`e5f2de4b`](https://github.com/chasmlol/spud/commit/e5f2de4b5a612eef3f2cd15e4a84672e9e0a0f1c), 2026-11-13 to 2026-11-14. Nothing was built or run. See the [README](README.md) for evidence levels and the licence note.
> **Topic:** Core insights from the wave-2 deep dives. This summary was written from the first 7 of the 14 wave-2 reports; all 14 are indexed in the README.

All spud findings are source reads (spud `8c990420`), not runtime results. Nothing is built into Mudcrab.

## Core insights

1. **Mudcrab's gap is gameplay rules, not rendering.** Spud has quests, conditions, Papyrus, saves, animation graphs, AI packages and combat in code. Mudcrab's converter, formats, streaming and renderer are level or ahead. Mudcrab's runtime for scripts, quests, saves, graphs and navigation does not exist yet.
2. **Spud is a map, not an oracle.** One contributor, days old, AI-assisted, no tests or CI. Its source is a fast way to see what each subsystem must handle. Its quirks (silent defaults, invented ordering) must not be copied.
3. **The recurring spud failure is silent defaults.** Missing Papyrus natives return a default after one warning. Unknown condition functions return 0. Missing Havok node classes are dropped at debug level. Audio conditions return 0. Mudcrab should count and report every fall-through; that is the best parity metric.
4. **Papyrus:** all 36 opcodes exist in Mudcrab's emitter, but the runtime is unwired. Six design rules transfer (attachment order, init gate, latent tokens, clock domains, bounded slices, ranked missing natives). One small converter bug is filed as [#571](https://github.com/Mudcrab-Team/mudcrab/issues/571): of 23,539 conditional jumps scanned in one converted pack, 7 test an `Int` and 1 tests an array directly, so Luau truthiness differs from Papyrus truthiness there. See [papyrus-vm](wave2/papyrus-vm.md).
5. **Behaviour graphs are the animation state machine, not the AI.** Mudcrab has no decoder. A separate Rust Havok library is under licence discussion; see issue [#572](https://github.com/Mudcrab-Team/mudcrab/issues/572). See [anim-graphs](wave2/anim-graphs.md).
6. **Quests and conditions:** unsupported conditions pass `== 0` tests by default; stage order and two-phase alias fill are the traps. See [quests-conditions](wave2/quests-conditions.md).
7. **Save/load:** spud never compares plugin hashes on load and keys everything by FormID. A reordered plugin set can load wrong values. Mudcrab can do better with the content-hash design it already has. See [save-load](wave2/save-load.md).
8. **Navigation:** no path spans a space; links ignore type and flags; cover, search grid and precomputed paths are decoded but unused; off-screen actors teleport. Mudcrab has the data and no consumer. (The re-audit found the "links ignore type" claim stale at `e5f2de4b`.) See [navigation](wave2/navigation.md).
9. **UI:** the movie-to-engine channel is one function. The cost is about 500 lines of C++ per-frame HUD rules, which a Bevy port must own. The AVM1 interpreter does not transfer. See [ui-movie](wave2/ui-movie.md).
10. **Audio:** ingestion (26 record types to 34 chunks) is reusable; playback is Windows-only. A portable backend needs 15 capabilities. Record the codec and HRTF licences from the start. See [audio](wave2/audio.md).

## Issues filed from the audit

[#555](https://github.com/Mudcrab-Team/mudcrab/issues/555) to [#570](https://github.com/Mudcrab-Team/mudcrab/issues/570) (one per area, with #570 as the tracking issue), [#571](https://github.com/Mudcrab-Team/mudcrab/issues/571) (`jmpt`/`jmpf` truthiness) and [#572](https://github.com/Mudcrab-Team/mudcrab/issues/572) (behaviour graphs, blocked on a licence question). The [README index](README.md#index-by-milestone) maps them to milestones.

## Verification

Cited lines and some counts were checked against the spud source for every report. The large tallies were not recounted.

## Open design question

Off-screen NPC policy: should an NPC that is off screen at package start be simulated or teleported? The navigation work depends on the answer. See [navigation](wave2/navigation.md).
