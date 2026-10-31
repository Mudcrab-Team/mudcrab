# Lighting mods as rendering-input evidence

The author pages identify useful input controls and content constraints for the
Skyrim rendering map. They do not establish vanilla equations, native light limits,
or the cause of Mudcrab's brightness regressions. The
[machine-readable map](mod-content-rendering.json) records 32 findings, 108 scope
entries, eight native anchor groups and ten research questions. Every finding
retains its primary source, version scope, open connection and acceptance gate.

The study covers nine mod pages and three public file listings, retrieved on
2026-10-10. Private evidence contains the fetched web-tool text and SHA-256 hashes.
Seven direct HTTP requests returned 403; their error bodies are rejected as
documentation. Public download links returned HTML landing pages. No archive,
plugin, NIF, script or texture payload was obtained. Page versions therefore pin
the documentation, not an installed package. No game session, mod installation,
native query or runtime comparison was performed.

## Author evidence and version boundaries

The changes below are author-documented. Record types are named only where the
author names them; other native links are questions to investigate. Historical
changelog entries retain their version and do not establish current archive
contents.

| Primary source | Page version / last update as displayed | Documented scope |
| --- | --- | --- |
| [Lux](https://www.nexusmods.com/skyrimspecialedition/mods/43158) — GGUNIT | 7.1 / 2025-12-08 | Interior placements, templates, image spaces, meshes and visual effects; ENB-dependent options. |
| [Lux Orbis](https://www.nexusmods.com/skyrimspecialedition/mods/56095) — GGUNIT | 4.5 / 2024-11-29 | Exterior artificial lights, radius/fade, shadow lights, fixture emittance and mesh resources. |
| [Lux Via](https://www.nexusmods.com/skyrimspecialedition/mods/63588) — GGUNIT | 2.2 / 2025-02-01 | Road lighting, bridges, fixture materials, LOD and linked enable controls. |
| [Window Shadows RT](https://www.nexusmods.com/skyrimspecialedition/mods/37831) — HHaley and Dlizzio | RTbeta / 2021-07-27 | Interior window shadows, light-fade and glow-map options, restricted mod combinations. |
| [Window Shadows RT — Updated](https://www.nexusmods.com/skyrimspecialedition/mods/111091) — CarbonDice and Dlizzio; uploader Zanderat | 1.4 / 2024-12-07 | Bulb and ambient colors, shadow fixes; older updates address image spaces, mists and window emittance. |
| [Enhanced Lights and FX](https://www.nexusmods.com/skyrimspecialedition/mods/2424) — anamorfus | 3.06 / 2017-10-06 | Separate main, Enhancer/Hardcore, Exteriors and Weathers controls. |
| [Relighting Skyrim SE](https://www.nexusmods.com/skyrimspecialedition/mods/8586) — NovakDalton, JawZ and Step Modifications | 3.1 / 2026-03-16 | Reference placement/fade/radius, emittance, unlit fixtures and JITR switching. |
| [Luminosity](https://www.nexusmods.com/skyrimspecialedition/mods/16830) — JonnyWang; uploader DrJacopo | 4.2 / 2021-04-11 | Lighting recoloring, template/fog inheritance and image-space controls. |
| [Ambiance](https://www.nexusmods.com/skyrimspecialedition/mods/46383) — TheMilesO | 1.1 / 2021-03-19 | Ambient/directional lighting, fog colors/intensity and selected image spaces. |

Lux's 6.9 portal-strict option trades reduced drawcalls against possible omitted
object lighting. Its 6.8 window planes, multibound and fog shaders raise separate
geometry, visibility and material questions. The stated four-light limit needs
native allocation proof.
[Lux changelog and description](https://www.nexusmods.com/skyrimspecialedition/mods/43158).

Orbis claims weather-mod compatibility while choosing lower fades for ENB.
That claim needs both record-winner and effective-state checks; it supplies no
vanilla coefficient.
[Orbis description](https://www.nexusmods.com/skyrimspecialedition/mods/56095).

The Window Shadows product label “simulated ray tracing” does not establish
native ray tracing, global illumination or a recovered algorithm. The original
and update must also be treated as separate resources.
[Original description](https://www.nexusmods.com/skyrimspecialedition/mods/37831),
[update requirements](https://www.nexusmods.com/skyrimspecialedition/mods/111091).

ELFX's module boundary matters: Enhancer adjusts fog and ambient appearance;
Exteriors changes landscape/no-fade flags; Weathers changes atmospheric conditions.
The combined screenshot cannot identify which input changed.
[ELFX module descriptions](https://www.nexusmods.com/skyrimspecialedition/mods/2424).

Current Relighting Skyrim explicitly excludes base Image Space, Light, Lighting
Template and Weather record changes. This can coexist with reference overrides
and weather/emittance links. Older Luminosity text describes RS more broadly;
the sources need their version boundaries. Neither statement substitutes for a
plugin inventory.
[RS compatibility](https://www.nexusmods.com/skyrimspecialedition/mods/8586),
[Luminosity compatibility](https://www.nexusmods.com/skyrimspecialedition/mods/16830).

Luminosity's no-added-bulbs statement covers versions 1–3. Its files still expose
a distinct 3.1 release alongside Cathedral Luminosity. The 4.2 changelog reports
contrast 1.0 and disabled eye adaptation; those are mod choices.
[Luminosity description/changelog](https://www.nexusmods.com/skyrimspecialedition/mods/16830),
[public file listing](https://www.nexusmods.com/skyrimspecialedition/mods/16830?tab=files).

Ambiance's author-selected fog/ambient reductions are not vanilla defaults.
Competing CELL/template changes are a documented compatibility concern.
[Ambiance description](https://www.nexusmods.com/skyrimspecialedition/mods/46383).

## Connections to the native map

The anchors below summarize existing
[input contracts](input-contracts.json), not new facts learned from mods.
Shader associations retain the pinned-package precondition from the
[native loader proof](native-shader-loader.json). Actual resource winners and
selected retail draws remain unobserved.

| Anchor | Existing connection | Required proof |
| --- | --- | --- |
| MC-A01 / I-C01 | Authored record/resource candidates are retained. | Exact plugin, archive, loose-resource and override winners. |
| MC-A02 / I-C15–16 | Selected shader light consumption; base and reference inputs are retained separately. | Effective radius/fade/color/transform/enable state, per-geometry list and shadow allocation. |
| MC-A03 / I-C03, I-C09–10 | Ambient affine builder/upload and selected fog inheritance. | Interior cube precedence, angles/fade, room owner and effective per-view values. |
| MC-A04 / I-C12–13 | Stable image-space packing, base preference and ordered modifier composition. | Active IMGS identity, IMAD curves/list/time/strength and adaptation state. |
| MC-A05 / I-C17–18, I-C23, I-C25, I-C38 | Selected material constants, raw keys, packed input layout and shader interfaces. | Fixture emission/controllers, full source stream binding, partitions and resource selection. |
| MC-A06 / I-C11, I-C25 | Project room links; native visibility remains open. | Containment, portals, multibounds, portal-strict branching and local-light ownership. |
| MC-A07 / I-C14, I-C35 | Selected Effect fog packing. | Effect material/blending, geometry and separate VOLI selection/composition. |
| MC-A08 / I-C27, I-C42 | Settings objects and selected consumers are cataloged. | Applied settings, terrain-light flag behavior and visibility distances. |

Three distinctions matter for brightness work:

- A glowing window or flame material and its attached illuminating light need
  separate source and draw receipts. Emission, weather emittance, bulb dimmer,
  exposure and bloom must be followed independently.
- CELL/LGTM distance fog, fog/beam geometry and weather volumetrics need separate
  records and pass evidence. A mod's fog change does not show that fog corrects
  another lighting error.
- Geometry partitioning, occlusion and scripted light switching may alter which
  light contributes to a draw. The observed result requires light-list and shadow
  allocation evidence, not a guessed brightness multiplier.

No studied archive has a verified IMAD presence or absence. The native IMAD
composition route is known statically; the active stack and concrete curve
evaluators remain separate gates. The extra RGB read at `0x14331D6F0` also has
unresolved meaning and cannot yet be called a mutable ambient setting.

## Bounded research and acceptance

The JSON attaches these questions to each finding. They are queued work, not
executed experiments.

| Question | Next owning-layer check | Acceptance |
| --- | --- | --- |
| MC-Q01 | Hash any later authorized archives/options and diff typed records/assets. | Reproducible FormIDs, masters, optional-plugin set and source winners. |
| MC-Q02 | Trace effective light producers near `0x14154B770` and `0x141548820`; keep directional entry-zero packing separate. | One source light/reference reaches a proven object, geometry list and constants. |
| MC-Q03 | Recover per-geometry list count/order, shadow-channel allocation and replacement. | Native cap/policy plus later isolated geometry and added-light controls. |
| MC-Q04 | Follow CELL/LGTM/room precedence into `0x1414EC3B0` and `0x14154B880`. | One source cube and fog owner, with inherited versus explicit values preserved. |
| MC-Q05 | Trace manager base pointers and concrete IMAD curves through `0x1414EEC10`, `0x14153D860` and `0x14065A250`. | Active base, ordered stack and uploaded channels, including adaptive state. |
| MC-Q06 | Follow fixture emission/glow/emittance/controllers through material loading and key selection. | Separate material and placed-light contributions, with source textures/controllers. |
| MC-Q07 | Trace original light/room flags to portal/multibound traversal and list filtering. | Byte-proven flag branch; later adjacent-room controls explain missing illumination. |
| MC-Q08 | Follow Effect material/blend inputs near `0x141556840`, `0x141556C40`, `0x141557090`; trace VOLI independently. | Distance fog, effect geometry and volumetric state identify the changed pass. |
| MC-Q09 | Locate landscape-light/no-fade flag branches and applied distance-setting consumers. | Source flag, active setting, list membership and terrain contribution agree. |
| MC-Q10 | Fix source winners, camera, time/weather, external renderer state and exposure before comparison. | Each screenshot difference is explained by records/assets and effective constants. |

These checks can distinguish an authored look from a renderer defect. They do not
justify importing a mod palette, fade scale, fog percentage or contrast setting
into the vanilla implementation.

