# L1 material and color pipeline

Tracks [L1 #131](https://github.com/Mudcrab-Team/mudcrab/issues/131), under [vanilla lighting parity #129](https://github.com/Mudcrab-Team/mudcrab/issues/129). L0 reference capture and L1 implementation can proceed concurrently. L0 evidence gates retail parity acceptance, not synthetic tests or implementation.

## Scope and evidence

Default target: unmodded Skyrim SE. Interior and exterior cases have equal priority. Mod research informs native behavior; addon lighting remains out of scope. L1 slices fix output-domain inconsistencies and static emission publication, with controlled probes. They do not establish Skyrim's image-space equations or select its final exposure/tone curve.

Code baseline: `a9f2310ccfc691eebb97fde18df1e8d334b7d744`; Bevy 0.19. The source trace below describes actual runtime behavior. Claims in older sky notes about encoded weather interpolation and fog equations still require the L0/L2 retail evidence; this change preserves those inputs.

## Pipeline trace and ownership

| Stage / owner | Input → output | Current limit / next owner |
|---|---|---|
| `converter/src/texture.rs`, `material.rs` | DDS channels → semantic KTX2 transfer format; diffuse/glow use sRGB aliases, normals/data use linear views | Shared source bytes may have distinct views; preserve alpha as data. Existing round-trip tests cover encoded channels. |
| `converter/src/material.rs` | Validated per-shape NIF values → glTF factors, extensions, source extras | #81: specular enable, normal-alpha mask and gloss exponent approximation. #82: static glow-slot eligibility and emission energy corrected; animated emission remains unsupported. #83: alpha/UV. #84: effect/editor surfaces. Remaining issues stay open; emission animation is outside #82. |
| Bevy glTF / `StandardMaterial` | sRGB textures decoded once; material factors and data textures remain linear → material response | Bevy's PBR BRDF is an approximation, not recovered Skyrim shading. Tangent/model-space normals require distinct treatment. |
| `shaders/terrain.wgsl` | Linear layer samples and normal data → weighted material response → PBR lighting | Authored normal conventions, layer semantics and specular response remain separate material probes. |
| Bevy `pbr_functions.wgsl` | Lights + material → exposure-scaled linear RGB | `Exposure.ev100` uses Bevy's `2^-EV100 / 1.2`. Current 9.7 is pinned from the prior default, not a Skyrim value. Emission's exposure weight is material-owned; default emission is exposure-independent. |
| `sky.rs`, `shaders/sky.wgsl` | Encoded weather-row mixing → one sRGB decode → linear palette brightness | Existing palette is a fixed clear-day approximation. Sky/fog palette values already occupy the composition domain; do not multiply them by camera exposure a second time. L2 owns authored state and its units. |
| `DistanceFog` | Exposed surface RGB + linear fog palette → fogged linear RGB | Existing exponential/Fog Far approximation stays explicit. Interior fog removal is current behavior, not complete Skyrim interior semantics. L2 owns the correction. |
| `render.rs` reflection camera | Scene at main-view exposure → `Rgba16Float`, tone mapping disabled | Floating linear target preserves values above 1. Reflection gate copies exposure even while inactive. Existing reflected layers and geometry selection remain unchanged. |
| `shaders/water.wgsl` | Exposed water lighting + exposed linear reflection → blend → surface fog | Reflection receives neither a second lighting evaluation nor a display transform. Existing Fresnel/waves and reflection coverage are approximations. |
| `color_pipeline.rs` / scene cameras | HDR composition → one full-view `TonyMcMapface` transform → display encoding | Includes sky, background, fog, opaque and transparent surfaces. Tone curve remains a pinned diagnostic baseline pending IMGS evidence; L4 owns adaptation/record-driven image-space behavior. |

The previous non-HDR mesh path tone-mapped in each material shader. The custom sky shader bypassed that operation, and reflection RGB passed through tone mapping in both the reflection and water passes. HDR composition moves the transform after composition without adding a new shader implementation. It changes images and consumes more render-target memory; target-hardware performance remains an acceptance requirement.

## Invariants

- V1: Every production scene camera and existing visual fixture uses explicit `SceneColorPipeline`: HDR, EV100 9.7, TonyMcMapface. These values remain provisional; defaults are not evidence of vanilla parity.
- V2: Reflection storage preserves linear values above 1; no tone map or sRGB target view before water sampling. Reflection exposure matches the main camera before rendering, including after exposure changes while water is invisible.
- V3: Sky, unlit mesh, fully fogged mesh, terrain emission and unit-reflecting water given equal composition-domain RGB produce matching output within 2/255 per channel. Exterior includes sky; interior has a black background. Test neutral gray and saturated HDR inputs.
- V4: Diagnostic inputs, camera and output settings, samples and verdict are recorded. Probe failure returns a nonzero status; stale reports are removed at startup. Synthetic consistency is not retail parity.
- V5: Preserve NIF source values and declared unsupported families. Do not compensate for pending material errors with global tint, exposure, ambient or emission changes.
- V6: Static emission → authored linear tint × multiplier × eligible glow sample; preserve zero, dim, HDR and black tint. Slot 2 glow ! Glow shader or Glow_Map; Own_Emit alone ≠ texture eligibility. Overflowing energy → contextual conversion error.
- V7: Converter schema 17 → rebuild GLBs from schemas 12–16; reuse verified unchanged texture/script/archive outputs only with matching source/configuration. Runtime accepts complete schemas 15–17; world database schema unchanged.

- V8: Finite signed NIF tint ! convertible; glTF nonnegative projection retains raw signed color/multiplier and names lower-clamp approximation. Positive channel energy follows V6; signed shader parity remains gap.

## Supported response and remaining material work

| Family / path | Current representation | L1 acceptance status |
|---|---|---|
| Ordinary lighting / opaque | glTF `StandardMaterial`, PBR lighting | Output consistency can be tested now; NIF specular fixes pending #81; static emission corrected by T6. |
| Alpha-tested / blended | glTF alpha modes + existing prepass | #83 and PR #99 remain separate dependencies. Do not declare caster silhouettes accepted before they are integrated and tested. |
| Tangent-space normal maps | Linear normal samples through glTF; terrain has its own tangent frame | Direction, handedness and specular-alpha probe still required. |
| Model-space normals | No established compatibility path in this slice | Named L1 gap; generic tangent interpretation cannot count as acceptance. |
| Environment-map / parallax / skin / hair / other NIF lighting variants | Raw source contract/extras plus generic approximation | Runtime compatibility inventory and per-family probes still required; retained metadata alone is not shader support. |
| Effect shader surfaces | Converted material approximation | #84 visibility and shader semantics unresolved. No general effect parity claim. |
| Water / sky | Custom Bevy shader paths | Output-domain test only; authored behavior and full-scene parity remain open. |

## Verification

- `cargo test -p engine --lib`: explicit scene settings, production reflection format, reflection exposure lifecycle, existing renderer/sky regressions.
- `cargo run -p engine --example color_pipeline_probe -- --output <dir> [--interior] [--gray]`: real GPU shader path and pixel comparison; uses headless GPU readback and requires a Vulkan adapter with at least 32 sampled-texture and sampler slots. The probe requests WebGPU features with terrain limits explicitly raised; it does not benchmark the full production device feature set. Software Vulkan is sufficient for functional validation, not performance acceptance.
- Run all four combinations: exterior/interior × gray/HDR. Each writes `probe.png` and `probe.json`. The probe fixes 800×600, orthographic camera `(0,0,10)`, EV100 9.7, TonyMcMapface, no dither/MSAA, full diagnostic fog on one swatch, unit reflectivity on water, and constant sky rows. All are isolated diagnostic settings, not shipping values.
- The probe uses the production reflection allocation and camera setup; its unlit source geometry, render layer and visibility are diagnostic. A clear-only reflection would not exercise material tone mapping. The HDR case includes blue = 2.0 to detect clipping and duplicate display transforms.
- `--legacy-output` is a negative control: restore the previous non-HDR cameras and 8-bit reflection target inside the probe. It must fail the consistency check, with a nonzero status and saved pixel differences. This option does not exist on the game CLI.
- Complete L1 acceptance additionally needs NIF-to-runtime material probes, integrated dependency fixes and matched vanilla neutral/material captures from L0. Leave #131 open until those gates pass.

## Static emission publication

[Emission issue #82](https://github.com/Mudcrab-Team/mudcrab/issues/82): `Own_Emit` declares own emittance; `Glow_Map` declares third-slot glow (`vendor/project-wormhole-nif/src/nif_flags.rs`). Glow shader type also permits slot 2. Own_Emit alone retains slot 2 as unclassified source data; no emissive texture sampling.

Publication: `peak = max(1, max(emissive_color))`; `emissiveFactor = max(0, emissive_color) / peak`; `emissiveStrength = emissive_multiple * peak`. glTF factor remains in [0,1]; reconstructed linear RGB retains nonnegative authored energy. Extension emitted whenever strength ≠ 1, including 0 and values below 1. Black tint stays black with glow present. Bevy 0.19 glTF loader multiplies factor by strength into `StandardMaterial.emissive`; glow uses sRGB decode once, alpha does not scale opaque emission. No camera/ambient compensation.

Signed NIF tints remain valid. Negative channels retain previous glTF lower-clamp approximation; raw color/multiplier retained under `extras.openSkyrim.sourceEmission`, with explicit representation label. Signed-emission shader behavior remains unsupported; this projection does not claim Skyrim parity. Strength overflow → contextual conversion error. Source contract retains original valid values. Existing negative-multiplier controller endpoint handling unchanged; animation and shader-family compatibility remain L1 gaps. Contributor reference inspected at `BimingtonBill/wah-krah-jol:ff96ac2b91bec0765d9ce59b2890449923f9d3ed`; emission publisher there retains old defects, so this slice uses current material owner directly.

`material_emission_probe --output <dir> [--interior] [--legacy-emission]`: converter-published synthetic NIF contracts → glTF/KTX2 → Bevy loader → GPU swatches. Nine cases: zero, dim, unit, HDR, dim HDR, black glow, untextured HDR, Own_Emit atlas, signed tint. Loaded factors checked against authored energy; glow view ! `Rgba8UnormSrgb`. Converted swatches compared with independently computed material RGB at tolerance 2/255; zero cases ! black, other references ! visible. 800×900, orthographic camera `(0,0,10)`, no lights/ambient/fog/dither/MSAA, pinned scene tone map/exposure. Interior/exterior here change diagnostic background only; no authored scene parity claim. Synthetic contract publication ≠ full NIF-file parse coverage. `--legacy-emission` restores old energy/eligibility defects inside probe and ! fail with exit 1. Every run records PNG, JSON, generated glTF/KTX2; removes stale verdicts before startup.

Converter cache schema 17: schemas 12–16 retain verified non-GLB entries/archive ingestion only when source and original configuration hash match; GLBs/world data rebuilt. Configuration changes still invalidate cache. Stage journal schema check rejects old staged GLBs. Runtime accepts complete converter schemas 15–17; world database schemas 3–4 and cell-cache version unchanged. Launcher requires latest converter schema.

For testing, reconvert to separate output directory with converter built from this branch, then run matching engine against that directory. Existing packs remain valid in engine but retain old emission until reconverted. Preserve old pack for rollback; older #137 engine rejects schema 17, so use new engine for new pack. Retail reconversion ! isolated matching package; delivery evidence recorded after successful conversion/startup.

Verification: `v6_emission_preserves_zero_dim_hdr_and_black_glow_energy`, `v6_own_emit_does_not_enable_slot_two_glow`, `v6_rejects_overflowing_emission_with_context`, `v8_signed_tint_keeps_source_and_clamps_only_gltf_negative_channels`, `recent_schema_migrations_reuse_only_unchanged_asset_kinds`, `v7_schema_16_rebuilds_meshes_and_reuses_compatible_assets`; engine runtime-schema acceptance tests; both probe backgrounds plus legacy negative control. Full converter/engine library suites, formatting and Clippy required before publication.

## Local verification, 2026-10-02

Headless Vulkan on llvmpipe / Mesa 26.2.2, LLVM 21.1.8. No renderer errors in the six final runs. These are functional shader checks, not target-hardware performance or Skyrim visual acceptance.

| Case | Largest channel difference (8-bit) | Expected verdict |
|---|---:|---|
| Exterior, HDR `(0.18, 0.4, 2.0)` | 1 | Pass |
| Interior, HDR | 1 | Pass |
| Exterior, gray `(0.18, 0.18, 0.18)` | 0 | Pass |
| Interior, gray | 0 | Pass |
| Previous output path, HDR | 39 | Fail (negative control) |
| Previous output path, gray | 3 | Fail (negative control) |

HDR mesh/sky/fog/water samples: `(114,151,239)`; terrain: `(114,152,239)`. Previous-path sky: `(118,170,255)`; water: `(107,136,200)`. All new-path gray samples: `(115,115,115)`. Interior background: black. Pixel tolerance was fixed at 2/255 before running; the negative controls returned exit code 1.

Static emission probe: exterior and interior both pass with maximum difference 1/255; all eight loaded material checks pass. Legacy negative control returns exit 1, maximum error 230/255, seven loaded-energy/eligibility checks fail. Zero/black/Own_Emit cases render `(0,0,0)`; dim `(68,21,73)`, unit `(123,49,130)`, textured HDR `(237,145,203)`. 353 converter tests pass (13 existing ignores); 230 engine library tests pass; formatting and converter/engine Clippy libraries/tests/examples pass with warnings denied. Same llvmpipe adapter as above. Evidence: `/home/dev/Projects/mudcrab-lighting-emission-evidence/{exterior,interior,legacy}`; functional synthetic proof, not Fiji performance or vanilla retail acceptance.

Signed-tint correction: 18 failing retail NIFs → 18 converted, zero skips; isolated private-fixture run. Nine-case exterior/interior GPU probes pass ≤1/255; raw signed metadata and nonnegative glTF projection checked. Full converter suite: 354 passed, 13 existing ignores; formatting and Clippy pass. Delivery/full-pack integration passed; T7 complete.

## Fiji delivery, 2026-10-02

Package: `/home/taylor/mudcrab-pr139-8db2a3d`; binaries ! commit `8db2a3d3ef07ef6dd92ad19f0c30d0c4c065da2e` (later documentation-only commits do not change deployed binaries). Separate schema-17 pack: complete; 18 converted, 257849 cache hits, zero skipped; inputs DDS 35663 / NIF 25388 / PEX 15162. World schema 4 integration passes; missing/invalid models 0. Converter quick check passes: 76213 files, 13.1 GB. Package hashes and bundled runtime dependency resolution pass.

RX 6700 XT / RADV NAVI22 / Mesa 26.2.2: nine-case exterior/interior probes both pass, maximum difference 0/255. Legacy control fails with exit 1, maximum error 231/255. Exact packaged `run-riverwood.sh --headless` exits 0 and records `OpenSkyrim runtime initialized`. Existing #137 package and original schema-16 asset pack preserved; original manifest/database/cell-cache hashes unchanged.

Evidence: package `DEPLOYMENT.json`, `conversion-report.json`, `material-probe/{exterior,interior,legacy}/probe.json`, `asset-check.log`, `smoke-new-assets.log`; local copies under `/home/dev/Projects/mudcrab-lighting-emission-evidence/signed-fix/`. Installed source NIFs stay private. Synthetic GPU/startup proof ≠ matched Skyrim scene acceptance or release performance; L0 comparison and remaining L1 families remain open.

## Tasks

id|status|task|cites
T1|x|Trace existing color/material owners and name unsupported paths|V5
T2|x|Use explicit HDR scene composition and linear reflection storage; synchronize exposure|V1,V2
T3|x|Render paired synthetic probes and record pixel evidence|V3,V4
T4|.|Integrate existing material/prepass/sampler fixes, add converted-NIF response probes|V5
T7|x|Restore signed-tint NIF compatibility; verify retail reconversion and delivered pack|V5,V6,V8
T6|x|Fix static emission publication; load converted materials and compare GPU swatches; migrate cache|V5,V6,V7
T5|.|Compare both scene types against L0 references; accept declared tolerances|V3,V5

## Bugs

id|date|cause|fix
B1|2026-10-02|Non-HDR mesh shaders tone-map while sky bypasses transform|V1,V3
B2|2026-10-02|Reflection is display-mapped before water applies another transform|V2,V3
B3|2026-10-02|Xvfb launch lacked `libxkbcommon-x11` runtime path; local software Vulkan rejected optional features; baseline WebGPU limits excluded terrain bindings|Headless readback; explicit 32 texture/sampler slots; no game renderer fallback change
B4|2026-10-02|Probe used unboxed `WgpuSettings` for Bevy 0.19 `RenderCreation::Automatic`|Box settings; compile-only correction, no new invariant
B5|2026-10-02|Own_Emit enables slot-2 glow; multiplier floor, HDR clipping and white fallback alter authored emission|V6,V7
B6|2026-10-02|Probe assumed glTF material handles were StandardMaterial in Bevy 0.19|Load production PBR /std labels; compiler catches type mismatch; no new invariant
B7|2026-10-02|Schema bump changes pinned manifest configuration hash|Snapshot diff reviewed: schema 17 and corresponding configuration hash only; V7
B8|2026-10-02|Rejecting signed NIF tint applies glTF domain to source data; Fiji reconversion skips 18 base/DLC/CC assets|V8; retain source tint; glTF lower clamp remains named approximation
B9|2026-10-02|Narrowed overflow regression left single-element test loop|Clippy catches mechanical shape; remove loop; no new invariant
