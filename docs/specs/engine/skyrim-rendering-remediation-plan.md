# Skyrim rendering remediation and validation plan

Status: stage A implemented and checked within its recorded limits, 2026-10-10. Target: vanilla Skyrim SE/AE, copied executable
1.7.104.0 and its pinned assets. This plan coordinates converter, material,
lighting, visibility, fog and image-space work. It does not mark the rejected
candidate, the brighter recovery checkpoint or any current rendering family as
retail-accepted.

The [stage A implementation receipt](../../research/skyrim-rendering-remediation-stage-a-20261010.md)
records checkpoint tooling, capture receipts, source-qualified diffuse publication
and the bounded output trace. R1's full draw/stage contract and R2's retail
observations remain open; implementation checks do not close them.

The [imported reference audit](../../research/skyrim-reference-import-20261010.md)
adds 78 screenshots and 84 fitted poses. Edition, environment and exact camera
receipts remain incomplete; its three small comparison views support qualitative
review without closing V7.

The objective is to reproduce Skyrim's selected rendering operations, effective
inputs, resource state and displayed result. First establish a reproducible
ordinary-surface-to-output case, then extend the same contracts to terrain,
interiors and specialized families. A scoped correction can proceed when its
own evidence closes; it need not wait for all rendering domains.

The [lighting hub](lighting-and-shading.md) owns current integration and launch
routes. The [native rendering map](../../research/skyrim-render-map/README.md)
owns original-byte evidence, open queries and runtime observations. The
[lighting/shadow contract](skyrim-lighting-and-shadows.md),
[image-space contract](image-space.md) and
[fog contract](distance-fog.md) retain their detailed equations and tests.
Where an older contract makes a stronger claim, reconcile its source receipts
with the newer map before implementing or downgrading that claim.

## 1. Starting state and evidence rules

The map currently contains 45 byte-checked native queries, 370 independently
decoded functions and 85,200 original instruction starts. All 135 package
groups have conditional native loader identities; 54 groups have complete
scoped image-space equations. **All 45 full input-to-pixel domains remain open,
and no matched retail frame has been captured.** Those counts describe static
coverage, not active retail execution.

| Finding | Evidence grade | Consequence |
| --- | --- | --- |
| Runtime accepts source-qualified native DDS views, but ordinary conversion publishes semantic transfer views and its dependency collector reads only `uri`. | Confirmed project discrepancy; scoped original texture-transfer evidence. | Publish and validate native views durably; verify their selected retail use separately. |
| Lighting Effect 1/2 survive NIF parsing but are omitted from material projection. | Confirmed data omission. | Preserve the values now; map their selected shader use before adding response. |
| Native preview uses generated tangent frames, one shared seven-light list and unoccluded local visibility; fallback uses different light units and selection. | Confirmed project boundaries; complete native producers remain open. | Recover per-draw inputs and qualify response paths separately. |
| Native ambient uses affine rows while fallback uses row constants; some interior and sun inputs remain approximate. | Confirmed project approximation. | Trace effective source/parent/room state before replacing it. |
| `--no-fog` also removes geometry clamps/scaling; enabling image space changes upstream normalization and presentation. | Confirmed combined interventions. | Existing toggle pairs cannot isolate fog or tone mapping. |
| Final tone-result transfer is explicitly a hypothesis in current code. | Unresolved output contract. | Trace the complete target/view-to-present chain before choosing a different default. |
| Terrain, water, unsupported materials, directional shadows and several effects use incomplete or approximate routes. | Declared implementation boundary. | Identify actual drawn routes; a loaded-material count cannot prove native coverage. |
| Lux/mod documentation and pinned rendering-mod source identify useful questions. | Authored-content claims or mod interventions; no acquired mod archive/runtime proof. | Use them to target native queries, never to supply vanilla coefficients. |

This table records the pre-stage-A discrepancies. Durable native diffuse
publication, native dependency validation and raw Lighting Effect preservation
are now implemented and fixture-checked. Their selected retail use remains open.

The dark, saturated and glowing captures show regressions but do not attribute
them. The brighter checkpoint preserves a useful comparison; it is not a
measured vanilla brightness target. The old L0 images are qualitative references
because camera, weather and adaptation metadata are incomplete.

