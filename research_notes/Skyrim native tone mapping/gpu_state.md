# Captured Skyrim GPU state and tone-map replay

Campaign started 2026-10-15 UTC; the five-pixel replay completed 2026-10-16 UTC.
One native Skyrim frame now connects the audited group-26 shader to bound GPU
constants, textures, samplers, and sampled values. Replaying its arithmetic on
the CPU agrees with GPU replay at five selected pixels, including a pixel with
visible bloom contribution and a clipped highlight. This validates a bounded
post-processing calculation; it does not establish full-frame or Mudcrab parity.

The approved TonyMcMapface/sRGB preset remains the default. This phase added
diagnostic tools and evidence without changing engine rendering code or presets.
Original captures and exported GPU resources remain private evidence outside Git.

## Capture identity and scene controls

Evidence root:
`tonemap-gpu-20261015T230233Z` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z`).
RenderDoc 1.45 captured DXVK's Vulkan translation of the isolated Fiji session.
The original `skyrim_frame59881.rdc` is **2,879,141,326 bytes**, about 2.88 GB,
and remains on Fiji at:

```text
/home/taylor/Games/MudcrabComparison/artifacts/tonemap-gpu-20261015T230233Z/skyrim_frame59881.rdc
```

Its SHA-256 is
`96ad236ca05c52c4ef66ef0a54648576e7634ca84630bf5e1695773769f728c0`.
The `probe receipt` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/sample-probes-02/receipt.json`)
checks that hash before and after replay. The capture was not copied into the
local worktree. Exported resources, original and instrumented shaders, logs, and
images remain private evidence outside Git.

Nearby console observations read game hour `12`, current weather `81a` as `1.00`,
weather transition fraction `1.00`, and
`getini "fShadowDistance:Display"` as `8000`. The raw console images and OCR are
retained under `console/`, with the shadow query separately recorded in
`shadow-readback.json` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/shadow-readback.json`).
These later session observations do not establish what value Skyrim consumed
for this frame: the earlier capture campaign's Documents INI on disk recorded
`23200` (see [the earlier runtime state](runtime_state.md#capture-linkage-and-remaining-limits)).
The controlling native input for that frame and the older reference screenshots
remains unresolved; the console query does not prove an override mechanism.
Weather and sun observations are also not frame-synchronized shader inputs.

The `native camera export` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/native-camera-export.json`)
has position `[21750, -44530, 140]`, rotation `[6.2308254, 1.5707964]` radians,
FOV `80`, and roll `0`. The independently decoded camera block gives horizontal
FOV `79.99999842452057` degrees and vertical FOV `50.534010187183334` degrees.
The frozen Mudcrab camera instead used horizontal FOV `72.73435310118231`, about
45 degrees vertical. See the [projection evidence](dxvk_capture_mapping.md#captured-camera-projection-event-11359)
for matrix offsets, inverse checks, and the remaining scene-geometry binding
limit. The frozen references were preserved.

The comparison profile's temporary preference change was restored after capture.
The `shutdown receipt` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/shutdown.json`)
records comparison game, Steam, and display services inactive, and the runtime
launch override removed. Offline replay does not launch or attach to Skyrim.

## Selected program and bound inputs

At event **11359**, the captured pixel shader's source name is
`fs.716590ec34046095c58617804cdff8cc`. It matches the original DXBC container's
header digest for group 26, `ISHDRTonemapBlendCinematic`, key `00000000`.
The captured SPIR-V SHA-256 is
`6c72aaa001228aa37ddf79dc31669a7087b9981f75f180b2fa36354dc59802be`.
This is source-name correlation to the audited retail shader, not an independent
dump and hash of the original runtime DXBC. The [DXVK mapping note](dxvk_capture_mapping.md)
documents that distinction and the translator revision.

The `event receipt` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/events-01/receipt.json`)
and its SPIR-V disassembly name the original registers explicitly. Their
descriptor decorations, rather than Vulkan binding numbers alone, connect the
shader operands to these resources:

