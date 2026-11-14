# Importer

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Feature atlas for one wave-1 area: spud feature rows compared with Mudcrab.

Evidence and tracker statuses are wave-1 findings, not runtime verification. Report references resolve from this directory.

| Feature | Spud evidence level | Mudcrab status | Gap class | Report ref |
|---|---|---|---|---|
| Install, MO2 and asset sources | impl | shipped | parity | [importer](../wave1/importer.md) |
| Plugin loading and overrides | impl | shipped | parity | [importer](../wave1/importer.md) |
| Record schema and typed fields | impl | shipped; projection gaps | different approach | [importer](../wave1/importer.md) |
| Compiled forms database | impl | shipped SQLite database | different approach | [importer](../wave1/importer.md) |
| Incremental asset cache | impl | shipped; #448, #450 | parity | [importer](../wave1/importer.md) |
| World and interior conversion | impl | shipped | parity | [importer](../wave1/importer.md) |
| Terrain and grass metadata | partial | partial; #282, #539 | unclear | [importer](../wave1/importer.md) |
| NIF mesh and effects conversion | impl | partial; material gaps #484, #490 | spud ahead | [importer](../wave1/importer.md) |
| Texture dependency and DDS | impl | shipped KTX2 | different approach | [importer](../wave1/importer.md) |
| Sound records and files | impl | issue only #521, #522 | spud ahead | [importer](../wave1/importer.md) |
| HKX animation and skeletons | impl | partial; #471, #512–#515 | spud ahead | [importer](../wave1/importer.md) |
| Actor models and textures | impl | partial; #470 | spud ahead | [importer](../wave1/importer.md) |
| Papyrus scripts and attachments | impl | partial PEX translation | spud ahead | [importer](../wave1/importer.md) |
| Navigation meshes | unverified | partial records; no runtime consumer | unclear | [importer](../wave1/importer.md) |
| Rules, input, doors and cameras | impl | partial | spud ahead | [importer](../wave1/importer.md) |
| Weather and sky data | impl | partial; #546 | spud ahead | [importer](../wave1/importer.md) |
| UI, strings and loading data | impl | absent Skyrim menu framework | spud ahead | [importer](../wave1/importer.md) |
| Far terrain and macro maps | impl | shipped LOD; #235, #240 | different approach | [importer](../wave1/importer.md) |
| Tree impostors and placements | impl | shipped object LOD; exact counterpart unclear | unclear | [importer](../wave1/importer.md) |
| Region proxy LOD | impl | shipped LOD | different approach | [importer](../wave1/importer.md) |
| Scoped import and far CLI | impl | shipped converter CLI and reuse-assets | different approach | [importer](../wave1/importer.md) |
| Certified cluster overlay derivation | unverified | draft PR #439; stack #427–#462 | unclear | [inventory](../mudcrab-inventory.md) |
| Atomic pack manifest and publication | unverified | shipped | unclear | [inventory](../mudcrab-inventory.md) |
