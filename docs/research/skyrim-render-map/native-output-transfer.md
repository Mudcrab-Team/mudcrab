# Native output targets, views and presentation

The examined frame route requests output target **0**. This extension follows
that index into image-space resource objects, connects target 0's cached views
to the current swapchain backbuffer, and verifies the target-list lookup used by
`OMSetRenderTargets`. It also identifies the conditional TAA and dynamic-resolution
upsample helpers after the tone/Copy router call.

These are static connections for `SkyrimSE.exe` **1.7.104.0**, SHA-256
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
No retail resource, active branch, selected draw or displayed pixel was observed.
Other SE/AE builds need separate evidence. The
[catalog](native-output-transfer.json) retains original instruction receipts,
source hashes, API operands and verified direct transfers.

## Renderer and backbuffer ownership

The initializer at `0x1400E97B0` writes address `0x143331F50` into the renderer-data
global `0x143330188` at `0x1400E97CC`. The examined frame passes renderer address
`0x143331F40` to Begin and End. A separate constructor at `0x141007B20` writes
its receiver plus `0x10` into the same global. Under these initialization paths,
renderer data starts **16 bytes into the renderer**. This offset is necessary
when comparing helpers that use different receiver bases.

Swapchain creation writes format 28, `DXGI_FORMAT_R8G8B8A8_UNORM`, at
`0x141009BC2`. Each window record occupies `0x50` bytes. Its base is
`renderer + 0x58 + 0x50*window_index`; swapchain, RTV and SRV pointers are at
window offsets `+0x18`, `+0x30` and `+0x38`.

The backbuffer helper at `0x141012480` obtains buffer 0 and creates its views:

| Native site | SDK operation | Retained arguments or destination |
| --- | --- | --- |
| `0x1410124B4` | `IDXGISwapChain::GetBuffer` | index 0; receiver from `data + 0x60 + 0x50*window_index` |
| `0x1410124DA` | `ID3D11Device::CreateRenderTargetView` | that buffer; null description; output at `data + 0x78 + 0x50*window_index` |
| `0x141012501` | `ID3D11Device::CreateShaderResourceView` | that buffer; null description; output at `data + 0x80 + 0x50*window_index` |

A null RTV description accesses mip level 0; a null SRV description uses the
resource's format. These API semantics come from Microsoft's
[RTV](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11device-createrendertargetview)
and [SRV](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11device-createshaderresourceview)
documentation. API success remains unobserved.

Begin selects the requested window, with a fallback to window 0 when the selected
swapchain pointer is null. It stores the window record at `0x143330198`, then
copies the window RTV and SRV into target 0 at `0x141009EE9` and `0x141009EFB`:

| Pointer | Renderer-relative destination | Data-relative destination |
| --- | --- | --- |
| target 0 RTV | `+0xA68` | `+0xA58` |
| target 0 SRV | `+0xA70` | `+0xA60` |

Initialization independently imports the first window's views into those same
data slots at `0x141007E02` and `0x141007E17`, under its local initialization guard.
The descriptor helper `0x141015400` copies **28 bytes**. Its inspected body
performs no resource allocation or color arithmetic.

## Requested output and cached OM binding

At `0x140656A73`, the frame zeros EDX before calling scene wrapper
`0x140656D10`. The wrapper preserves that argument and forwards it to
`0x140656E60`. The scene stores it, then passes it in ECX to pre-image-space
`0x14153AC00` at `0x1406578D1`, when the scene's two enable bytes permit that call.
Pre-image-space passes the saved output in R8D to manager `0x1414EDBF0` at
`0x14153ADE7`, with source index 1. This establishes one requested-output route;
other frame clients have separate argument flows.

The manager selects HDR index 30 or Copy index 34 from byte `0x1420D75E4` and
calls router `0x1414EE060` at `0x1414EDD68`. Its local direct-output decision
selects the caller's output or intermediate target 41. The router initializes an
output reference with index R9D in reference field `+0x8`, stores that reference
in effect resource slot 0, then invokes the selected effect's virtual Render.

Within the examined HDR Render, its final child receives that same output
reference in resource slot 0 at `0x14153D747`. The generic image-space shader
Render at `0x141564E30` reads resource slot 0's index through `0x1415550B0`, then
passes it to list-stage helper `0x141015730`. That helper stages color slot 0
through `0x1410155B0` and removes additional color slots from the cached list.
The HDR child's identity and shader dispatch remain prerequisites; these argument
connections do not prove that a selected retail tone draw reached them.

