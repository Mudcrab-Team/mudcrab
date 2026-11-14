# Rendering

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Feature atlas for one wave-1 area: spud feature rows compared with Mudcrab.

| Feature | Spud evidence level | Mudcrab status | Gap class | Report ref |
|---|---|---|---|---|
| Graphics device | impl | shipped Bevy renderer; Metal and Windows backends | different approach | [render](../wave1/render.md) |
| Frame ordering | impl | shipped Bevy render schedule | different approach | [render](../wave1/render.md) |
| GPU timings | impl | shipped profiling tools | parity | [render](../wave1/render.md) |
| Descriptor and texture strategy | impl | shipped Bevy; strategy differs | different approach | [render](../wave1/render.md) |
| Memory and lifetime | impl | shipped Bevy; exact policy unclear | unclear | [render](../wave1/render.md) |
| Shared GPU arenas | impl | partial indirect/LOD path; exact counterpart unclear | unclear | [render](../wave1/render.md) |
| GPU-driven mesh submission | impl | partial indirect path; draft clusters #441, #533 | spud ahead | [render](../wave1/render.md) |
| Material specialization | impl | shipped NIF materials; #435, #484, #490 | parity | [render](../wave1/render.md) |
| Depth and MSAA | impl | shipped renderer; exact policy unclear | unclear | [render](../wave1/render.md) |
| Culling and LOD | partial | shipped LOD; cluster drafts #441, #533 | unclear | [render](../wave1/render.md) |
| Terrain | impl | shipped; #236, #547 | parity | [render](../wave1/render.md) |
| Grass | impl | open PR #539 | spud ahead | [render](../wave1/render.md) |
| Distant trees | impl | shipped object LOD; impostor coverage unclear | unclear | [render](../wave1/render.md) |
| Sky, weather and clouds | impl | partial; #546 | spud ahead | [render](../wave1/render.md) |
| Local lighting | impl | partial; #480, #488 | spud ahead | [render](../wave1/render.md) |
| Directional and local shadows | impl | shipped shadows; local parity unclear | unclear | [render](../wave1/render.md) |
| Shadow caching | partial | shipped shadows; cache semantics unclear | unclear | [render](../wave1/render.md) |
| Lighting and effect shaders | impl | partial NIF materials; #435, #484, #490 | spud ahead | [render](../wave1/render.md) |
| Skin, hair, eyes, glow, parallax and environment | impl | partial materials; #490 | spud ahead | [render](../wave1/render.md) |
| Snow materials and screen effect | impl | partial environment; exact coverage unclear | unclear | [render](../wave1/render.md) |
| Material shadow evaluation | impl | shipped shadows and materials; fidelity unclear | unclear | [render](../wave1/render.md) |
| Water, reflection and refraction | impl | partial; #299, #501, #545 | spud ahead | [render](../wave1/render.md) |
| Water history and ripples | impl | partial rippled water; #545 | unclear | [render](../wave1/render.md) |
| Screen-space reflections | impl | partial water reflection; exact coverage unclear | unclear | [render](../wave1/render.md) |
| Ambient occlusion | impl | shipped Bevy renderer; configured support unclear | unclear | [render](../wave1/render.md) |
| Volumetric lighting | impl | partial lighting; exact coverage unclear | unclear | [render](../wave1/render.md) |
| HDR, exposure, bloom and tone mapping | impl | shipped color pipeline | parity | [render](../wave1/render.md) |
| Depth of field and distant blur | impl | shipped Bevy renderer; feature coverage unclear | unclear | [render](../wave1/render.md) |
| Anti-aliasing and temporal policy | impl | shipped Bevy renderer; policy differs | different approach | [render](../wave1/render.md) |
| Particles and effects | impl | partial NIF material/effect support | spud ahead | [render](../wave1/render.md) |
| Rain and snow | impl | partial weather; #546 | spud ahead | [render](../wave1/render.md) |
| Async work and performance instrumentation | impl | shipped streaming, pacing and profiling | different approach | [render](../wave1/render.md) |
| Vanilla appearance | unverified | partial; platform parity unverified | unclear | [render](../wave1/render.md) |
| Certified cluster rendering | unverified | draft PR #441, #533 | unclear | [inventory](../mudcrab-inventory.md) |
