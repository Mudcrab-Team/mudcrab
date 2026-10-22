# Next tone-map experiment: native CPU coefficient evidence

Follow-up: [the parameter-upload trace](gpu_binding_trace.md) now closes the
static CPU-to-GPU packing gap described below. [The output trace](output_transfer_trace.md)
also proves `cb12[42].x = 1 / fGamma` on the audited route. This note preserves
the earlier investigation; see [runtime observations](runtime_state.md) for the
later process readbacks and their limits.

Research date: 2026-10-15. This is a focused read-only follow-up to
[the native audit](native_rea.md). It reuses saved Ghidra instruction receipts,
reads the pinned executable, and independently checks the relevant function with
LLVM. It does not observe Skyrim execution, change Fiji, launch a game, build the
engine, or establish selected-frame parity.

## White is transformed before the native parameter write

The saved receipt
[`expansion-07b-hdr-cinematic-parameters.json`](</Users/taylor/.local/share/mudcrab-research/render-map-20261010/native-expansion/native/expansion-07b-hdr-cinematic-parameters.json>)
contains `HDRParameters`, function `0x14153D860`–`0x14153DC2E`. Contrary to the
earlier report's broad unresolved wording, this function does supply a concrete
**resolved-field to CPU-parameter transformation**. The final CPU-parameter to
retail-shader constant-buffer bridge remains open.

At `0x14153D8CA`, RAX receives manager singleton `0x1433D40A0`. The function
subsequently reads `manager + 0xDC`. The pinned public layout places resolved
`ImageSpaceData` at manager `+0xC8`, and HNAM White at base `+0x14`, consistent
with this read. The prior native print-function evidence independently
corroborates the base-data field layout. This is the manager's resolved base
field, not a fresh read of one selected IMGS record.

Define `W = *(float*)(manager + 0xDC)`. The instructions establish:

```text
reinhardWhite = W * *(float*)0x1420D7DE0
filmicWhite   = W * *(float*)0x1420D7DF8
filmic        = *(byte*)0x1420D7DC8

if filmic == 0:
    k_cpu = 1 / (reinhardWhite * reinhardWhite)
else:
    x = max(filmicWhite - 0.004, 0)
    h = (x * (6.2*x + 0.5)) / (x * (6.2*x + 1.7) + 0.06)
    k_cpu = h^(-2.2)
```

This is a conceptual formula, not a claim of bitwise equivalence after
reassociation. The relevant sites are:

| Sites | Evidence |
| --- | --- |
| `0x14153D94E`–`0x14153D99F` | Read the two scale globals and resolved White; multiply each scale by White. |
| `0x14153D9FE` | Test the filmic byte. |
| `0x14153DA10`–`0x14153DA57` | Filmic branch: subtract `0.004`, lower clamp, rational function, call power helper with exponent `-2.2`. |
| `0x14153DA61`–`0x14153DA69` | Other branch: square the scaled White and divide `1` by it. |
| `0x14153DA6D`–`0x14153DA99` | Write CPU parameter float4 index 1: `(receiveBloomThreshold, k_cpu, float(filmic), 0)`. |

The power call target `0x1415A98CA` is an indirect jump through IAT slot
`0x1417C8E88`. The PE import table identifies that slot as
`api-ms-win-crt-math-l1-1-0.dll!powf`; this corroborates the exponent operation
instead of inferring it from the nearby literal alone.

The setting objects contain pointers to the following exact names in the pinned
PE. These are static setting identities and initial values, not measured
runtime INI values:

| Value address | Name-pointer address | Referenced string | Initial value |
| --- | --- | --- | --- |
| `0x1420D7DC8` | `0x1420D7DD0` | `bUseFilmicCurve:Display` | `0` |
| `0x1420D7DE0` | `0x1420D7DE8` | `fReinhardWhiteScale:Display` | binary32 `1.100000023841858` |
| `0x1420D7DF8` | `0x1420D7E00` | `fFilmicWhiteScale:Display` | binary32 `1.399999976158142` |

For orientation, using those static scale initializers gives mathematical
Reinhard coefficients approximately `0.8264462452` for White `1` and
`0.7496111748` for binary32 CloudyDAY White `1.0499999523162842`. These are
calculated examples, not runtime values or a prescribed screenshot setting.

## What the parameter writer proves

The saved receipt
[`expansion-04-shader-bindings.json`](</Users/taylor/.local/share/mudcrab-research/render-map-20261010/native-expansion/native/expansion-04-shader-bindings.json>)
contains function `0x14152B170`–`0x14152B1A5`. It reads the float-array pointer
from parameter object `+0x40`, then stores four floats at
`array + 16*index`, where index is EDX. Under the Microsoft x64 calling
convention its four components come from XMM2, XMM3, and the next two stack
arguments, respectively. Fresh LLVM disassembly of the original PE corroborates
that setter.

