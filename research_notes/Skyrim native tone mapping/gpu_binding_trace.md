# Native HDR parameters to retail GPU constants

Research date: 2026-10-15. This closes the static parameter-packing gap identified
in [the CPU coefficient note](next_step.md). The exact pinned executable and
shader package connect the tone-map CPU tuple to group 26/27 `cb2[2]`, cinematic
grading to `cb2[3]`, and tint to `cb2[4]`. The same generic uploader connects the
adaptation CPU tuple to group 34 `cb2[2]`.

This is a conditional static connection, not an observation of a running frame.
It assumes successful construction, the pinned package supplying the opened
resource, successful API calls, and execution reaching this route. Effective
settings, runtime values, active draws, textures, and screenshot provenance remain
unobserved.

## Concrete mapping

The uploader treats each remap byte as a **float offset**, multiplying it by
four to address the mapped GPU buffer. It copies one CPU float4 per iteration.
The remap is therefore necessary: CPU float4 index 1 does not become GPU float4
index 1 in the cinematic shaders.

| Program | CPU float4 index | Serialized remap byte | GPU byte offset | Shader operand | Meaning from the CPU writer |
| --- | --- | --- | --- | --- | --- |
| Group 26/27 | 0 | 0 | 0 | `cb2[0]` | Flags tuple; zeroed in the examined HDR route |
| Group 26/27 | 1 | 8 | 32 | `cb2[2]` | `(receiveBloomThreshold, whiteCoefficient, float(filmic), 0)` |
| Group 26/27 | 2 | 12 | 48 | `cb2[3]` | `(saturation + boost, 0, contrast + boost, brightness + boost)` |
| Group 26/27 | 3 | 16 | 64 | `cb2[4]` | `(tint.red, tint.green, tint.blue, tint.amount)` |
| Group 27 | 4 | 20 | 80 | `cb2[5]` | Fade tuple, conditionally written when resolved manager `+0x118` is positive |
| Group 34 | 2 | 8 | 32 | `cb2[2]` | `(0, 0, adaptationCoefficientZ, adaptationCoefficientW)` |

Group 32 has the same first three remap bytes `(0, 4, 8)`. That establishes its
index-2 packing if supplied the tuple. This investigation does not prove that
the examined HDR writer's child-8 object is passed to group 32: setup links that
object to group 34, not group 32.

## From the CPU writer to the selected parameter object

`HDRParameters` at `0x14153D860` reads the HDR object's parameter array through
`HDR +0x30`. At `0x14153D9A5` or `0x14153D9DC`, it selects entry 3 or 2,
respectively. Those are the fade and ordinary cinematic parameter objects. It
writes float4 indices 1, 2, and 3 through setter `0x14152B170`. The setter reads
the array pointer at parameter `+0x40` and writes at `array + 16*index`.

Setup `0x14153CAB0` links manager effect slot 55 to HDR child 2 and slot 56 to
child 3. It preserves the primary-to-image-space pointer adjustment `+0x90` and
stores the corresponding parameter object through HDR array `+0x30`. The
existing native registration/package audit identifies slot 55 as group 26 and
slot 56 as group 27; these are recovered factory indices, not values inferred
from package ordinal arithmetic.

HDR rendering `0x14153D120` repeats the same `manager +0x118 > 0` selection at
`0x14153D70C`–`0x14153D734`. It retrieves the shader and parameter from the
matching child 2/3 arrays, places the parameter in R8 at `0x14153D7B4`, and calls
image-space virtual `+0x08` at `0x14153D7BD`.

The group-26 constructor `0x1414FB7F0` installs primary vtable `0x141B36428`
and image-space-subobject vtable `0x141B364D0`. Original PE data resolves the
subobject's `+0x08` method to `0x141564E30`. That wrapper stores its R8 argument
in global current-parameter pointer `0x143698A80` at `0x141564E60`, then restores
the primary pointer by subtracting `0x90` at `0x141564F83`. It clears the current
pointer after rendering at `0x141565060`.

The route through `0x141555820`, `0x141560340`, and `0x141562270` retains the
primary shader receiver. The last helper dispatches primary virtual `+0x30`.
For group 26/27, the original primary vtables resolve that method to
`0x141564A80`, the generic geometry/constant setup described below.

