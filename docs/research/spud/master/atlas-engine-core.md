# Engine core and Papyrus

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Feature atlas for one wave-1 area: spud feature rows compared with Mudcrab.

| Feature | Spud evidence level | Mudcrab status | Gap class | Report ref |
|---|---|---|---|---|
| Frame and world scheduling | impl | shipped Bevy schedules | different approach | [engine-core](../wave1/engine-core.md) |
| First-person camera | impl | partial; #511 | spud ahead | [engine-core](../wave1/engine-core.md) |
| Third-person camera | impl | partial; coverage unclear | unclear | [engine-core](../wave1/engine-core.md) |
| Movement and locomotion | impl | partial; #464, #506, #544 | spud ahead | [engine-core](../wave1/engine-core.md) |
| Keyboard, mouse and controller | partial | shipped controls; gameplay actions limited | unclear | [engine-core](../wave1/engine-core.md) |
| Game clock | impl | partial sky time; game calendar unclear | unclear | [engine-core](../wave1/engine-core.md) |
| Grass generation and streaming | impl | open PR #539; #282 | spud ahead | [engine-core](../wave1/engine-core.md) |
| Menu backdrop | impl | absent Skyrim menu | spud ahead | [engine-core](../wave1/engine-core.md) |
| Precipitation occlusion | impl | partial sky; counterpart unclear | unclear | [engine-core](../wave1/engine-core.md) |
| Graphics presets and user settings | impl | shipped; #229, #262 | parity | [engine-core](../wave1/engine-core.md) |
| Benchmark harness and scripted automation | impl | shipped profiling and route tools | different approach | [engine-core](../wave1/engine-core.md) |
| Live control socket | impl | shipped live console; protocol differs | different approach | [engine-core](../wave1/engine-core.md) |
| Papyrus bytecode execution | impl | partial PEX to Lua | spud ahead | [engine-core](../wave1/engine-core.md) |
| Papyrus tasklets, scheduling and waits | impl | partial Papyrus runtime | spud ahead | [engine-core](../wave1/engine-core.md) |
| Papyrus events and attachments | impl | partial Papyrus runtime | spud ahead | [engine-core](../wave1/engine-core.md) |
| Papyrus native API | partial | partial Papyrus runtime | unclear | [engine-core](../wave1/engine-core.md) |
| Papyrus support counts | unverified | partial; complete coverage unverified | unclear | [engine-core](../wave1/engine-core.md) |
| Cell and space streaming | impl | shipped streaming and pacing | different approach | [engine-core](../wave1/engine-core.md) |
| World simulation | impl | partial static world | spud ahead | [engine-core](../wave1/engine-core.md) |
| Weather and interior environment | impl | partial; #480, #488, #546 | spud ahead | [engine-core](../wave1/engine-core.md) |
| Water rendering state | impl | partial; #299, #501, #545 | spud ahead | [engine-core](../wave1/engine-core.md) |
| World effects and reference effects | impl | partial; exact coverage unclear | unclear | [engine-core](../wave1/engine-core.md) |
| World map camera and UI scenes | impl | absent Skyrim world map | spud ahead | [engine-core](../wave1/engine-core.md) |
| Comparison mode | impl | shipped captures; equivalent unclear | unclear | [engine-core](../wave1/engine-core.md) |
| Synthetic test cache | impl | shipped dummy-content fixtures | different approach | [engine-core](../wave1/engine-core.md) |
| Face tint application | impl | partial actor appearance; #470 | unclear | [engine-core](../wave1/engine-core.md) |
| Door helpers | impl | shipped transitions; #473 | parity | [engine-core](../wave1/engine-core.md) |
| Developer console and cell commands | impl via app | shipped; #494 | parity | [inventory](../mudcrab-inventory.md) |