| Native register | Captured resource | Dimensions and view | Shader use |
| --- | --- | --- | --- |
| `t0` | `ResourceId::351` | 480×270, `R11G11B10_FLOAT` | Bloom-like RGB from the audited operand chain |
| `t1` | `ResourceId::335` | 1920×1080, `R11G11B10_FLOAT` | Scene RGB from the audited operand chain |
| `t2` | `ResourceId::396` | 2×2, `R11G11B10_FLOAT` | Adaptation pair `P.xy` |
| Output | `ResourceId::363` | 1920×1080, `R8G8B8A8_UNORM` | Ordinary group-26 RGBA output; blending disabled |

Views have identity swizzles and are not sRGB-corrected. Sampler push indices
are `s0=1`, `s1=4`, `s2=1`. Heap element 1 uses linear filtering; element 4 uses
point filtering. Both clamp to the edge and select mip 0. Thus bloom and
adaptation use linear filtering, while the scene uses point filtering.
Storage formats and the absence of an sRGB view do not establish the physical
or color meaning of the scene values produced by earlier passes.

The relevant captured constant rows are:

| Operand | Captured binary32 values |
| --- | --- |
| `cb2[0]` | `(0, 0, 0, 0)` |
| `cb2[2]` | `(0.800000011920929, 0.8264462351799011, 0, 0)` |
| `cb2[3]` | `(1.5, 0, 1.399999976158142, 1.1349999904632568)` |
| `cb2[4]` | `(0.8941176533699036, 0.8392156362533569, 0.7725489735603333, 0.41999998688697815)` |
| `cb12[42].x` | `1` |
| `cb12[43].xy`, `cb12[44].z` | `(1, 1)`, `1` |

The selected curve is therefore Reinhard in this draw. The coefficient, grading
rows, and exponent agree with the earlier [CPU observations](runtime_state.md).
This does not select Reinhard for every Skyrim configuration or frame.

## Sampled adaptation and five-pixel arithmetic check

The stored 2×2 adaptation texture has red/green pairs
`(0.359375, 0.359375)`, `(0.375, 0.375)`,
`(0.36328125, 0.36328125)`, and `(0.37109375, 0.37109375)`.
The `storage analysis` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/adaptation-storage-analysis.json`)
retains every packed word and its independently tested unsigned-float decoding.
The GPU sampling probes then measured the following values, rather than
substituting raw texels for filtered samples:

| Pixel `(x,y)` | Sampled `P.x` | Sampled `P.y / P.x` | Bloom contribution | Maximum RGB absolute error |
| --- | ---: | ---: | --- | ---: |
| `(960,540)` | `0.3671875` | `1` | Sampled RGB is zero | `8.940696716308594e-8` |
| `(1100,300)` | `0.3693840503692627` | `1` | Sampled RGB is zero | `0` |
| `(400,100)` | `0.359375` | `1` | Sampled RGB is zero | `5.960464477539063e-8` |
| `(502,1014)` | `0.36346435546875` | `1` | Nonzero sample and weight | `5.960464477539063e-8` |
| `(1702,1078)` | `0.37109375` | `1` | Nonzero sample; weight clamps to zero | `0` |

At `(502,1014)`, sampled bloom RGB is
`(0.188446044921875, 0.220428466796875, 0.16650390625)` and the replay's bloom
weight is `0.5027501583099365`. At `(1702,1078)`, scene RGB is
`(1.078125, 1.0625, 1.0625)` and final shader RGB is `(1,1,1)`.
These cases extend the check beyond zero-bloom, unclipped pixels.

Across all five pixels, maximum RGB error is `8.940696716308594e-8`; maximum
RGBA error is `2.384185791015625e-7`. Every comparison passes the declared
absolute tolerance `2e-6` with relative tolerance `0`. The reference uses
binary32 storage, flushed arithmetic subnormals, fused multiply-adds, and the
captured DXVK dot-product sequence: one multiply followed by two fused
accumulations. It preserves separate logarithm, multiplication, and exponential
instructions, including the final exponent. No artistic sRGB conversion is added.

The sampled `P.x` is also the native contrast pivot. It varies by pixel in this
frame. These measurements do not justify replacing the approved pivot with a
new constant such as `0.36`. Equal `P.x` and `P.y`, and therefore exposure ratio
1, are observations of this frame, not universal adaptation behavior. The
adaptation producer and its temporal history still require separate validation.

## How the GPU samples were measured

RenderDoc's shader debugger rejected this module's `SPV_KHR_float_controls2`
extension and associated capabilities. That failure is retained in
`pixel-probes-01.json` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/pixel-probes-01.json`).
PixelHistory remained available.

