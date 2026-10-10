The broader native diffuse correction passes source, migration and capture checks, but the full-pack frames still fail visual acceptance. Pale roof ridges and white stone patches remain against dark ground and trees. The user's preferred brightness from the older build remains an acceptance target.

Original legacy DDS headers and preserved compressed mip blocks qualify 3,706 diffuse sources. The native loader uses their UNORM formats; the existing PBR views retain their transfer behavior. This extends the [earlier texture-transfer correction](skyrim-native-texture-transfer-20261010.md) without changing authored saturation, brightness, contrast, tint, gamma, fog or sunlight RGB. Explicit sRGB sources and unqualified views remain excluded.

| Coverage stage | Count | Scope |
| --- | ---: | --- |
| Scanned GLBs | 30,061 | Static pack inventory |
| Diffuse views | 63,160 | Static material references |
| Qualified legacy sources | 3,706 | 1,993 DXT1; 1,713 DXT5 |
| Verified canonical/alias KTX paths | 7,449 | Exact original compressed blocks |
| Annotated diffuse views | 62,864 | Applied private migration |
| Changed GLBs | 20,088 | Applied private migration |
| Byte-identical native aliases | 3,724 | Applied private migration |
| Unqualified diffuse views | 296 | Preserved without annotation |
| Qualified views eligible under material policy | 34,764 | Across 11,373 GLBs; geometry and runtime binding unverified |

The migration preserves the base manifest and source-file identities, geometry payloads, prior authored JSON fields and PBR texture views. Its 13 readback gates pass. This audit checks pinned manifests, proof/report hashes and recorded per-file invariants; it does not independently rehash every changed GLB. Publication through the durable converter is still separate work. Static annotations and material eligibility do not establish activation on every draw.

Six completed 1920×1080 Riverwood terrace captures hold the camera, noon clear weather (`0x81A`), fog and IMGS (`77704`) fixed. Two old-direction eight-texture frames, two [recovered local sun](skyrim-native-sun-trajectory-20261010.md) eight-texture frames and two recovered-sun full-pack frames each use automatic or fixed adaptation. All captures exit successfully and settle with zero pending or unavailable dependencies. The 138 capture checks, 44 sun-report checks, 52 primary-pair checks, 36 automatic/fixed pair checks and 13 migration checks pass: 283 recorded gates.

The 48 GPU point readbacks are eight named pixels per capture at frame 510. Their shared tone-equation reconstruction differs by at most 1.23e-07. Readback is asynchronous; these checks do not establish exact screenshot-frame pairing or identify entire materials. Runtime reports list 2,035 native candidate/rendered primitives, 1,668 fallback primitives and 63,220 approximate standard primitives in each capture. Those preparation counts are not measured GPU draws. Each capture reports 15,077 visible meshes and 115 pipelines; shadow caster counts change from 88 to 158 with the direction correction.

The table gives relative changes in mean final-pixel linear sRGB luminance Y. Sun pairs change the frozen engine's CPU trajectory with the eight-texture pack retained. Coverage pairs use the same new engine and change only the source-qualified pack views. Automatic pairs include scene-dependent adaptation and bloom; fixed pairs hold A constant and retain bloom.

| Region | Sun, automatic A | Sun, fixed A | Coverage, automatic A | Coverage, fixed A |
| --- | ---: | ---: | ---: | ---: |
| Roof | +11.51% | +13.73% | -2.24% | +0.00% |
| Wood wall | +186.87% | +209.96% | -7.25% | +0.00% |
| Stone | +16.89% | +18.09% | -4.34% | +0.96% |
| Grass/soil | -29.16% | -25.28% | +0.88% | +22.53% |
| Fern rectangle | -9.62% | -10.56% | +502.84% | +845.38% |
| Sapling rectangle | -9.10% | +1.54% | +28.92% | +57.38% |
| Sky | -2.76% | +0.00% | -2.30% | +0.00% |

The fixed coverage pair preserves every RGB8 pixel in the roof, wood and sky rectangles. All eight selected pre-HDR source RGBA samples remain identical in both coverage pairs, even where surrounding rectangles change. Mixed foliage rectangles include cutouts and background; their large changes cannot be assigned to a single material. Automatic adaptation couples the broader source correction to other regions, including already-qualified roof and wood. These measurements do not imply a uniform brightness gain.

The full automatic frame has 389 of 45,000 roof pixels and 192 of 10,400 stone pixels at an RGB8 channel endpoint. Stone p99 Y remains 1.0 in all six frames. The selected stone point retains pre-HDR source `[1,1,1]` and white tone output. This point and the remaining pale patches survive the coverage correction; [bloom/specular isolation](skyrim-glow-isolation-20261010.md) supplies separate evidence about their cause.

Fixed A.x=A.y=`0.25318095088005066` replaces spatial and temporal history everywhere using one older roof sample. It is a diagnostic, not a proposed default or a retail adaptation reference. The only argument normalization parses the equivalent spellings `0.25318095` and `.25318095` as binary32; raw arguments remain in the private evidence and all sampled fixed values match exactly. Moving from automatic to fixed A changes regions differently:

| Region | Old eight-texture | Native sun, eight-texture | Native sun, full pack |
| --- | ---: | ---: | ---: |
| Roof | -0.28% | +1.71% | +4.04% |
| Wood wall | -12.50% | -5.46% | +1.93% |
| Stone | -6.95% | -5.99% | -0.78% |
| Grass/soil | -25.71% | -21.65% | -4.83% |
| Sky | +1.66% | +4.54% | +7.01% |

Source correction and visual acceptance remain separate. The trajectory assumes an identity Sky parent and compiled settings; the retail parent, exceptional modes and live settings remain unverified. The complete retail HDR-to-backbuffer pass chain is still open. [Normal conventions](skyrim-native-normal-convention-20261010.md), unsupported materials, foliage fill, terrain and mixed fallback responses remain gaps. The [subsurface audit](skyrim-subsurface-audit-20261010.md) does not establish missing foliage fill as the cause of dark walls. No global grading or gamma coefficient was selected from these images, and no whole-frame histogram was fitted to a differently framed retail reference.

The [compact evidence summary](skyrim-native-diffuse-coverage-20261010.json) contains hashes, static counts, control gates, region definitions, quantiles, chroma, endpoint metrics and GPU point samples. It omits original game bytes and the 20,088-row migration ledger.