For an ordinary target list, state apply at `0x141010DE0` reads each cached index
and loads its RTV using:

```text
RTV = *(renderer_data + 0xA58 + 48*target_index)
```

The ordinary-list path requires dirty bit 1 and cube index `-1`. At
`0x141010FC0`, context virtual `+0x108` submits the resulting list through
`ID3D11DeviceContext::OMSetRenderTargets`. When slot 0's staged index is 0 and the
imports remain intact, its RTV is the imported current-window backbuffer RTV.
The helper `0x141009960` stages viewport state; it does not perform the OM bind.
The method identities use the pinned [D3D11 ABI catalog](d3d11-abi.json).

## Conditional work after tone/Copy

The factory's pointer loads, null-preserving `-0x90` adjustments, constructor
arguments and owner stores establish two additional helper identities. Their
shader registrations come from the existing
[connection catalog](native-image-space-connections.json).

| Manager field | Constructor / Render | Stored shader dependencies |
| --- | --- | --- |
| `+0x1F0` | `0x141544D30` / `0x141544DA0` | slots 151–153: `ISTemporalAA`, `ISTemporalAA_UI`, `ISTemporalAA_Water` |
| `+0x1F8` | `0x141546490` / `0x141546520` | slot 154: `ISUpsampleDynamicResolution` |

The manager can call the TAA helper at `0x1414EDDBF`. The helper returns its input
index when byte `+0x18` is zero; that byte is initialized from mutable global
`0x1420D8C28`. Its active route stages the requested output through
`0x1410155B0` at `0x141544E37`, reads the input SRV and maintains conditional
history targets 81–84. Helper state selects ordinary, UI or water shader setup.
History validity, selected shader key and effective enablement remain open.

The manager's later upsample call is `0x1414EDF9C`, subject to the helper's
`+0x8` flag and dynamic-resolution state. The helper returns its input when its
global guard or the emitted scale-comparison branches skip the work. Its active
route stages the requested output at `0x141546609` and reads the input SRV at
`0x141546626`.

Both helpers read source SRVs at
`renderer + 0xA70 + 48*input_index`, equivalent to
`renderer_data + 0xA60 + 48*input_index` under the established alias. These loads
stage PS resource slot 0. A later apply and successful draw still need selected
runtime evidence. The manager also has deferred-effect and copy branches; this
map supplies no universal sequence of postprocess passes.

## Presentation and the remaining transfer gap

End loads the current-window pointer, takes its swapchain at `+0x18` and calls
virtual `+0x40`, `IDXGISwapChain::Present`, at `0x14100A012`. `SyncInterval` is
the runtime DWORD at `renderer_data + 0x30`; flags are zero. This receiver
connection closes the examined backbuffer/window association to the API
presentation boundary, subject to the ownership and execution preconditions.

UNORM storage has no sRGB render-target write conversion. That format fact does
not establish the shader output's numeric domain, a scene gamma policy or the
displayed transfer. Microsoft distinguishes UNORM and sRGB format behavior in
the [functional specification](https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm);
the [output-merger documentation](https://learn.microsoft.com/en-us/windows/win32/direct3d11/d3d10-graphics-programming-guide-output-merger-stage)
describes the conversion applied when an sRGB render target is used.

A conditional post-image client at `0x14153ABB0` also stages target 0, then enters
`0x141520CB0`. Its subsystem, selected shaders, blending and composition are
unresolved. Address proximity does not establish a UI role. Every selected
operation after the tone output, API results, display gamma, compositor or
translation-layer behavior and screenshot transfer still require observation.
These findings justify capture boundaries; they supply no new brightness,
saturation, fog or gamma defaults.

## Verification and next evidence

Five new bounded read-only queries reused the partial persistent project with
analysis disabled. They requested 24 spans and exported 3,633 unique original
instruction starts. Twenty-three recovered functions supplied 1,776 instructions
for independent LLVM decoding. All file-backed span, original instruction and
LLVM comparisons passed. One data seed without file backing supplies no live
pointer value.

The catalog separately rechecks 33 selected receipts from new and cached queries:
1,569 unique original instruction starts, 24 signed-rel32 direct transfers and
five SDK method identifications. Counts overlap prior evidence and must not be
added to the central union. No recovered function export was truncated.

The next runtime capture should pair the selected tone/HDR child with its output
reference, shader stage hashes, constants, input and output views, then record
each later draw/copy through the last target-0 write and Present. It should retain
raw resource pixels before capture/display encoding. Until that record exists,
the complete tone-to-present pixel transfer and native numeric domains remain
open.
