# Formats

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Feature atlas for one wave-1 area: spud feature rows compared with Mudcrab.

| Feature | Spud evidence level | Mudcrab status | Gap class | Report ref |
|---|---|---|---|---|
| Shared cache envelope | impl | shipped directory pack and manifest | different approach | [formats](../wave1/formats.md) |
| Binary IO and byte order | impl | shipped versioned formats; endian handling unclear | unclear | [formats](../wave1/formats.md) |
| Form database | impl | shipped SQLite, schema 9 | different approach | [formats](../wave1/formats.md) |
| DDS images | impl | shipped KTX2, #222 | different approach | [formats](../wave1/formats.md) |
| Versioned native cache family | impl | shipped glTF, rkyv, SQLite and manifest | different approach | [formats](../wave1/formats.md) |
| Inspection CLI | impl | shipped converter and world-inspect | parity | [formats](../wave1/formats.md) |
| Certified cluster payload | unverified | draft PR #430 | unclear | [inventory](../mudcrab-inventory.md) |
