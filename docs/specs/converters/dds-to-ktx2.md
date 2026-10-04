# DDS to KTX2 Texture Conversion

OpenSkyrim converts extracted Skyrim DDS assets ahead of time to KTX2 containers. Two output
profiles exist:

| Output profile | Texture policy |
| --- | --- |
| **Desktop/native** (default) | Preserve compatible DDS block formats inside KTX2; transcode only when required. |
| **Portable fallback** | UASTC for sources with no native mapping, with matching supercompression. |

The desktop profile keeps BC1-BC7, R8, and RGBA8 payloads byte-for-byte: no re-encoding loss,
no 2x BC1/BC4 growth, and the runtime samples the original GPU format directly.

## Semantic encoding contract

Color space is derived from the consuming slot, never from a filename suffix or file extension.

| Slot semantics | Encoding |
| --- | --- |
| base color, emissive, specular/gloss, detail, environment cube | sRGB color |
| tangent-space normal, water flow normal | linear normal |
| metallic/roughness, occlusion, height, masks, inner layer, greyscale | linear data |

The converter collects semantics from every published GLB plus `texture_sets` and water rows in the
world database. A texture reached through incompatible classes is rejected because one KTX2 URI
cannot safely represent both transfer functions. Unclassified assets remain linear/data rather than
being guessed from their names.

## Supported DDS topology and formats

- BC1, BC2, BC3, BC4, BC5, BC6H and BC7 2D textures are covered by conversion fixtures.
- Six-face cubemaps and reachable 3D volume textures preserve their topology.
- Ordinary texture arrays are rejected explicitly.
- All authored DDS mip levels are preserved independently; the converter does not replace
  them with a generated chain. A representation that would lose source levels is a hard failure.
- RGBA alpha is retained for opaque, cutout and blended material consumers.

## Native-block preservation

Preservable sources (FourCC DXT1-DXT5 plus DXGI BC1-BC7, R8, RGBA8, 2D/cubemap/volume) map to
their native `VkFormat` with sRGB vs linear taken from slot semantics, never the filename. Each
mip level's bytes copy verbatim; cubemap levels gather one slice per face, volume levels keep
their depth slices. The DFD is generated from the target format. Unmapped DXGI formats and L8 fall
back to the UASTC path.

Uncompressed 2D textures with whole-byte channels (24-bit B8G8R8, X8R8G8B8, A8R8G8B8, A8B8G8R8
and other RGB(A) bitmask layouts, DXGI B8G8R8A8/B8G8R8X8) are not re-encoded with UASTC, which
is slow. Their decoded mips are block-compressed on the CPU (`intel_tex_2`) and stored as native
BC7 in every slot: the fast alpha profile when the layout has an alpha channel, the fast opaque
profile otherwise (treated as opaque: alpha at or near 255, since BC7 mode 6 can land a step or
two below it). sRGB vs UNORM comes from the slot encoding. BC7 is what the runtime
transcoded the former UASTC output to on desktop, so GPU memory does not change. Like the
preserved BC sources, this output belongs to the desktop profile: GPUs without BC support (Adreno
on Android) cannot sample it, and a portable profile would have to encode these textures and the
preserved BC sources to UASTC instead. No converter option selects that profile yet.

Each mip is padded to whole 4x4 blocks by replicating edge pixels, so 1x1 and odd-sized mips keep
the block counts the container expects; rows that already fill whole blocks are compressed where
they lie. The output is lossy, unlike the byte-copy formats. It is deterministic on one machine but
not across CPU generations: `intel_tex_2` picks its ISPC kernel at run time, and the AVX2 kernel
(which fuses multiply-adds) can choose different blocks than the SSE2, SSE4 and AVX kernels, which
agree with each other. Output hashes are only compared against outputs the same run wrote, so this
does not affect caching or verification. Rows are tight or DWORD-aligned, which
only differ for 24-bit layouts: a mip 0 header pitch that names exactly one of them decides for
every mip; otherwise a payload of exactly the aligned chain's size means aligned rows, and anything
else is read tight, as `image_dds` reads it. Bytes after the last mip are ignored. A texture whose
payload is shorter than its chain falls back to UASTC instead of failing, and the packed attempt's
reason is chained onto a later failure's error. Cubemaps and volumes of these layouts, 16-bit
formats, palettes and L8 also fall back to UASTC. Under `--texture-encoder gpu` these textures are
encoded to UASTC on the GPU instead.

Byte preservation is asserted per mip level in fixtures, and a Bevy engine test loads native
output through `ktx2_buffer_to_image` verifying GPU format, dimensions, and mip count.

## Per-mip Zstandard supercompression

Every output level (native or UASTC fallback) is Zstandard-compressed after its faces and
slices assemble into the complete level. Compression never applies to faces independently:
independently compressed faces differ in length and would break cubemap level assembly.
The KTX2 header carries scheme 2 with compressed `byte_length` and true
`uncompressed_byte_length` per level; `texture_zstd_level` (default 6, 0 disables) sets the
level and participates in the manifest configuration hash. No rate-distortion tuning is
applied: this stage is lossless, and any lossy tuning stays a separate quality decision.

The runtime enables Bevy `zstd_rust` so `ktx2_buffer_to_image` decodes each level before
format mapping. A Bevy engine test asserts supercompressed and plain outputs decode to
identical image bytes.

## Publication and validation

Preservable sources keep their blocks; fallback sources decode each mip/face/slice to RGBA and
encode as UASTC. UASTC remains the fallback for every semantic class because the Bevy runtime
path supports its KTX2 supercompression contract consistently. Before an
output is atomically renamed into place, the converter verifies:

- KTX2 signature, transfer function and image-level table;
- width, height, depth, mip count, face count and layer count against the DDS;
- encoded byte length and deterministic SHA-256;
- expanded RGBA byte size, color model and supercompression mode.

Temporary files are removed after a failed validation/publication. Batch conversion is staged and
only becomes a complete conversion manifest when every supported input converts and every generated
artifact validates. A texture the installed game data contains but the converter cannot publish
still fails the run; a texture reference whose source the game data does not contain does not. The
converter drops those references - including a base-color URI - and every material slot that pointed
at them from the published GLB, records the dropped path under that mesh's
`pruned_texture_references` entry in `conversion-manifest.json`, and still publishes the manifest
with `complete: true`, so the mesh renders with the maps that do exist.
The standalone texture-closure report uses format version 3, records the converter schema and the
same metadata per texture, and never reports success for missing required assets or conversion
failures.

## Round-trip acceptance

Automated fixtures transcode the resulting UASTC through the desktop BC7 path and decode it again.
They verify that cutout alpha retains transparent and opaque regions, asymmetric tangent-space
normal vectors keep X/Y orientation and Z intensity, and distinct authored mip colors/alpha remain
in their original levels. Installed cubemap and volume fixtures can also be exercised through the
`OPENSKYRIM_DDS_FIXTURE` and `OPENSKYRIM_VOLUME_DDS_FIXTURE` test environment variables.
