# Distance fog implementation

The [lighting and shading hub](lighting-and-shading.md) owns the combined launch policy and remaining acceptance work.

The engine now implements the record resolution and fog arithmetic recovered in the
[Skyrim design spec](distance-fog.md). It replaces the exponential, single-color approximation.
This is an implementation of the verified distance-fog paths, with explicit material and color
pipeline approximations. Exact visual parity remains unverified until matched retail captures
establish the active settings, render states and working color domain.

## Record and runtime ownership

Converter metadata **26** and world database schema **9** retain `environment_records`, a typed
projection of winning `WTHR`, `CLMT`, `LGTM`, `CELL`, `WRLD`, `REGN`, relevant `GMST`, `IMGS`
and `VOLI` inputs. IDs are remapped through the package load order. Authored floats retain their
IEEE bits, including negative zero, NaN and large fog maxima. Missing fields remain distinct
from authored zero. The existing raw record table is retained.

`EnvironmentCatalog` loads the projection once and closes SQLite before rendering. Exterior
CELL lookup follows the same LAND-preferred ownership as streaming. Interior resolution uses
an explicit CELL identity. Older databases remain readable; they use a stated clear-day exterior
fallback and disable unknown interior fog. Reconvert the package to use authored environments.

`EnvironmentState` holds the current/previous weather, game hour, transition state, room
templates and room factor. The resolver implements separate scalar day weights and four-key
color weights, weather acceleration/midnight wrapping, per-field CELL inheritance, room distance
sanitation, native mode selection and the previous-only room asymmetry. Transition completion
clears the previous weather before preparation only when progress exceeds one.

The initial exterior weather is the winning `SkyrimClear` record unless explicitly selected.
Region polygons, priorities, weather chances and GLOB links are retained for a future scheduler.
The engine does not yet simulate automatic region selection, weather scheduling or game time.

## Rendering paths

| Path | Implemented behavior | Remaining limit |
|---|---|---|
| Opaque depth fog | Camera reverse depth reconstructs view Z, then finite retail forward depth and the recovered biased distance. The pass runs after opaque rendering and before transparents, preserves coverage alpha, respects the far sentinel and protects other viewports sharing a target. | Native SAO/composite ownership and source color domain need a retail capture. MSAA and non-perspective views are explicitly skipped. |
| Ordinary native Lighting | Supported native materials keep diffuse and specular separate for the recovered clamps, then opaque depth fog or the declared transparent geometry route finishes once. Vertex fog uses finite forward clip-XYZ length before perspective divide. | Native environment-map response and specialized shader families remain unsupported. |
| Terrain and fallback Lighting | The same geometry metric and fog ownership apply to Bevy shading. | Bevy combines lit/specular response, so its native clamp inputs retain a documented combined-response proxy. |
| Converted Effect | Ordinary, additive, multiplicative and premultiplied fog arithmetic; retained raw NiAlpha RGB blend factors and alpha threshold. Opaque/Mask Effect uses one depth-fog owner in this engine. | Source flags to native shader descriptor selection remain a semantic proxy. Framebuffer alpha retains Bevy OVER; opaque Effect native draw ownership still needs capture. |
| Water | Ordinary vertex fog, saturating RGB framebuffer scale and recovered output-depth bias. Fog precedes material postprocessing. | Existing water alpha/coverage remains an engine choice. Underwater, additional-light, refraction and alternate native Water routes remain unimplemented. |
| Sky | Resolved normalized byte RGB reaches the sky and background without an extra gamma decode. Each camera draws its own dome. | Analytic dome rows differ from native vertex-color interpolation; Sky noise and framebuffer-scale ownership need capture. |
| Reflection | World fog and sky are explicit, HDR storage has no display transform, and observer exposure/projection are synchronized. Current camera/water transforms are resolved before sky following and propagation. | Native reflection scenegraph ownership and draw selection remain capture gates. Geometry material fog uniforms currently use the main world's far plane. |

Image-space and geometry constant packing preserve their different native equal/NaN guards.
Authored fog color alpha is not opacity. Fog maxima and room factors receive no invented `0..1`
clamp. Inherited interior Fog Clip caps camera far metadata separately from fog Far; leaving
the interior restores the configured value. Fog reconstruction uses that finite distance,
but Bevy's infinite reverse-Z raster projection and ordinary visibility checks do not clip
geometry there. Finite geometry clipping remains an implementation gap. The recovered internal
range remap is an explicit state input, without a guessed INI binding.

