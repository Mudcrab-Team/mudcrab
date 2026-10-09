# Detecting z-fighting regressions

Z-fighting is **fixed for now**, as accepted by the user after the Fort Sungard
check and follow-up visual testing. Reopen the issue if another case appears.
The audit and regression evidence below remain available for that investigation.

The first reusable detector is [audit-z-fighting.py](../../scripts/audit-z-fighting.py).
It inventories authored depth requirements, finds overlapping triangles inside
converted models, and finds identical world placements. It records coverage gaps
and cannot certify that a rendered world has no z-fighting.

The next acceptance layer is a deterministic GPU surface audit. Together, these
checks let us fix shared causes across asset families and retain a regression
gate. A geometric overlap is a candidate; it does not by itself show visible
flicker or a conversion defect. Vanilla assets are the reference inputs, but
their overlapping surfaces still need their native render rules.

## Native evidence from the REA investigation

Reused the toolchain, verified executable and persistent project prepared by
`t3/explore-rea-skyrim`. REA 6.0.0's scoped Ghidra doctor passes. That investigation
already established that REA's first Skyrim query exceeds its 330-second cold
import deadline. This investigation used its existing persistent Ghidra fallback
with `-process SkyrimSE.exe -noanalysis -readOnly`; it did not repeat the import,
alter the saved program or execute Skyrim.

Target: Skyrim SE **1.7.104.0**, preferred image base `0x140000000`, SHA-256
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
Addresses below are preferred image virtual addresses, not live process addresses.
The earlier project import has partial analysis. Eight recovered function listings,
containing 2,705 instructions, were checked against the original PE bytes and an
independent LLVM decoder. LLVM's standalone LOCK prefixes were joined to their
following instruction for eight comparisons. The selected listings are untruncated;
this does not imply that the whole executable is fully analyzed.

The private evidence is retained under this worktree's ignored
`target/z-fighting/`. `depth-02.json`, `depth-03.json`,
`depth-decompile.json`, their run manifests, and `native-crosscheck.json`
record source mappings, instructions and decompiler output. These files contain
proprietary analysis and must stay out of Git.

Observed native behavior:

- The lighting geometry function at `0x141549550` reads shader flags at property
  offset `+0x38`. Around `0x14154a72d`, an unset bit 32 selects depth mode 1.
  Around `0x14154a769`, an unset bit 31 selects mode 0. These branches run only
  outside the function's special accumulation-hint 2/3 path; pass overrides
  therefore matter as well as the authored flags.
- The shader-property pass builder at `0x14151a160` tests `0x0c000000`, the
  Decal/Dynamic_Decal flag pair. One branch writes accumulation hint 2 or 3
  to a render pass at offset `+0x1c`. Other branches use those flags in fade and
  technique selection. This proves distinct native pass handling. It does not
  establish a numeric depth bias to copy into Bevy.
- The converter retains `shaderFlags1`, `shaderFlags2`, `shapeBlock`
  and `shaderBlock` in each tagged GLB material. At the audit baseline, the
  [native material hook](../../crates/engine/src/nif_material.rs) handled
  normal convention, specular mask and UV transform, but did not read depth
  or decal flags. Ordinary glTF alpha blending already disables depth writes
  in Bevy; that case is not counted as an opaque/cutout depth-write gap.