The sampling collector pins the original SPIR-V hash, DXVK source name, entry
point, named resources, sample definitions, output type, and four final output
stores. It removes only those stores. Scene and adaptation probes write the
existing sampled float4 to `SV_Target`; the bloom probe writes the selected RGB
phi values plus alpha 1. All other original instruction words remain in order.
Coordinates, descriptor accesses, sampler indices, branches, and sample
instructions are retained in the instrumented module.

For each probe, RenderDoc temporarily replaces the shader in its offline replay
controller. PixelHistory executes the shader on the replay GPU using an
`R32G32B32A32_SFLOAT` attachment with blending disabled. Its `shaderOut` values
retain samples above 1; the separate `postMod` values include the original
UNORM target conversion and must not be substituted for sampled inputs.
See RenderDoc's [float output attachment](https://github.com/baldurk/renderdoc/blob/v1.45/renderdoc/driver/vulkan/vk_pixelhistory.cpp#L3233)
and [blend override](https://github.com/baldurk/renderdoc/blob/v1.45/renderdoc/driver/vulkan/vk_pixelhistory.cpp#L3711).

The `probe receipt` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/sample-probes-02/receipt.json`)
records all three replacement removals and resource frees as successful. Original
shader outputs match before and after restoration, and the capture hash is
unchanged. If replacement removal fails, the collector retains the possibly
active resource until controller shutdown instead of freeing it prematurely.

These are **instrumented GPU replay measurements**, not direct reads of
intermediate registers from the original frame. A replacement can change driver
optimization. The independent comparison is between CPU arithmetic using those
measured inputs and PixelHistory's replay of the unmodified shader at the same
pixels. Five successful pixels do not constitute a full-frame arithmetic proof.

The `vertex evidence` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/tonemap-vertex-evidence.json`)
also retains post-VS bytes and indices. The fullscreen quad has clip-space
corners at ±1, clip `w=1`, UV corners 0–1, and a viewport with `y=1080` and
height `-1080`. These establish affine pixel-center UVs for this draw; UVs were
not inferred solely from output dimensions.

## Presentation and visual comparison limits

The captured sequence includes a group-34 adaptation draw at event 11302,
group 26 at 11359, a later draw at 11411 consuming the tone-map output and other
resources, and DXVK presentation at 11430. This is a partial sequence, not a
complete pass graph or reconstruction of bloom and adaptation history.

At presentation event 11430, all **2,073,600 pixels** have identical stored RGBA
channel bytes between its `R8G8B8A8_UNORM` input and `B8G8R8A8_SRGB` output
after accounting for channel order. The `byte comparison` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/presentation-byte-comparison.json`)
records both storage hashes. This closes the stored-byte comparison for that
draw only. It does not establish display color management, the earlier scene
numeric domain, or the behavior of intervening post-processing.

Two Metal captures use the corrected horizontal FOV 80: the approved Tony/sRGB
control and the record/Reinhard experiment. Both captured cameras report vertical
FOV `50.53401176308017` degrees. Their camera, source, readiness,
grading, backend, and reference checks are retained in
`hfov80-validation.json` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/hfov80-validation.json`).
That bounded validation does not establish visual parity. The record/Reinhard
experiment still uses Bevy scene inputs, its existing grading domain, fixed
sample/target luminance, and the artistic pivot; it does not consume the new
native sampled textures. The frozen approved and Skyrim references remain
separate comparison images.

