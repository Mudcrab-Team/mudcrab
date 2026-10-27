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
the same surface as the rendered terrain. This adds about 4.3 KiB to each
committed exterior terrain root while grass is enabled; retained copies in the
generated pool count toward its budget. The mapping is reference-counted so
pending workers retain it safely. Converted assets must not be rewritten while
the engine is running.

Both merged terrain and its quadrant fallback supply the selected, welded
height field to grass; cache heights never replace the surface actually drawn.

Candidates follow a cell-shifted R2 sequence, seeded with cell ID and slot index.
Every accepted instance retains its original slot index and batches retain slot
order. Effective blend weight is shared among a texture's nominated species;
weights for repeated species accumulate across layers. The INI type cap selects
the first N species in stable cache FormID order per texture, and the texture
threshold rejects blend weights below its value. Both apply again after jitter.
The current converter and cache do not retain LTEX GNAM occurrence order; the
shared validator requires ascending species IDs. xEdit defines Grasses as a
sorted array, which does not establish a native first-N priority rule. Preserving
GNAM occurrence order would require a converter/cache contract change before
the runtime could use it. This cap remains MudCrab policy, not Skyrim parity.
Species that fail slope
or water rules cannot compete. Each eligible species scores its blended weight
and density through a deterministic exponential race; the highest score wins.
An independent density test controls occurrence. This deterministic sequence
and weighted competition rule are MudCrab policy.

Position range supplies deterministic XY jitter over the total span
`-range/2..range/2`, matching the earlier precached implementation. Jitter that
leaves the cell is rejected instead of being clamped into an artificial edge row.
Exclusion is checked only at the final position. Slope and water
rules and the selected species' painted texture support are rechecked after
jitter. Height range supplies variation around unit scale, clamped to 0..2.
Colour Range supplies darken-only brightness `b = 1 - r*u`, with record range
`r` clamped to `[0,1]` and deterministic random channel `u` in `[0,1)`.
Brightness is stored as `round(255*b)` and decoded as `byte/255`, so it never
exceeds 1; rounding changes the ideal lower bound by at most `0.5/255`.
Zero range is exactly 1. Scale keeps its separate 0..2 encoding.
These variation equations are MudCrab policy. Slope is evaluated
in degrees from the rendered terrain triangle's geometric normal. Height uses
the terrain mesh's anti-diagonal triangles, including after position jitter;
bilinear height interpolation can bury grass on a nonplanar quad. The spacing option controls the
candidate count `ceil((4096 / spacing)^2)`. It does not impose another minimum
distance between accepted clumps: that extra rejection thinned coverage compared
with the precached reference, which permits neighboring clumps to overlap. R2
candidates and weighted species competition remain deterministic; individual
placements are not identical to the old world-aligned lattice.

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

The color pass receives the existing directional shadows using Bevy's prepared
terrain view bindings, cascades, filtering and normal/depth bias. Direct light
and shadow sampling use the same first prepared directional light for the view,
respecting Bevy's ordering and render layers. Shadow visibility multiplies only
the direct sun term, retaining the existing ambient response and exposure:
`(ambient + sun * Lambert * visibility) * exposure / pi`. With no directional
light there is no direct term; disabled shadows use visibility 1. Shadowing never
changes texture/material alpha, sample coverage or depth-prepass cutouts. Grass
still does not cast shadows or add local lights or fog; this is not full PBR parity.

Controlled matched-view measurements from the 2026-10-27 grass colour-layer
experiment and back-light sweep favored k=0 in all five sampled biomes. The
combination of one-sided lighting, shadow reception, darken-only range and an
unproven vertex-space conversion closed about 79% of the measured mean-RGB gap.
These are empirical design evidence, not claims about Skyrim's internal shader
or exact Colour Range distribution. This implementation uses the first three
changes only: authored vertex RGB remains numerically unchanged, with no extra
sRGB conversion, LAND tint, ambient retuning or weather changes. Runtime instance
brightness changes require no converted grass-cache/schema update. Instance
size, placement/counts and generation-budget accounting remain unchanged.
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
fields. Resident desired
cells stay pinned; a result that cannot fit is discarded with a diagnostic
instead of exceeding the pool budget. Rejections are retried after the desired
cell set changes. Generation is prefetched one cell ahead in the player's
direction of travel, while rendering still waits for a committed cell root.
Prefetched results are reused only when their surface matches the committed
field. Reloaded cells with changed welds regenerate grass; pending results from
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
| `[Grass]` | `iMaxGrassTypesPerTexure` | `2` | MudCrab default | Species cap per texture; INI spelling retained |
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
candidate slots per cell versus 4,096 at 64. Authored mesh/node dimensions are
preserved. Matching default inputs and terrain grounding does not establish
exact Skyrim placement, variation, animation or visual parity.
