# Native final exponent and fGamma

Research date: 2026-10-15. The pinned retail executable establishes that the
final group-26/27 shader exponent `cb12[42].x` receives **`1.0f / fGamma`** on
the examined native upload route. This closes the setting-to-exponent gap using
original instructions, copied-buffer offsets, and the D3D11 binding arguments.
It does not infer the relationship from a public shader label.

This is a static result for executable SHA-256
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`
and shader-package SHA-256
`72cfc73eb0938d5949f0b3a8683722a33b671a34b41b750ba138de8d154c5fbc`.
It assumes the expected objects and package are used, API operations succeed,
and execution reaches the route. No effective runtime setting, GPU input, active
draw, screenshot, or display transfer is observed by this investigation.

## Setting identity and scalar producer

The original PE has one pointer to exact string `fGamma:Display` at
`0x141A74720`. That pointer is stored at setting `+0x10`, address
`0x1420D0350`; the setting's float value is at `0x1420D0348`, eight bytes
before the name pointer. Its static initializer is binary32 `1.0`. This is an
initial file value, not an effective INI value or a measured session default.

Function `0x14100AE80` tests byte `0x143336400`; a zero value skips the
examined frame-buffer update. On the active route, these instructions establish
the scalar calculation:

| Native site | Original instruction | Consequence |
| --- | --- | --- |
| `0x14100B299` | `MOVSS XMM1,dword ptr [0x141B5BE28]` | Loads binary32 literal `1.0`. |
| `0x14100B314` | `MOVAPS XMM0,XMM1` | Copies that numerator. |
| `0x14100B317` | `DIVSS XMM0,dword ptr [0x1420D0348]` | Divides by the `fGamma:Display` float. |
| `0x14100B31F` | `MOVSS dword ptr [RBP + 0x1E0],XMM0` | Stores the result in the CPU block. |

No clamp, reciprocal helper, `2.2` multiplier, or additional exponent transform
intervenes in this producer-to-store chain. The producer uses single-precision
division; the mathematical notation `1/fGamma` does not promise bitwise
agreement with a reassociated shader expression. The function does not establish
that arbitrary zero, negative, or nonfinite setting values produce valid output.

Private evidence: [setting references](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-output-20261015/output-query-01.json>),
[producer and buffer references](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-output-20261015/output-query-02.json>).

## Exact offset and upload

Let `S` denote the function's RSP after its stack allocation. Prologue
`0x14100AE85`–`0x14100AE8E` makes `RBP = S + 0x100`. The stored scalar is
therefore at `S + 0x2E0`.

At `0x14100B46C`, the copy source begins at `S + 0x40`. The scalar's byte
offset within that source is consequently:

```text
(0x100 + 0x1E0) - 0x40 = 0x2A0 = 42 * 16
```

The producer maps the buffer handle loaded from global `0x143331F08`, using
context global `0x143331F30` and context virtual `+0x70` at `0x14100B440`.
The pinned D3D11 ABI identifies this as `Map`; arguments select subresource 0,
`D3D11_MAP_WRITE_DISCARD` (4), flags 0, and a stack `D3D11_MAPPED_SUBRESOURCE`.

When Map supplies a nonnull destination, five iterations copy `0x80` bytes each,
then a tail copies `0x50` bytes. The total copied block is `0x2D0` bytes, or 45
float4s. The scalar belongs to the tail's third float4: source load
`0x14100B4DD` and destination store `0x14100B4E9` copy it without arithmetic.
The function then loads the same buffer-handle global at `0x14100B503` and
calls context virtual `+0x78`, `Unmap`, at `0x14100B50D`.

Thus the scalar reaches the mapped buffer at byte `0x2A0`, component x of
float4 index 42. This conclusion follows from original stack and copy operands,
not from the field order of a public C++ reconstruction.

## Buffer slot 12 and the shader consumer

Renderer Begin function `0x141009D30` binds that same buffer-handle global to
both vertex and pixel constant-buffer slot 12. The pixel binding is explicit:

| Site | Argument or operation |
| --- | --- |
| `0x141009EA8` | R9 receives `&0x143331F08`, the buffer-pointer array. |
| `0x141009EAF` | EDX receives `0xC`, StartSlot 12. |
| `0x141009EB7` | R8D receives 1, NumBuffers. |
| `0x141009EBB` | Context virtual `+0x80`: `PSSetConstantBuffers`. |

The vertex binding precedes it at `0x141009E9E`, through context virtual
`+0x38`. A separate native rebind helper, `0x1410107B0`, repeats the same
slot-12 VS/PS bindings. These are possible bindings on the examined route;
other code or replacement shaders can change effective runtime state.

The freshly rehashed group-26 and group-27 pixel bytecode matches the previously
audited arithmetic. Their final RGB sequence clamps the graded RGB, takes
`log2`, multiplies by `cb12[42].x`, then applies `exp2`. Conceptually, for
ordinary finite valid inputs, this is the final power
`saturate(gradedRGB)^(1/fGamma)`. Group 27 applies its fade afterward.

Private evidence: [binding query](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-output-20261015/output-query-03.json>).
API identities: [pinned D3D11 ABI](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/d3d11-abi.json).
Shader operands and D3D edge behavior: [audited retail arithmetic](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/image-space-arithmetic.md:80).

## Runtime readback and remaining transfer limits

A future bounded read-only memory observation can read the binary32 setting at
the relocated address `0x1420D0348`, verify the adjacent name pointer and exact
string, and report a **derived** reciprocal. That is process-setting evidence;
it is not a direct read of the GPU constant buffer or a synchronized frame.
The upload's Map pointer is stack-local and transient. Reading the opaque D3D
buffer handle at `0x143331F08` does not expose its GPU bytes.

This closes the source of the audited shader's final exponent. It does not
establish the sampled scene/bloom/adaptation textures' numeric domains, SRV
format conversions, later post-processing, translation-layer behavior, window
compositing, display gamma, or screenshot encoding. In particular, the
reciprocal-gamma power is not proof of the exact IEC sRGB transfer function or
proof that its input is scene-linear.

The existing [output-target audit](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/native-output-transfer.md)
separately establishes a conditional route to an `R8G8B8A8_UNORM` backbuffer.
That format has no sRGB render-target write conversion. The route still includes
conditional later operations and does not establish the final displayed transfer.

## Verification

- Three successful bounded Ghidra queries reused `SkyrimMovement17104` with
  `-noanalysis -readOnly`; no cold import or executable launch occurred.
- Original PE/package hashes matched the pinned targets. All 542 instructions
  in four relevant functions matched the original PE bytes and independently
  calculated file offsets. Independent Apple LLVM decoding matched all starts
  and encodings, with zero mismatches and no prefix-normalization joins.
- Verified original setting-name pointer, name string, static float initializer,
  numerator literal, scalar instruction chain, stack-offset arithmetic, copy
  extent, buffer-handle identity, and slot-12 binding operands.
- Group-26/27 pixel bytecode slices matched their existing inventory hashes.
  The final log/multiply/exp arithmetic is reused from the audited shader corpus;
  this research closes its native producer rather than claiming a fresh shader
  disassembler run.
- A seed labeled `per-frame-create` landed in resource cleanup. It is excluded
  from creation claims. That label did not establish the buffer's construction.
- No engine/default edits, tests/builds, captures, runtime-memory reads, game
  launches, Fiji changes, or publication occurred in this static investigation.

The [machine-readable verification](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-output-20261015/output-verification.json>)
retains source receipt hashes, function checks, exact setting identity, producer
offsets, binding arguments, and limitations. Private binaries and disassembly
remain outside Git.
