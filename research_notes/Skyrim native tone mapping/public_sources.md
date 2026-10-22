# Skyrim native tone mapping: public source findings

Research date: 2026-10-15. Scope: public source inspection, with a clearly separated crosscheck against existing local retail-bytecode research. No engine changes, game launches, Fiji changes, publication, or shader redistribution. Public source snapshots are retained privately under `/private/tmp/mudcrab-native-tonemap-public-20261015/`.

## Can public research identify Skyrim's native tone curve and operation order?

### Takeaway

Yes. Historical reverse-engineered source exposes two scalar tone-curve alternatives, luminance-based exposure and rescaling, bloom, cinematic grading, a sampled-average contrast pivot, output clamp and power, and optional fade. These formulas agree with existing locally audited retail shader research. A later public implementation called "vanilla" changes several of those operations; the retail bytecode must decide that conflict.

### Cited Findings

- Community Shaders commit `d106f94a41b7db187f70bcc43ccb933a04868521` (2024-08-04, PR 359, "imagespace shaders support") supplies a readable `ISHDR.hlsl`. The PR explicitly calls these image-space shaders reverse engineered. This is community reverse engineering, not Bethesda source publication. — [Original PR](https://github.com/community-shaders/skyrim-community-shaders/pull/359); [pinned shader](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/ISHDR.hlsl)
- The shader reads scene RGB from `t1`, bloom/image RGB from `t0`, and two average/adaptation values from `t2`. It computes a scalar `Y=max(dot(sceneRGB,[0.2125,0.7154,0.0721]),0.00001)`, then exposed scalar `Z=Y*(avg.y/avg.x)`. — [ISHDR lines 85-98](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/ISHDR.hlsl#L85-L98); [luminance weights](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/Common/Color.hlsli#L1-L6)
- With `p=cb2[2].y`, its named Reinhard branch is `F(Z)=Z*(Z*p+1)/(Z+1)`. Its named Hejl-Burgess-Dawson branch uses `x=max(0,Z-0.004)` and `F(Z)=p*(((6.2*x+0.5)*x)/(x*(6.2*x+1.7)+0.06))^2.2`. Selection is `cb2[2].z>0.5`. The source establishes those names and equations; it does not prove how authored White or an INI option produces `p` or the selector. — [curve functions](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/ISHDR.hlsl#L41-L51); [branch and exposure](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/ISHDR.hlsl#L94-L106); [2.2 constant](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/Common/Color.hlsli#L1)
- It reconstructs RGB as `sceneRGB*(F(Z)/Y) + bloomRGB*saturate(cb2[2].x-F(Z))`. Thus the historic implementation is luminance based, rather than applying the nonlinear curve separately to red, green, and blue. Bloom is combined before cinematic grading. — [ISHDR lines 94-107](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/ISHDR.hlsl#L94-L107)
- For reconstructed RGB `C`, `L=dot(C,weights)`, saturation `s=cb2[3].x`, brightness `b=cb2[3].w`, contrast `k=cb2[3].z`, tint RGB `t=cb2[4].xyz`, and tint amount `a=cb2[4].w`, the RGB part is `graded=b*lerp(lerp(L,C,s),L*t,a)`, then `contrasted=lerp(avg.x,graded,k)`. The contrast pivot is an input from the average texture, rather than a fixed artistic constant such as 0.18. These source labels remain public reverse-engineering names until their native writers are verified. — [ISHDR lines 107-113](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/ISHDR.hlsl#L107-L113)
- RGB is clamped to [0,1], then raised to `FrameParams.x`; the optional fade is applied afterward. Public `FrameBuffer.hlsli` labels `FrameParams.x` "inverse fGamma" and implements `pow(linearColor,FrameParams.x)`. A power exponent is directly evidenced; naming it exact IEC sRGB encoding, inferring the actual exponent, or inferring the sampled color domain requires additional native/runtime evidence. — [ISHDR lines 109-117](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/ISHDR.hlsl#L109-L117); [FrameParams layout](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/Common/FrameBuffer.hlsli#L14-L24); [power function](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/Common/FrameBuffer.hlsli#L67-L70)
- The historical shader's adaptation branch uses previous adapted xy, a weighted downsample, and `previous + sign(delta)*clamp(abs(cb2[2].wz*delta),0.00390625,abs(delta))`. It therefore contains state, not only a static exposed curve. Native writers and timing are needed to map this to authored eye-adaptation settings. — [ISHDR lines 57-83](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/ISHDR.hlsl#L57-L83)

The following is a conceptual RGB summary, not a replacement for exact D3D instruction ordering:

```text
Y = max(dot(scene, [0.2125,0.7154,0.0721]), 0.00001)
Z = Y * (adapted.y / adapted.x)
Q = selected_curve(Z, parameter_p)
C = scene * (Q / Y) + bloom * saturate(bloom_parameter - Q)
L = dot(C, [0.2125,0.7154,0.0721])
G = brightness * lerp(lerp(L,C,saturation), L*tint_rgb, tint_amount)
G = lerp(adapted.x, G, contrast)
O = pow(saturate(G), output_exponent)
O = lerp(O, fade_rgb, fade_amount)  // only when fade variant selected
```

All operations in this summary are sourced by the pinned shader links above. Its public variable name `linearColor` does not by itself establish a physically linear color domain.

### Inferences

- The original Photo Mode cinematic/tint values alone cannot reproduce the final output: they omit the active curve branch, its parameters, exposure/adaptation texture values, bloom input, and output exponent. This follows from the independent inputs in the equations above.
- Bevy TonyMcMapface plus the same numeric cinematic/tint values is a different composite transform. Reproducing the Skyrim equations would require an alternate full pipeline, rather than turning off Bevy's mapper while retaining the approved artistic grade.

### Gaps

- Public source does not establish which curve branch was selected in the reference screenshot, actual constant values, adaptation state, final active modifiers, render-target/SRV formats, or full frame pass order.
- Public `Param.y` is a curve parameter. Do not assume `White`, `1/White`, or `1/(White*White)` without the CPU producer.
- The public output exponent label is not a fresh native verification of its setting source, actual value, or output-surface encoding.

## Which public sources are useful, and where do mod replacements diverge?

### Takeaway

Use the historical scalar shader for readable corroboration, CommonLib for data layout and native targets, and retail bytecode/REA for authority. Do not substitute current Community Shaders image-space code for vanilla Skyrim.

### Cited Findings

- Original Community Shaders image-space support PR 359 documents reverse-engineered shaders and incomplete coverage, and explicitly states VR shaders were not adapted. Its version scope matters. — [PR 359](https://github.com/community-shaders/skyrim-community-shaders/pull/359)
- PR 438 deliberately adds a neutral tonemapper based on Frostbite research to fix overbright PBR regions. It is a replacement/change, not vanilla documentation. — [PR 438](https://github.com/community-shaders/skyrim-community-shaders/pull/438)
- Commit `a00ff9d3eb1397fedf4165a15581f5d3a2a68fe2` (2024-11-12) is titled "remove added tonemapping, per-channel vanilla" and marks a code block "Vanilla tonemapping and post-processing". Nonetheless its source applies the curve per RGB channel, uses a lower-only clamp before output, and applies fade before the output power. Those choices differ from both the earlier public source and the audited local retail package. The label does not resolve the conflict. — [commit](https://github.com/community-shaders/skyrim-community-shaders/commit/a00ff9d3eb1397fedf4165a15581f5d3a2a68fe2); [changed implementation lines 99-137](https://github.com/community-shaders/skyrim-community-shaders/blob/a00ff9d3eb1397fedf4165a15581f5d3a2a68fe2/package/Shaders/ISHDR.hlsl#L99-L137)
- Current public `dev` revision retrieved was `3424ae12a5b0c10c2d45058b99d84bf30874cd71`. Its `ISHDR.hlsl` contains HDR display support, hue-preserving alternatives, a modified shadow contrast function, and linear-lighting switches. These are explicit modifications. — [pinned current shader](https://github.com/community-shaders/skyrim-community-shaders/blob/3424ae12a5b0c10c2d45058b99d84bf30874cd71/package/Shaders/ISHDR.hlsl)
- CommonLibSSE-NG revision `b93280e832f263dbef44e44cbe2936622a02f91a` identifies `ImageSpaceBaseData` (0x50 bytes): HNAM HDR (0x24 bytes), CNAM saturation/brightness/contrast (0x0C), TNAM amount/RGB (0x10), and depth of field (0x10). Its HDR fields are eye-adaptation speed, bloom blur radius, bloom threshold, bloom scale, receive-bloom threshold, White, sunlight scale, sky scale, and eye-adaptation strength. — [ImageSpaceData.h lines 5-90](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/I/ImageSpaceData.h#L5-L90)
- `TESImageSpace` places that authored base data at +0x20. `ImageSpaceManager` publishes singleton relocation IDs `(527731,414660)`, base/override/underwater/console pointers at +0xA8/+0xB0/+0xB8/+0xC0, resolved `ImageSpaceData` at +0xC8, and `usesLDR` at +0x210. These are version-sensitive CommonLib labels, not an audited native address map for every executable. — [TESImageSpace.h](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESImageSpace.h); [ImageSpaceManager.h](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/I/ImageSpaceManager.h)

Local native corroboration, kept separate from public claims: existing [`image-space-arithmetic.md`](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/image-space-arithmetic.md), lines 29-102, derives the scalar luminance equations from private `shadersfx/shaders011.fxp` SHA-256 `72cfc73eb0938d5949f0b3a8683722a33b671a34b41b750ba138de8d154c5fbc`. Group 26 shader hash is `1def29cca7e582b149a09bd15524793593293ae2d94fc20b3d030bf9f91e39fc`; optional-fade group 27 hash is `781978201a0b01ba4107b11abbe52456b5f20b49b998f1d0ba7aa9d057133ea1`. This corroborates the old public scalar source and conflicts with the later "per-channel vanilla" label. A sibling researcher is freshly checking original package identity and byte spans; this note does not claim to have repeated that byte audit itself.

### Inferences

- Useful REA path: native `ImageSpaceManager` state, authored/resolved/override data, shader constant producers, shader/pipeline selection, frame constants, and sampled-average/adaptation producers. Then compare the actual shader constants with raw IMGS values.
- The `Param` register comments in newer mod source are useful search hints. They are not proof of a retail record-to-constant transformation.

### Gaps

- Creation Kit public wiki pages at `https://ck.uesp.net/wiki/Imagespace` and `https://ck.uesp.net/wiki/ImageSpace` returned HTTP 403. Their text was not read or used as evidence.
- Repositories `XJDHDR/TES-Shaders` and `Nukem9/SkyrimSETest` were inspected at tree level. No new matching native SSE image-space implementation was extracted from them for this note.
- REA must verify each native address against the executable version and distinguish authored IMGS settings, active modifier data, resolved manager values, and final GPU constants.

## What do the existing Photo Mode numbers capture?

### Takeaway

The public Photo Mode implementation copies the current base image-space record into an override and exposes cinematic/tint controls. Its displayed cinematic numbers do not expose all inherited HDR/adaptation settings or necessarily equal the final post-modifier GPU constants.

### Cited Findings

- `powerof3/PhotoMode` revision `bf50c35ffbf0b71a174411800e826cc40635443b` copies `*ImageSpaceManager::currentBaseData` into a local `ImageSpaceBaseData` and sets `overrideBaseData` to that structure. — [Filters.cpp lines 7-14](https://github.com/powerof3/PhotoMode/blob/bf50c35ffbf0b71a174411800e826cc40635443b/src/PhotoMode/Tabs/Filters.cpp#L7-L14)
- Its filters UI edits `overrideData->cinematic.brightness`, saturation, contrast, tint amount and tint RGB directly. It does not expose `hdr.white`, adaptation settings, bloom settings, or the shader's output exponent in these controls. It also offers image-space modifiers through a separate path. — [Filters.cpp lines 40-65](https://github.com/powerof3/PhotoMode/blob/bf50c35ffbf0b71a174411800e826cc40635443b/src/PhotoMode/Tabs/Filters.cpp#L40-L65)
- Native image-space data separates base data from modifier data and blend amounts. The modifier record supports multiply/add entries for cinematic settings and multiple HDR settings, so a base/override snapshot is not generally equivalent to final resolved parameters. — [ImageSpaceData.h lines 124-133](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/I/ImageSpaceData.h#L124-L133); [TESImageSpaceModifier.h](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESImageSpaceModifier.h)

### Inferences

- If the captured reference uses this Photo Mode behavior, the cinematic values inherit HDR settings from the current base image space when Photo Mode starts. Reading only the visible brightness/saturation/contrast/tint sliders leaves crucial tone-mapping inputs unrecorded.
- Before a faithful parity experiment, record the full base and resolved image-space structures, active modifiers, actual curve selector/parameters, adapted average xy, output exponent, and replacement-shader/mod state.

### Gaps

- This lookup did not verify the exact Photo Mode build used in the historical screenshot, its complete active mod list, active image-space modifier stack, or a runtime receipt from that capture. Current public source is a strong implementation explanation, not proof of those historical runtime facts.
- No frame was launched or captured. No live Skyrim parameter value is claimed here.
