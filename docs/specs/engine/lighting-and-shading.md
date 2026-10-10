# Lighting and shading

This is the current integration plan for Skyrim lighting, material response,
shadows, fog and image-space processing. The reference is vanilla Skyrim SE/AE;
recovered native operations are pinned to the copied `SkyrimSE.exe` 1.7.104.0
and its recorded shader package. Implementation checks and visual captures do
not establish a matched retail image.

The [central native rendering map](../../research/skyrim-render-map/README.md)
tracks frame dispatch, resources, all selected package and executable-embedded
shader programs, authored/runtime inputs and the remaining proof required for
each connection. It separates byte-checked static behavior from effective retail
state. Its open connections must not become implementation defaults.

The [remediation and validation plan](skyrim-rendering-remediation-plan.md)
sets the implementation order, owning files, capture contracts, scene matrix
and promotion gates. It starts with coherent frame receipts and source-data
preservation, then corrects each stage against its selected original inputs.

The [stage A implementation receipt](../../research/skyrim-rendering-remediation-stage-a-20261010.md)
records the first diagnostic and source-preservation changes. New conversion
uses producer **27** and records independent source-qualified native diffuse
views plus raw Lighting Effect 1/2 bits. The existing lighting test pack remains
producer **26**, with its original manifest intact. Supported capture routes now
emit immutable per-image request/view/diagnostic receipts; the private screenshot
GPU transfer and full image-frame join remain pending. Arithmetic readbacks
precede FXAA and do not dump the production output. The bounded
[native output trace](../../research/skyrim-render-map/native-output-transfer.md)
connects target 0's cached views to the swapchain under its recorded guards;
effective output arithmetic and retail pixels remain unverified.

The [reference import audit](../../research/skyrim-reference-import-20261010.md)
records the new screenshot/pose archives and three qualitative comparison
fixtures. They include a byte-matched pre-SE Inn image and edition-unverified
references. Their incomplete camera/environment state supplies no defaults or
matched SE/AE acceptance.

The [technical-notes comparison](../../research/skyrim-lighting-notes-comparison-20261010.md)
checks CK authoring claims against the current data and rendering consumers.
It records missing interior, local/FX and room behavior, preserves the distinction
between template controls and material specular, and separates those findings
from the rejected exterior images. The instrumented reference replays used the
preserved older pack; new converter publication is not asset activation.

The [research-note corpus](../../research/skyrim-render-map/research-notes/README.md)
preserves incoming notes and links stable claim IDs to native, shader, record,
mod-source and project evidence. Its [lighting/color model](../../research/skyrim-render-map/research-notes/world-model.md)
tracks ownership between these layers. Model dependencies are investigation
questions; they do not supply a recovered frame sequence or visual defaults.

## Consolidation and package contract

On 2026-10-09, this worktree incorporated the uncommitted rendering work from
thread `68505759-f655-46eb-a471-4f079d8efff2`, whose HEAD was `c39449b5`.
The receiving worktree's HEAD was `f40ca1df`. This combined the source snapshot
with the receiving thread's fog, image-space and exterior-shadow corrections;
it did not merge every commit from the source branch. Original research and
source-thread capture results remain historical evidence with their own
executable, package and settings identities.

The combined package uses converter metadata **26**, world database schema
**9**, lighting source contract **1**, and cell-cache version **3**. The refreshed
world pack retains the source thread's compatible producer-25 native mesh
outputs. Metadata refresh does not add missing native surface contracts to an
older GLB. Producer identity alone does not prove shader support; closure and
material-contract validation still apply.

After verifying original model bytes, metadata refresh can correct a native
texture view's availability when both the original DDS inventory and source VFS
prove its source absent. Authored material fields and mesh data stay unchanged.
The current manifest records derived model hashes; rebuild provenance keeps
original hashes and identifies every changed view. Missing publication of a
present source texture fails dependency validation. See the
[database schema](../converters/db-schema.md) for the correction contract.

The published correction covers nine views in eight models. Mesh BIN data and
authored material fields remain exact. Rewriting JSON normalizes 37 diagnostic
`sourceFrame.bitangentStored` decimals while preserving their exact binary32
values; provenance records that precision boundary.

Current local artifacts are staged under:

- World assets: `/Users/taylor/Projects/mudcrab/modern_assets_lighting_test`.
- Combined binaries and capture output:
  `/Users/taylor/.local/share/mudcrab-research/lighting-consolidation-20261009/`.

Build and capture verification is recorded separately for the combined snapshot.
Source-thread test results cannot substitute for these checks. Raw retail
executables, extracted shader programs and native instruction listings remain
in private research storage; repository documents contain interpreted behavior,
source identities and evidence limits.

## Implementation ownership

| Area | Current contract | Detailed reference |
| --- | --- | --- |
| Authored records and environment | Winning record provenance, ordered DALC, per-field interior inheritance, active climate/time intervals and explicit unresolved values | [Lighting runtime](skyrim-lighting-runtime.md), [database schema](../converters/db-schema.md), [environment preparation](../../research/skyrim-environment-preparation-20261009.md) |
| Ordinary native materials | Native diffuse/specular, normal-alpha mask, model-space response, alpha comparisons, retained dependencies and reported unsupported families | [Lighting and shadow contract](skyrim-lighting-and-shadows.md), [color pipeline](color-pipeline.md) |
| Direct-light visibility | Bevy directional shadow producer, source cast/receive participation and per-view caster readiness | [Lighting and shadow contract](skyrim-lighting-and-shadows.md), [exterior shadow audit](../../research/skyrim-exterior-shadow-audit.md) |
| Distance fog and sky | Recovered authored inputs and supported geometry/fullscreen paths; reflection preserves scene color before display processing | [Fog specification](distance-fog.md), [fog implementation](distance-fog-implementation.md), [sky](sky.md) |
| HDR and image space | Stable authored IMGS, recovered reduction/adaptation, bloom, Cinematic/Fade and exclusive final tone mapping | [Image-space specification](image-space.md) |
| Capture and diagnostics | Owned image targets, streaming plus current GPU color/prepass/shadow readiness, explicit fallback reports | [Visual testing](skyrim-lighting-visual-testing.md), [reference shots](reference-shots.md) |

World lighting and fog use synchronized weather/hour controls. The world lighting
resolver uses the active `ResolvedEnvironment` climate/GMST time intervals when
available; a standalone scene retains the documented reference timing fallback.
Stable IMGS sunlight scale is applied once to authored native sunlight. Native
material response uses unexposed response units through the recovered fog and
image-space stages when supported IMGS is active. The photographic adapter is
retained for the optional Bevy presentation. Terrain, water and unsupported
surfaces use Bevy lighting proxies, normalized by `pi/12000` exposure while
supported IMGS is active in authored preview. Enabling image space gives that path
exclusive tone mapping through `Tonemapping::None`; the ordinary photographic
adapter remains available for comparisons.

The composed visual fixture retains its chosen lighting, fog and display
presentation. Its settings are useful for inspecting material response and
must not become implicit world defaults.

## Current launch controls

The latest combined texture/sun candidate is rejected for appearance. The
earlier brighter build is available as an explicit recovery checkpoint:

```sh
./scripts/skyrim-world-lighting-baseline.sh
```

This launcher pins the earlier executable and original asset manifest, selects
clear noon weather, and enables fog and image space. It retains the earlier
native output and fallback exposure together. Oversaturation and clipped
highlights remain unresolved; the user's preference for its brightness does
not establish acceptance of the complete image. This recovers a frozen test
route without reverting the current renderer source or overwriting research
artifacts.

The [recovery record](../../research/skyrim-lighting-regression-recovery-20261010.md)
identifies the normalization boundaries and the fresh baseline replay. Seven
primary near/sky rectangles reproduce the earlier pixels exactly; distant
geometry differs, so the full image is not claimed identical.

Subsequent candidates must be compared against this checkpoint and a retail
reference before becoming the user-test route. An individually recovered native
operation or a passing equation test does not establish an improved combined
image. Keep changes isolated and retain both captures when appearance regresses.

The older exploratory launcher remains available:

```sh
./scripts/skyrim-world-lighting-preview.sh
```

The world launcher selects Riverwood, clear weather `0x81A`, hour 17, authored
preview materials and local lights. Distance fog starts disabled for lighting
inspection. The default shadow range is **8,000 Creation units** with a
**2,048** map, independent of streaming radius. The older 18,000/4,096 visual-test
INI is retained as an explicit fixture; the world launcher does not import it.