The field names and depth-mode interpretation come from pinned
[CommonLibSSE-NG shader flags](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/B/BSShaderProperty.h)
and [depth modes](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/D/DepthStencilDepthModes.h).
Mode 0 is disabled and mode 1 is test without write. These references supply
labels; the version-matched executable supplies the branch evidence. The
[renderer research source](https://github.com/Nukem9/skyrimse-test/blob/328916305165a46c4e4b527735bbcfd46b09a0ca/skyrim64_test/src/patches/TES/BSGraphics/BSGraphicsRenderer.cpp)
also shows raster bias state and a viewport depth adjustment. Its older addresses
were not reused for this executable, and its bias values are not a parity proof.

## Static audit

Run the fast material and placement inventory:

```sh
python3 scripts/audit-z-fighting.py /absolute/path/to/modern_assets \
  --materials-only --out /absolute/path/to/material-depth-risks.json
```

Run geometry too, with bounded work per model:

```sh
python3 scripts/audit-z-fighting.py /absolute/path/to/modern_assets \
  --jobs 4 --progress --out /absolute/path/to/z-fighting.json
```

Repeat selected models at a higher comparison budget:

```sh
python3 scripts/audit-z-fighting.py /absolute/path/to/modern_assets \
  --include 'architecture/farmhouse/*.glb' \
  --max-comparisons 10000000 --out /absolute/path/to/farmhouse-depth-risks.json
```

Reports contain model paths, candidate GLB SHA-256 values, node/mesh/primitive/
triangle identities, source NIF block identities, and placement FormIDs. Examples
are capped separately from totals; capping examples never makes a scan pass.
The report includes the audit script SHA-256. Input assets and the world database
are opened read-only.

Exit codes:

- `0`: no candidates or coverage gaps in the requested static scope.
- `1`: candidates found, with complete coverage in that scope.
- `2`: incomplete coverage or invalid input, including a material-only run.

`rendered_z_fighting_verified` is always false in this tool. The material section
is an authored-requirements inventory against the baseline loader's omissions;
it still lists decal requirements after the renderer fix below. Closing a
requirement requires evidence of the runtime state actually applied to that
material/pass, not deleting its authored flag from the report.

### Geometry method

Decode dense float32 positions and unsigned indices from the embedded GLB
buffer, respecting accessor offsets and stride. Traverse the default scene and
compose every node transform. Preserve repeated mesh instances and account for
Bevy's mirrored-transform culling. Build a bounding volume tree over triangle
bounds. For each candidate pair:

1. Require overlapping bounds expanded by the plane tolerance.
2. Require almost parallel planes and a maximum vertex-to-plane separation
   within the tolerance in both directions.
3. Clip one projected triangle against the other and require positive overlap
   area. Shared edges and point contacts are excluded.
4. Exclude opposing single-sided faces that cannot both face one camera.

Defaults are a plane tolerance of `1e-4` GLB scene units and a minimum overlap
area of `1e-8` scene units squared. These are geometric filters, not a bound on
GPU depth precision. Camera distance, slope, depth format and bias can make
other separations unstable. Pairs within one primitive are checked as well as
pairs between shapes and instances. Texture alpha and surface color are not
sampled, so some candidates will be invisible or visually identical.

Skinning, animation, morph targets, sparse/normalized geometry, unsupported
primitive extensions and exhausted budgets are explicit coverage gaps. A
comparison budget retains already observed pairs but marks the remaining search
incomplete. Geometry is evaluated within each model; this version does not
compare different placed models against each other, terrain, water or LOD.

### Placement method

Scan modeled references in `skyrim_world.db`. Group exact same normalized model
paths and position/rotation/scale values within the same exterior worldspace
or interior cell. Exterior references can match across cell boundaries;
unrelated interiors cannot match. Deleted and initially disabled references are
excluded. References with enable parents remain unknown because the static
audit does not evaluate the live enable graph. NULL header flags, or NULL
transforms on references remaining after those exclusions, are coverage gaps.
`null_placement_fields` counts affected references, not individual fields.
Distinct FormIDs at the same transform remain candidates, not an instruction
to delete either reference.

## Real-asset findings

The [aggregate evidence](../evidence/z-fighting-audit-20261008.json) records input,
script and private report hashes. The source conversion manifest and world
database hashes were unchanged at the end of the scan.

The complete material inventory read **25,388 GLBs** and **65,638 materials**.
There were no unreadable GLBs. It found:

| Authored requirement | Materials |
|---|---:|
| Decal | 3,964 |
| Dynamic_Decal | 3,882 |
| Depth writes disabled, opaque or cutout | 3,622 |
| Depth testing disabled | 14 |
| Missing source flag annotation | 1,657 |

Requirement counts overlap. There are **4,195 distinct materials** with one or
more listed requirements, across **2,765 models**. Across all alpha modes, 8,699 annotated materials have
depth writes disabled; 5,077 of those use ordinary alpha blending and are not
counted as opaque/cutout depth-write gaps.

The database inventory read **861,730 modeled references**. It excluded 9,399
deleted/initially-disabled references and reported 29,325 references with unknown
conditional enable states. It found **1,196 duplicate-placement groups**,
containing **1,217 extra placements**. These are exact-transform candidates;
base records and live enable state still need checking before any fix.

A focused geometry run completed all **31 selected models**, checking **24,255
triangles** and finding **62 overlapping pairs**. Representative results:

- `architecture/farmhouse/farmhouse01.glb`: 8 pairs. One pair is in shape block
  15, shader block 16, triangles 658 and 198, with approximately zero plane
  separation and overlap area 1.7363 scene units squared.
- `landscape/roads/roadstraight01.glb`: 2 pairs, including an overlap between
  shape blocks 7 and 10. Its maximum plane separation is about `7.9e-7` scene
  units and overlap area about 101.149 scene units squared.

These results show why neither "flag every repeated triangle" nor "apply bias
to every overlap" is sufficient: overlaps exist within ordinary authored
shapes, between shapes, and between placements. Compare source geometry and a
retail frame before classifying one as a Mudcrab defect.

The full geometry scan then selected every model and ran with four workers, a
100,000-triangle limit and a 200,000-comparison limit per file. It finished in
550.014 seconds, with no input errors:

- **22,632 models** completed geometry checks, covering **16,917,992 triangles**.
- **618,071 overlapping pairs** were observed across **3,126 models**. This
  includes pairs retained from searches that later hit their budget; it is a
  lower bound on candidates, not a visible-defect count.
- **2,756 models** remain incomplete: 2,753 hit the comparison limit and three
  hit the triangle limit. The JSON names every incomplete model and reason.

The audit exited `2`, as intended for incomplete coverage. Source annotations
and conditional enable states also remain coverage gaps. Larger budgets can
extend the geometry search, but they cannot replace the runtime and retail
checks below.

## First renderer fix: authored decals and depth state

The user confirmed widespread flickering in the Fort Sungard development build.
The selected `impextwalldivider01.glb` contains 238 overlapping triangle pairs,
including identical wall/decal triangles. Its cutout overlay has Decal and
Dynamic_Decal flags, depth testing enabled, and depth writes disabled. The
baseline rendered it as ordinary depth-writing cutout geometry.

The [native material hook](../../crates/engine/src/nif_material.rs) now constructs
a [depth material](../../crates/engine/src/nif_depth.rs) for tagged decals,
depth-disabled materials, and opaque/cutout materials without depth writes:

- Use authored depth testing and writing in the color pass. Disabling testing
  also disables writing, matching the recovered disabled depth mode.
- Draw these surfaces in the sorted color pass after opaque/cutout receivers.
  Keep the base material's authored alpha mode in its GPU data: opaque alpha
  stays opaque, and cutout fragments still discard below their threshold.
- Apply a positive constant raster bias of four reversed-Z depth units only
  to authored decals. This is a tested renderer policy; the native numeric
  bias had not been recovered at this stage. Geometry and source asset files are unchanged.
- Exclude these surfaces from the camera depth prepass. Exclude decals and
  depth-disabled effects from shadow casting. Other materials that disable
  camera depth writes retain the stock shadow-depth pipeline.
- Retain the native base material for asset validation and collision
  classification. Retain the stock source dependency when cloning scenes.
  Generic glTF and ordinary depth-writing opaque/cutout materials keep their
  existing path; ordinary depth-tested alpha blending already avoids writes.

The hook caches depth state per glTF load because Bevy creates a child load
context for scene construction. Reading the parent material directly during
the scene hook failed the first GPU run; the corrected cache passes scene-only
loading and cloning.

The new [GPU regression probe](../../crates/engine/examples/material_depth_probe.rs)
loads synthetic glTF through the production hook and scene spawner. It renders
coplanar and slightly recessed decals, reverses source order between panels,
and uses different triangle diagonals. Each view checks a vertex-alpha cutout
hole, a visible decal, and a closer opaque occluder. It retains the production
depth prepass and uses nine perspective views at distances 4, 8 and 12, with
angles up to 60 degrees. No Skyrim assets are needed:

```sh
cargo run --profile quick -p engine --example material_depth_probe -- \
  --output target/z-fighting/depth-fixed
cargo run --profile quick -p engine --example material_depth_probe -- \
  --output target/z-fighting/depth-legacy --legacy-depth
```

On Apple M1 Pro / Metal, the fixed path passed all **54 samples** and its scene
binding checks. The legacy control failed in **8 of 9 views** and exited `1` as
expected. PNGs and per-sample JSON are retained under ignored
`target/z-fighting/depth-probe-v2/`. The engine suite passes **331 tests**,
including depth-state, malformed-annotation and scene-cloning regressions.

These checks verify authored depth state at the selected GPU poses. The user
still reported flicker on Fort Sungard's stone walls and dirt/moss patches at
this stage; the [second renderer fix below](#second-renderer-fix-decal-slope-bias)
records the later fix and scoped visual acceptance.
Duplicate placements, ordinary shape overlaps, terrain/road intersections and
LOD transitions also remain candidates. The full surface-ID detector below
remains work.

## Second renderer fix: decal slope bias

The follow-up REA queries recovered raster-state creation at `0x14102b6c0`
and decal-pass selection at `0x14151fd60`. The latter selects bias modes 6/8
for accumulation hints 3/2 when native bias is enabled; its alternate camera
path selects modes 7/9. The raster descriptor's field order matches
[D3D11_RASTERIZER_DESC](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ns-d3d11-d3d11_rasterizer_desc).
Modes 6/8 use constant bias `-1`, slope bias `-0.65` and clamp `-100`.
The slope term depends on screen-space depth slope, rather than being a fixed
mesh displacement. [Microsoft's depth-bias formula](https://learn.microsoft.com/en-us/windows/win32/direct3d11/d3d10-graphics-programming-guide-output-merger-stage-depth-bias)
also explains why constant-bias units depend on the depth format and values.

The state application at `0x141010de0` uses bias mode `0x1420cfca0` to
select the raster state and adjust viewport maximum depth. The viewport table
is copied from `0x141a76790` by `0x14102b680`; modes 6/8 contain offsets
`0.000026`/`0.000028`. Mode `0x1420cfcb0` is a blend-state selector and must
not be mistaken for the raster-bias mode.

Eight additional function listings, containing **1,251 instructions**, match
both the original PE source mappings and an independent LLVM decoder. The
viewport constants and clamp also match the original PE bytes. Private queries,
run manifests and cross-checks are retained under ignored
`target/z-fighting/native-raster/` and `target/z-fighting/raster-crosscheck.json`.
They use the same verified executable and read-only project as the first fix.

The renderer now adds a slope bias of **+0.65** to authored decals in Bevy's
reversed-Z color pass. This adapts the recovered negative native slope bias;
it retains the first fix's small constant bias of four. Ordinary materials
receive neither bias. Native constant units, viewport offsets and camera
conventions have not been mapped to Bevy, so this is not complete numeric
parity or proof that the user's remaining flicker has been resolved.

### Real-wall coverage check

[wall_depth_probe.rs](../../crates/engine/examples/wall_depth_probe.rs) loads
Fort Sungard's `impextwalldivider01.glb` through the production hook. It retains
source positions, transforms, normals, UVs, vertex alpha and texture alpha,
then uses red stone, green decals and blue trim to identify coverage losses.
It uses the placement of reference `0001B0EE`, rebased world coordinates,
MSAA off, the depth prepass and GPU occlusion culling.

All 238 decal triangles have exact matching receiver triangles. The probe
renders receiver IDs, decal IDs, isolated decal coverage and the composite at
each pose. Matching IDs restrict the comparison to pixels over the same
receiver triangle; nearer faces around a corner are excluded. Diagnostic ID
colors use independent vertices without moving their positions or changing
alpha. A one-pixel erosion excludes alpha and silhouette boundaries.

The check covers 12 views at distances 130, 250, 500 and 1,000 world units,
angles through +/-80 degrees and two small camera translations. The updated
path has **zero missing decal pixels across 561,467 matched pixels**. The
planted `--omit-decals` control fails all 12 views. The older constant-only
path and legacy path also pass this isolated wall at these poses. Therefore
this check establishes coverage and control sensitivity, but does not
reproduce or prove the cause of the user's full-scene flicker.

```sh
cargo run --profile quick -p engine --example wall_depth_probe -- \
  --assets /absolute/path/to/modern_assets --output target/z-fighting/wall-fixed
cargo run --profile quick -p engine --example wall_depth_probe -- \
  --assets /absolute/path/to/modern_assets --output target/z-fighting/wall-control \
  --omit-decals
```

The synthetic probe also passes all 54 samples with gameplay occlusion culling,
and another 54 samples at scale 128 in rebased Fort Sungard coordinates. It
continues to test masked holes and closer opaque occluders. Its legacy control
fails 8 of 9 views. The engine suite passes all 331 tests. Aggregate results
and build hashes are recorded in
[z-fighting-slope-fix-20261008.json](../evidence/z-fighting-slope-fix-20261008.json).

The manually tested build is under ignored `target/z-fighting/slope-build/`.
Run `run-fort-sungard.command` there to open the same Fort Sungard cell. Earlier
builds remain available for comparison. The packaged build's eight-second
windowed check rendered 326 frames with zero asset, material or renderer
validation failures; 290 instances were still pending at exit. This verifies
startup, not visual stability.

After receiving this build, the user reported no further flickering on the
inspected Fort Sungard stone walls and dirt/moss patches. This records a passing
in-game visual check for those surfaces.

The follow-up routes were Rorikstead's farmhouse, Whiterun's southern approach
road and Solitude docks. The user then reported no further visual issues and
asked to treat z-fighting as fixed until more is found. The issue is closed
provisionally on that basis. This records user acceptance without claiming
exhaustive world or camera-route coverage.

## GPU detector and acceptance contract

This specification is retained for investigation if more cases appear; it is
not implemented by the static audit. A diagnostic surface-ID path would:

1. Give each rendered surface a stable identity containing source FormID,
   model, NIF shape, primitive/triangle, material and render path. Terrain IDs
   include cell and patch; LOD IDs include chunk and level. Instancing must
   retain per-placement identity. Export the effective depth compare/write,
   raster bias, alpha discard and pass ordering for each draw.
2. Freeze scene time, simulation, particles, skin poses, streaming membership,
   weather, wind, TAA jitter and exposure after assets are ready. Record all
   frozen state. Draw the exact production positions and alpha discard into an
   ID/depth diagnostic target. A simplified solid-color shader must retain
   production vertex displacement and fragment discard.
3. Render a second surface layer while excluding the first winner's surface ID
   per pixel. Clear the second depth target so equal-depth fragments remain
   eligible. Retain the two effective depths, IDs, slopes and authored flags.
   Record incomplete allocation/readback or unsupported material paths as
   coverage failures.
4. Compare ordinary and reversed/randomized submission order for opaque/cutout
   competitors without changing their pipeline state or intentionally ordered
   passes. A winner change at the same frozen camera identifies an ordering
   dependency. Require distinguishable production surface output before calling
   it visible z-fighting. Blended surfaces need their own ordered-layer audit;
   changing blend order alone is not a z-fighting verdict.
5. Repeat small recorded camera translations/rotations and near/mid/far views.
   Evaluate effective depth separation in the actual target depth format, with
   raster bias and slope included. Attribute changes near competitor depths
   to the same pair. Exclude silhouettes, alpha edges and ordinary occlusion
   transitions using the geometry and coverage masks; retain ambiguity instead
   of converting it into a pass.
6. Export candidate masks, cropped production images, both surface identities,
   camera state, draw state, asset/executable hashes, and the reproduction route.
   Use confirmed candidates to choose matching unmodded retail views. Retail
   captures decide whether an overlap is a regression or native behavior.

Two surface layers expose nearest competitors in one view. Occluded surfaces
need further views or deeper peeling. One finite route cannot certify the entire
game. An acceptance result must name worldspaces/interiors, camera routes,
distance bands, renderer paths, resolution, GPU backend and render-state
variants covered. Sweep all placements in the declared content scope; use
geometry candidates to add targeted close and distant views. Report unavailable
cells/materials, unresolved enable states and unsupported paths separately.

Start regression routes at road overlays and farmhouse shapes, authored decal
families, rocks intersecting terrain, water boundaries and full-detail/LOD
transitions. Each test needs a planted positive control: a visibly different
coplanar overlay, duplicate placement, missing decal rule or simultaneous
full-detail/LOD surface. A detector that misses a control must fail acceptance.

The completion rule is zero unresolved confirmed artifacts, every candidate
either fixed or explained with source/retail evidence, and no coverage gaps in
the declared scope. Preserve that scope and evidence when reporting confidence;
do not replace it with an unqualified claim that no further issues exist.

## Fix ownership

- **Depth and decal rules:** extend the native material/render path using the
  verified source flags and native pass rules. Apply compatible behavior to
  color, depth prepass and shadows. Verify blend/cutout and special pass overrides.
  A constant in `StandardMaterial.depth_bias` alone does not reproduce the
  observed native behavior. The first fix uses an explicit renderer bias policy
  with GPU regression evidence. The second fix adds recovered slope handling;
  full numeric parity still requires mapping native raster/viewport state and
  camera conventions to Bevy.
- **Duplicate placements:** check winning records, parent enable rules and
  spawn lifecycle. Fix converter/load-order or streaming ownership if it
  creates duplicate live surfaces. Do not globally deduplicate distinct FormIDs.
- **Geometry differences:** compare the winning NIF to its GLB, including node
  hierarchy and transforms, triangle/strip conversion and alpha coverage. Fix
  conversion errors at their owner; preserve intentional authored layers.
- **Terrain/full-detail/LOD overlap:** record simultaneous live surfaces and
  transition ownership, then fix visibility or transition state. Per-model
  geometry scans cannot prove these paths clean.

The static investigation did not change assets or placements. This branch now
includes the detector, the renderer fix above, synthetic regressions, and this
evidence and acceptance specification.

## Validation

The new detector has 25 synthetic tests covering partial overlap, shared edges,
nonparallel planes, opposing faces, transforms including mirrors, index stride,
scene instances, incomplete geometry, bounded examples, exit codes and placement
scope. The initial audit run passed all 65 Python script tests with a physical
temporary path and GNU coreutils on this Mac:

```sh
TMPDIR=/private/tmp python3 -m unittest discover -s scripts/tests -p 'test_*.py'
```

The existing shell tests also require GNU `realpath` on PATH. Repeating the full
suite in the default macOS environment fails on the `/var` temporary-directory
symlink and BSD `realpath`; this is not the environment used for the initial
pass. The current detector-only run passes all 25 tests with `TMPDIR=/private/tmp`.
The detector itself needs Python 3.9 or later and no additional packages.

The renderer fix passes 331 engine tests and all engine examples compile.
The real-assets Fort Sungard startup rendered 327 frames with zero asset-load,
material-validation, or renderer-validation failures. It still had 283 pending
asset instances when the eight-second check ended, so this is startup evidence,
not complete content coverage or a performance acceptance run. Three subsequent
close-view captures settled, but surrounding geometry obscures the selected
triangle pair; those images do not verify its visual stability. Retail gameplay
and a whole-game zero-artifact acceptance result remain unverified.

### PR validation on current main (2026-10-09)

The PR branch was rebased onto main at `c39449b`. Workspace compilation,
formatting and strict Clippy pass. The detector's 25 tests pass. The Rust
commands used the locked, offline `quick` profile and the shared Cargo cache.
The [PR validation record](../evidence/z-fighting-pr-validation-20261009.json)
contains source hashes and aggregate results.

The unfiltered workspace run fails the unchanged converter test
`asset_path::tests::later_roots_override_and_same_root_collisions_fail`.
It expects two distinct filenames differing only by case; this Mac's
temporary filesystem creates one file. With that test skipped, all 1,101
other Rust tests pass, including 338 engine tests; 18 tests are ignored.
The filtered invocation's Criterion benchmark rejects the libtest `--skip`
argument. Its seven cases pass separately in benchmark test mode. Workspace
doctests also pass, with no doctests defined. These commands omit the shared
target directory path:

```sh
cargo test --locked --offline --profile quick --workspace --all-targets \
  --no-fail-fast -- --skip asset_path::tests::later_roots_override_and_same_root_collisions_fail
cargo test --locked --offline --profile quick -p dummy-content --bench writers -- --test
cargo test --locked --offline --profile quick --workspace --doc
```

Rebuilt headless GPU probes pass all 54 synthetic samples at each of two
scales. The legacy control fails eight of nine views as expected. The real-wall
probe has zero missing decal pixels across 561,467 matched pixels; omitting
decals fails all 12 views. The same fixture and camera pose are retained as
[legacy-control](../evidence/z-fighting-synthetic/legacy-control.png) and
[authored-depth](../evidence/z-fighting-synthetic/authored-depth.png) images.
The control removes the decal's authored flags to select the old material path.
Manual acceptance remains associated with the archived slope-fix build.
