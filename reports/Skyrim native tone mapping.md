# Skyrim’s native tone mapping is extractable

Research baseline: `100cae44018338f061e19560563d9d354b276620`, before the local
record-loader implementation. Current loader behavior and native verification
are documented in [the color-pipeline specification](../docs/specs/engine/color-pipeline.md).

Follow-up, 2026-10-15: [native parameter packing](../research_notes/Skyrim%20native%20tone%20mapping/gpu_binding_trace.md)
and [the reciprocal-gamma exponent](../research_notes/Skyrim%20native%20tone%20mapping/output_transfer_trace.md)
are now traced through exact upload/binding paths. [Runtime observations](../research_notes/Skyrim%20native%20tone%20mapping/runtime_state.md)
record live settings, resolved fields, CPU tuples, console frames, and a fresh
camera export. The report below preserves the earlier research baseline;
its claims that those static connections remain open have been superseded.

**Yes: Skyrim Special Edition’s authored HDR settings are extractable, and public research plus audited retail shaders establishes a concrete native tone-mapping model.** This investigation independently decoded the authored values, refreshed the retail shader identity checks, and recovered a useful native diagnostic through read-only Ghidra queries. That is enough to challenge or guide Mudcrab’s TonyMcMapface plus experimental sRGB grading. It does not establish the effective settings, shader selection, adaptation history, or output transfer of the reference screenshot. Preserve the approved Bevy preset while closing those specific gaps; replacing it now would exchange a known visual result for an incomplete reconstruction.