The [CK technical-notes comparison](../../research/skyrim-lighting-notes-comparison-20261010.md)
records current consumer gaps and their scene scope. Before the next source-fix
comparison, verify native-view declarations in the actual requested pack and
their selected bindings. The preserved producer-26 pack remains a control;
producer-27 converter tests do not establish its migration or activation.

Incoming research is retained in the [note corpus](../../research/skyrim-render-map/research-notes/README.md),
with stable claims, source hashes, code anchors, conflicts and next proof questions.
The [lighting/color model](../../research/skyrim-render-map/research-notes/world-model.md)
connects those questions to the existing native map. Notes can add investigation
work; changing rendering still requires the owning-layer proof and validation
gates below. Use Luna for delegated note tracing and retain the primary review.

Record missing effective values as unknown. In particular, do not rename
`0x14331D6F0` as additive ambient or assign zero without its writers and object
meaning; do not equate native parameter channel 2 with shader `cb2`; and do not
turn a shader name, compiled initializer or INI request into active state.

## 2. Dependency order and first implementation slice

The [interactive dependency view](skyrim-rendering-remediation-plan.html)
shows parallel work and the exit condition for each phase.

| Order | Work that can run together | Required before integration |
| --- | --- | --- |
| A | R0 checkpoint identity; R1 coherent capture diagnostics; R2 retail session/output tracing; R3 source-data publication. | Diagnostics and preservation integrate on their own gates. A retail correction needs captured routes and original inputs for its declared scope. |
| B | R4 output/numeric domains; R5 effective lighting; R6 authored material response. Shadow and special-family investigation can continue alongside them. | The affected source-to-stage connection closes and its independent probe passes. |
| C | R7 atmospheric/HDR composition; R8 visibility/terrain/interiors; R9 remaining families and temporal effects. | Each change consumes qualified domains and the relevant per-view/per-draw state. |
| D | R10 combined visual, lifecycle, hardware and user-test acceptance. | Required cases pass separately; unknown or unsupported cases remain listed. |

For the first slice, use clear-noon Riverwood terrace and material closeup:
identify one ordinary wood/stone primitive, one terrain or fallback primitive
and the output chain. Retain the street shot as a whole-view regression control.
Collect source identities, pre-fog response, post-fog scene, reduction/history,
bloom, graded output and presented values. Find the earliest divergent stage.
Do not compensate for it at a later stage.

The first implementation changes are R0/R1 instrumentation and R3 preservation.
The first visual correction is whichever scoped producer or consumer R2/R4
proves wrong. This ordering does not assume gamma, ambient, fog, subsurface
response or shadows caused the regression.

## 3. Remediation work packages

### R0 — Preserve controls and identify candidates

Keep the hash-checking
[brighter-checkpoint launcher](../../../scripts/skyrim-world-lighting-baseline.sh)
and its binary/assets intact. Preserve current rejected candidates separately.
Snapshot HEAD, the tracked patch and hashes of untracked source files: HEAD
`f40ca1df770eab4ace83b335b6b38168bd83af60` alone cannot identify this dirty
worktree. Include Cargo.lock, shader hashes, converter contract, asset manifest,
source winners, settings and shot fixture.

Build each later candidate under a new private `CARGO_TARGET_DIR`; capture into
new directories. A manifest describes every input and output hash. Never reuse
a receipt after changing its inputs. The existing recovery receipt establishes
selected-region replay, not whole-frame equality.

**Exit:** checkpoint and candidate can be replayed without overwriting each
other; changed inputs are detectable. Artifact presence and hashes must be
verified during implementation, not inferred from the launcher.

### R1 — Capture the pixels and state of one rendered frame

Extend [shots](../../../crates/engine/src/shots.rs),
[GPU readiness](../../../crates/engine/src/render.rs),
[lighting reports](../../../crates/engine/src/lighting_runtime.rs) and
[image-space diagnostics](../../../crates/engine/src/image_space.rs).
Publish an immutable per-shot sidecar that joins the screenshot, intermediate
readbacks, effective constants and resources to one actual rendered frame/view.
Distinguish requested, submitted and readback-completed frames.