Private historical receipts outside this repository: binding queries 02–05 under
`MudcrabResearch/tonemap-binding-20261015/`. Prior registration evidence was
recorded in `native-image-space-connections.json` in a separate historical
worktree; that file is not present in this checkout.

## Serialized metadata supplies the missing offsets

Pixel-record creator `0x14100FFE0` reads 64 serialized metadata bytes directly
into pixel object `+0x40` at `0x1410100A5`–`0x1410100B2`. It then reads four
selector bytes. The third selector chooses the buffer pointer stored at pixel
object `+0x30`; sites `0x14101016A`–`0x141010184` establish that correspondence.
The selected package's relevant records contain:

| Pixel record | Record start in `shaders011.fxp` | First relevant remap bytes | Four selector bytes |
| --- | --- | --- | --- |
| Group 26, key 0 | 67,512,856 | `0, 8, 12, 16` | `0, 0, 23, 52` |
| Group 27, key 0 | 67,515,216 | `0, 8, 12, 16, 20` | `0, 0, 23, 52` |
| Group 32, key 0 | 67,523,000 | `0, 4, 8, 24, 28, 32, 36, 40` | `0, 0, 23, 52` |
| Group 34, key 0 | 67,526,164 | `0, 4, 8, 24, 28, 32, 36, 40, ...` | `0, 0, 23, 52` |

Remap metadata begins 12 bytes after each listed record start. The loader puts
it into the exact byte table indexed by the uploader. The native selector
helper `0x141010290` stores the selected pixel object at `0x1420CFF50`; generic
setup subsequently reads that same global. Package record identity is already
linked to native shader construction in the existing loader audit.

Private historical verification outside this repository preserves metadata
hashes, full first-20 remap values, selector bytes, original primary-vtable
pointers, and bytecode hashes. It is stored as
`MudcrabResearch/tonemap-binding-20261015/binding-verification.json`.

## Upload and slot binding

Generic setup `0x141564A80` reads the selected pixel object into R13 at
`0x141564B46`. It maps the buffer at pixel `+0x30` and retains its mapped
destination pointer at `+0x38`. The float4 copy loop at
`0x141564C69`–`0x141564CC4` reads:

```text
parameter = *(pointer*)0x143698A80
cpu = *(float4**)(parameter + 0x40)
pixel = *(pointer*)0x1420CFF50

for i in 0 .. shader.pixelParameterCount:
    floatOffset = *(byte*)(pixel + 0x40 + i)
    copy16(mappedPixelBuffer + 4*floatOffset, cpu + i)
```

The exact loop advances its source index by four floats and multiplies the
remapped destination offset by four bytes. It neither changes component order
nor inserts a numeric transform. It unmaps that same buffer through context
virtual `+0x78` at `0x141564CFB`, then calls context virtual `+0x80` at
`0x141564D2D` with `(StartSlot=2, NumBuffers=1, ppBuffers=&pixel.buffer30)`.
The pinned D3D11 ABI identifies `+0x80` as `PSSetConstantBuffers`. That closes
the connection to shader constant-buffer slot 2, rather than guessing that an
object's channel number equals a GPU slot.

API mapping source: `d3d11-abi.json` in a separate historical worktree; it is
not present in this checkout. Shader consumers are described by the audited
retail arithmetic note in that worktree, also not present here.

## Adaptation producer scope

The writer selects parameter child 8 at `0x14153DB12`–`0x14153DB16` and writes
its index 2 at `0x14153DBE3`. Setup links child 8 to manager effect slot 64,
group 34 `ISHDRDownSample16LightAdapt`, at `0x14153CE71`–`0x14153CED8`.
HDR rendering retrieves that same child shader/parameter at
`0x14153D539`–`0x14153D545` and passes the parameter in R8 to its image-space
virtual `+0x08` at `0x14153D5EE`.

Group 34's original primary vtable uses the same `0x141564A80` geometry setup.
Its remap byte 2 is 8, so the tuple reaches `cb2[2]` on this route. Combining
that connection with the independently audited shader operands establishes:

