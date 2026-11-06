# Native image-space objects and dispatch

This extension connects all 124 image-space package groups to distinct native
effect-table slots, then follows eight helper objects and 26 stored shader
dependencies. The [connection catalog](native-image-space-connections.json)
retains original instruction receipts, constructor arguments, pointer adjustments,
guards and ten curated direct transfers. The existing
[frame topology](frame-topology.md) remains the broader conditional call map.

These are static connections in the pinned executable. Successful object and
shader construction, the recorded package supplying the opened resource and the
caller reaching each branch are prerequisites. No active retail effect, dispatch,
resource generation or numeric input is observed.

## Effect table and object ownership

The factory at `0x1414EEE10` registers image-space shader subobjects in the
manager's pointer array at `+0x28`. Direct registration at `0x1414EDAA0` uses that
manager; registration at `0x14150B880` receives the manager adjusted by `+0x20`
and stores through its array at `+0x8`. Both address the same table. Constructed
shader primary pointers are adjusted by `+0x90` for registration. Factory setup
recovers each primary pointer with `-0x90`, preserving null, then passes it to
the helper constructor for storage.

All slots below come from factory operands and constructor field stores. They
are not assigned by subtracting a constant from a package ordinal. Three
registration cases retain additional pointer/index proofs. The group 23 index
flow includes RDI preservation under the selected Microsoft x64 ABI.
[Microsoft calling convention](https://learn.microsoft.com/en-us/cpp/build/x64-calling-convention?view=msvc-170).

| Manager field | Helper constructor | Stored dependencies, in field order |
| --- | --- | --- |
| `+0x168` | `0x14153DD30` | slot 102: `ApplyReflections` |
| `+0x170` | `0x14153DF40` | slot 103: `ISApplyVolumetricLighting` |
| `+0x178` | `0x14153E420` | slot 104: `ISBasicCopy` |
| `+0x180` | `0x14153E4A0` | slots 105, 119, 120, 121, 122, 124: blur, downsample, ignore-brightest downsample, their compute variants, image-based lens flares |
| `+0x188` | `0x141540300` | slots 106–113: H/V compute blur for volumetrics, reflections, parallax masks and depth of field |
| `+0x190` | `0x141540AC0` | slots 114–116: volumetric composition and lens-flare shaders |
| `+0x1D0` | `0x1415434C0` | slots 134, 135, 137, 145: SAO H/V compute blur, camera-Z/mips compute, raw AO compute |
| `+0x1D8` | `0x141543D00` | slots 146, 147: `ISSILComposite`, `ISSILRawInd` |

Shader ownership narrows the helper's identity; it does not establish that all
its dependencies execute. In particular, the history buffers in `+0x180` do not
identify it as TAA. The native TAA groups have separate registrations. SIL and
SAO names also do not establish enabled indirect lighting or occlusion.

The `+0x170` renderer returns its input when object byte `+0x8` is zero. The
`+0x180` renderer does the same for byte `+0x30`, initialized from the writable
global `0x1420D7E50`. The `+0x1D8` renderer tests byte `+0x10`, initialized from
`0x1420D8AD8`. Their effective values are unobserved. The constructor flags
at `+0x188/+0x40` and `+0x190/+0x18` are recorded as writes; this map does not
assign them an early-return role.

## A conditional wrapper and its target flow

The wrapper at `0x1414EDFD0` calls the `+0x1D8` renderer, saves its returned target
in EBX, calls the `+0x180` renderer, then calls the `+0x190` renderer. Each nested
helper retains its own conditions. The third call receives the saved target from
the **first** helper, not the second helper's returned EAX. A linear chain of
output-to-input connections would misrepresent this code.

If the final result differs from the requested output, the wrapper routes effect
slot 34, package group 10 `ISCopy`, to that output. The catalog pins these four
call sites and the intervening register flow. This is one wrapper's behavior,
not a universal postprocess order.

## Compute selection, binding and dispatch

The image-space shader primary virtual at `+0x60` resolves to `0x141565170`.
That wrapper calls primary virtual `+0x50`, reads the created compute owner at
shader `+0x198`, then calls owner virtual `+0x10` with key zero and the supplied
group counts. The original `BSComputeShader` vtable at `0x141B4FBF8` resolves that
method to `0x141589CF0`.

On a matching key, `0x141589CF0` tails into `0x14100F4E0`. A missing entry emits
a diagnostic and returns. The successful helper commits cached state, uses the
device context at `0x143331F30`, then performs these original virtual operations:

| Original site | SDK operation | Arguments retained |
| --- | --- | --- |
| `0x14100F513`, vtable `+0x228` | `CSSetShader` | selected entry's native handle at `+0x60`; no class instances |
| `0x14100F530`, vtable `+0x238` | `CSSetConstantBuffers` | slot 0, count 1, buffer pointer at `0x143331F00` |
| `0x14100F559`, vtable `+0x148` | `Dispatch` | original x/y register arguments and z stack argument; tail virtual jump |

The method slots come from the pinned [D3D11 ABI](d3d11-abi.json), with original
receiver/argument flow attached here. The `+0x1D0` camera-Z/mips call supplies
`ceil((width >> 1)/8)`, `ceil((height >> 1)/8)`, `1`; this dimension formula belongs
to that call. Committed views, samplers, constants, resource generations and live
dimensions remain open.

## HDR parameter connection

The original RTTI vtable write establishes `ImageSpaceEffectHDR` construction at
`0x14153C9B0`, setup at `0x14153CAB0`, and its enable callback at `0x14153DCF0`.
The callback returns true if reached; the separate router enablement still
applies.

Setup selects effect slot 58, group 29 `ISHDRDownSample4`, recovers the
shader primary pointer and creates a parameter object. It assigns parameter
channel 2 a 64-byte block at `0x1420D7410`, then links that shader and parameter
to HDR child index 4. The linking function at `0x141554150` stores the shader
subobject through HDR array `+0x18` and the parameter through array `+0x30`.

This closes a static ownership/parameter link. It does **not** identify the block's
components as contrast, exposure, gamma or saturation, or prove that parameter
channel 2 becomes the selected shader's `cb2`. That requires the complete transfer
and its writers. The separately examined cinematic primary vtable's slots 10 and
11 are RET callbacks;
they supply no constant upload. A nearby buffer-copy helper cannot acquire the
cinematic role from its address or a speculative query label.

The [shipped equations](image-space-arithmetic.md) retain exact shader-side buffer
operands. Their linkage to these native parameters, active IMGS/IMAD data, view
ownership and intermediate numeric domains remains an acceptance question.

## Verification and limits

Seven successful bounded read-only queries add 67 requested spans, 9,618 unique
original instruction starts and 57 independently decoded functions. The LLVM
comparison checks 5,563 instructions; all byte/instruction comparisons pass.
These counts overlap prior evidence and must not be added to the central union.
One invalid-seed invocation produced no evidence; its log remains private and
does not contribute to the successful-query count.

The catalog also retains rejected seed roles: an alleged HDR constructor was a
size/reference helper, two alleged cinematic sites lacked the type-to-callback
connection, and a supposed compute setup seed lay inside an existing renderer.
Keeping those exclusions prevents query labels from becoming native facts.

Full helper closure, sibling HDR parameter transfers, mutable-global writers,
actual branch selection and matched retail intermediates remain open. This
extension changes the research map; it supplies no renderer tuning values.