The stock Bevy `DistanceFog` component is removed from `FogCamera` views. Terrain and native
NIF materials disable stock material fog. Shared shader imports give geometry variants the same
reconstructed finite forward coordinates for fog arithmetic; raster positions retain infinite
reverse-Z. All six terrain layers share their identical repeat sampler, avoiding Metal's
16-sampler limit without changing texture filtering.

Removing `FogCamera` clears the environment-owned opaque parameters and restores an unchanged
camera far cap. Explicit parameters on unmarked views remain supported. Shared geometry inputs
ignore inactive cameras and require valid perspective planes. Reflection target dimensions follow
the observer's aspect up to 1024 pixels per side; common integer ratios remain exact, while unusual
coprime ratios use the nearest bounded pixel size.

## Controls and diagnostics

The engine accepts layered Skyrim INI files through the existing `--ini` option:

| Setting | Consumer |
|---|---|
| `bSAOApplyFog:Display` | Enables the opaque depth fog pass; geometry fog is independent. |
| `fNearDistance:Display` | Perspective near plane; default `15`. |
| `fPostLitClamp:General` | Lighting clamp input; default `1`. |
| `fPostSpecClamp:General` | Specular clamp input; default `1`; separate on supported native Lighting, combined on Bevy fallback response. |
| `fPostEnvMapClamp:General` | Retained environment-map clamp input; separate native environment-map response is pending. |

Diagnostic CLI options are `--fog-hour`, `--fog-weather`, `--fog-interior-cell`,
`--fog-framebuffer-scale` and `--fog-report`. Form IDs accept decimal or `0x` hex.
`--fog-interior-cell` changes environment resolution only; it does not load interior geometry.
The framebuffer scale defaults to the native global's initial reciprocal, bits `0x3F555555`.
That initial value is evidence of initialization, not a measured active retail frame.

```sh
cargo run --locked -p engine -- \
  --assets /path/to/converted-assets \
  --fog-hour 12 --fog-weather 0x81A \
  --fog-report /path/to/private/fog-inputs.json
```

The report includes resolved mode, CELL/weather/climate IDs, field sources, transition state,
scalars, both RGB colors, distinct packed fog vectors, settings and authored IMGS/VOLI targets.
Image-space/volumetric targets remain unblended; their native blend weights are unresolved.
The report describes implementation inputs, not retail measurements or a full draw-state capture.

`environment-audit` reads a caller-supplied ordered plugin list, decodes environment records and
checks the production database exporter without converting meshes or textures:

```sh
cargo run --locked -p converter --bin environment-audit -- \
  /path/to/Skyrim/Data /path/to/ordered-plugins.txt /path/to/private/environment-audit.json
```

An audit of every copied plugin describes package winners. It establishes an effective retail
load order only after the active profile and implicit Creation Club entries are verified.

## Validation and acceptance

CPU tests cover field inheritance, climate boundaries, byte-color arithmetic, room sanitation,
transition completion, native packing guards, finite depth reconstruction, camera far restoration
and per-camera sky ownership. Typed-record tests cover bit-preserving serialization, legacy
payloads, ID remapping and transactional export failures.

The GPU integration test uses original constant-color geometry and independent numerical
references. It checks opaque fog, transparent phase order, preserved alpha, the fog depth
sentinel and background bypass, split viewports and production terrain/water shaders. It does
not test finite geometry clipping. The native material probe loads
synthetic converted scenes through the production material hook and checks distinct Lighting,
Effect and blend/test routes. These establish implementation behavior on the tested adapter.

```sh
cargo test --locked -p engine --test fog_gpu -- --ignored --nocapture
cargo run --locked -p engine --example fog_material_probe -- --output /path/to/private/probe
python3 scripts/check-distance-fog-spec.py
```

Retail acceptance follows the [capture matrix](distance-fog.md#74-required-comparison-matrix).
The [implementation audit](../../research/skyrim-fog-implementation-audit.md) records IMGS,
VOLI, source-lighting, Sky, finite clipping and capture-environment findings. Do not tune global
tint or exposure to hide those unresolved differences. Raw retail assets, shader bytecode and
captures stay outside the repository; public fixtures use original data and interpreted measurements.