The authored controls are well defined. Pinned CommonLibSSE-NG and xEdit schemas agree that `IMGS.HNAM` contains nine floats; `CNAM` contains saturation, brightness, and contrast; `TNAM` contains tint amount followed by RGB. The current converter already retains those fields and the weather’s sunrise/day/sunset/night `IMSP` links. An independent read-only decode of the current archived database recovered these values for **`ISSkyrimClearDAY` `0x00012F88` and `ISSkyrimCloudyDAY` `0x00012F87`, both `Update.esm` winners**. Numbers below are readable roundings of binary32 values, not runtime readings. ([CommonLib, `b93280e`](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/I/ImageSpaceData.h); [xEdit, `9fb0168`](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas#L5729); [authored extraction](../research_notes/Skyrim%20native%20tone%20mapping/authored_settings.md))

| HNAM field | ClearDAY `0x12F88` | CloudyDAY `0x12F87` |
|---|---:|---:|
| Eye Adapt Speed | 45 | 40 |
| Bloom Blur Radius | 7 | 7 |
| Bloom Threshold | 0.8 | 0.8 |
| Bloom Scale | 4 | 3 |
| Receive Bloom Threshold | 0.8 | 0.7 |
| White | 1 | 1.05 |
| Sunlight Scale | 2.8 | 3 |
| Sky Scale | 0.05 | 0.16 |
| Eye Adapt Strength | 5 | 5 |

The database snapshot contains 304 IMGS records, 272 IMAD records, and 110 weather records. Its SHA-256 is `5dd8984382c04bc34ed2d1225be1cf664e5abda14c0b32e97b090395d6d0e30d`. This extraction verifies archived payloads and recorded load order; original ESM bytes were not revalidated against the screenshot installation. Clear-day values also vary across dawn, dusk, and night, so there is no single universal Skyrim HDR tuple. ([Extraction and provenance](../research_notes/Skyrim%20native%20tone%20mapping/authored_settings.md))

**Photo Mode’s visible grade omits inherited HDR state.** The pinned implementation copies the current base image space into an override, then exposes cinematic/tint controls. Its sliders do not expose HNAM or adaptation history. The supplied snapshot—saturation `1.66`, brightness `1.15`, contrast `1.50`, tint `0.56`, RGB `(0.88,0.81,0.72)`—resembles cloudy-day CNAM/TNAM, but cloudy-day saturation is `1.6`; no inspected IMGS matches the snapshot’s combined CNAM range. Numerical resemblance cannot identify active weather, interpolation, modifiers, or inherited HDR. Mudcrab’s subsequent brightness increase to `1.38` is a separate artistic adjustment. ([Photo Mode, `bf50c35`](https://github.com/powerof3/PhotoMode/blob/bf50c35ffbf0b71a174411800e826cc40635443b/src/PhotoMode/Tabs/Filters.cpp#L7-L65); [extraction](../research_notes/Skyrim%20native%20tone%20mapping/authored_settings.md); [current pipeline](../docs/specs/engine/color-pipeline.md))

The audited retail group 26, `ISHDRTonemapBlendCinematic`, supplies the following **conceptual RGB transform**. Here `A`, `B`, and `P` mean samples from texture slots 1 RGB, 0 RGB, and 2 XY. They are explanatory names, not recovered authored structures. `lerp(x,y,a)=x+a*(y-x)`; scalar terms broadcast to RGB. The exact audit preserves D3D instruction order and floating-point behavior; this summary does not promise bitwise equivalence. The source audit was recorded in a separate historical worktree and is not included in this checkout.

```text
L = (0.2125, 0.7154, 0.0721)
Y = max(dot(A,L), 0.00001); Z = Y*(P.y/P.x); k = cb2[2].y
if cb2[2].z > 0.5:
    x = max(Z-0.004, 0)
    Q = k * (x*(6.2*x+0.5)/(x*(6.2*x+1.7)+0.06))^2.2
else:
    Q = Z*(1+k*Z)/(1+Z)
C = A*(Q/Y) + B*saturate(cb2[2].x-Q)
K = dot(C,L)
G = lerp(K,C,cb2[3].x)
G = lerp(G,K*cb2[4].xyz,cb2[4].w)
G = P.x + cb2[3].z*(cb2[3].w*G-P.x)
outputRGB = saturate(G)^cb12[42].x
```

The order is luminance exposure and scalar curve, RGB reconstruction plus bloom-like input, saturation, tint using the same luminance, brightness, contrast about **sampled `P.x`**, RGB clamp, then final power. Group 27 applies fade afterward, including alpha, without another output clamp. The curve’s internal `2.2` and final exponent are separate operations. Neither establishes IEC sRGB encoding. Mudcrab’s fixed `0.18` pivot is not a measured substitute for `P.x`.

Historical Community Shaders source corroborates this scalar path and names the branches Reinhard and Hejl-Burgess-Dawson. Its named grading fields explain the readable reconstruction; they do **not** prove native HNAM/CNAM/TNAM-to-register assignments. In particular, authored `White` has not been connected to `k`, adaptation controls to coefficients, or `fGamma` to the effective final exponent. A later commit titled “per-channel vanilla” instead curves RGB independently and changes clamp/fade order, conflicting with the audited package. Current `dev` also contains explicit HDR-display, hue-preserving, contrast, and lighting changes. It cannot serve as an unqualified vanilla implementation. ([Historical shader, `d106f94`](https://github.com/community-shaders/skyrim-community-shaders/blob/d106f94a41b7db187f70bcc43ccb933a04868521/package/Shaders/ISHDR.hlsl#L41-L117); [later implementation, `a00ff9d`](https://github.com/community-shaders/skyrim-community-shaders/blob/a00ff9d3eb1397fedf4165a15581f5d3a2a68fe2/package/Shaders/ISHDR.hlsl#L99-L137); [current snapshot, `3424ae1`](https://github.com/community-shaders/skyrim-community-shaders/blob/3424ae12a5b0c10c2d45058b99d84bf30874cd71/package/Shaders/ISHDR.hlsl))

Fresh REA 6.0.0 `doctor --provider ghidra` and `inspect-artifact` succeeded. Native recovery used **direct saved-project Ghidra in read-only mode**, following an earlier REA cold-import timeout. Independent LLVM decoding corroborated all **1,399 instructions across six recovered functions**, including original PE bytes. The pinned executable is `1.7.104.0`, SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`. Fresh checks also matched eight shader slices to the prior audited package, SHA-256 `72cfc73eb0938d5949f0b3a8683722a33b671a34b41b750ba138de8d154c5fbc`. This refresh verifies identity and static evidence; **no running frame or group-26 execution was observed**. ([Native findings and receipts](../research_notes/Skyrim%20native%20tone%20mapping/native_rea.md))

A concrete next extraction path is native **`PrintHDRParam`**. Function `0x1403769C0` reads manager singleton `0x1433D40A0`, selects override base data at `+0xB0` when present, otherwise current base data at `+0xA8`, then prints HDR, cinematic, and tint fields. This independently corroborates their layout and diagnostic selection. It still omits the complete resolved modifier state, GPU packing, and temporal texture contents. Existing HDR parameter-object links likewise do not prove that native parameter channel 2 becomes shader `cb2`. ([Fresh native findings](../research_notes/Skyrim%20native%20tone%20mapping/native_rea.md); the separate ownership-limits audit is in a historical worktree and is not included here.)

The remaining work has four bounded gates. First, bind one reference capture to executable/plugin/mod hashes, weather and time blends, current/override base data, resolved manager data, and active modifiers; use `PrintHDRParam` for the selected-base receipt. Second, trace resolved fields through CPU transforms into each consumed constant, including curve selector, white-derived parameter, grading, and output exponent. Third, capture the frame’s selected shaders, constant buffers, adaptation/history texels, bloom inputs, resource formats, SRV conversions, and output encoding. Measure adaptation over time to establish coefficient units and history updates. Fourth, validate a separate native pipeline against captured inputs and intermediates, then compare matched interior/exterior scenes with camera, lighting, weather, fog, and exposure fixed. Those receipts can justify replacing the Bevy path; authored sliders or curve resemblance alone cannot. Keep Cool Gray, TonyMcMapface, and the approved sRGB snapshot available throughout. No engine changes, launches, builds, tests, commits, or publication were performed for this report.
