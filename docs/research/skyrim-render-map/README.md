# Skyrim rendering evidence map

This is the central native rendering map for the copied Skyrim SE/AE target.
The selected shader package is fully inventoried, and all 135 group identities
have native loader links under the recorded package condition. Frame dispatch, resources,
material producers and input-to-pixel connections remain a partial native trace.
**No complete retail input-to-pixel trace or matched retail frame is closed.**
Open connections remain explicit; they do not acquire a default from a shader
name, a community reconstruction, a configuration file or a Mudcrab screenshot.

The [remediation and validation plan](../../specs/engine/skyrim-rendering-remediation-plan.md)
turns these findings into work packages, dependencies and separate numeric,
capture, retail, hardware and user-test gates.

The target is `SkyrimSE.exe` **1.7.104.0**, SHA-256
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
The original file was reread and its identity checked on 2026-10-10; the refreshed
Fiji installation matches it. Findings are scoped to this target and its pinned
assets. A different executable, shader package, resource override or active
configuration needs its own receipt.

## Map contents

| Map | What it establishes | What it leaves open |
| --- | --- | --- |
| [Frame topology](frame-topology.md) / [JSON](frame-topology.json) | Byte-checked conditional calls, effect dispatch, binding/commit routes, resource creation and presentation; explicit pass coverage. | Actual enabled branches, live effect/shader identities, complete call closure and resource generations at each draw. |
| [Shader families](shader-families.md) / [catalog](shader-inventory.json) | All 135 package groups and 16,044 programs: 3,390 VS, 12,635 PS and 19 CS; 8,057 unique bytecodes; signatures, declared bindings, constant reads, control-flow and instruction footprints. | Complete symbolic equations, remaining source-to-technique selection, sampler/view state and active use. |
| [Native shader selection](shader-selection.md) / [loader proof](native-shader-loader.json) / [selection questions](shader-selection-backlog.json) | Constructor, section order, metadata/payload reads, key-based cache and checked shader-binding connections, under the pinned package condition. | Effective opened resource, remaining selection/override paths and selected retail draws. |
| [Image-space native connections](native-image-space-connections.md) / [JSON](native-image-space-connections.json) | 124 effect-slot links, eight helpers with 26 shader fields, ten direct transfers, typed compute dispatch and one HDR child parameter connection. | Full helper closure, parameter meanings/transfers, mutable-global writers, effective branches and committed resources. |
| [Native output transfer](native-output-transfer.md) / [JSON](native-output-transfer.json) | One static output-target-0 route, renderer/data alias, backbuffer view imports, cached OM lookup and Present receiver; separate TAA and upsample ownership. | Selected draws/resources, every post-tone operation, numeric domains and displayed pixels. |
| [Shipped image-space arithmetic](image-space-arithmetic.md) / [equations](image-space-arithmetic.json) | Complete scoped equations for 54 groups / 108 records / 54 distinct programs: tone, adaptation, bloom/blur, fog/composition and SnowSSS; 1,821 executable instructions. | CPU producers, numeric/view domains, active pass order and equations for the rest of the package. |
| [Embedded shaders](embedded-shaders.md) / [catalog](embedded-shaders.json) | 57 executable shader occurrences, 53 unique bytecodes, fresh decoding and retained reflection metadata. | Native purpose and actual selected draw paths beyond the separately traced creation links. |
| [Input contracts](input-contracts.md) / [JSON](input-contracts.json) | 45 input domains, separately graded source/CPU/shader connections, retained settings and native query acceptance conditions. | Effective records/overrides, remaining producers and complete source-to-selected-draw connections. |
| [Native identity](native-identity.md) / [types](native-types.json) | Original-byte RTTI, class hierarchies, candidate vtables and method addresses; target version and file mappings. | Method semantics, object construction/selection and rendering code missed by name selection. |
| [D3D11 ABI](d3d11-abi.json) / [imports](native-render-imports.json) | Pinned primary SDK method slots, format enums and selected original import entries. | Native receiver identity and runtime API/state sequence. |
| [Runtime observations](runtime-observations.md) | Required session, view, resource, constant, shader, branch and output receipts for every rendering family. | A runnable retail session and the receipts themselves. |
| [Content lighting mods](mod-content-rendering.md) / [JSON](mod-content-rendering.json) | Nine author/maintainer pages, version boundaries and 32 findings tied to native questions. | Actual archive/record/mesh diffs and native/runtime confirmation of author claims. |
| [Rendering mod source](mod-rendering-hooks.md) / [JSON](mod-rendering-hooks.json) | Six pinned repositories, 22 interventions, 101 file/line/hash anchors and separately graded hook references. | Original target resolution of source IDs/offsets, mod execution and vanilla equivalence. |
| [Combined gap queue](rendering-gaps.md) | Prioritized output, fog, light, visibility, material and scheduling questions with query IDs and acceptance conditions. | The connections and observations named in each acceptance gate. |
| [Coverage snapshot](coverage.json) | Artifact hashes, verification counts, evidence boundaries and outstanding gates at this checkpoint. | Exhaustive reverse engineering or visual parity. |

