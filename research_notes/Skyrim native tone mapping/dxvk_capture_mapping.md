# Mapping a DXVK capture to Skyrim's retail shaders

Research date: 2026-10-15. This note describes the source contract used to
identify shaders and recover their inputs from a Vulkan capture, followed by
a bounded readback of the captured camera matrices. Tone-map draw identity and
sampled texture values belong in the capture campaign's receipts and follow-up
analysis.

## Translator and capture provenance

The comparison uses GE-Proton11-5. Its upstream tag pins DXVK source revision
`3a4c6fa3cb1548d56a90a38dd8f526b6c13e63fd` and that revision pins dxbc-spirv
`c5c1a5b9b61773d59592f628faa39be9add26282`. The runtime log reports DXVK 3.0.2
with additional commits. Preserve the exact log version and loaded DLL hashes
separately; a distribution tag alone does not establish binary/source equality.

Capture uses stock Nixpkgs RenderDoc 1.45. A separate headless build from the
same RenderDoc version enables its Python replay module without Qt or PySide.
The Python module and its linked `librenderdoc.so` must come from the same build.
This captures DXVK's translated Vulkan commands and SPIR-V, rather than the
native D3D11 command stream or original DXBC bytes.

Campaign controls are recorded under
`data/visual-comparisons/tonemap-gpu-20261015T230233Z`: later native console
readback of `fShadowDistance:Display` is `8000`, game hour is `12`, weather `81a`
is current with transition fraction `1`, and the camera export is verified.
The earlier campaign's Documents INI on disk recorded `fShadowDistance=23200`
([earlier runtime state](runtime_state.md#capture-linkage-and-remaining-limits)).
Neither observation establishes the shadow distance consumed by the captured
frame, and the later console query does not prove an override mechanism. The
native controlling input for the capture and the historical reference remains
unresolved. These checks constrain the session; they do not identify a shader,
establish cross-renderer projection parity, or attribute settings to earlier
screenshots.

Sources: [GE-Proton submodule](https://github.com/GloriousEggroll/proton-ge-custom/tree/GE-Proton11-5/dxvk),
[DXVK compiler submodule](https://github.com/doitsujin/dxvk/tree/3a4c6fa3cb1548d56a90a38dd8f526b6c13e63fd/subprojects/dxbc-spirv),
[RenderDoc 1.45 Python module build](https://github.com/baldurk/renderdoc/blob/v1.45/qrenderdoc/Code/pyrenderdoc/CMakeLists.txt).

## Shader identity from the original DXBC header

This DXVK revision uses the DXBC container's stored 16-byte header digest when
creating an ordinary pixel-shader key. `DxvkShaderHash::toString()` emits `fs.`
followed by those bytes in lowercase hexadecimal. It does not compute SHA1 of
the full bytecode. The compiler carries this name into SPIR-V `OpString` and
`OpSource` metadata.

The following expected names were computed from the private retail package
after checking its SHA256 and each slice's SHA256 against the existing audit.
The binaries remain outside the repository.

| Retail pixel shader | Length | Expected DXVK source name | Full DXBC SHA256 |
| --- | ---: | --- | --- |
| Group 26, `ISHDRTonemapBlendCinematic`, key 0 | 1876 | `fs.716590ec34046095c58617804cdff8cc` | `1def29cca7e582b149a09bd15524793593293ae2d94fc20b3d030bf9f91e39fc` |
| Group 27, `ISHDRTonemapBlendCinematicFade`, key 0 | 1932 | `fs.fdfcf2b11d504e811c554edbf4363aad` | `781978201a0b01ba4107b11abbe52456b5f20b49b998f1d0ba7aa9d057133ea1` |
| Group 32, `ISHDRDownSample4LightAdapt`, key 0 | 1348 | `fs.07875551bed7d3d60be52b1574c49330` | `f290f4ff09e163bb5f574824d4407b81d44a1fea47855f5325522e7b505f8826` |
| Group 34, `ISHDRDownSample16LightAdapt`, key 0 | 1348 | `fs.e60b2b1475688431053f488dd6875c44` | `675f8c78eab2aa9a0877d6a55185d82911a66b7fc91569dc1331e8916ee2adda` |

A matching source name provides header-digest correlation to the audited
shader. It does not extract or independently hash the original runtime DXBC.
A future launch with `DXVK_SHADER_DUMP_PATH` can supply that stronger check.
Keep the captured SPIR-V bytes, their SHA256, source metadata, and event ID.

Sources: [key selection](https://github.com/doitsujin/dxvk/blob/3a4c6fa3cb1548d56a90a38dd8f526b6c13e63fd/src/d3d11/d3d11_device.cpp#L2256),
[key formatting](https://github.com/doitsujin/dxvk/blob/3a4c6fa3cb1548d56a90a38dd8f526b6c13e63fd/src/dxvk/dxvk_shader_key.cpp#L40),
[stored container digest](https://github.com/doitsujin/dxbc-spirv/blob/c5c1a5b9b61773d59592f628faa39be9add26282/dxbc/dxbc_container.h#L93),
[name passed to the compiler](https://github.com/doitsujin/dxvk/blob/3a4c6fa3cb1548d56a90a38dd8f526b6c13e63fd/src/d3d11/d3d11_shader.cpp#L30),
[SPIR-V source metadata](https://github.com/doitsujin/dxbc-spirv/blob/c5c1a5b9b61773d59592f628faa39be9add26282/spirv/spirv_builder.cpp#L665).

## Recovering original registers from Vulkan bindings

For the audited Shader Model 5.0 shaders, dxbc-spirv names resources using their
original registers: `t0`, `t1`, `t2`, `cb2`, `cb12`, and `sN`. Join each captured
resource's name to its final SPIR-V `DescriptorSet` and `Binding` decorations,
then read the corresponding descriptor at the selected event. DXVK remaps
bindings using the pipeline layout, so a Vulkan binding number alone does not
identify an original D3D11 register.

For an identified group-26/27 shader, the existing retail arithmetic assigns
`t0` to bloom-like RGB, `t1` to scene RGB, and `t2.xy` to the adaptation pair
`P`. It reads the tone-map and grading tuples from `cb2[2..4]` and the final
exponent from `cb12[42].x`. Those meanings come from the audited DXBC operand
chain; captured descriptors establish which resources supply them in a frame.
Preserve image and view formats, subresources, sampler state, buffer ranges and
dynamic offsets alongside readback bytes.

Two translation paths require additional handling:

- **Constant-buffer addresses:** DXVK can replace an in-bounds constant buffer
  with a 64-bit buffer address in push constants, preserving its `cbN` debug
  name. If no descriptor exists, use the captured push-member offset and type,
  resolve its address against RenderDoc buffer `gpuAddress`/`length` ranges,
  and validate the requested range before reading that buffer. Do not assume
  that every `cbN` remains a uniform-buffer descriptor.
- **Sampler heap:** DXVK can replace individual samplers with `sampler_heap`
  and push sampler indices. Original `sN` names are carried into push-data
  member names. Follow the captured layout and any 16-bit packing to select
  the heap element used by the shader; heap order alone does not identify `sN`.

Sources: [resource names](https://github.com/doitsujin/dxbc-spirv/blob/c5c1a5b9b61773d59592f628faa39be9add26282/dxbc/dxbc_resources.cpp#L621),
[SM5 register spelling](https://github.com/doitsujin/dxbc-spirv/blob/c5c1a5b9b61773d59592f628faa39be9add26282/dxbc/dxbc_converter.cpp#L3789),
[pipeline descriptor remapping](https://github.com/doitsujin/dxvk/blob/3a4c6fa3cb1548d56a90a38dd8f526b6c13e63fd/src/dxvk/dxvk_shader_ir.cpp#L114),
[constant-buffer address lowering](https://github.com/doitsujin/dxvk/blob/3a4c6fa3cb1548d56a90a38dd8f526b6c13e63fd/src/dxvk/dxvk_shader_ir.cpp#L869),
[sampler indices](https://github.com/doitsujin/dxvk/blob/3a4c6fa3cb1548d56a90a38dd8f526b6c13e63fd/src/dxvk/dxvk_shader_ir.cpp#L778),
[RenderDoc buffer addresses](https://github.com/baldurk/renderdoc/blob/v1.45/renderdoc/api/replay/data_types.h#L736).

## Evidence boundary

Source inspection establishes the mapping procedure. A frame claim additionally
requires the captured shader's raw source metadata, final descriptor mapping,
resource bytes, and event ordering. A matching list of resource names is only a
candidate until shader identity is checked. Replaying the captured post-process
inputs can then test arithmetic separately from lighting, materials and fog;
it does not by itself establish Skyrim scene parity in Mudcrab.

## Captured camera projection: event 11359

The `event receipt` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/events-01/receipt.json`)
exports the pixel-stage `cb12` descriptor from `ResourceId::464`, backing-buffer
offset `2121728`, length `720`. The `raw buffer slice` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/events-01/event-11359-Pixel-constant_blocks-2.bin`)
has SHA256 `7b3c22ea6f541a2dffe1eb75b45ab224b8d1c54bc05a9629abf9a46fc7d69cf6`.
Independent little-endian binary32 decoding gives:

| Component | Value | Byte offset in exported slice | Byte offset in backing buffer |
| --- | ---: | ---: | ---: |
| `cb12[4].x` | `1.191753625869751` | 64 | 2121792 |
| `cb12[5].y` | `2.118673324584961` | 84 | 2121812 |
| `cb12[24].x` | `1.191753625869751` | 384 | 2122112 |
| `cb12[25].y` | `2.118673324584961` | 404 | 2122132 |
| `cb12[40].xyz` | `(21750, -44530, 140)` | 640 | 2122368 |
| `cb12[41].xyz` | `(21750, -44530, 140)` | 656 | 2122384 |

Rows 4–7 form a centered perspective projection `P` with diagonal scales
`x=1.191753625869751`, `y=2.118673324584961`, and clip `w=z`. Rows 24–27 repeat
that matrix exactly. Multiplying `P` by rows 0–3 reproduces rows 8–11 within
`8.1e-8` maximum absolute error; multiplying it by rows 20–23 reproduces the
identity matrix within `9.2e-8`. These internal checks support the projection,
view and inverse-projection interpretation independently of a Photo Mode label.

For this centered projection, `horizontalFov=2*atan(1/x)` and
`verticalFov=2*atan(1/y)`. The inferred aspect `y/x=1.7777779556061655` agrees
with the event's `1920×1080` output to binary32 precision. The Vulkan viewport
has negative height for its coordinate flip; its magnitude is `1080`.

| Camera source | Horizontal FOV, degrees | Vertical FOV, degrees |
| --- | ---: | ---: |
| Captured native projection | `79.99999842452057` | `50.534010187183334` |
| Frozen Mudcrab camera | `72.73435310118231` | `45.00000125223908` |
| Native minus frozen Mudcrab | `7.265645323338262` | `5.534008934944254` |

The frozen `Mudcrab camera file` (private historical evidence outside Git: `data/visual-comparisons/brightness-plus20-20261014T162641Z/camera.json`)
uses the same output dimensions and position, but a narrower FOV. Its implied
projection scales are approximately `1.139493` times the native scales. This
closes the horizontal/vertical convention for the native projection stored in
this captured camera block: its approximately `80` degrees is horizontal.

It does not establish the FOV of the historical Skyrim reference, verify the
projection actually used by every scene-geometry draw, or prove full camera
parity. The observed post-process binds this camera block but need not read its
matrix rows; scene-vertex bindings remain a separate check. Position equality
also does not settle rotation conventions, clipping, viewport cropping, or
other passes' camera state. These measurements do not change the frozen images
or infer a color-grading correction from the framing difference.
