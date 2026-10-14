# Native target and rendering identifiers

The target was reread from the original PE on 2026-10-10. Its SHA-256 is
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`,
its size is 37,910,440 bytes, and its file/product version resource is 1.7.104.0.
It is an AMD64 PE32+ image with preferred base `0x140000000`.
The [identity record](target-identity.json) includes section mappings and the
version-resource file offset. The refreshed Fiji file has the same hash.
This pins one executable; it does not establish behavior for other SE/AE builds.

## Original-byte inventories

| Inventory | Result | Evidence boundary |
| --- | --- | --- |
| RTTI type descriptors | 8,099 structurally recognized class/struct descriptors | Includes long template names; presence is not execution or a complete code inventory. |
| Rendering name candidates | 560 descriptors selected by the recorded keyword expression | Keyword selection can miss rendering code with unrelated names. |
| Complete-object locators | 8,632 structurally matched locators | Signature, self RVA, type RVA and hierarchy mapping were checked. |
| Rendering vtable candidates | 984 tables associated with selected descriptors | Pointer sequences stop at a non-executable pointer or 192 slots; this is not an authoritative method count. |
| Normal imports | 32 DLL descriptors | Dynamic and delay-loaded imports need separate tracing; COM methods are not import symbols. |
| Embedded shader occurrences | 57 bounded DXBC records | Fresh decoding and reflected metadata are recorded in [embedded shaders](embedded-shaders.json); creation and draw use are separate connections. |

[Native types](native-types.json) records descriptor, locator, vtable and method
addresses plus normalized RTTI hierarchy records. Each method pointer falls in a
PE section marked executable. That check does not establish its function body,
name or operation. A subsequent byte-checked native query must supply behavior.
The keyword expression and safety bounds remain attached to the catalog.

The hierarchy check reads the x64 MSVC class hierarchy and base descriptors from
the PE. It preserves flattened base order, member/vbtable displacement and
attributes. For example, the HDR Cinematic shader's `ImageSpaceEffect` base has
member displacement 144. Its effect vtable and main `BSShader` vtable must remain
distinct when resolving indirect callbacks. A type association does not prove
that an object of that type is selected in a frame.

[Rendering imports](native-render-imports.json) retains the normal-import entries
for `d3d11.dll`, `dxgi.dll` and `d3dx9_42.dll`. The first two name
`D3D11CreateDeviceAndSwapChain` and `CreateDXGIFactory`; their IAT addresses are
`0x1417C92E0` and `0x1417C9340`. The D3DX entries expose matrix/vector helpers.
Imported API names identify tracing seeds, not an active rendering API sequence.

## Method and provenance

The catalog generator reads original file bytes with a section-based RVA mapper.
It does not execute the game, rewrite the PE or use recovered Ghidra memory as
the source of file bytes. Its hash is attached to the identity record. Private
artifacts reside under
`/Users/taylor/.local/share/mudcrab-research/render-map-20261010/identity/`.

Native function evidence uses the saved, partially analyzed Ghidra project with
bounded read-only queries and no new whole-program analysis. Exported spans and
instructions are compared with the original PE; recovered function instructions
are independently decoded with LLVM. Truncation and missing recovered listing
remain explicit. Missing references in this project cannot prove the absence of
writers, users or alternate paths.

Public records contain identities, addresses, typed metadata and interpreted
behavior. Executables, shader payloads, instruction listings, textures and
original asset payloads remain in private research storage.