`--no-fog` currently bypasses geometry fog finishing, including source clamps
and framebuffer scaling, in both native and fallback materials. Fog-free
comparisons are diagnostic; the behavior of a retail fog-disable flag remains
unresolved.

```sh
./scripts/skyrim-world-lighting-preview.sh --game-hour 12 --fog --image-space
```

`--weather` and `--fog-weather` select the same weather;
`--game-hour` and `--fog-hour` select the same hour. Repeated options apply in
argument order. `--fog` restores source-derived distance fog; `--image-space`
enables the recovered stable-record HDR path. Lighting and fog reports are
written to the selected world output directory. Extra options follow launcher
defaults, and environment variables can override the binary, assets and output.
Missing executables produce an isolated build instruction instead of rebuilding
the shared target.

## Combined verification

The 2026-10-09 combined snapshot passed 1,064 selected Rust tests, strict Clippy checks,
64 Metal probe cases and the explicit fog GPU comparison. Existing ignored tests
and the converter fixture requiring a case-sensitive filesystem remain recorded
in the private validation report. The source snapshot port audit accounts
for all 70 source-thread changes.

The published pack passed the converter's full 76,213-file hash check and an
independent 50-gate audit. That audit covers 96,909 typed lighting records,
75,073 bit-preserving environment records, 80,213 runtime files and 112,830
external dependency checks. Of 4,673 LOD chunks, 4,670 retain exact source bytes;
three were regenerated after deterministic winning LAND selection. Original
source inventories and hashes remain unchanged.

The merged engine captured all three Riverwood lighting poses at 1920×1080 on
Metal, with every streaming and GPU readiness gate satisfied. The first terrace
view needed roughly 38 seconds to settle. The fixture now declares a 90-second
budget; omitted budgets still use 30 seconds, and a timeout still fails the run.
Earlier timeout captures remain diagnostic evidence.

The same terrace also settled with source-derived fog and stable-record image
space enabled. Both runs use clear weather `0x81A`, hour 12 and the same camera
pose; the combined run selects `ISSkyrimClearDAY` and reports the recovered fog
owner and exclusive HDR composition.

Private `validation-progress.json`, `final-asset-verification.json`, binary
identity records and per-run `argv-provenance.json` files record the commands,
hashes and results under the artifact root above. These checks establish
integration and capture readiness. Matched vanilla visual parity remains open.
The fog-free views have weak direct contrast; the combined HDR view has bright
roof highlights and deep foliage shadows. Coarse distant terrain is visible in
both. Source light units, additive ambient and scene exposure remain unresolved.

## Saturation investigation, 2026-10-10

Four matched noon Riverwood captures of the previous build control fog and
image space separately.
Fog already reduces bright-material chroma and output endpoints: with HDR
active, roof pixels with a channel at 255 fall from 77.82% to 18.08%. The same
fog increases chroma in selected shaded wood and distant mountain regions.
It does not act as a uniform desaturation stage. These are integrated toggle
comparisons: `--no-fog` also bypasses geometry clamps/framebuffer scaling, and
HDR also changes sunlight scaling and presentation.

`ISSkyrimClearDAY` retains authored saturation 1.5, tint amount 0.42, brightness
1.135 and contrast 1.4. Tint follows saturation; its coefficient on the original
colour deviation is `1.5 × (1 − 0.42) = 0.87` before brightness, contrast and
clipping. That coefficient does not predict final perceptual saturation.

The native path had retained the photographic adapter when IMGS was active:
`(12000/pi) × cameraExposure` is approximately 3.827 at EV100 9.7. Applying that
factor before native fog/clamps changes their nonlinear arithmetic. Supported
world IMGS now selects native output scale 1 with camera exposure bypassed.
Authored grading and recovered fog/HDR equations are preserved. Runtime reports
record the selected scale, exposure flag and Bevy fallback conversion.