Remaining work includes scene input domains and lighting calibration,
directional ambient lighting, materials and shadow state, distance-dependent
fog, bloom production, adaptation history, and the remaining post-processing
chain. Additional weather, time, and interior captures are needed before
treating these frame-specific values as a general native pipeline.

## Tools and repeatable commands

The three new tools are:

- [Capture inspector](../../scripts/inspect_skyrim_gpu_capture.py): selected
  graphics/compute stages, named resources, buffers, shaders, formats, and samplers.
- [GPU sample collector](../../scripts/sample_skyrim_tonemap_gpu.py): pinned,
  replay-only output probes, unmodified shader outputs, restoration and hash checks.
- [Arithmetic replay](../../scripts/replay_skyrim_tonemap.py): explicit sampled
  inputs and constants for audited groups 26/27, without inferred texture sampling
  or color conversion.

The focused Python selection passes **58 tests** across those tools. Synthetic
tests cover guards, cleanup, arithmetic and packed storage decoding; actual
GPU replay and Metal captures are separate evidence. No tests were added for
these documentation updates.

With the matched RenderDoc 1.45 Python module and replay library available on
Fiji, the existing capture can be inspected again without launching the game:

```bash
skyrim_evidence=/home/taylor/Games/MudcrabComparison/artifacts/tonemap-gpu-20261015T230233Z
export XDG_RUNTIME_DIR=/run/user/1000
export PYTHONPATH=/nix/store/3rj4aid9pdnahscq2cl5cz3lb72wkl6a-renderdoc-headless-python-1.45/lib
export LD_LIBRARY_PATH=/nix/store/lk4614327cn1h9z3m6cvnls95n13m1cj-vulkan-loader-1.4.357.0/lib:/run/opengl-driver/lib
python3 "$skyrim_evidence/inspect_skyrim_gpu_capture.py" \
  "$skyrim_evidence/skyrim_frame59881.rdc" \
  --event 11302 --event 11359 --event 11411 --event 11430 --tail 0 \
  --output "$skyrim_evidence/events-recheck"

python3 "$skyrim_evidence/sample_skyrim_tonemap_gpu.py" \
  "$skyrim_evidence/skyrim_frame59881.rdc" \
  --source-shader "$skyrim_evidence/events-01/event-11359-Pixel-shader.bin" \
  --capture-sha256 96ad236ca05c52c4ef66ef0a54648576e7634ca84630bf5e1695773769f728c0 \
  --event 11359 --shader-id 'ResourceId::27852' --output-id 'ResourceId::363' \
  --pixel 960,540 --pixel 1100,300 --pixel 400,100 \
  --pixel 502,1014 --pixel 1702,1078 \
  --output "$skyrim_evidence/sample-probes-recheck"
```

All output paths must be fresh. The prepared sampled-input JSON joins captured
constant rows, vertex-derived UVs and the GPU probe's `shaderOut` values. From
the local repository, the retained five-pixel calculation can be repeated with:

```bash
local_evidence=data/visual-comparisons/tonemap-gpu-20261015T230233Z
python3 scripts/replay_skyrim_tonemap.py \
  --input "$local_evidence/sampled-native-inputs-02.json" \
  --output "$local_evidence/native-arithmetic-recheck.json" \
  --mad-mode fused --dot-mode fused-chain --atol 2e-6 --rtol 0
```

The authoritative retained result is
`native-arithmetic-replay-02.json` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/native-arithmetic-replay-02.json`),
using `sampled-native-inputs-02.json` (private historical evidence outside Git: `data/visual-comparisons/tonemap-gpu-20261015T230233Z/sampled-native-inputs-02.json`).
The record keeps input/script hashes, individual component errors, the arithmetic
policy, and the limits of the measurement.