Consequently, the writes in `HDRParameters` identify CPU float4 indices. They do
not alone prove that CPU index 1 becomes group-26 `cb2[2]`, or that CPU index 2
becomes group-32 `cb2[2]`. Reflection, parameter packing, the caller's pointer
adjustment, and the actual GPU buffer upload must close that bridge. A similar
shape or register number cannot close it.

## Cinematic and tint CPU writes

The same function reads resolved cinematic and tint fields and writes:

```text
CPU index 2 = (saturation + globalSaturationBoost,
               0,
               contrast + selectedContrastBoost,
               brightness + selectedBrightnessBoost)
CPU index 3 = (tint.red, tint.green, tint.blue, tint.amount)
```

The read/write sites are `0x14153D8D8`–`0x14153D979`,
`0x14153DA9E`–`0x14153DABB`, and `0x14153DAC0`–`0x14153DAE6`.
Byte `0x14343635E` chooses one of two global brightness/contrast boosts.
The setting-name pointers identify `fGlobalSaturationBoost:Display`,
`fGlobalContrastBoost:Display`, `fGlobalMapContrastBoost:Display`, and
`fGlobalMapBrightnessBoost:Display`. The byte's full runtime meaning is not
established here. Do not assume the direct CNAM values always reach the shader
unchanged.

## Adaptation CPU coefficients

The later part of `HDRParameters` selects a different parameter object and
writes its CPU index 2. With conceptual names:

```text
s = globalEyeAdaptSpeedScale * resolvedEyeAdaptSpeed
q = globalEyeAdaptStrengthScale * resolvedEyeAdaptStrength
T = 30 * selectedTimer

coefficient_z = 1 - max(1 - 0.01*s,   0)^T
coefficient_w = 1 - max(1 - 0.01*s/q, 0)^T
CPU index 2  = (0, 0, coefficient_z, coefficient_w)
```

The sites are `0x14153DAEB`–`0x14153DBE3`. The ordinary timer is read at
`0x1420D68D4`. If pointer `0x14219E5C0` is non-null and its dword `+0x160`
is positive, the function instead reads `fConstHDRAdaptTimerForMenu:Display`.
The exact class/field meaning and ordinary timer units/source remain unclosed.
Do not label T as elapsed seconds from this evidence alone.

Static setting identities and initial values:

| Value address | Referenced name | Initial binary32 value |
| --- | --- | --- |
| `0x1420D7D80` | `fGlobalEyeAdaptSpeedScale:Display` | `2` |
| `0x1420D7D98` | `fGlobalEyeAdaptStrengthScale:Display` | `0.4000000059604645` |
| `0x1420D7D68` | `fConstHDRAdaptTimerForMenu:Display` | `0.30000001192092896` |

The retail adaptation equation is independently known from groups 32/34.
Joining these CPU coefficients to its exact shader lanes still requires the
parameter-upload bridge. Neither source proves which route, coefficient tuple,
history texture, or time interval generated the reference screenshot.

## Useful next implementation

A separate experimental pipeline can now use the audited scalar retail curves
and **record-derived White with an explicitly selected setting scale**. Store
scale/branch assumptions as experiment data with provenance, rather than
silently making them universal Skyrim constants. The static initializer choice
supports a bounded first experiment; it does not identify the reference frame.

Keep adaptation inputs, output exponent, and bloom choice explicit in each
capture receipt. A first curve comparison can hold sampled-average values
fixed, defer temporal adaptation and bloom, and state those omissions. Both
curve alternatives should remain selectable until the reference frame's branch
is measured. The approved TonyMcMapface/sRGB preset remains the default.

Remaining gates: effective runtime setting values; resolved manager inputs and
modifiers; exact CPU-to-GPU packing; selected shader branch; adaptation texture
contents/history; scene/SRV numeric domain; output exponent and later transfer.
The equations permit an honest experiment while those gates remain open.

## Verification performed

- Original PE SHA-256 freshly matched
  `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
- All 186 saved `HDRParameters` instruction encodings matched original PE
  bytes using their retained file mappings.
- Independent Apple LLVM disassembly of
  `0x14153D860`–`0x14153DC2E` returned the same 186 instruction starts and
  encodings, with zero mismatches.
- Scale, selector, formula literals, setting-name pointers, and strings above
  were read directly from the original PE file mapping.
- No new Ghidra import or analysis was needed. The existing evidence used the
  saved read-only project; this follow-up adds interpretation and a fresh byte
  crosscheck, not a claimed new decompiler result.
