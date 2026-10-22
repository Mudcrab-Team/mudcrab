# Skyrim native tone mapping: REA and retail shader evidence

Follow-up: later work closes [CPU parameter packing](gpu_binding_trace.md) and
[the final exponent producer](output_transfer_trace.md), and adds
[read-only runtime observations](runtime_state.md). The findings below describe
the earlier static audit; its unresolved lists are historical.

## What did the fresh REA and native queries establish?

### Takeaway
REA is installed and usable for safe artifact inspection; its scoped Ghidra health check passes. The native analysis in this investigation used the existing saved Ghidra project directly, in read-only mode, rather than REA's cold-import analysis provider. Two successful new queries and a fresh byte check corroborate existing retail shader equations; they do not observe a running Skyrim frame.

### Cited Findings
- Installed REA is `6.0.0`. Fresh `rea doctor --provider ghidra --format json` exits 0 with `healthy: true` for the requested scope; `environment_healthy: false` reflects unrelated/informational Hopper, IDA, skill, and client-registration checks. Do not describe every provider/integration as healthy. — [Fresh doctor receipt](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/rea-doctor.json>); [REA upstream](https://github.com/morluto/rea).
- Fresh `rea inspect-artifact <SkyrimSE.exe> --format json` exits 0. REA's safe artifact graph provider identifies a PE with SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`; evidence ID `ev_6943ebb010f3b8c0e8628d22f8dd6c22c47836e9e69e12055f13d44ec30f60a9`. This inventories an artifact; it does not decompile tone mapping. — [Fresh REA artifact receipt](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/rea-inspect-artifact.json>).
- Fresh direct Ghidra `12.1.4` queries use `-process SkyrimSE.exe -noanalysis -readOnly` against saved project `SkyrimMovement17104`; query 02 completes in `6.64645` seconds, query 03 in `4.52638` seconds, both exit 0 with evidence written. Earlier full-program analysis was partial. — [Query 02 manifest](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-query-02-manifest.json>); [Query 03 manifest](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-query-03-manifest.json>).
- The pinned native executable is the previously inventoried Skyrim SE/AE `1.7.104.0` target; new REA identity and Ghidra program SHA agree with the target bytes. No compatibility claim for other executable versions follows. — [Prior target fingerprint](/Users/taylor/.local/share/mudcrab-research/rea-20261008/evidence/skyrim-source-sha256.txt); [Fresh REA artifact receipt](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/rea-inspect-artifact.json>); [Fresh Ghidra query](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-query-02.json>).
- Independent LLVM decoding of all six recovered functions in the two fresh queries agrees on `1,399` instruction starts/encodings. Every checked instruction byte agrees with the original PE file mapping. This supports static instruction interpretation, not runtime selection or physical units. — [Fresh native/LLVM verification](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-llvm-byte-verification.json>).
- First query attempt failed before analysis because a `JAVA_TOOL_OPTIONS` `user.home` path contained spaces and Java parsed `Support/MudcrabResearch/...` as a separate option. Retrying with task-owned writable no-space home fixed the launcher. Failed attempt is retained and excluded from successful-query counts. — [Failed query log](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-query-01-stdout.log>).

### Inferences
- Saved-project reuse provides a practical bounded path for further focused REA-assisted native work. Claiming that REA's decompiler recovered these functions in this turn would be inaccurate; direct Ghidra supplied the function evidence.

### Gaps
- No runtime capture, debugger attachment, game launch, target execution, shader dispatch capture, builds/tests, engine edits, staging/commits, GitHub publication, or Fiji changes occurred.
- REA's first whole-program import previously exceeded its provider startup deadline; no cold import was retried. Partial saved analysis means missing references cannot prove absence of consumers or writers.

## What tone curve and grading arithmetic exists in the audited retail package?

### Takeaway
The pinned retail package contains a luminance-based tone map with two scalar curve alternatives, bloom, saturation/tint, brightness/contrast about an adaptation-texture value, RGB clamp, and a final power exponent. Its exact arithmetic already existed in the research corpus; this turn freshly rehashed the package and eight relevant shader slices against that corpus. This is an audited retail shader path, not proof that a given screenshot selected it.

### Cited Findings
- The current private package `shaders011.fxp` is `67,768,128` bytes and SHA-256 `72cfc73eb0938d5949f0b3a8683722a33b671a34b41b750ba138de8d154c5fbc`, matching the existing audited equation catalog. Eight serialized VS/PS slices for groups 26, 27, 32 and 34 match their expected bytecode hashes. Original binaries and shader disassembly remain outside the repository. — [Fresh package and slice verification](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/retail-bytecode-verification.json>); [Existing equation catalog](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/image-space-arithmetic.json).
- Group 26 `ISHDRTonemapBlendCinematic`, key `00000000`, PS offset `67,512,936`, length `1,876`, hash `1def29cca7e582b149a09bd15524793593293ae2d94fc20b3d030bf9f91e39fc` has 49 executable instructions. Group 27 fade PS hash is `781978201a0b01ba4107b11abbe52456b5f20b49b998f1d0ba7aa9d057133ea1`. Existing derivation separately compared shader operands and literal bits against fresh disassembly and source package bytes. This turn verified identity, not a new independent operand derivation. — [Fresh slice verification](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/retail-bytecode-verification.json>); [Existing derivation provenance](/Users/taylor/.local/share/mudcrab-research/render-map-20261010/shaders/image-space-arithmetic/derive-provenance.json); [Readable audited equations](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/image-space-arithmetic.md).
- Shader-side symbols below are descriptive only: `A` is sampled scene RGB at texture slot 1, `B` sampled bloom-like RGB at slot 0, `P` sampled slot-2 `xy`, `L=(0.2125,0.7154,0.0721)`, `k=cb2[2].y`. No effective numeric inputs or complete texture-domain/SRV contracts were observed. Exact arithmetic uses D3D float operations; these simplified formulas are explanatory and do not promise bitwise-equivalent reassociation. — [Audited equations and limitations](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/image-space-arithmetic.md); [D3D floating-point rules](https://learn.microsoft.com/en-us/windows/win32/direct3d11/floating-point-rules).

```text
Y = max(dot(A, L), 0.00001)
R = P.y / P.x
Z = Y * R

if cb2[2].z > 0.5:
    a = max(Z - 0.004, 0)
    Q = k * ((a*(6.2*a + 0.5)) / (a*(6.2*a + 1.7) + 0.06))^2.2
else:
    Q = Z*(1 + k*Z)/(1 + Z)

rgb = A*(Q/Y) + B*saturate(cb2[2].x - Q)
K = dot(rgb, L)
graded = lerp(K, rgb, cb2[3].x)
graded = lerp(graded, K*cb2[4].xyz, cb2[4].w)
contrasted = P.x + cb2[3].z*(cb2[3].w*graded - P.x)
outRGB = saturate(contrasted)^cb12[42].x
```

- The scalar `Q` applies to luminance and returns to RGB by `Q/Y`; this package path does not independently tone-map R/G/B. The final RGB clamp occurs before the final power. In group 27 the fade happens after that power and includes alpha, without another output clamp. — [Group 26–27 audited arithmetic](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/image-space-arithmetic.md).
- The contrast pivot in the actual operand chain is `P.x`, sampled from texture slot 2. Treating that as a fixed `0.18` is unsupported. Similarly, the `2.2` inside one tone-curve branch and the independent final `cb12[42].x` power are separate operations; neither alone proves the complete display transfer. — [Group 26–27 audited arithmetic](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/image-space-arithmetic.md).
- Groups 32/34 share adaptation arithmetic: `delta=A.xy-P`, `rate=delta*[cb2[2].w,cb2[2].z]`, `step=min(abs(delta), max(abs(rate), 1/256))`, `candidate=P+sign(rate)*step`; output `xy` falls back to previous `P` if any accumulator RGB component is NaN. Their CPU coefficient writers and units remain unclosed. — [Audited downsample/adaptation equations](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/image-space-arithmetic.md); [Fresh group 32/34 slice verification](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/retail-bytecode-verification.json>).

### Inferences
- The formula resembles a white-scaled Reinhard branch and a second filmic/HBD-style rational branch, but names do not select which branch runs. The shader's `k` need not equal the authored IMGS white field directly; the CPU might transform it.
- A Bevy TonyMcMapface output followed by fixed-pivot grading is not mathematically interchangeable with this combined retail shader path.

### Gaps
- Runtime-selected group/key, curve selector, actual sampled adaptation texture contents, exposure ratio, bloom producer/input domain, final exponent, surface formats/SRV conversion, blending, and whole-frame pass order were not observed.
- Parent public researcher reports later Community Shaders sources call a per-channel implementation “vanilla”; that conflicts with this package's audited scalar luminance arithmetic. Earlier public reconstruction corroborates the scalar path. Source version and retail bytecode must take priority over an unqualified “vanilla” comment. See the public researcher's pinned-source notes for URLs and revisions.

## How far can authored ImageSpace settings be connected to native runtime state?

### Takeaway
A fresh native console-print function confirms that Skyrim exposes HDR and cinematic data through the manager's current/override base-data pointers, with the known authored field offsets. The path from selected/blended ImageSpace data into every shader constant and adaptation input remains incomplete; do not substitute static record fields or Photo Mode sliders for effective shader inputs.

### Cited Findings
- Fresh function `0x1403769C0` reads manager singleton pointer `0x1433D40A0`, chooses manager `+0xB0` if non-null, otherwise `+0xA8`, then prints fields at offsets `+0x00`, `+0x20`, `+0x04`, `+0x08`, `+0x0C`, `+0x10`, `+0x14`, `+0x18`, `+0x1C`. Literal strings name eye-adaptation speed/strength, bloom fields, target luminance and sunlight/sky. This confirms the selected base-pointer diagnostic path, not resolved shader state. — [Fresh query 03, console-print seed](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-query-03.json>); [Native/LLVM verification](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-llvm-byte-verification.json>); [String inventory](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/string-inventory.json>).
- In the same function, native instructions `0x140376AA1`–`0x140376AD7` load base-data `+0x28` as the second value for `Saturation / Brightness / Contrast`, `+0x24` as the first, and `+0x2C` as the third; it separately loads `+0x30`, `+0x34`, `+0x38`, `+0x3C` for tint printing. This corroborates CNAM saturation/brightness/contrast and TNAM layout without assuming their final shader register packing. — [Fresh query 03](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-query-03.json>); [Independent instruction verification](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-llvm-byte-verification.json>).
- Existing RTTI-linked `ImageSpaceEffectHDR` constructor `0x14153C9B0` and setup `0x14153CAB0` are freshly recovered. Setup places 64-byte block `0x1420D7410` into parameter channel 2 via `0x14152B1B0`, then associates it with a child shader through `0x141554150`. Query 02 independently re-finds the original block references. The existing corpus identifies group-29 downsample ownership, but does not prove this channel equals `cb2` in the selected shader. — [Fresh query 02](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-query-02.json>); [Existing native ownership map](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/native-image-space-connections.md).
- Follow-up of additional block references finds setup/binding routines `0x141550D20` and `0x141583FA0`. Those uses do not establish effective HDR numeric writers. The first was requested with a provisional `hdr-parameter-writer` query label; the recovered body constructs parameter links, so that label must not be promoted into a semantic conclusion. — [Fresh query 03](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-query-03.json>).
- Fresh executable string inventory contains `fConstHDRAdaptTimerForMenu:Display`, `fGlobalEyeAdaptSpeedScale:Display`, `fGlobalEyeAdaptStrengthScale:Display`, `fGamma:Display`, `SetHDRParam`, `PrintHDRParam`, `ToggleHDRMultipleLuminance`, and HDR downsample/adaptation/tonemap class names. The chosen saved database has no recovered incoming xrefs for several setting-name strings; that is incomplete analysis, not proof of unused settings. — [Fresh string inventory](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/string-inventory.json>); [Fresh reference query](</Users/taylor/Library/Application Support/MudcrabResearch/tonemap-20261015/native-query-02.json>).

### Inferences
- Native `PrintHDRParam` is a useful future diagnostic for the manager's selected base data. Its preference for override/current base pointers means it still does not expose every modifier, temporal adaptation value, sampled history texel or final constant-buffer transformation.
- Existing ownership/reflection metadata and the native manager layout provide a finite next investigation: find the writer of the effective ImageSpaceData fields, then trace their stores into the exact constant-buffer components and texture inputs consumed by groups 26/27/32/34.

### Gaps
- Not closed: weather/time interpolation -> selected base/IMAD/console/underwater overrides -> manager resolved data -> CPU transforms -> exact constant upload/SRV generation -> selected retail dispatch.
- No conclusion that ClearDAY, CloudyDAY, or any other IMGS was actually selected for the comparison screenshot. Rounded Photo Mode values and numerical resemblance cannot establish record identity.
- No claim that authored adaptation speed is seconds, authored strength is exposure stops, white is `1/white^2`, the final exponent equals `1/fGamma`, or a particular branch is active until its CPU producer and effective runtime state are established.
