# Runtime grass

Grass is opt-in with `--grass`. Conversion exports winning GRAS/LTEX records
and bakes [versioned masks](../converters/grass-cache.md). The engine reads
`grass_cache.rkyv` only when grass is enabled in a streamed world. Requesting
grass in a benchmark-only or non-streaming fixture run prints one warning that it
has no effect, without changing camera anti-aliasing or terrain streaming radii.
A missing,
old or invalid archive disables grass for that pack with one warning; the world
still loads and capture readiness no longer waits for grass. Existing
terrain, models, camera anti-aliasing and cell commit behavior remain unchanged
when grass is disabled.

`GrassPlugin` observes committed exterior cell roots in `PostUpdate`; it adds
no generation work to `collect_cells`. At most four asynchronous placement jobs
run or wait on Bevy's `AsyncComputeTaskPool`. Workers borrow normalized 17x17
blend grids, species rules and 32-unit exclusion bits directly from a validated
immutable memory mapping. Committed cells retain an immutable shared copy of
their post-weld 33x33 height field, so grass roots, slope and water decisions use
the same surface as the rendered terrain. When LAND VCLR is present, the same
allocation also retains 33x33 scalar luminance samples. Heights add 4356 bytes
per committed exterior root while grass is enabled; colours add another 4356
bytes. The full retained surface counts toward the generated pool's budget. The mapping is reference-counted so
pending workers retain it safely. Converted assets must not be rewritten while
the engine is running.

Both merged terrain and its quadrant fallback supply the selected, welded
height field to grass; cache heights never replace the surface actually drawn.

Each admitted species owns an R2 stream seeded from cell and species identity.
The configured cap admits exactly N resolved entries in each LTEX's GNAM order
(default two, zero none). Repeated nominations sum blend support without dividing
by species count. Each species independently tests continuous support times authored
density against one deterministic draw, reused at the final jittered root. Other
species cannot steal its roots. This is MudCrab's native placement design.
An admitted rule without a usable model still consumes its prefix place; the cap
does not backfill from later GNAM entries. Output batches use FormID order.

Position range supplies deterministic XY jitter over the total span
`-range/2..range/2`, matching the earlier precached implementation. Jitter that
leaves the cell is rejected instead of being clamped into an artificial edge row.
Exclusion is checked only at the final position. Slope and water
rules and the selected species' painted texture support are rechecked after
jitter. Height range supplies variation around unit scale, clamped to 0..2.
Colour Range and terrain VCLR jointly supply darken-only brightness:

```text
r = clamp(Color Range, 0, 1)
u = deterministic random channel 9 in [0,1)
s = 2*u - 1
L = .299*(R/255) + .587*(G/255) + .114*(B/255)
b = clamp(L * (1-r+s*r), 0, 1)
byte = round(255*b)
```

