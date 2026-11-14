# Physics

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Feature atlas for one wave-1 area: spud feature rows compared with Mudcrab.

| Feature | Spud evidence level | Mudcrab status | Gap class | Report ref |
|---|---|---|---|---|
| Static cell collision | impl | partial NIF collision; #420, #474–#477, #486 | spud ahead | [physics](../wave1/physics.md) |
| Terrain collision | impl | shipped terrain collision | parity | [physics](../wave1/physics.md) |
| Static shapes and collision layers | impl | partial; #474–#477 | spud ahead | [physics](../wave1/physics.md) |
| Walking and jumping controller | impl | partial; #464, #482, #544 | spud ahead | [physics](../wave1/physics.md) |
| Stable collision and tick readiness | impl | partial streaming; equivalent policy unclear | unclear | [physics](../wave1/physics.md) |
| Raycasts | impl | shipped Rapier integration; specific coverage unclear | unclear | [physics](../wave1/physics.md) |
| Movable rigid bodies | partial | unverified | unclear | [physics](../wave1/physics.md) |
| Constraints | partial | unverified | unclear | [physics](../wave1/physics.md) |
| Trigger volumes | partial | unverified | unclear | [physics](../wave1/physics.md) |
| Ragdolls | partial | unverified | unclear | [physics](../wave1/physics.md) |
| Cloth and hair | unverified | unverified | unclear | [physics](../wave1/physics.md) |
| Physics backend | impl Jolt | shipped Rapier | different approach | [inventory](../mudcrab-inventory.md) |
| Navmesh traversal | impl in game | absent runtime; #385 | spud ahead | [inventory](../mudcrab-inventory.md) |