Correcting native output alone darkens buildings against the still bright
terrain. The fallback conversion now uses exposure `pi/12000`, equivalent to
EV100 approximately 11.636, in both world and reflection views. Bevy's diffuse
reference under the project's 12000-lux sun then has normalized response 1;
the explicit `12000/pi` ambient proxy scale cancels too. This is a project unit
conversion, not a measurement of retail light intensity or a replacement for
native terrain/water shading. Default emission and unlit effects, sky and fog
already bypass camera exposure. The photographic preview restores EV100 9.7
when supported IMGS is unavailable.

The normalized candidate also has a matched HDR pair with fog enabled and
disabled. Fog reduces roof chroma from 0.0392 to 0.0207 and foreground grass/soil
chroma from 0.0414 to 0.0293, while increasing shaded wood from 0.0114 to 0.0241
and distant mountain chroma from 0.00561 to 0.04234. These are final captured
pixel measurements, including clamps, adaptation and display grading; they do
not isolate a single fog equation. Both captures settle with no pending GPU
draw dependencies, at the same pose, noon clear weather and asset manifest.

The candidate removes the roof endpoint excess and the bright terrain mismatch,
but remains too dark for visual acceptance. Whole-frame mean linear luminance
falls from 0.1666 to 0.0416; absolute chroma falls while mean HSV saturation rises.
This is a verified input conversion, not an accepted oversaturation or retail
parity fix. The separate test executable is
`/Users/taylor/.local/share/mudcrab-research/saturation-20261010/bin/engine-tested`;
the previous world-preview launcher binary is retained.

Validation includes 443 engine library tests, native material GPU readback at
multiple camera exposures, and colored/gray diffuse-reference checks through
mesh, terrain, reflected water, fog and sky. The reference paths agree within
1/255. Restoring the old EV100 9.7 fails the three lit paths while fog and sky
still pass. These checks verify project units and output ownership; they do
not establish retail light intensity or visual parity.

This corrects a native input-domain mismatch. Retail light producers, live
ambient, volumetrics, modifier composition, retail texture/target transfer and approximate
terrain/water/fallback responses remain acceptance work. Any claim that one of
those effects compensates for the remaining appearance needs further evidence.
Controlled captures and measurements are stored privately under
`/Users/taylor/.local/share/mudcrab-research/saturation-20261010/`.

## L0 visual comparison, 2026-10-10

The [L0 comparison](../../research/skyrim-l0-visual-comparison-20261010.md)
retrieves all 18 original screenshots from issue #130: 12 exterior and 6
interior Steam Skyrim SE references. Their camera, weather, in-game time and
adaptation history were not recorded. They support qualitative material and
shadow review; they do not establish an exposure or fog-density target.

Riverwood references show readable shaded wood and stone beneath bright roofs.
The normalized candidate has broad dark wall and foliage regions in both fog
states. Its matched fog/HDR pair shows fog lifting selected shaded wood while
lowering roof luminance, without restoring reference-like wall detail. Other
retail views show strong blue distant haze. Missing clouds and coarse distant
surfaces remain independent scene differences.

The current preview places the noon sun at the zenith, giving ideal vertical
walls zero direct diffuse contribution. Verify sun direction, directional
ambient, native light producers and texture/output transfer separately before
changing brightness or saturation. The comparison records concrete controls;
authored fog and IMGS coefficients remain unchanged.

## Brightness and native texture sampling, 2026-10-10

The normalized build remains too dark for visual acceptance. The earlier bright
build supplies a useful appearance reference, but its light scales changed
before fog, clamps, adaptation and bloom. Restoring those scales would change
several causes at once; authored saturation remains unchanged during the input
investigation.

The [native DDS trace](../../research/skyrim-native-texture-transfer-20261010.md)
identifies an independent discrepancy: ordinary legacy DXT1/3/5 diffuse textures
reach Skyrim's shader through UNORM views. The current pack's semantic PBR views
decode sRGB before native lighting. Nine original Riverwood DDS sources and
their 18 canonical/alias KTX2 payloads were verified against the retained blocks.
An explicitly typed DX10 sRGB source retains its decode; source transfer cannot
be inferred from the converted KTX2 header alone.