```text
shader rate.x uses cb2[2].w = CPU adaptationCoefficientW
shader rate.y uses cb2[2].z = CPU adaptationCoefficientZ
```

This settles packing, not timer units, selected timing branch, temporal texture
state, NaN fallback frequency, or which scene pixels feed the accumulated values.

## Bounded runtime readback locations

The static chain supplies useful locations for a future read-only process-memory
observation. Relocate every preferred executable VA below by the verified module
base delta; object-relative offsets are unchanged.

```text
manager = read_u64(relocate(0x1433D40A0))
effects = read_u64(manager + 0x28)
hdr = read_u64(effects + 30*8)
require read_u64(hdr) == relocate(0x141B443C8)

parameters = read_u64(hdr + 0x30)
ordinary = read_u64(parameters + 2*8)
fade = read_u64(parameters + 3*8)
adaptation = read_u64(parameters + 8*8)

for (name, parameter, minimumCount) in [
    ("ordinary", ordinary, 4),
    ("fade", fade, 4),
    ("adaptation", adaptation, 3),
]:
    require parameter != 0
    require read_u64(parameter) == relocate(0x141B40A38)
    float4Count = read_u32(parameter + 0x48)
    require float4Count >= minimumCount
    float4Array = read_u64(parameter + 0x40)
    require float4Array != 0
    retain name, float4Count, float4Array
```

The factory's HDR-constructor call at `0x1414EF07A` and slot-30 store at
`0x1414EF0D4` establish the first chain. Constructor `0x14152AE10` installs
the parameter vtable at `0x14152AE23`. Allocator `0x14150B630` stores its R8D
float4 count at parameter `+0x48` and its allocated pointer at `+0x40`; the count
does not denote scalar floats. Ordinary/fade reads of indices 1–3 require count
at least 4; adaptation index 2 requires count at least 3. Read only those bounded
tuples after validating all mappings and pointers.

Ordinary/fade coefficient, grade, and tint tuples are at array byte offsets
`+0x10`, `+0x20`, and `+0x30`. Adaptation coefficients are at adaptation-array
`+0x20`. Resolved manager `+0x118 > 0` selects fade rather than ordinary on the
examined route. Global current-parameter pointer `0x143698A80` is transient and
is cleared after rendering, so the stable HDR-owned arrays are more useful for
an unsynchronized observation.

Process-memory reads are not atomic across a frame. Repeated consistent values
can establish observed process state over a read interval; they cannot alone
establish exact GPU inputs for a particular screenshot or draw. This note
contains static locations only; it performs no runtime reads itself.

## Verification and limits

- Reused the existing Ghidra `SkyrimMovement17104` project with
  `-process SkyrimSE.exe -noanalysis -readOnly`; no cold import or target execution.
- Four successful queries completed in approximately 5.39, 4.00, 3.78, and 3.96
  seconds. An initial launcher attempt failed because `JAVA_HOME` was absent;
  its retained manifest records exit 1 and no evidence. Retrying with the pinned
  task toolchain succeeded.
- Original PE SHA-256 freshly matched
  `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
  Original package SHA-256 freshly matched
  `72cfc73eb0938d5949f0b3a8683722a33b671a34b41b750ba138de8d154c5fbc`.
- All 2,522 retained instructions across 18 functions and one additional factory
  span matched original PE bytes
  and independently calculated file offsets. Independent Apple LLVM decoding
  matched their starts and encodings after joining two standalone `LOCK`
  prefixes to their following instructions. The two joins are explicitly
  recorded; there were no remaining mismatches.
- Four serialized pixel metadata blocks and bytecode slices matched the
  existing inventory hashes. Group 26/27/32/34 geometry-callback pointers were
  read directly from the original PE.
- No engine edits, default changes, builds, captures, game launches, Fiji
  changes, debugger attachment, or GitHub publication occurred in this research.

The resolved White transformations documented in [next_step.md](next_step.md)
can now be connected statically to the exact shader coefficient and branch
lanes. Effective runtime setting values and the reference screenshot's selected
branch remain separate gates. This result does not justify replacing those
unknowns with static initializers or promoting tuned overrides to native values.