Add drawn primitive/material/path identity, not just loaded candidates. Support
arbitrary sample coordinates/regions and floating-point stage dumps. Current
Stage A diagnostics cover eight fixed points with GPU-reported coordinates.
Capture readbacks use request IDs; periodic readbacks use per-view identities.
The actual screenshot GPU copy/transfer identity remains unobserved, so the
image-frame join stays pending despite the passing request/view/diagnostic join.

Add diagnostic controls that independently isolate fog blending, geometry
finishing, direct/ambient/specular/emission, visibility, adaptation, grading,
bloom and output conversion. These are proposed controls, not existing CLI
flags. Freeze all other resource bytes, constants and history during replay.
[Profiling metadata](../../../crates/engine/src/profiling.rs) now records actual
target dimensions. Complete the effective render-scale and counter evidence;
unavailable counters remain unavailable.

**Exit:** a deliberate stale-frame, wrong-view, reused-resource-generation or
required-native/fallback mismatch fails capture validation. Images, state and
readbacks share verified frame/view/resource identities. Numerical reproduction
of those pixels belongs to V4/V5 after the affected contracts close.

### R2 — Establish retail observations and close the selected trace

Verify a runnable game session on the existing Fiji installation or another
explicitly identified compatible host, preserving active workloads. Record
executable/package hashes, launch/API/translation route, loaded modules,
plugin/resource winners and effective INIs. The 2026-10-10 read-only Fiji probe
matched the executable and shader archive to the pinned local files by hash and
size. A read-only followup resolved Steam's selected `GE-Proton` tool to the
installed GE-Proton11-5 entrypoint. Game startup and the capture interface remain
unverified.