The runtime now accepts source-qualified native diffuse views with separate
asset identities. `nativeSampleTransfer`, `nativeSourceFormat`,
`sourceDdsSha256` and `nativeUri` select an explicit native loader view while
preserving the PBR handle. Unknown, incomplete or conflicting provenance rejects
native selection. Older unannotated assets retain their compatibility view.
That scoped frozen-pack test preceded Stage A. Normal converter publication now
integrates native qualification, URI dependencies, pruning, repair, validation
and caching. The preserved producer-26 lighting pack has not been regenerated
with this publication path.

The [source-qualified test](../../research/skyrim-native-texture-view-test-20261010.md)
uses eight verified diffuse textures, reused in 301 GLBs / 749 material views.
All 447 engine library tests and strict Clippy pass; all 28 Metal material GPU
cases pass, and the old-decode negative control fails the intended case. The new
binary with original assets reproduces the fog-enabled baseline regions exactly.
With the corrected native views, selected roof, wood and stone Y increase by
5.90, 3.55 and 3.85 times while authored controls stay fixed. Ground and foliage
remain dark, and the no-fog roof clips heavily. This closes the scoped sampling
test; it does not close whole-scene brightness or retail visual acceptance.

The user subsequently rejected that fog-enabled screenshot as glowing. The
[glow isolation](../../research/skyrim-glow-isolation-20261010.md) repeats its
material region measurements and separates bloom from the surface response.
Bloom off reduces roof and stone mean Y by only 1.63% and 3.13%; eliminating
native specular reduces them by 13.43% and 4.91%, with adaptation coupling.
The measured white stone point survives both controls and is already white
before image-space composition. This frame remains a failed visual gate.
The eight-source test leaves inconsistent native sampling across the rest of
the pack. The subsequent [coverage test](../../research/skyrim-native-diffuse-coverage-20261010.md)
qualifies 3,706 legacy sources and applies 62,864 diffuse-view annotations in
20,088 GLBs. Original KTX2 bytes, PBR views and normal handles remain unchanged.
The broader captures still fail visual acceptance; terrain and unsupported
material families retain a different response from ordinary native surfaces.
These are file-view counts, not evidence that every view is drawn natively.

With recovered sun direction and adaptation held constant, expanding from eight
sources to the qualified pack leaves roof, wood and sky rectangles byte-identical.
Stone mean Y rises 0.96% and the ground rectangle rises 22.53%; these are mixed
regions, not isolated material measurements. Automatic adaptation couples the
coverage change across the frame. All six comparison captures settle with zero
unavailable or pending dependencies. Draw-identified native, terrain and fallback
response comparisons remain necessary before assigning the brightness imbalance
to a coefficient or effect.

The separate output-transfer diagnostic preserves all authored inputs. With
fog/HDR enabled, treating the HDR result as linear raises selected roof, wood,
grass and sky luminance by 4.41, 9.39, 6.07 and 1.74 times. Whole-frame brightness
is close to the earlier bright build, but the roof stays 58% darker while wood
and sky become more than twice as bright. That imbalance prevents accepting the
output diagnostic by its whole-frame mean. The verified UNORM backbuffer does
not yet close the intervening presentation pass chain.

A separate read-only review finds no analogous extra sRGB decode in native
weather sunlight or directional ambient. Raw RGB bytes are normalized by
`1/255`, blended directly and copied into native uniforms. `SunlightScale` is
applied once after replacing the prepared input, without accumulating between
frames. Existing native traces corroborate byte normalization and ambient-row
preparation; effective light units and the retail sun trajectory remain open.
The previous preview's noon zenith was an explicit approximation. The subsequent
local-trajectory recovery below replaces it; live parent and override state
remain open.

## Exterior sunlight direction, 2026-10-10

The [native sun trace](../../research/skyrim-native-sun-trajectory-20261010.md)
recovers the full-mode local trajectory, direction column and consumer negation.
The preview now uses that arithmetic with original CLMT endpoint hours and
the selected executable's compiled sun settings. It uses an identity sky-parent
world rotation in our renderer; this is reported explicitly, not claimed as a
captured retail transform. Interior direction and unresolved-interior fallback
behavior are preserved.

Sunrise and sunset midpoints, extended by half `fSunAlphaTransTime`, determine
the daylight branch. A linear X traversal and a wrapped night branch construct
the local direction; this path contains no trigonometric orbit. The weather
color extension is not part of these sun endpoints. At clear noon with original
CLMT bytes `[33,60,96,123]`, the engine toward-light direction is approximately
`[0.5274865,0.8241976,-0.2060494]`, rather than overhead.

