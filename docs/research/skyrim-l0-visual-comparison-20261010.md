# L0 screenshots and the current Riverwood lighting candidate

The L0 Riverwood references retain more detail in shaded wood, stone and trees
than the current normalized lighting candidate. Fog lifts some of our dark
regions but leaves broad wall and foliage areas dark. This supports the visual
rejection of the candidate; it does not identify an exposure, ambient or
saturation correction.

## Reference provenance

The screenshots are in the comments of
[L0 issue #130](https://github.com/Mudcrab-Team/mudcrab/issues/130), rather than
a pull request:

- [Exterior comment](https://github.com/Mudcrab-Team/mudcrab/issues/130#issuecomment-5962439668):
  12 original JPEGs.
- [Interior comment](https://github.com/Mudcrab-Team/mudcrab/issues/130#issuecomment-5962440793):
  6 original JPEGs.

The author identifies Steam Skyrim SE, AppID `489830`, captured on 2026-10-02
after loading a downloaded post-Helgen save. Exact executable version, load
order and graphics settings are not pinned with this set. The comments
explicitly leave camera pose/FOV, weather, in-game time and exposure-settling
history unrecorded. Caption times such as `17:06:33` are capture timestamps;
they do not establish an in-game hour.

All 18 originals were downloaded and their SHA256 identities verified
independently. Each is 2560×1440 RGB JPEG, with no embedded ICC profile or EXIF.
That absence cannot establish the original display transfer or compression
history. Images were retained without color adjustments.

[L1 issue #131](https://github.com/Mudcrab-Team/mudcrab/issues/131) also
classifies these images as visual references with incomplete capture metadata.
The screenshot in [PR #137](https://github.com/Mudcrab-Team/mudcrab/pull/137)
is a synthetic color-pipeline fixture and supplies a different kind of evidence.

## Useful exterior views

Ordinals follow attachment order in the exterior comment.

| View | Original attachment | What it shows |
| --- | --- | --- |
| 03 | [Riverwood street](https://github.com/user-attachments/assets/8b30018b-111b-4b04-86e9-3f9b86da43eb) | Warm thatch, bright path, readable shaded facades and gray atmospheric separation |
| 04 | [Under an eave](https://github.com/user-attachments/assets/e504157f-08ab-4002-85d6-8b95b025ae6b) | Dark wood grain and stone detail remain visible beneath the roof |
| 06 | [Inn and steps](https://github.com/user-attachments/assets/b75dcd58-247a-4bda-9801-9960046ead3d) | Roof light varies across the surface; shaded walls, steps and branches retain detail |
| 08 | [Riverwood terrace](https://github.com/user-attachments/assets/e12fd1ae-64f9-4e3e-bfe6-47c1450400ee) | Pale golden straw, readable gray wood and stone, olive foliage and bright ground |
| 11 | [Distant landscape](https://github.com/user-attachments/assets/fd02e5a1-ea0d-4f2d-be1e-7995fc96a156) | Substantial blue haze, cloud structure and layered mountain separation |

Retail shadows can be deep, and distant haze can be blue. The useful mismatch
is the extent and readability of shaded surfaces in our candidate, rather than
the presence of dark pixels or a particular haze hue.

Our current sky lacks the reference cloud structure; our distant geometry and
surface detail differ too. Actors and other scene content also differ. Their
effect on HDR adaptation has not been isolated. The six interior references
remain available for interior work and were not used to calibrate exterior
lighting.

## Controlled Mudcrab comparison

The current `common-domain-fog-hdr` and `common-domain-hdr-only` images share
the capture executable, asset manifest, camera, weather `0x81A`, hour 12,
native inputs and HDR controls. Both settled after 521 frames with no pending
recorded GPU dependencies. They use worldspace `0x3c`, grid `[5,-12]`,
position `[23400,-47100,400]`, yaw 315°, pitch 7°, HFOV 75° at 1920×1080.

The previous `fog-hdr` image shares this fixture, assets and effective authored
inputs. It is a regression control for the implementation change, not a retail
capture. The candidate captures used frozen executable SHA256
`6acf86f4fe73be0369c7992fd886a6e08285cfce9b148969dc25eb9b4281beef`;
the later test handoff has a separate executable identity.

Within the matched candidate pair, selected shaded-wood mean linear luminance
rises from 0.00423 without fog to 0.00961 with fog. Roof luminance falls from
0.07527 to 0.04973. These are final-image region measurements. The fog toggle
also controls geometry clamps/framebuffer finishing, and both runs include
shared adaptation and grading. This comparison establishes an integrated
implementation effect, not a retail fog compensation rule.

Different retail views and missing capture state prevent pixel matching or
fitting an exposure, fog density, tint or saturation coefficient. Whole-frame
histograms would mix camera, geometry, sky and lighting differences.

## Checks before changing brightness

The current noon sun is a concrete approximation: `preview_sun_direction`
in `crates/engine/src/environment_preview.rs` returns effectively `[0,1,0]`.
The native direct diffuse term in
`crates/engine/src/nif_material/native_common.wgsl` uses
`clamp(dot(normal,L),0.,1.)`, with `L` in the normal's coordinate space.
An ideal vertical wall therefore receives no direct diffuse contribution while
an upward-facing roof receives it. Normal maps, material response and ambient
still affect the final surface.
This explains a property of our input; retail sun state remains unrecorded.

Ambient is already present: the captured matrix evaluates to positive RGB
values for the six axis normals, including vertical walls. The native shader
adds that contribution outside direct-light shadow visibility. Dark output
does not prove that an ambient term is missing.

HDR grading provides another specific control. The current shader computes
`J = A.x + contrast * (brightness * H - A.x)`, where `H` is the channel after
saturation and tint and `A.x` is the adaptation pivot. With day contrast 1.4
and brightness 1.135, a channel clamps to zero when `H/A.x ≤ 0.25173`.
Actual GPU adaptation and per-surface `H` were not captured in the runtime
report. This establishes a mechanism to measure, without attributing the
darkness to authored contrast.

1. Freeze effective light RGB, ambient rows, materials, fog and HDR controls;
   compare the current zenith sun with a separately recorded oblique direction.
   Inspect direct and ambient contributions on the same wall and roof.
   Keep this a diagnostic control until retail trajectory is recovered.
2. Verify directional ambient construction and evaluation, coordinate mapping,
   material normals and native light diffuse/fade producers. Recover their
   inputs before changing ambient intensity.
3. Trace source texture sampling and output transfer through native material,
   fog and HDR stages. Check adaptation inputs and composition with known
   values before introducing a gamma or exposure correction.
   Read back source HDR, actual adaptation, bloom and pre/post-grade channels.
   Current diffuse aliases use sRGB; sampled retail DXBC alone does not prove
   the original SRV format. The native HDR output also uses an encoded-output
   bridge before Bevy's sRGB store; retail target transfer remains unverified.
4. Compare shadow visibility separately from diffuse response, and evaluate
   missing clouds, volumetrics and distant terrain as distinct scene changes.
5. Obtain a retail capture with recorded camera, weather, time, effective
   runtime inputs and exposure state before claiming quantitative parity.

These checks leave authored fog and IMGS coefficients intact. The source
equations already recovered remain evidence; approximate input producers and
unverified display transfer remain acceptance work.

## Evidence and artifacts

The [comparison inventory](skyrim-l0-visual-comparison-20261010.json) records
all stable attachment URLs, hashes, container metadata, Mudcrab capture
identities and comparability checks. No raw retail executable or shader bytes
are included.

Originals, the interactive comparison and independent reviews are stored under
`/Users/taylor/.local/share/mudcrab-research/l0-comparison-20261010/`.
The private `analysis/darkness-source-audit.md` records source locations and
controls; `analysis/darkness-coefficient-controls.json` records the ambient
and grading calculations with their captured input identities.
The current and previous engine controls remain under
`/Users/taylor/.local/share/mudcrab-research/saturation-20261010/captures/`.
This review changed documentation and analysis artifacts; it did not change
renderer settings or the retained world-preview launcher executable.