The [interactive viewer](map.html) follows the curated call edges and filters input
domains and shader groups. Its capture gates stay open. The combined native
verification covers 45 successful bounded queries, 370 independently decoded functions and
85,200 original instruction starts, with no byte mismatches. The original-byte
checks include 37 distinct curated call/tail-transfer sites. These are counts of
checked evidence, not the size of Skyrim's entire rendering implementation.

[The checker](../../../scripts/check-skyrim-render-map.py) validates identities,
references, evidence hashes and package coverage. With `--target`,
`--shader-archive` and `--native-evidence`, it also rereads the original files and
private exports. [The viewer builder](../../../scripts/build-skyrim-render-map.py)
regenerates the page from the current public maps and coverage snapshot.

The output-transfer extension has separate verification counts. With `--target`,
the checker rereads its 1,569 curated instruction starts, decodes 24 direct
transfers and checks five encoded SDK call offsets. Add
`--native-output-evidence /path/to/native-output` to reread its five private query
exports and verify saved LLVM receipt hashes. This checks byte and receipt
coherence; it does not independently reconstruct all receiver, register and
control-flow meanings. The extension overlaps existing evidence and is **not
added to the central native union**.

Input-map project-source receipts retain their original hashes. After an
implementation change, pass `--project-source-evidence /path/to/checkpoint/workspace`
to check those historical files against the checkpoint receipt and map pins.
Coverage records the receipt hash, selected file hashes and differences from the
current source. It does not refresh the earlier interpretations or validate the
whole checkpoint inventory. Without this option, changed source pins fail the
check.

The main package payload is 67,768,128 bytes, SHA-256
`72cfc73eb0938d5949f0b3a8683722a33b671a34b41b750ba138de8d154c5fbc`.
Its archive is SHA-256
`fd1ba630f443353ed21ed4ca0380968f9c7091c8434518ea3cc79bc6d38c18e7`.
Every package entry was compared with original/extracted bytes and freshly
decoded. None has an RDEF chunk, so original buffer/resource variable names are
unavailable there. All 57 embedded occurrences retain RDEF; their names and
types remain scoped to those programs.

## Evidence boundaries

A native static connection requires the selected executable, original-byte
verification, the relevant operands/control flow and a stated scope. An
independently decoded instruction stream checks the recovered listing; it does
not turn a possible branch into observed execution. Partial Ghidra references
cannot establish that no other caller, writer or path exists.

A shader static connection identifies exact bytecode and its arithmetic or
operand consumption. The shipped program's presence does not prove that a
particular material selects it or that a resource named in a reconstruction
reaches its slot. Instruction footprints are inventories. The separate image-space
map closes equations for its stated 54-group scope, not the entire package.