Absent VCLR is white (`L=1`). The same formula applies to every instance,
including white and near-white terrain, independently of GRAS lighting flags.
On white/absent VCLR the continuous pre-clamp support is `[1-2r,1)`;
the lower tail clamps at zero, and byte rounding can reach 255. This replaces
the former `1-r*u` law with support `[1-r,1]`: white terrain also becomes darker
for nonzero Colour Range. Zero range is neutral only on white/absent VCLR;
otherwise it retains terrain luminance. Brightness decodes as `byte/255`, so
**255 is exactly 1.0** and no instance brightens above 1. Scale keeps its
separate 0..2 encoding. The formula is independently expressed from
[FasterNGIO's EmitBlade](https://github.com/panthuncia/FasterNGIO/blob/5a1bff6ad3e44b5b9c4b5acdd0663ae20860ca40/src/Grass/Internal/PlacementCommon.h#L157-L166),
with the same pinned author's [GPL v3 reference](https://github.com/panthuncia/FasterNGIO/blob/5a1bff6ad3e44b5b9c4b5acdd0663ae20860ca40/LICENSE).
MudCrab is GPL-3.0-or-later and can use its GPL v3 option consistently with
that reference. No C++ scaffolding is copied; RNG and its signed mapping remain
MudCrab policy, without a claim of exact native distribution.

Luminance interpolates the committed LAND colours using the renderer's fixed
`[a,b,c,b,d,c]` triangles at the final encoded, post-jitter XY root. This differs
from the reference's alternating diagonals. There is no gamma conversion or
direct RGB hue multiplication. The immutable surface retains colours once per
commit, includes them in equality and pool accounting, and regenerates on a
colour-only terrain replacement. A speculative cache surface has no colours;
it cannot attach as a match to a committed coloured surface.

Slope is evaluated
in degrees from the rendered terrain triangle's geometric normal. Height uses
the terrain mesh's anti-diagonal triangles, including after position jitter;
bilinear height interpolation can bury grass on a nonplanar quad. The spacing option controls the
candidate count `ceil((4096 / spacing)^2)` per species per cell. It does not impose
another minimum distance between accepted clumps: that extra rejection thinned
coverage compared with the precached reference, which permits neighboring clumps
to overlap. R2 candidates and independent per-species acceptance remain
deterministic; individual placements are not identical to the old world-aligned
lattice.

Water comparison mode names come from [xEdit's GRAS definition](https://github.com/TES5Edit/TES5Edit/blob/dev-4.1.5/Core/wbDefinitionsTES5.pas).
With signed distance `d = terrain_height - water_height` and record distance
`u`, the engine uses the following predicates. A cell without water accepts all
water comparison modes.

| Mode | Placement predicate |
| --- | --- |
| 0 | `d >= u` |
| 1 | `0 <= d <= u` |
| 2 | `d <= -u` |
| 3 | `-u <= d <= 0` |
| 4 | `abs(d) >= u` |
| 5 | `abs(d) <= u` |
| 6 | `d <= u` |
| 7 | `d >= -u` |

Modes 6 and 7 constrain the named side and permit the opposite side. These
predicates are MudCrab policy interpreting the xEdit labels.
[CommonLibSSE-NG's TESGrass definition](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/main/include/RE/T/TESGrass.h)
identifies slope degrees and the vertex-lighting, uniform-scaling and
fit-to-slope flags. The custom shader consumes those flags.

Workers emit a 16-byte instance: cell-relative quantized XYZ (`u16`), full-turn
rotation (`u8`), scale (`u8`), brightness (`u8`), Creation-space normal (`i8[3]`)
and candidate slot (`u32`). A species batch supplies height minimum/span, flags,
wave period, absolute cell grid and the authored converted glTF path. The
renderer draws each authored primitive with its node hierarchy transform,
authored texture scale/offset and shared immutable instance buffer. The texture
transform applies to the same UV varying used by color and depth cutouts.
Grass creates entities per species/primitive,
with no per-instance entities. Depth prepass and main pass use the same authored
texture/material alpha cutout expression. Vertex RGB retains its authored tint,
multiplied by material RGB and instance brightness. Grass vertex alpha does not
control opacity: [grass authoring accounts](https://stepmodifications.org/forum/topic/20502-im-making-a-grass-mod-and-need-some-tips/)
identify it as an animation/wind weight, while the
[NIF definitions](https://github.com/niftools/nifxml/blob/develop/nif.xml)
describe its storage without prescribing a grass opacity equation. The converted
alpha attribute remains intact but is unused by the current grass wind model;
an exact Skyrim wind equation or ambient-alpha interpretation is not established.
Opaque primitives retain full coverage
regardless of texture, material or vertex alpha. Only the main world camera receives
grass draws, and grass does not enter shadow phases or water reflections.
The color pass uses the camera's Bevy tone-mapping method, LUT and color grading
for non-HDR targets, matching the color treatment of standard scene materials.
HDR targets defer tone mapping to the later shared pass; depth remains colorless.
This prevents exposed sunlight from clipping directly into an LDR target.
Lighting uses one-sided Lambert diffuse `max(dot(N_blade,L),0)`, with no
back-light term. Cards remain visible from both sides; the authored, transformed
blade normal is kept on both raster sides without a front-facing flip. This
matches the measured k=0 model: an authored normal facing away from the sun
receives ambient only. The GRAS Vertex Lighting flag selects interpolated vertex
lighting; otherwise the blade normal is evaluated per fragment.
[Creation Kit's Grass documentation](https://ck.uesp.net/w/index.php?title=Grass&oldid=11790)
describes Vertex Lighting as using the terrain normal underneath the grass.
MudCrab's blade-normal branch does not yet implement that behavior. Flag masks
are `0x01` Vertex Lighting, `0x02` Uniform Scaling and `0x04` Fit to Slope;
the audited official grasses use `0x06`, leaving Vertex Lighting off.

The existing shader byte decode remains `byte/255`; 128 is approximately half
brightness. LAND VCLR was already decoded for terrain and remains absent from
the serialized grass cache. Grass now consumes the committed terrain snapshot,
so no converter, shader, cache schema or pack rebuild is needed. Wave Period drives the current scrolling
wind model, but its exact native timing and waveform remain unverified.

The color pass receives the existing directional shadows using Bevy's prepared
terrain view bindings, cascades, filtering and normal/depth bias. Direct light
and shadow sampling use the same first prepared directional light for the view,
respecting Bevy's ordering and render layers. Shadow visibility multiplies only
the direct sun term, retaining the existing ambient response and exposure:
`(ambient + sun * Lambert * visibility) * exposure / pi`. With no directional
light there is no direct term; disabled shadows use visibility 1. Shadowing never
changes texture/material alpha, sample coverage or depth-prepass cutouts. Grass
still does not cast shadows or add local lights; this is not full PBR parity.

After exposure-scaled lighting, grass uses Bevy's existing `apply_fog` with the
same prepared view fog uniforms and specialization as terrain, before tone
mapping. Each surviving alpha-tested fragment supplies its own world position.
No DistanceFog component leaves the HDR color unchanged; fog parameters and the
fog model remain owned by the existing camera/sky setup.

Controlled matched-view measurements from the 2026-10-27 grass colour-layer
experiment and back-light sweep favored k=0 in all five sampled biomes. The
combination of one-sided lighting, shadow reception, darken-only range and an
unproven vertex-space conversion closed about 79% of the measured mean-RGB gap.
These are empirical design evidence, not claims about Skyrim's internal shader
or exact Colour Range distribution. The current implementation retains one-sided lighting and shadows, and adds the
attributed luminance/range law above. Authored vertex RGB remains numerically
unchanged, with no extra sRGB conversion, ambient retuning or weather changes.
Instance size remains 16 bytes. The configured prefix can change species admission
and instance counts; retained colour samples also increase CPU pool accounting.
Grass enables 4x MSAA on the world camera, including while interactive renderer
preferences change. The saved MSAA preference still applies to reflections and
runs without grass. Both fragment passes use `sample_index` to discard samples
outside the same derivative-based alpha coverage count, and emit a static full
sample mask. Constant-alpha mask interiors use the authored cutoff comparison,
including equality, and smoothing applies at alpha edges. This retains the same
sample prefix as a dynamic coverage mask,
while avoiding a reproduced pipeline-compilation crash in the tested AMD Vulkan
driver when exporting a dynamic `sample_mask`. A shared wind
texture drives coherent wind using absolute world coordinates across origin
rebases. Distance fades retain a deterministic prefix of candidate slots.

The runtime keeps resident and recently generated cells in a byte-budgeted LRU
pool, accounting for generated arrays, batch metadata and retained source height
and luminance fields. Resident desired
cells stay pinned; a result that cannot fit is discarded with a diagnostic
instead of exceeding the pool budget. Rejections are retried after the desired
cell set changes. Generation is prefetched one cell ahead in the player's
direction of travel, while rendering still waits for a committed cell root.
Prefetched results are reused only when their surface matches the committed
field. Reloaded cells with changed welds or colours regenerate grass; pending results from
retired cell roots cannot be attached to a new root.
Species batches are admitted under the upload count budget, and GPU buffer
preparation applies its own admission limit when meshes become ready.

| Option | Default | Meaning |
| --- | --- | --- |
| `--grass` | off | Enable grass in the streamed world |
| `--grass-radius` | 2 | Cell radius, clamped below terrain stream radius |
| `--grass-uploads-per-frame` | 2 | Species uploads per frame; zero is unlimited |
| `--grass-density` | 1 | Density multiplier, finite 0..16 |
| `--grass-min-spacing` | 20 | Creation-unit spacing/candidate budget, at least 8 |
| `--grass-fade-start` | 7000 | Distance where prefix thinning starts |
| `--grass-fade-end` | 8000 | Distance where grass reaches zero density |
| `--grass-cache-mib` | 64 | Generated CPU pool budget; zero is unlimited |

Options are validated for both CLI and programmatic configurations. Fade
distances must be finite, with `end > start >= 0` when fading is enabled;
disabled fading also accepts a zero range. Grass generation time per cell
and main-thread grass streaming time per frame are recorded as
`grass/generate_cell` and `grass/stream_frame` in `ProfilingState`; pool bytes,
pending jobs and upload counts are also reported. Bounded CPU/GPU admission
reduces frame spikes, but does not establish an absolute no-stutter guarantee.
Release performance budgets and matched visual captures remain the evidence
for a particular scene and hardware. Authored mesh fidelity, exact Skyrim placement,
collision-shape exclusions and universal grass-mod compatibility are not
claimed by synthetic generation tests.

Enabled grass requires a radius of at least one cell and a terrain stream radius
of at least two, so grass can stay inside the terrain window. Effective fade end
is capped at `effective_grass_radius * 4096` Creation units; fade start is scaled
by the same ratio. CLI grass runs choose terrain radius three unless explicitly
overridden, allowing the default grass radius two and fade 7000..8000. With an
explicit terrain radius two, grass radius falls to one and fade to 3584..4096;
startup diagnostics report this clamp. A cell can leave this generated ring only after its nearest edge has
reached zero density, preventing whole-cell disappearance during ordinary cell
crossings. Radius zero cannot provide this guarantee and is rejected when grass
is enabled. Acceptance and multi-shot captures wait for desired grass
generation/admission and actual GPU draw readiness, including newly admitted
batches; grass preparation failures fail an automated screenshot run.

Saved renderer preferences may widen automatically selected terrain coverage,
and retain its grass minimum. An explicit terrain radius still clamps grass.
Interactive terrain-radius changes recompute the same fade bound before the
next grass draw. Capture receipts record grass readiness and current failures
at extraction time, as the multi-shot settle gate does before requesting a frame.

## Skyrim INI settings

Grass uses the existing `--ini <path>` reader shared with
[terrain distances](lod-architecture.md#distance-configuration-skyrim-ini-parity).
Files apply in the order given, later files override earlier ones key by key,
and explicit numeric grass options override every file wherever they appear.
Grass works without an INI file, using the defaults below.

```text
engine --assets <converted> --grass --ini <path-to-Skyrim.ini> --ini <path-to-SkyrimPrefs.ini>
```

The shared reader accepts UTF-8 with an optional BOM, case-insensitive sections
and keys, and whole-line `;`/`#` comments. The last duplicate assignment wins.
Unknown keys are ignored. An unreadable selected file prints a warning and is
skipped; a malformed scalar prints a warning and leaves that setting unchanged.
The final grass configuration must still pass runtime validation. INI values
update generation settings only when grass is active; the report retains imported
values while grass is disabled. Explicit numeric options keep their usual validation. INIs are
read only and selected by explicit path; the engine does not discover or copy
the player's files. The planned converter follow-up that copies those files
into a pack will feed this same `--ini` path.

These are the fifteen honoured grass keys. All retain their `[Grass]` section.
The three fade-start defaults present in the shipped `SkyrimPrefs.ini` match
that file; every other value is an explicit **MudCrab default**.

| Section | Key | Default | Default source | Engine use |
| --- | --- | --- | --- | --- |
| `[Grass]` | `iMinGrassSize` | `20` | MudCrab default | Candidate spacing/density |
| `[Grass]` | `iMaxGrassTypesPerTexure` | `2` | MudCrab default | Exact resolved-species cap (zero admits none); INI spelling retained |
| `[Grass]` | `fTexturePctThreshold` | `0.0` | MudCrab default | Minimum blended texture weight |
| `[Grass]` | `iGrassCellRadius` | `2` | MudCrab default | Generated cell radius |
| `[Grass]` | `fGrassStartFadeDistance` | `7000.0` | Shipped `SkyrimPrefs.ini` | Start of distance thinning |
| `[Grass]` | `fGrassFadeRange` | `1000.0` | MudCrab default | Fade end is start plus range |
| `[Grass]` | `bDrawShaderGrass` | `1` | MudCrab default | Enable grass draws when `--grass` is requested |
| `[Grass]` | `bEnableGrassFade` | `1` | MudCrab default | Enable distance thinning |
| `[Grass]` | `fGrassMinStartFadeDistance` | `0.0` | Shipped `SkyrimPrefs.ini` | Reported menu metadata |
| `[Grass]` | `fGrassMaxStartFadeDistance` | `7000.0` | Shipped `SkyrimPrefs.ini` | Reported menu metadata |
| `[Grass]` | `fGrassWindMagnitudeMin` | `5.0` | MudCrab default | Reported only |
| `[Grass]` | `fGrassWindMagnitudeMax` | `125.0` | MudCrab default | Reported only |
| `[Grass]` | [`bAllowCreateGrass`](https://ck.uesp.net/wiki/Floating_Grass_Fix) | `1` | MudCrab default | Reported legacy cache switch |
| `[Grass]` | [`bAllowLoadGrass`](https://ck.uesp.net/wiki/Floating_Grass_Fix) | `0` | MudCrab default | Reported legacy cache switch |
| `[Grass]` | `bGenerateGrassDataFiles` | `0` | MudCrab default | Reported legacy cache switch |

The UESP links document the cache-control keys in a workaround; they do not
establish defaults or file precedence. Menu min/max fade settings are metadata,
not a second clamp on the selected start distance. Disabling fade keeps full
density up to cell unloading, so the zero-density cell-crossing guarantee no
longer applies. Legacy cache switches and imported wind magnitudes do not
control MudCrab's cache or wind model. Imported spacing below eight is outside
the bounded CPU generator's supported range and is rejected. Startup
diagnostics report effective grass settings and each key's `--ini` path or
`default` source, with explicit numeric overrides reported separately.

`iMinGrassSize` controls density, not mesh height: spacing 20 provides 41,944
candidate slots per species per cell versus 4,096 at 64. Authored mesh/node dimensions are
preserved. Matching default inputs and terrain grounding does not establish
exact Skyrim placement, variation, animation or visual parity.

## Normal maps and authored wind weight

Static grass keeps authored positions, normals and UVs; the converter builds
an area-weighted UV tangent frame and exports its XYZ plus handedness.
Normal textures are sampled as linear data in the fragment shader and transformed
through that frame. Legacy geometry without TANGENT uses position/UV derivatives;
degenerate frames or absent maps keep the geometric normal. The frame follows Bevy's bitangent convention; DirectX-tagged normal maps
flip Y once, matching the native material contract. Node reflection/nonuniform
scale, instance yaw, slope and authored UV transforms move the frame with the mesh.
The fragment re-orthogonalizes it against the interpolated normal. Both raster
sides retain authored orientation. Wind displaces positions while retaining the
undeformed shading frame, matching the existing native wind policy.

Vertex alpha linearly weights wind displacement (absent colors use full weight).
Zero alpha is motionless; alpha does not control opacity. Coverage uses texture
and material alpha. Public study: Community Shaders contributors, RunGrass and
GrassLighting at commit 1ed40cb2fdf6099eeb7068661f632d71f0661b70 (GPL-3.0 with
project exceptions). Its alpha-weighted wind supports this interpretation; its
feature normal mapping does not establish vanilla NIF normal-slot usage. Wave
and lighting formulas here remain native MudCrab choices.

## Live grass tuning

Select a flat TOML file with `--grass-tuning docs/examples/grass-tuning/flat-b.toml`
(or `MUDCRAB_GRASS_TUNING`). Fileless startup and empty valid TOML use the same
flat-b defaults. `--grass-tuning-info <path>` prints the accepted settings before
app or GPU creation. Content-hash polling runs at most once per elapsed second;
valid edits atomically update uniforms without regenerating placement or shaders.
Missing/unreadable/invalid edits retain the last good values. Unknown keys warn;
finite values outside the ranges below clamp with warnings. Inputs over 64 KiB
are rejected. Accepted file hash, effective values and revision are frozen with
captured frames; callbacks never reread the file.

The optional `mode` is `flat` (default), `d`, `e`, or `f`. Explicit D/E comparison
presets set ambient, sun, shadows and saturation to1, with sun angle1 for D and0
for E. F uses an unlit shadow visibility with floor .68. Scalar fields override
the selected preset. Modes and scalars share the same hot-reload uniform path.

For normalized authored blade normal N and sun direction L, let c=dot(N,L).
The direct response is `mix(1,clamp((c+wrap)/(1+wrap),0,1),sun_angle_weight)
+ backlight*max(-c,0)`. GRAS vertex-lighting retains the interpolated vertex
response; otherwise the fragment evaluates its interpolated normal.
Shadow visibility V is `shadow_floor + (1-shadow_floor) *
mix(1,native_shadow,shadow_strength)`. D/E illumination is
`(ambient*ambient_strength + sun*direct_response*sun_strength*V)*exposure/pi`.
F illumination is simply V, ignoring ambient, sun and exposure. Alpha coverage,
terrain shadow filtering and display tone mapping retain the production paths.

| Knob | Default | Accepted range | Effect |
| --- | --- | --- | --- |
| brightness | 1.15 | 0..8 | Final linear HDR RGB multiplier |
| ambient_strength | 1.15 | 0..8 | Ambient/sky multiplier (D/E) |
| sun_strength | .1 | 0..8 | Direct diffuse and specular sun multiplier (D/E) |
| sun_angle_weight | 0 | 0..1 | Interpolate between sun-facing and wrapped normal response |
| wrap | 0 | 0..1 | Wrapped Lambert response above |
| shadow_strength | .3 | 0..1 | Interpolate native visibility toward no shadow |
| shadow_floor | 0 | 0..1 | Minimum visibility; F preset .68 |
| backlight | 0 | 0..1 | Add response from away-facing authored normals |
| specular_strength | 0 | 0..1 | Dielectric GGX sun sheen multiplier (D/E) |
| specular_roughness | .5 | .089..1 | Perceptual roughness, squared to GGX alpha |
| saturation | .75 | 0..4 | Linear HDR saturation around Rec.709 luminance |
| vclr_tint_strength | 1 | 0..1 | LAND luminance strength |
| color_range_strength | 1 | 0..1 | GRAS color-range variation strength |
| root_darkening | 0 | 0..1 | Multiply by `1-root_darkening*clamp(original_uv.y,0,1)` |

The last darkening convention assumes the root lies at authored UV.y=1 and
leaves meshes without UVs unaffected; arbitrary modded UV layouts may differ.
Texture transforms do not move this root fraction. Saturation uses
`max(mix(dot(HDR,vec3(.2126,.7152,.0722)),HDR,saturation),0)` before root
and brightness multiplication. LAND's existing encoded-input luminance retains
`.299,.587,.114`; these are separate operations in different color spaces.

Sheen uses the isotropic equations inspected in Bevy 0.19's local
`pbr_lighting.wgsl`: `D_GGX`, `V_SmithGGXCorrelated`, and `F_Schlick`.
For `alpha=max(roughness^2,.007921)`, `k=alpha/(1-NdotH^2+(NdotH*alpha)^2)`,
`D=k^2/pi`; `Vsmith=.5/(NdotL*sqrt((NdotV-alpha^2*NdotV)*NdotV+alpha^2)
+ NdotV*sqrt((NdotL-alpha^2*NdotL)*NdotL+alpha^2))`;
F=.04+.96*(1-VdotH)^5. The added HDR sheen is
`sun*sun_strength*specular_strength*D*Vsmith*F*NdotL*exposure*V`.
Dot products clamp to their nonnegative domains; NdotV and visibility's
denominator have epsilon guards. Strength zero bypasses sheen, as does F mode.
This is a direct dielectric highlight, without an environment reflection probe.

The old per-instance brightness byte is preserved exactly. With both tint and
range strengths at one the shader uses that byte directly. Otherwise it uses
`clamp(mix(1,LAND,tint_strength)*mix(1,R,range_strength),0,1)`, where
`R=1-2*GRAS_color_range+2*random*GRAS_color_range` includes negative values.
Generation encodes LAND and R in the high 14 bits of the slot word:
7-bit UNORM LAND (maximum error 1/254) and 7-bit signed-range R (error 1/127).
The low 18 bits hold every candidate at supported spacing >=8 (maximum
262144 candidates). `GrassInstance::candidate_slot()` decodes CPU identity;
both CPU and GPU fading ignore metadata. No sidecar allocation or instance-size
increase occurs. No-file generation uses the same metadata and original brightness byte.

To tune live, launch the engine with the usual asset/world arguments plus
`--grass --grass-tuning docs/examples/grass-tuning/flat-b.toml` and edit
that file while it runs. Begin with brightness, then ambient/sun strength,
then angle/wrap and shadows; adjust tint/saturation and roots last, and add
sheen only if needed. `darker-start.toml` uses brightness .59 from the provisional
rounded luminance ratio .065/.110. This is a starting hypothesis; tone mapping,
scene differences and local shadows prevent it from proving a Skyrim match.

## Default appearance and tuning

Without a file, grass uses flat-b: sun angle weight 0, sun strength 0.1,
shadow strength 0.3, ambient strength 1.15, saturation 0.75 and brightness 1.15.
Authored vertex RGB, Color Range and terrain VCLR tint stay enabled. The appearance
pipeline and packed tint metadata are active with or without a tuning file.
`--grass-tuning <toml>` overrides `MUDCRAB_GRASS_TUNING`; edits hot reload at most
once per elapsed second by content hash. Invalid or missing files keep the last
good settings and warn. A valid empty file resets to defaults. Inputs are limited
to 64 KiB. `--grass-tuning-info <toml>` validates and reports settings before app
or GPU creation. Captures freeze the accepted hash and effective settings with
the actual frame; they do not reread the file in the callback.

Explicit `--shots` runs freeze wind time and allow `-on`/`-off` shot-name pairs
to suppress grass draws while retaining placement and readiness accounting.
No experimental gamma or alternate placement switches are included.

The default tuning mode is `flat`; `d`, `e` and `f` remain explicit named presets
for comparisons. Omitted mode selects `flat`, including an empty TOML file.
All production startup paths use the tuning uniforms; there is no shading
experiment environment switch. Authored normal maps use the preserved glTF frame
and one DirectX Y conversion, consistent with the ordinary native material path.
