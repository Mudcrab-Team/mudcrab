# Rendering questions from native code and mod audits

This queue joins the native map with two internet studies:
[authored lighting changes](mod-content-rendering.md) and
[source-available rendering interventions](mod-rendering-hooks.md).
The former covers nine author/maintainer pages; the latter pins six repositories.
Their detailed queries keep their original IDs. These priorities describe what
to trace next, not accepted vanilla algorithms or a proposed visual preset.

## Close brightness and color at the output path

The [shipped equations](image-space-arithmetic.md) establish conditional
operations that can affect brightness or saturation. Tone groups 26/27 combine
sampled inputs, perform four-component transforms, clamp RGB and apply a
logarithm/exponential chain. The fade variant adds a later RGBA blend. Fog groups
112/113 and lighting groups 98–100 have separate multipliers, conditions and
output clamps. Their resource producers and effective pass selection remain open.

These are concrete candidates to follow. They do not establish which operation
caused Mudcrab's dark or glowing screenshots. A fog-off result cannot supply an
exposure or ambient coefficient. The required connection is the effective input,
selected program, committed resources/constants, intermediate output and next
consumer in one matched view.

The [native extension](native-image-space-connections.md) closes effect-slot
identities, selected helper ownership, a compute binding/dispatch route and one
HDR child parameter link. That child is slot 58 / group 29 `ISHDRDownSample4`.
Its parameter channel number alone does not establish a shader buffer slot.
The tone and adaptation producers still need their own transfers.

## Combined tracing queue

| Priority | Connection to recover | Current evidence and query IDs | Acceptance |
| --- | --- | --- | --- |
| 1 | Active shader, resource and record winners | All 135 native package-group links; MC-Q01/10, M-Q01 | Exact opened bytes, plugin/loose/archive winners, key, shader object and branch at the selected draw. |
| 1 | Target formats and numeric domains | Target creation and ABI; M-Q02/06 | Scene, depth, adaptation, bloom and final descriptors, view formats, producers/consumers and shared color writers. |
| 1 | HDR, adaptation, bloom and modifiers | Scoped equations and static HDR child link; MC-Q05, M-Q07 | Packing/upload connects IMGS, ordered IMAD and frame state to each operand; selected intermediates establish order and feedback. |
| 1 | Distance fog, fog geometry and VOLI | Fog equations, Effect packing and volumetric helper ownership; MC-Q08, M-Q08/12 | Separate record/material producers, enabled branches, resources and composition. |
| 2 | Base/reference lights and fixture emission | Selected light consumption and authored controls; MC-Q02/03/06, M-Q04/10 | Effective color, radius, fade, dimmer, attenuation, transforms, enable state, geometry lists and shadow allocation; emission traced separately. |
| 2 | Interior ambient, templates and visibility | Ambient builder and inheritance segments; MC-Q04/07 | CELL/LGTM/room precedence, directional cube preparation, portal/multibound decisions and final per-view constants. |
| 2 | Terrain, LOD and large shadows | Settings/terrain/shadow candidates; MC-Q09, M-Q11 | Caster eligibility, cascade ranges, distance/fade consumers and effective mesh/light flags in the comparison view. |
| 2 | Skin, foliage, snow and transmission | Soft/rim/back segments and full SnowSSS program; M-Q09 | Original family/flag/texture selection, constants, surfaces and output; mod screen-space diffusion remains separate. |
| 3 | Scheduling and alternate permutations | Conditional frame graph and 124 effect registrations; M-Q03/04 | Relevant callbacks and branch predicates, then selected alternate keys; construction order supplies no universal pass order. |

The source studies did not execute their proposed native queries. The native
extension addresses parts of HDR/VOLI/compute ownership; it does not close an
entire row.

## Retain each mod's scope

Lux's author describes placements, imagespaces, meshes and portal-related
changes. These expose controls in different owning layers; they do not prove a
native light limit or provide vanilla brightness values.
[Lux author documentation](https://www.nexusmods.com/skyrimspecialedition/mods/43158).

Current Relighting Skyrim documentation excludes base IMGS, LIGH, LGTM and WTHR
edits while describing reference placement/fade/radius edits. Luminosity's
historical no-added-bulbs claim and its later contrast/adaptation choices have
separate version scopes. Actual plugin diffs remain unread.
[Relighting Skyrim](https://www.nexusmods.com/skyrimspecialedition/mods/8586),
[Luminosity](https://www.nexusmods.com/skyrimspecialedition/mods/16830).

The pinned Community Shaders code contains shader replacement, target changes,
additional light extraction, optional color conversions and added screen-space
effects. Its gamma helper and Linear Lighting compensations are mod operations;
they cannot identify the retail shader's input domain.
[Pinned color helper](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/package/Shaders/Common/Color.hlsli),
[pinned Linear Lighting source](https://github.com/community-shaders/skyrim-community-shaders/blob/d001d4de3cde4feeec539dbaca5481cdda2b1b15/src/Features/LinearLighting.cpp).

Source conflicts stay recorded: terrain's field-address predicate, Auto
Parallax's flag-clearing branch, differing Clone3D slot assumptions and a stale
particle-support absence claim. Hook IDs, slots and offsets are query seeds until
resolved against original target bytes. ENB documentation supplies interface and
release claims; its rendering core was not recovered in this study.

No renderer correction follows from these mod defaults. Implementation requires
the original producer-to-output contract and its selected retail evidence.