ABI labels use Microsoft's pinned
[D3D11 header](https://github.com/microsoft/win32metadata/blob/76c04c2021ef4a831a6f1e06d9566002d746139b/generation/WinSDK/RecompiledIdlHeaders/um/d3d11.h),
[DXGI header](https://github.com/microsoft/win32metadata/blob/76c04c2021ef4a831a6f1e06d9566002d746139b/generation/WinSDK/RecompiledIdlHeaders/shared/dxgi.h)
and [format enum](https://github.com/microsoft/win32metadata/blob/76c04c2021ef4a831a6f1e06d9566002d746139b/generation/WinSDK/RecompiledIdlHeaders/shared/dxgiformat.h).
The catalog preserves exact line numbers and file hashes. Receiver and argument
provenance must still be proved in Skyrim before assigning an API operation.

Serialized source fields and configured settings establish stored data. Source
reconstructions establish search seeds. Mudcrab implementation establishes
project behavior. None establishes effective retail state. A runtime connection
requires the active object/branch, selected program, committed state and resource
receipts at the draw or dispatch; uncaptured values remain null.

## Full tracing scope

The frame map follows scheduling and scene traversal into batching, shader
setup, state/resource commits and presentation. Its conditional postprocess map
covers depth and shadow paths, SAO/fog, lighting composition, reflections,
volumetrics, HDR/bloom/adaptation, depth of field, temporal/motion processing and
UI paths at the evidence level attached to each row.

The input map covers plugin/resource winners; WTHR, CLMT, REGN, WRLD, CELL, LGTM,
IMGS, IMAD, VOLI, LIGH, WATR and reference overrides; material flags/parameters;
texture transfer and slot selection; authored vertex layouts/frames; transforms,
streams, indices, instances and culling; near terrain and LOD; trees and grass;
skin, face, hair and eyes; snow, parallax and environment maps; effects,
particles and decals; controllers, skeletons, skin partitions and morphs; camera,
sky and precipitation; settings, samplers and shared output controls. A row's
inclusion means it has been inventoried, not that its entire behavior is known.

## Connections that must not be collapsed

The inventoried Lighting vertex program consumes authored tangent, bitangent and
normal independently. Native layout/component decoding and static package
association are traced under the recorded package condition. Original stream
binding must still connect those operands to the source vertices in a selected
draw. Retained frame metadata or regenerated tangents cannot fill that connection.

Legacy DDS diffuse UNORM view creation is recovered in the native loader. That
supersedes an earlier semantic sRGB rule for the supported legacy path. UNORM
sampled code values do not by themselves establish a physical light domain.
Explicit typed formats, masks, specialized slots and selected draw views need
their own proofs. See the input map's cross-audit reconciliation.

Weather colors, ambient preparation, default sun writes, sunlight consumption,
local sun trajectory, stable IMGS packing and ordered IMAD composition have
recovered static segments. Live light replacement/dimmer, the meaning/value of
global RGB reads, parent transforms, active modifiers/curves and per-view ownership
remain separate questions. A zero-filled image location or a default writer does
not prove the effective value.

Shader setup, queued target changes and committed device state are distinct.
Indirect callbacks require the correct subobject/vtable. Conditional effect
array scans and ping-pong targets must retain their guards; flattening them into
one universal pass sequence would discard native behavior.

## Current implementation boundary

The pinned input map describes its recorded source bytes. R3 converter edits now
change `material.rs` and `mesh.rs`; the checker records that difference and checks
their earlier pins against the frozen source-before checkpoint. Those historical
receipts do not establish the new converter's behavior. See the current
[remediation plan](../../specs/engine/skyrim-rendering-remediation-plan.md) for R3
implementation and validation status. The rejected renderer candidate and the
frozen brighter recovery checkpoint retain their recorded status in
[lighting and shading](../../specs/engine/lighting-and-shading.md).
The failed screenshots do not supply native exposure, gamma, saturation,
ambient or fog values. A possible corrective effect stays open until its own
producer, selected shader and output connection are verified.

Private evidence resides under
`/Users/taylor/.local/share/mudcrab-research/render-map-20261010/`,
`/Users/taylor/.local/share/mudcrab-research/lighting-remediation-20261010/native-output/`
and the source-before checkpoint under
`/Users/taylor/.local/share/mudcrab-research/lighting-remediation-20261010/checkpoints/source-before-aq14hktq/`,
alongside the linked earlier research roots. Executables, original assets, shader
payloads and full native listings remain private. Public maps publish interpreted
behavior, curated instruction receipts, typed metadata, addresses, hashes and
explicit limits.
