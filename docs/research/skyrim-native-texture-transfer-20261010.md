# Skyrim native texture sampling and display boundary

Skyrim SE `1.7.104.0` samples ordinary legacy DDS diffuse textures through
**UNORM views**. Mudcrab's unannotated assets give its native surface shader the PBR
material's **sRGB view**. For the inspected Riverwood wall and roof textures,
this inserts a decode before native lighting, fog, adaptation and grading.
That is a verified sampling discrepancy. The subsequent
[source-qualified texture-view test](skyrim-native-texture-view-test-20261010.md)
records its controlled brightness effect and remaining visual limits.

UNORM here means that the shader receives normalized stored code values. It
does not establish that the authored pixels represent physical linear-light
RGB. The correction belongs at the texture view boundary; it supplies no new
light coefficient, saturation setting or display gamma.

## Evidence and limits

| Item | Verification |
| --- | --- |
| Executable | `SkyrimSE.exe`, version `1.7.104.0`, image base `0x140000000` |
| Executable SHA-256 | `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f` |
| Native queries | Seven bounded queries against the existing partial Ghidra project, read-only, with analysis disabled; no import or game execution |
| Byte check | 3,844 distinct exported instruction addresses checked against the executable; zero mismatches |
| Independent decode | Five narrow ranges decoded with LLVM 21.1.8; 504 common instruction boundaries, bytes and mnemonics agree, corroborating format selection, GPU creation and diffuse binding |
| Source textures | Nine original DDS files matched to `Farmhouse01` material metadata; all are legacy DXT1 or DXT5 |
| Payload check | All 18 canonical/alias KTX2 view payloads, after Zstd decompression, match their original DDS compressed blocks exactly |

The [interpreted evidence index](skyrim-native-texture-transfer-20261010.json)
contains addresses, artifact hashes, DDS/KTX2 identities and the migration
table. Executable bytes, DDS header bytes, shader binaries and disassembly
remain private. Missing references in the partial project do not prove that
a runtime operation is absent. No live retail material or presentation state
was captured.

## DDS format reaches the diffuse sampler unchanged

The source load at `0x141582680` wraps the stream at source-texture offset
`0x40`, calls the renderer DDS wrapper, and stores the resulting GPU texture
at source-texture offset `0x48`.

The wrapper `0x14100DCC0` passes zero for the optional sRGB conversion argument.
The stream parser `0x14101EE90` forwards it to `0x14101F570`. That creator:

1. Reads a DX10 header's format directly at `0x14101F616`.
2. Calls the legacy format mapper `0x14101FED0` for other DDS files.
3. Calls the sRGB remapper `0x14101F0A0` only when the optional argument is
   nonzero. This source-load path passes zero.

The legacy mapper selects:

| DDS FourCC | Texture and SRV format | DXGI value |
| --- | --- | ---: |
| DXT1 | `BC1_UNORM` | 71 |
| DXT2 / DXT3 | `BC2_UNORM` | 74 |
| DXT4 / DXT5 | `BC3_UNORM` | 77 |