Obtain original selected D3D11 shader/state/constant/resource receipts with
verified suitable instrumentation. A translated capture needs both original
binding identity and translated representation. Validate the capture route
before expanding the matrix. RenderDoc's documented backend support includes
D3D11 on Windows and Vulkan on Windows/Linux; it does not provide a Metal route.
For Mudcrab, choose tooling for its actual backend, or use owned readbacks.
Tool availability alone does not establish usable retail capture.
[RenderDoc backend documentation](https://github.com/baldurk/renderdoc/blob/v1.x/README.md)

Follow selected source fields through native producers, upload/remapping,
selected shader operands and downstream consumers. Trace actual target/view
formats, alpha/blend state and every copy/tone/UI/present transfer. Reuse the
persistent partial Ghidra project; corroborate each native claim against
original bytes and independent decoding.

**Exit:** the first declared case has a closed, scoped source-to-output trace
and at least three unchanged retail captures. If session/instrumentation remains
unavailable, independent probes and source preservation can continue, but
retail acceptance stays pending.

### R3 — Publish source-qualified textures and retain material fields

Owners: converter
[material](../../../crates/converter/src/material.rs),
[texture](../../../crates/converter/src/texture.rs),
[mesh](../../../crates/converter/src/mesh.rs), cache, pipeline, check/repair,
shared contracts and runtime
[material validation](../../../crates/engine/src/nif_material.rs).

Publish `nativeUri`, `nativeSampleTransfer`, `nativeSourceFormat` and
`sourceDdsSha256` from the winning DDS source, separately from PBR semantic
views. Preserve qualified legacy UNORM and explicit supported DX10 sRGB cases;
untraced formats remain gated. Enumerate both views in dependency validation,
pruning, repair, manifests and cache identity. Retain raw Lighting Effect 1/2
and authored frame components without assigning unproved response semantics.

Version affected contracts only after deciding the actual migration. Prove
incremental invalidation, missing-source behavior and clean regeneration on
small fixtures before refreshing the full pack. Metadata-only refresh must not
pretend to add missing mesh attributes or shader support.

**Exit:** source values/block or decoded-sample identities survive the declared
conversion; native/PBR views remain independent; missing required publication
fails closure; alpha/normal data remain unchanged. The wrong-transfer negative
control must fail. Source publication can pass before retail use is observed.

### R4 — Correct numeric domains and final output ownership

Owners: [environment render state](../../../crates/engine/src/environment_render.rs),
color pipeline, material adapters, image-space Rust/WGSL and target setup.
Recover the actual domain at each texture sample, light/material response,
fog operation, HDR intermediate and final target/view. Count transfer, exposure,
scale, power and clamp operations explicitly for each route.

The `12000/pi` and fallback `pi/12000` conversions are project adapters;
they are not retail photometric measurements. Changing image-space routing also
changes those inputs. Test native, terrain/fallback, emission, sky and reflection
through the same established output owner; fix producer/consumer pairs together.
Do not select `--image-space-output display-encoded` or
`--image-space-output linear` from visual preference.

**Exit:** captured ramps and HDR samples match each recorded intermediate and
the selected final transfer. Duplicate decode/encode or exposure controls fail.
Each route has one documented display owner; reflection stays in its qualified
scene domain until composition.

### R5 — Resolve effective sunlight, ambient and local lights

Owners: environment/preview, lighting catalog/settings/runtime, shared lighting,
[lights](../../../crates/engine/src/lights.rs) and native material uniforms.
Recover active sun identity, RGB/dimmer/SunlightScale, direction, parent
transform and exceptional branches. Recover affine ambient rows, extra RGB
writers and per-field CELL/LGTM/room inheritance, including explicit zero.

Replace the shared camera-nearest seven-light preview with the observed
per-draw light list, ordering, radii/fades/dimmers and shadow-channel assignment.
Reconcile fallback's separate 64-light, sRGB/inverse-square route. The selected
ordinary shader's seven-local bound, shadow budgets and Lux/mod limits are
different contracts.

**Exit:** cardinal/diagonal normal probes, parent transforms, sun/weather/time
changes, room boundaries and oversubscribed-light fixtures reproduce effective
operands and selection. Ambient/emission remain independent of direct-light
visibility unless the selected original path proves otherwise.

### R6 — Qualify authored frames and material response by permutation

Owners: NIF parsing, converter mesh/material contracts, native material support,
native color/prepass/shadow WGSL and GPU material probes.
Close source stream allocation/stride/offset ownership before evaluating the
recovered packed T/B/N decode. Start with rigid, positive uniform scale; qualify
mirrored UVs, negative/nonuniform transforms, rebasing, skinning and morphing
separately. Do not substitute generated MikkTSpace for an authored frame.

Trace flags, source texture slots, cache packing, technique key, constants and
state for diffuse/specular/emission, alpha and soft/rim/back branches. Implement
each supported permutation independently. A proved packed cache slot is not
automatically a NIF source-slot contract.

**Exit:** source components and independently derived normal/response samples
match; asymmetric normal maps expose frame errors; color/prepass/shadow alpha
silhouettes agree. The selected soft directional addition is tested at its
actual shadow/composition position. Unsupported permutations fail strict
coverage rather than silently using ordinary response.

### R7 — Reproduce fog, sky, HDR and history composition

Owners: environment/fog/sky, native and fallback fog shaders, image-space
resources/graph/WGSL and settings. Recover per-view projection, near/far planes,
clipping, fog/VOLI state and pass ownership; current shared material fog uses
world-camera planes, including reflection paths.

Validate distance fog blending separately from geometry clamps/framebuffer
scaling, fog/beam geometry, VOLI and sky. Preserve depth, alpha/discard,
opaque/transparent ordering and clear-depth behavior. Derive weather/time/room
interpolation from effective producers rather than creating appearance presets.

Close IMGS and ordered IMAD activation/evaluation before enabling transitions
currently rejected by the stable resolver. Match reduction extents, sampler
state, adaptation initialization/update/reset, history ping-pong, bloom and
Cinematic/Fade. Quantize at every observed intermediate format, including
R11G11B10_FLOAT or RGBA16_FLOAT branches. Preserve the traced StageC graph:
its third helper receives the first helper's saved target, not the preceding
helper's return. Do not infer parameter-to-buffer mapping from equal numbers.

**Exit:** frozen-input stage replay and temporal sequences match the selected
resources/branches; zero fog blend can retain independent finishing; clamp,
bloom and output operations remain in their proved positions. Main/reflection
views cannot consume stale or shared wrong-view state.

### R8 — Replace provisional visibility, terrain and interior behavior

Owners: shadow producer/settings, caster participation, terrain/LOD materials,
render readiness, light lists, rooms/portals and capture navigation.
Trace native shadow allocation, range/focus/cascades, bias/filtering and receiver
sampling. Separate authored mountain NIFs from LAND/terrain and LOD. Current
8,000-unit/2,048-map preview settings and template `bDrawLandShadows=0` do not
prove native effective caster rules.

Recover terrain layer/atlas/morph/fade response and AO input/composition
ownership. Add actual interior loading/navigation and room-boundary capture;
`--fog-interior-cell` only selects fog data. Separate room visibility,
light selection and light shadowing. Preserve off-camera required casters.

**Exit:** per-light visibility affects assigned terms only; caster/receiver,
alpha cutouts, mountain/terrain/LOD, cascade and room transitions match their
receipts. Rebase/unload/reload/slot reuse cannot resurrect stale state.
Budget exclusions are deterministic and reported; required-case exclusions fail.

### R9 — Close specialized families and optional effects

Keep an explicit ledger for skin/face/hair/eyes, trees/grass, snow, parallax,
environment maps, water/reflection/underwater, particles/effects/decals, SAO,
VOLI, TAA, motion blur, depth of field and UI. Each row needs source selection,
producer, shader/state, resource ownership and temporal/reset tests.

Investigate Skyrim's material-specific soft/rim/back response and SnowSSS
separately. Group 123's eleven-vector snow arithmetic does not establish general
skin diffusion; Community Shaders' screen-space scattering is an added mod
implementation. Do not enable blanket transmission to fill dark surfaces.
Lux/Orbis authored meshes, placements, portals and fades guide missing-case
research; acquired versioned content diffs would be a separate evidence step.

**Exit:** each declared enabled family has its own closed contract and probes.
A proved disabled branch can close that disabled profile only. Optional mod
behavior uses a separate manifest/target; no mod coefficient is promoted to
vanilla. Full rendering parity remains pending while required families are open.

### R10 — Integrate, review and promote a candidate

Run the gate sequence below for the declared supported case set. Compare
intermediates first, then matched retail output, then whole views and temporal
behavior. Keep the brighter checkpoint and previous candidates for comparison.
Report build, numeric, capture, visual, retail and performance verdicts
separately. Change the user-test default only after required checks and visual
review; retain the explicit rollback route.

Extend the acceptance runner to consume an exact rendering-profile manifest,
record its hash in each run/baseline and verify required active branches and
draw coverage. Include lighting mode, local lights, fog/image space, weather,
hour and effective settings. The current campaign defaults to approximate
world rendering; preserve it as regression coverage and add the remediation
profile campaign. Its pass cannot measure an unexercised native/HDR path.

**Exit:** the evidence package names supported cases, first-divergence results,
remaining gaps, hardware and review. A better-looking scoped candidate is not
labelled full Skyrim parity.

## 4. Validation gates and required receipts

| Gate | Pass condition | Cannot substitute |
| --- | --- | --- |
| V0 Identity | Binary/source/patch/assets/settings/fixture hashes identify a replayable case. | HEAD or a filename alone. |
| V1 Capture coherence | One immutable render-frame/view receipt joins state, resources, readbacks and image; readiness passes. | Latest asynchronous report or PNG existence. |
| V2 Native contract | Original bytes, producer, selected operands, state and consumer close for the declared slice. | Shader inventory, possible call or mod reconstruction. |
| V3 Data/selection | Winning source fields and overrides survive conversion and reach the correct owner. | Retained metadata without effective selection. |
| V4 Isolated response | Independent expected values agree with production GPU samples under declared format/error bounds; relevant negative controls fail. | A reference that repeats the implementation's mistakes. |
| V5 Composition/history | Selected resources, branch, order, intermediate output and next consumer agree, including resets. | A plausible final image. |
| V6 Visibility/lifecycle | Light channels, casters, receivers, transitions and generations follow observed rules. | Turning off costly or darkening features. |
| V7 Retail appearance | Matched repeated retail captures and predeclared thresholds pass for every required case. | Old L0 images or unmatched screenshots. |
| V8 Functional/performance | Scoped functional checks and the target-hardware campaign pass independently for the exact profile and verified active branches. | Approximate-mode timing, windowless capture FPS or software GPU timing. |
| V9 Promotion | Required gates, visual review, limitations and rollback are recorded for the candidate. | Synthetic checks promoted to whole-game parity. |

The full capture sidecar contract remains a required target. The first implementation
covers request/view/diagnostic joins and selected arithmetic readbacks. The private
screenshot GPU copy identity is unobserved, so the image-frame and overall joins
remain pending. Arithmetic diagnostics precede FXAA and are not production stage
dumps. Missing draw, source-winner, binding and stage evidence stays explicit.
The full contract must contain:

- Run, case, shot and view IDs; requested/submitted/rendered/readback frames;
  real/game time, fixed-step policy and readiness result.
- Executable/source/patch/lockfile, GPU/driver/API, target dimensions/render
  scale, output/present mode, asset/plugin/archive/loose winners and settings.
- Camera/worldspace/CELL/room, projection/clipping/jitter/origin; current/next
  weather and weights, CLMT/IMGS/ordered IMAD/VOLI, water and history/reset state.
- Draw/dispatch owner and technique/hash; primitive/material/source identities;
  selected native/fallback route; stream layout/stride and blend/depth/raster state.
- Resource/view formats and IDs, allocation/update generations, extents,
  mip/slice/sample count, bindings/samplers and exact constant bytes/remapping.
- Stage readbacks with format/domain/coordinate, image hashes, comparison
  regions, threshold provenance and independent gate verdicts.

Cross-frame history links are explicit. A missing or inconsistent required
field prevents V1/V2/V7 for that case; it must not be filled from a default.

### Numeric and causal tests

Preserve exact source integers, binary32 components and packed constants where
the contract requires them. Floating-point GPU expectations account for
instruction order, multiply-add/transcendental rules and intermediate format
rounding. Declare operation-specific bounds before evaluating a candidate;
existing fixture tolerances do not become retail whole-image limits.
[D3D11 floating-point rules](https://learn.microsoft.com/en-us/windows/win32/direct3d11/floating-point-rules)

Sampler/view state is part of an expected sample, including filtering, address
modes, LOD and swizzle. Record it rather than guessing from the image.
[Shader-model sample semantics](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/sample--sm4---asm-)

Use independent decoded-equation/CPU oracles, then production GPU probes, then
selected retail draws, then whole scenes. Freeze observed upstream inputs for
stage ablations. The existing fog × image-space comparison is a four-route
integration control, not a causal fog/tone experiment.

Required negative controls include wrong texture transfer, duplicate output
conversion, generated-for-authored frames, ambient multiplied by direct
visibility, wrong local shadow channel, soft response at the wrong shadow
position, opaque cutout shadows, stale resource/history generation, wrong-view
fog constants and simplified StageC routing. A probe that passes its relevant
negative control cannot establish the invariant.

Predeclare wood, stone, roof, foliage, terrain, sky and mountain regions.
Capture at least three unchanged retail baselines and before/after controls per
varied input. Derive appearance thresholds from measured repeatability before
scoring changes. Use pre-tone floating-point samples for radiance ratios;
PNG metrics describe displayed contrast unless inversion is verified. Record
domain, animation/jitter policy, silhouette, hue/clipping, leakage and temporal
stability. No invented universal SSIM or whole-scene pixel threshold applies.

### Scene matrix

| Cases | Required evidence / current prerequisite |
| --- | --- |
| Clear noon and late afternoon, Riverwood terrace/street/material closeup | Three existing exterior poses; ordinary materials, terrain, sky and mountains. Match retail camera/FOV/state first. |
| Overcast, dawn/dusk and night | Resolve actual winning weather IDs, sun/ambient and selected IMGS; do not invent presets. |
| Dense fog and weather transition | Current/next weather, interpolation, opaque/geometry fog, VOLI, translucent ordering and sky/clear depth. Transition evaluator needs qualification. |
| Interior and adjacent-room crossing | CELL/LGTM/room inheritance, explicit zeros, portals, local shadows and emissive fixtures. Current shots runner skips interiors. |
| Material swatches and original assets | Tangent/model-space, asymmetric normals, mirrors/nonuniform/rebase/deformation, diffuse/specular/emission/soft/rim/back and alpha. |
| Terrain, mountains, LOD, trees/grass/cutouts | Near/far material response, morph/fade/wind, cast/receive, off-camera casters, shadow silhouettes and contact. |
| Visibility/AO settings | Change one observed resolution/range/filter/bias/normal-source consumer; record direct and ambient separately. |
| HDR and modifiers over time | Bright-to-dark/dark-to-bright, first frame, entry/exit/reset, frozen/adapting history, IMAD order, odd target sizes and format branches. |
| Water, reflection and underwater | Per-view projection/state, qualified scene-domain resources, ownership, transitions and one final display transform. |
| Specialized/temporal/UI families | Skin/face/hair/eyes, snow/parallax/environment, particles/decals, TAA/motion blur/DoF and UI when enabled; unimplemented required rows remain pending. |

The windowless runner writes an owned `Rgba8UnormSrgb` target. It requires
ten quiet frames after a two-second warmup, plus streaming and current GPU
color/prepass/shadow readiness. The Riverwood fixture has a 90-second budget.
A timeout can write a diagnostic image but fails the run. Preserve these gates.
Shots whose worldspace differs from the selected run, and interior shots,
currently skip; a required skipped row cannot pass. Its 16 ms runner interval
makes it unsuitable for performance acceptance.
See [reference-shot behavior](reference-shots.md).

## 5. Implementation-phase command recipes

These commands are prepared routes, not execution results of this plan.
Prerequisites include compatible assets, cached locked dependencies for offline
builds and a usable GPU. Capture commands remain readiness/regression evidence
until R1/R2 supply coherent retail/state receipts. Choose a new run directory.

```sh
validation_root=/Users/taylor/.local/share/mudcrab-research/lighting-remediation-20261010
export CARGO_TARGET_DIR="$validation_root/build"

cargo build --offline --locked --profile quick -p engine --bin engine
cargo build --offline --locked --profile quick -p engine --examples
```

Run relevant tests for the changed stage, not every probe after every edit:

```sh
cargo test --offline --locked -p engine --lib fog::tests
cargo test --offline --locked -p engine --lib image_space::tests
cargo test --offline --locked -p engine --lib lighting_catalog::tests
cargo test --offline --locked -p engine --lib lighting_settings::tests
cargo test --offline --locked -p engine --lib nif_material::tests
cargo test --offline --locked -p engine --lib shots::tests
cargo test --offline --locked -p engine --test fog_gpu -- --ignored --nocapture
```

For source publication and migration changes:

```sh
cargo test --offline --locked -p converter --lib material::tests
cargo test --offline --locked -p converter --lib texture::tests
cargo test --offline --locked -p converter --lib mesh::tests
cargo test --offline --locked -p converter --lib cache::tests
cargo build --offline --locked --profile quick -p converter --bin converter
"$validation_root/build/quick/converter" check "$validation_root/new-pack" --full
```

The final command requires the new pack to have been generated and identified.
It checks manifest file existence, size and hashes. Stage A also checks qualified
native KTX2 shape, format, transfer and distinct contained files, plus authored
diffuse-view coverage for current meshes. Original source/binding closure and
selected retail use remain separate audits. Neither successful hashing nor a
test filter with zero matched cases can satisfy the migration gate.

Existing response and composition probes:

```sh
"$validation_root/build/quick/examples/native_lighting_probe" --output "$validation_root/probes/native-lighting" --native-image-space-input
"$validation_root/build/quick/examples/image_space_probe" --output "$validation_root/probes/image-space"
"$validation_root/build/quick/examples/image_space_scene_probe" --output "$validation_root/probes/image-space-scene" --diagnostic
"$validation_root/build/quick/examples/image_space_scene_probe" --output "$validation_root/probes/image-space-packed" --packed
"$validation_root/build/quick/examples/fog_material_probe" --output "$validation_root/probes/fog-materials"
```

Retain the alpha, normal, specular, emission and color-output probes and their
negative controls. Add authored-frame, soft-response, graph and capture-coherence
fixtures as their contracts close. A required packed-format test fails when its
format is unsupported; an RGBA fallback cannot satisfy that format gate.

Replay the brighter checkpoint:

```sh
MUDCRAB_BASELINE_OUTPUT="$validation_root/captures/brighter-checkpoint" \
  ./scripts/skyrim-world-lighting-baseline.sh \
  --shots crates/engine/tests/fixtures/lighting-riverwood-shots.json \
  --shots-out "$validation_root/captures/brighter-checkpoint"
```

Capture a clear-noon candidate:

```sh
MUDCRAB_WORLD_BINARY="$validation_root/build/quick/engine" \
MUDCRAB_WORLD_OUTPUT="$validation_root/captures/candidate-clear-noon" \
MUDCRAB_RENDER_OUTPUT_TRACE=1 \
  ./scripts/skyrim-world-lighting-preview.sh \
  --game-hour 12 --fog --image-space \
  --image-space-report "$validation_root/captures/candidate-clear-noon/image-space-gpu.json" \
  --shots crates/engine/tests/fixtures/lighting-riverwood-shots.json \
  --shots-out "$validation_root/captures/candidate-clear-noon"
```

Use separate output directories for `--no-fog` and late-afternoon controls.
For the photographic route, use the preview launcher without
`--image-space`; there is no `--no-image-space` option. The baseline launcher
always enables image space. These route comparisons do not freeze upstream
response; use R1 stage replay for causal tests.

For static evidence verification:

```sh
python3 scripts/check-skyrim-render-map.py \
  --target /Users/taylor/.local/share/mudcrab-research/rea-20261008/targets/SkyrimSE.exe \
  --shader-archive '/Users/taylor/.local/share/mudcrab-research/rea-20261008/retail/fiji-skyrim/Data/Skyrim - Shaders.bsa' \
  --native-evidence /Users/taylor/.local/share/mudcrab-research/render-map-20261010 \
  --native-output-evidence /Users/taylor/.local/share/mudcrab-research/lighting-remediation-20261010/native-output \
  --project-source-evidence /Users/taylor/.local/share/mudcrab-research/lighting-remediation-20261010/checkpoints/source-before-aq14hktq/workspace
```

The checker verifies public/private static receipts and identities; it does not
execute Skyrim or establish active resources or visual parity.

## 6. Hardware acceptance, rollback and reporting

Use release builds for performance, identical hardware/drivers/resolution,
settings, scenario paths and durations, with three repetitions. Measure CPU
environment/streaming work, GPU response/shadows/AO/postprocess, draw/caster
counts, I/O, target memory and temporal growth separately. Record actual
dimensions and presentation state. Mac Metal functionality and appearance
receive their own verdict; they do not satisfy the Windows hardware campaign.

Retain the existing [Phase 2 campaign](../../roadmap/02-acceptance.md):
average FPS at least 60, frame P95 at most 16.67 ms, memory growth at most
0.5 GiB and zero streaming failures. Regressions above 5% warn and above 10%
fail after its documented absolute noise floors. Preserve its required quality,
robustness, release-build, asset-closure, minimum-duration and visual-signoff
gates. A quick or software-GPU result proves plumbing/function only.

Apply those policies to the exact-profile runner described in R10. Baseline
compatibility must include rendering-profile identity and required branch
coverage, not only hardware. Do not compare approximate and native/HDR timings
as an unchanged-profile regression.

Every correction's evidence package includes source/contract scope, native
receipts, independent numeric/GPU results, settled captures, retail comparisons,
temporal/lifecycle results and hardware verdicts. Give each gate
`pass`, `fail` or `pending`, with the failed or missing artifact identified.
Report the first divergent stage and leave unsupported families visible.

If a candidate regresses, preserve it and return user testing to the explicit
checkpoint launcher; do not overwrite history or fit a global brightness,
saturation, ambient, emission or shadow-opacity coefficient. Keep the default
route unchanged until the candidate passes its required gates and visual review.

The initial planning checkpoint added documentation only. Stage A subsequently
implemented and checked the bounded instrumentation, publication and static
trace recorded in its receipt. Full capture identity, retail execution, visual
corrections and appearance acceptance remain open.