The compiled settings are `fSunXExtreme=400`, `fSunYExtreme=25`,
`fSunZExtreme=-100`, `fSunDirXExtreme=400`, and `fSunAlphaTransTime=2`.
Runtime setting overrides, the native sky parent's effective rotation,
exceptional sky modes and update timing remain unresolved. The correction
changes direction, not sunlight RGB or authored brightness/saturation. All 454
engine library tests and strict Clippy pass; independent native direction
oracles match binary32 bits at six hours, including daylight boundaries.
Automatic and fixed-adaptation captures both retain pale roof/stone hotspots.
The corrected direction brightens the selected wall region and darkens the
foreground. This closes the local arithmetic discrepancy, not scene appearance.

## Normal frames, 2026-10-10

The [farmhouse audit](../../research/skyrim-native-normal-convention-20261010.md)
finds no demonstrated doubled Y flip or doubled Creation basis conversion.
Stone and two roof primitives preserve their source normals exactly in
binary32. The active native path applies one sampled-Y correction to its
generated Mikk frame; StandardMaterial's independent fallback flag is not read
by that shader.

Skyrim's ordinary vertex shader consumes authored tangent, bitangent and normal
vectors independently. Our renderer regenerates tangents, reconstructs the
bitangent and normalizes vertex N/T. Retained source metadata is not evaluated.
This is a concrete parity gap, but the audit does not attribute the captured
hotspots to it. An authored/generated/geometric-normal comparison requires
source input-layout and component-decoding proof before selecting a correction.

## Subsurface and foliage lighting, 2026-10-10

The [subsurface audit](../../research/skyrim-subsurface-audit-20261010.md) finds
no active soft-light, rim-light, back-light or transmission response. Double-sided
foliage receives ordinary illumination. The Riverwood foliage subset retains
53 soft-light materials and their slot-2 maps, but neither native nor fallback
shading consumes that authored contribution. The older pack omits the source
rolloff/power parameters. Stage A retains Lighting Effect 1/2 as raw binary32 bits
for new conversion output, without adding a shading response.

Shipped ordinary and tree shaders supply a recoverable directional soft-light
equation. Its inspected addition sits outside the ordinary directional shadow
multiplier. Implementing it requires retained source parameters, correct slot
semantics, explicit permutation support and independent GPU checks. This missing
foliage fill is a plausible contributor to dark crowns; it does not establish
the cause of dark walls or justify a global brightness change.

## Remaining acceptance work

Use this sequence for further integration. Preserve the existing source and
numerical evidence while recording each new closure against the selected
executable and package.

1. Compare the verified combined build and settled world captures with matched
   vanilla retail frames and runtime constants. Keep weather, hour, camera,
   fog, image-space and asset identities attached to every comparison.
2. Capture effective light settings and dimmer overrides, the sun parent's
   world transform, live additive ambient and active weather-transition selection.
   Retain approximate coefficients and timing fallbacks as named assumptions
   until verified. The default sun producer and local trajectory are traced;
   live world-state selection remains open.
3. Trace interior CELL/LGTM cube precedence, room ownership and per-draw local
   light selection; add interior navigation and capture coverage.
4. Recover shadow depth production, filter/bias/cascade behavior and SAO
   selection/composition. Current Bevy visibility and disabled/unimplemented AO
   remain separate acceptance boundaries.
5. Preserve original DDS view transfer through the complete conversion contract;
   implement authored soft-light parameters and shader response with explicit tree
   support limits. Extend supported shader families, then trace MODS/TXST/MATO texture overrides,
   snow participation and terrain LOD geometry/material response. Riverwood
   captures still show coarse far terrain. Existing material fallbacks stay
   visible in support reports and fail strict tests that require native coverage.
6. Close animated IMGS/IMAD state, retail sampler/format selection and final
   display transfer. Stable-record HDR arithmetic does not establish modifier
   stacking, transition behavior or output parity.

The G1–G8 evidence gates in the imported lighting contract retain their detailed
trace requirements. This page owns current integration order and launch policy;
the individual specifications own equations, record layouts and evidence.