These are distinct from the corresponding sRGB formats, 72, 75 and 78.
[Microsoft's DXGI format definitions](https://learn.microsoft.com/en-us/windows/win32/api/dxgiformat/ne-dxgiformat-dxgi_format)
identify those values.

The GPU constructor `0x14101F230` copies the selected format into both the
Texture2D description and the explicit SRV description. Its calls at
`0x14101F3B1` and `0x14101F504` create those objects. An explicit typed DX10
sRGB format therefore remains sRGB through this path. Typeless formats and
other legacy layouts require separate handling.

For an ordinary DDS-backed diffuse binding, `0x141548820` reads material
offset `0x48` at `0x14154930F` and calls `0x14154C9A0` with slot zero. That
helper follows source texture `+0x48` to GPU texture `+0x10` and assigns its
existing SRV to the slot. It creates no replacement view. The alternative
render-target-backed diffuse branch is outside this DDS conclusion.

## Current conversion and binding discrepancy

[Converter material publication](../../crates/converter/src/material.rs#L563)
chooses sRGB aliases for diffuse, glow and detail from their semantic roles.
[DDS-to-KTX2 format selection](../../crates/converter/src/texture.rs#L403)
also chooses transfer from the material slot and discards the original DDS
transfer distinction. `Ktx2Metadata` retains the resulting format and encoding,
but does not retain the original DDS format.

[Native surface construction](../../crates/engine/src/nif_material.rs#L441)
initially copies `StandardMaterial.base_color_texture`; the material hook now
replaces that native binding when explicit DDS provenance selects a separate
native URI. Its
[fragment shader](../../crates/engine/src/nif_material/native_material.wgsl#L18)
samples that handle before applying the native response. Updating a
`nativeSurface.textureViews` entry alone does not replace this bound handle.

A separate native typed view must preserve the source blocks and sampler.
Encoding a sampled sRGB value afterward cannot reproduce UNORM filtering:
sRGB decode occurs before texture filtering. Alpha remains a data channel.
[Microsoft's D3D11.3 format and sampling specification](https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm)
defines that ordering.

## Verified Riverwood migration scope

All paths below are under `textures/architecture/farmhouse/`. The canonical
and sRGB-alias KTX2 files currently both use sRGB formats: Vulkan 134 for BC1
and 138 for BC3. Original DDS hashes and both KTX2 hashes are in the evidence
index; those identities qualify a migration of this inspected pack.

| DDS | Original | Native sampling | `Farmhouse01` material | Static native selection |
| --- | --- | --- | --- | --- |
| `stonewall01.dds` | DXT1 | UNORM code values | `:1` | Eligible |
| `thatch02.dds` | DXT5 | UNORM code values | `:4` | Eligible |
| `woodpost02.dds` | DXT5 | UNORM code values | `:5` | Eligible |
| `woodwall01.dds` | DXT5 | UNORM code values | `:6` | Eligible |
| `farmhouse01.dds` | DXT1 | UNORM code values | `:7` | Eligible |
| `woodwalkway01.dds` | DXT1 | UNORM code values | `:19` | Eligible |
| `thatch03.dds` | DXT5 | UNORM code values | `:25` | Eligible |
| `farmwindowinterior01.dds` | DXT1 | UNORM code values | `:14` diffuse | PBR fallback |
| `farmwindowinterior01_m.dds` | DXT1 | UNORM code values | `:14` glow | PBR fallback |

The seven eligible materials use the supported ordinary Lighting/default
response and pass the inspected flag/alpha metadata checks. The glowing
window's family/type and flags are unsupported. Actual activation also depends
on geometry, transforms, retained frame metadata, texture load status and the
selected renderer mode. This table is a static selection audit, not a live
draw receipt. A native-view correction leaves unsupported vegetation, glow
and other PBR fallback shading outside its scope.

Durable selection needs original-source provenance: `nativeSourceFormat`
kind/value, `sourceDdsSha256`, `nativeSampleTransfer` (`identity` or
`srgb_decode`) and a distinct `nativeUri`. These are implementation inputs,
not proof of a completed converter contract. Dependency publication, pruning,
cache identity and validation remain required. Existing semantic KTX2 tags
cannot recover whether their original DDS was legacy UNORM or explicit DX10
sRGB.

## Backbuffer proof and remaining display gate

`0x141009B00` sets the swapchain format to DXGI 28, `R8G8B8A8_UNORM`, at
`0x141009BC2`. `0x141012480` obtains its backbuffer and creates RTV and SRV
objects with null descriptions. Those views inherit the resource format;
there is no hardware sRGB conversion at this write boundary.
[Microsoft's SRV creation contract](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11device-createshaderresourceview)
documents inherited format selection.

The recovered HDR result's path to that backbuffer remains untraced. The
backbuffer finding alone does not choose Mudcrab's `display-encoded` versus
`linear` HDR-output hypothesis. Keep that diagnostic independent of the
verified material sampling correction. Fog, adaptation, bloom, grading,
native light production and fallback PBR can still change the final appearance;
none supplies an established compensation for the extra material decode.
